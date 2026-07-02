//! The Sunshine integer-exact pacing loop (`run`) and the per-tick frame acquisition
//! (`snapshot`). Split out of `device.rs` to keep both files cohesive and under the line
//! budget; behaviour is unchanged from the original `dxgi.rs`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
	IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
	DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

use super::device::{Capture, CaptureDevice, RunExit};
use super::platform::HiResTimer;
use crate::Frame;

impl CaptureDevice {
	/// The Sunshine pacing loop. Calls `on_frame` once per paced client tick with the
	/// CURRENT pool texture (fresh capture OR reused last frame), so the encoder sees a
	/// steady cadence even on a static desktop. Returns `RunExit::Stop` when `stop` is set or
	/// after a failed reinit, or `RunExit::Switch(idx)` when the host requested a different
	/// monitor (the caller rebuilds capture+encode on it — the new output may be on another GPU).
	pub unsafe fn run(
		&mut self,
		client_fps: u32,
		draw_cursor: bool,
		// Live monitor switch: when the host writes an output index here (≠ u32::MAX and ≠ the
		// current output), we return `RunExit::Switch(idx)` so the caller rebuilds on it.
		requested_output: &AtomicU32,
		stop: &AtomicBool,
		mut on_frame: impl FnMut(&Frame),
	) -> RunExit {
		let fps = client_fps.max(1);
		// NOTE: the display keep-alive (Sunshine SetThreadExecutionState) and the TIME_CRITICAL
		// thread priority are engaged ONCE for the whole capture-thread life by `lib.rs`'s thread
		// body (`_wake` / `_prio`). We must NOT re-engage them here: SetThreadExecutionState is
		// thread-global sticky state (not a refcount), so a second guard dropped when `run` returns
		// (a monitor Switch or a Stop) would CLEAR the keep-awake request that the outer lib.rs
		// guard is still logically holding — letting the panel power-save mid-rebuild and (on some
		// drivers) drop Desktop Duplication, compounding the switch stall.
		// Integer-exact frame interval in ns. We accumulate against a QPC anchor rather
		// than sleeping a fixed amount each loop, so rounding never drifts the cadence.
		let interval_ns: i64 = 1_000_000_000i64 / fps as i64;

		let timer = match HiResTimer::new() {
			Ok(t) => t,
			// Without the hi-res timer we'd be stuck at ~64 fps; bail so the host can fall
			// back to ffmpeg rather than silently stream at the wrong rate.
			Err(_) => return RunExit::Stop,
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

		// Native dimensions and rotation the ENCODER was built for (the duplication mode at build
		// time, set by build_duplication before run() is called). A host display MODE change
		// invalidates the duplication (ACCESS_LOST → Capture::Reinit); after the reinit we compare
		// against these to distinguish a RESOLUTION or ROTATION change (rebuild the encoder — it is
		// sized to the old dims and bakes the old rotation) from a refresh-rate-only change (keep
		// the encoder; just force one IDR so the client re-syncs).
		let built_w = self.dup_desc.ModeDesc.Width;
		let built_h = self.dup_desc.ModeDesc.Height;
		// Rotation at build time (degrees CW: 0/90/180/270) — from the same dup_desc. DXGI
		// keeps Width/Height UNCHANGED on a rotation-only change (it always reports the unrotated
		// scan-out surface; orientation lives in dup_desc.Rotation), so a rotation change is
		// INVISIBLE to the Width/Height comparison below without this extra baseline.
		let built_rotation = self.rotation_deg();
		// Set after a same-resolution reinit; the next emitted Frame carries it so the encoder
		// forces an IDR (the client dropped the GOP it was mid-decode of during the blip).
		let mut force_next_idr = false;

		while !stop.load(Ordering::Relaxed) {
			// Live MONITOR switch (session-menu picker → CaptureHandle::switch_output): hand the
			// requested output back to the caller, which rebuilds capture+encode on it. We do NOT
			// re-duplicate in place — the new monitor may be owned by a different GPU, and a D3D11
			// device can only DuplicateOutput a monitor on its own adapter (the cross-GPU bug).
			// ATOMIC read-and-clear (`swap`), NOT load-then-store: the host writes this atom from
			// another thread (CaptureHandle::switch_output). The old load-then-store was a TOCTOU —
			// a switch request landing BETWEEN the load and the clear was wiped by the clear, so it
			// was LOST and the stream stayed on the old monitor until a *different* monitor re-armed
			// the atom (the "switch takes 3-4 s / never changes, fixed by switching elsewhere"
			// outlier). swap consumes exactly the value it reads. A request for the monitor we are
			// already on is still correctly a no-op.
			let req_out = requested_output.swap(u32::MAX, Ordering::AcqRel);
			if req_out != u32::MAX && req_out != self.output_idx {
				return RunExit::Switch(req_out);
			}

			let now0 = self.qpc.now_ns();
			let mut deadline = start + frame_no * interval_ns;
			if now0 - deadline > 2 * interval_ns {
				// Big overrun (a stall / long IDR encode / scheduler hiccup): re-anchor the
				// cadence to NOW instead of bursting several catch-up frames back-to-back —
				// the client renders a catch-up burst as a jump+gap latency SPIKE. One long
				// tick now costs ~1 frame, not a burst.
				start = now0 - frame_no * interval_ns;
				deadline = now0;
			}
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
					// Mode/format/access change: hybrid-GPU output reparenting, a host RESOLUTION
					// change, a refresh-rate change, or a transient ACCESS_LOST (UAC secure desktop,
					// fullscreen app) — all invalidate the duplication. Rebuild it, then decide.
					// Don't emit a frame this tick.
					self.teardown_duplication();
					// Snapshot the streamed output index BEFORE reinit(): its find_output can fall
					// back to a DIFFERENT monitor (index 0) when the streamed one was hot-unplugged,
					// silently changing self.output_idx.
					let prev_output_idx = self.output_idx;
					if self.reinit().is_err() {
						// Couldn't rebuild — give the host a chance to restart us.
						return RunExit::Stop;
					}
					// reinit() rebuilt the duplication at the (possibly new) mode. If the RESOLUTION
					// or ROTATION changed, the encoder + its NV12 target / VideoProcessor are sized
					// to the OLD dimensions and bake the OLD rotation (Encoder.rotation is captured
					// once at build time and drives VideoProcessorSetStreamRotation every frame).
					// Bail so the capture loop (lib.rs) rebuilds capture+encoder at the new size/
					// rotation — that path also forces a fresh IDR burst, so the client re-syncs
					// cleanly. If only the refresh rate changed (same WxH, same rotation) the
					// encoder is still valid: keep it (no costly NVENC re-open) but force ONE IDR
					// on the next frame so the client re-syncs after the blip instead of freezing
					// until the next safety GOP.
					//
					// NOTE: DXGI keeps Width/Height UNCHANGED on a rotation-only change (it reports
					// the unrotated scan-out surface; orientation lives in dup_desc.Rotation), so
					// without the rotation check a landscape↔portrait flip slips through as a
					// "rate-only reinit" and the stream permanently bakes the stale orientation.
					//
					// Also treat an output-index CHANGE as a switch even when WxH/rotation are
					// identical (two same-mode monitors): reinit() re-targeted a different output, so
					// the outer lib.rs loop must rebuild to republish current_output()/build_gen —
					// otherwise the host input path stays mapped to the vanished monitor's rect and
					// absolute clicks land at the wrong virtual-desktop coordinates for the session.
					if self.output_idx != prev_output_idx
						|| self.dup_desc.ModeDesc.Width != built_w
						|| self.dup_desc.ModeDesc.Height != built_h
						|| self.rotation_deg() != built_rotation
					{
						return RunExit::Switch(self.output_idx);
					}
					force_next_idr = true;
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
					let prev_output_idx = self.output_idx;
					if self.reinit().is_err() {
						return RunExit::Stop;
					}
					// If reinit() fell back to a different monitor (streamed output gone), rebuild via
					// the outer loop so current_output()/build_gen + the host input rect follow it —
					// same silent-desync guard as the Reinit arm above.
					if self.output_idx != prev_output_idx {
						return RunExit::Switch(self.output_idx);
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
						// Consume the one-shot: the first frame emitted after a same-res reinit
						// carries the forced-IDR request; later frames clear it.
						force_idr: std::mem::take(&mut force_next_idr),
					};
					on_frame(&frame);
				}
			}
		}
		// `stop` was set — the session is ending.
		RunExit::Stop
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
