//! The Sunshine integer-exact pacing loop (`run`) and the per-tick frame acquisition
//! (`snapshot`). Split out of `device.rs` to keep both files cohesive and under the line
//! budget; behaviour is unchanged from the original `dxgi.rs`.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

use super::device::{Capture, CaptureDevice};
use super::platform::HiResTimer;
use super::{raise_thread_priority, DisplayKeepAlive};
use crate::Frame;

impl CaptureDevice {
    /// The Sunshine pacing loop. Calls `on_frame` once per paced client tick with the
    /// CURRENT pool texture (fresh capture OR reused last frame), so the encoder sees a
    /// steady cadence even on a static desktop. Returns when `stop` is set, or after a
    /// failed reinit.
    pub unsafe fn run(
        &mut self,
        client_fps: u32,
        draw_cursor: bool,
        stop: &AtomicBool,
        mut on_frame: impl FnMut(&Frame),
    ) {
        let fps = client_fps.max(1);
        // Keep the display awake (Sunshine SetThreadExecutionState) so an idle host doesn't sleep
        // the panel mid-stream, and raise this thread's priority so the pacing sleep wakes on time
        // (jitter on the wake-up directly shows as frame-cadence jitter on the client).
        let _keepalive = DisplayKeepAlive::engage();
        raise_thread_priority();
        // Integer-exact frame interval in ns. We accumulate against a QPC anchor rather
        // than sleeping a fixed amount each loop, so rounding never drifts the cadence.
        let interval_ns: i64 = 1_000_000_000i64 / fps as i64;

        let timer = match HiResTimer::new() {
            Ok(t) => t,
            // Without the hi-res timer we'd be stuck at ~64 fps; bail so the host can fall
            // back to ffmpeg rather than silently stream at the wrong rate.
            Err(_) => return,
        };

        // Has the pool texture ever held a real frame? Until the first successful acquire
        // we have nothing to encode, so we skip on_frame on pure-timeout startup.
        let mut have_content = false;

        // QPC anchor: the deadline for frame N is anchor + N*interval. Pure integer math.
        // Mutable so we can re-anchor after a reinit (see the Capture::Reinit/Error arms),
        // otherwise the post-reinit deadlines are all in the past and the loop bursts to
        // catch up at the wrong cadence.
        let mut start = self.qpc.now_ns();
        let mut frame_no: i64 = 0;

        while !stop.load(Ordering::Relaxed) {
            let deadline = start + frame_no * interval_ns;
            frame_no += 1;

            // 1. Grab a frame, WAITING up to this frame's deadline for a real screen/pointer
            //    update. A 0 ms poll (the old code) returns WAIT_TIMEOUT the instant DXGI has
            //    nothing queued *right now* and reuses the stale surface — on hybrid-GPU hosts
            //    DXGI hands updates over sluggishly, so new content (cursor moves, typed text)
            //    got sampled at only ~2-3 Hz while the stream padded to the paced fps → the
            //    "cursor teleports / typing comes in bursts, background looks smooth" stutter.
            //    Blocking up to the deadline lets the about-to-arrive update land in THIS tick;
            //    we still fall back to the reused surface on a genuine timeout. Pacing to the
            //    deadline is preserved by the post-acquire sleep below.
            let now = self.qpc.now_ns();
            let wait_ms = if deadline > now {
                (((deadline - now) / 1_000_000).min(interval_ns / 1_000_000)) as u32
            } else {
                0
            };
            let cap = self.snapshot(wait_ms, draw_cursor);

            // 2. Pace: if the real frame arrived before the deadline, sleep the remainder so
            //    the encoder still sees an even cadence (hi-res; returns at once if we're late).
            let now2 = self.qpc.now_ns();
            if deadline > now2 {
                timer.sleep_for(std::time::Duration::from_nanos((deadline - now2) as u64));
            }

            match cap {
                Capture::Frame => {
                    have_content = true;
                }
                Capture::Timeout => {
                    // No change within this interval. Encode the reused surface (is_new=false)
                    // so the client still gets a paced stream at the target fps.
                }
                Capture::Reinit => {
                    // Mode/format/access change (incl. hybrid-GPU output reparenting).
                    // Tear down + rebuild, then resume. Don't emit a frame this tick.
                    self.teardown_duplication();
                    if self.reinit().is_err() {
                        // Couldn't rebuild — give the host a chance to restart us.
                        return;
                    }
                    // The size may have changed; refresh and re-anchor the clock so the
                    // next deadlines aren't computed against a stale start. Without this,
                    // the reinit backoff/retry sleeps push every elapsed deadline into the
                    // past and the loop fast-forwards a catch-up burst at the wrong cadence.
                    start = self.qpc.now_ns();
                    frame_no = 0;
                    continue;
                }
                Capture::Error(_e) => {
                    // One reinit retry on an unclassified error, then exit cleanly.
                    self.teardown_duplication();
                    if self.reinit().is_err() {
                        return;
                    }
                    // Re-anchor the pacing clock after the reinit sleeps, as above.
                    start = self.qpc.now_ns();
                    frame_no = 0;
                    continue;
                }
            }

            // 3. Hand the encoder the current pool texture. Valid only for this callback
            //    (the texture is reused next tick). `is_new` distinguishes fresh vs reuse.
            //    Read w/h from dup_desc each tick so a post-Reinit resolution change is
            //    reflected (the pool was rebuilt to the new size by build_pool).
            if have_content {
                let is_new = self.last_was_new;
                let w = self.dup_desc.ModeDesc.Width;
                let h = self.dup_desc.ModeDesc.Height;
                // Pull the LIVE cursor position (cheap user32 call) so the cursor tracks the OS
                // pointer at full tick rate instead of DXGI's sluggish PointerPosition cadence.
                if draw_cursor {
                    self.refresh_live_cursor();
                }
                // Composite the cursor onto a fresh pool→present copy EVERY tick (covers the
                // Capture::Frame and the Capture::Timeout reuse path alike, so a cursor over a
                // static desktop still moves). Returns `present` when the cursor was drawn,
                // else the clean `pool` — either way a valid BGRA texture the encoder reads.
                if let Some(texture) = self.composite_cursor(draw_cursor) {
                    let frame = Frame {
                        texture,
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        width: w,
                        height: h,
                        is_new,
                    };
                    on_frame(&frame);
                }
            }
        }
    }

    // ── pacing-loop internals ──────────────────────────────────────────────

    /// Acquire one frame (with `timeout_ms`), copy it into the pool texture, optionally
    /// composite the cursor, and release the DXGI frame. Classifies the HRESULT into a
    /// `Capture` variant the loop acts on.
    pub(super) unsafe fn snapshot(&mut self, timeout_ms: u32, draw_cursor: bool) -> Capture {
        let dup = match self.dup.as_ref() {
            Some(d) => d.clone(),
            None => return Capture::Reinit,
        };
        let pool = match self.pool.as_ref() {
            Some(p) => p.clone(),
            None => return Capture::Reinit,
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // AcquireNextFrame: 0 ms ⇒ return WAIT_TIMEOUT immediately if nothing changed.
        match dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
            Ok(()) => {}
            Err(e) => {
                self.last_was_new = false;
                return match e.code() {
                    c if c == DXGI_ERROR_WAIT_TIMEOUT => Capture::Timeout,
                    // ACCESS_LOST = another app took exclusive fullscreen / the desktop
                    // switched (UAC secure desktop); rebuild the duplication.
                    c if c == DXGI_ERROR_ACCESS_LOST => Capture::Reinit,
                    c if c == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE => Capture::Reinit,
                    _ => Capture::Error(e),
                };
            }
        }

        // ── cursor cache update (cheap; only when we'll actually draw it) ────────────
        // DXGI reports pointer position/visibility (LastMouseUpdateTime) and a NEW shape
        // (PointerShapeBufferSize) on this frame_info. We refresh the cache here, while we
        // still hold the acquired frame, but draw it later (after ReleaseFrame) from the
        // cache. On any cursor-path error we just stop drawing the cursor — capture is
        // never broken (the `let _ =` swallows + we leave the previous cache intact).
        if draw_cursor {
            let _ = self.update_cursor_cache(&dup, &info);
        }

        // Even on a "frame", LastPresentTime == 0 means only the mouse moved (no desktop
        // change). We still treat it as a frame so cursor motion shows, but mark is_new
        // off the accumulated-frames count so the encoder can prefer a tiny P-frame.
        let desktop_changed = info.LastPresentTime != 0;

        // Copy the acquired surface into our pool texture so we can ReleaseFrame right
        // away (DXGI only holds ONE frame; not releasing promptly stalls capture).
        let copy_res = (|| -> windows::core::Result<()> {
            if let Some(res) = resource.as_ref() {
                let src: ID3D11Texture2D = res.cast()?;
                // Straight GPU→GPU blit (same B8G8R8A8 format) on the immediate context.
                self.context.CopyResource(&pool, &src);
            }
            Ok(())
        })();

        // Always release, even if the copy failed — otherwise the next AcquireNextFrame
        // deadlocks.
        let _ = dup.ReleaseFrame();

        if let Err(e) = copy_res {
            self.last_was_new = false;
            return Capture::Error(e);
        }

        // Cursor compositing is NOT done here. `pool` is kept as the CLEAN desktop; the
        // cursor is blended onto a separate `present` texture in `composite_cursor()`,
        // which `run()` calls on EVERY emitted tick — including the static-desktop
        // Capture::Timeout reuse path — so a cursor moving over an unchanged desktop still
        // animates and never smears a stale copy into the pool. We only refreshed the cache
        // (position + shape) above while the frame was held.

        self.last_was_new = desktop_changed;
        Capture::Frame
    }
}
