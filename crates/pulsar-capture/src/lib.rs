//! Pulsar native host capture+encode crate (Windows-only).
//!
//! Replaces the host `ddagrab → h264_nvenc` ffmpeg path with native DXGI Desktop
//! Duplication capture → the NVENC SDK → a hand-rolled RTP packetizer (the Sunshine
//! technique, no ffmpeg). ffmpeg stays as the fallback: this crate is *only* engaged
//! when the host's NVENC branch is selected, and any init failure makes the host fall
//! straight back to ffmpeg.
//!
//! ## Crate layout (the seam three implementers agree on)
//! - `lib.rs`  (this file) — public API + the capture/encode thread + the init handshake,
//!   and the **shared types** (`Frame`) that `dxgi.rs` ↔ `encode.rs` exchange.
//! - `dxgi.rs` — `CaptureDevice` (owns the D3D11 device) + the Sunshine pacing loop.
//! - `encode.rs` — `Encoder` (borrows the device) — NVENC SDK session + `rtp.rs` packetizer.
//!
//! The whole crate is gated `#![cfg(windows)]`-style: on non-Windows targets every public
//! item degrades to a stub so the Cargo workspace still builds everywhere (the host only
//! *calls* into it under `#[cfg(windows)]`, but keeping the symbols present means the crate
//! can live in `members` unconditionally without breaking `cargo check` on mac/Linux).
#![allow(clippy::missing_safety_doc)]

use std::io;

// ===========================================================================
// Public, target-independent API surface (present on every platform)
// ===========================================================================

/// Everything the native path needs, derived 1:1 from `on_stream`'s effective values.
#[derive(Clone, Debug)]
pub struct CaptureConfig {
	/// `eff_w`  — 0 ⇒ use the duplicated output's native width.
	pub width: u32,
	/// `eff_h`  — 0 ⇒ native height.
	pub height: u32,
	/// `eff_fps` — client cadence; drives the pacing loop.
	pub fps: u32,
	/// `eff_bitrate` in kbps.
	pub bitrate_kbps: u32,
	/// e.g. `"rtp://10.0.0.5:9000"` — built by the caller (same string as `plan.dest`).
	pub dest: String,
	/// `H264` | `H265` | `Av1`. v1 implements H264; others should be rejected by the
	/// caller so the host takes the ffmpeg branch.
	pub codec: Codec,
	/// Monitor index, 0 (matches `ddagrab=output_idx=0`).
	pub output_idx: u32,
	/// `plan.low_latency` → preset/tune/rc selection.
	pub low_latency: bool,
	/// Composite the cursor (v1: parsed but no-op unless `cursor.rs` landed).
	pub draw_mouse: bool,
}

/// Video codec selector. The native NVENC path implements all three (`H264`, `H265`,
/// `Av1`); AV1 encode needs an Ada/Ampere+ NVENC (RTX 40) — on older GPUs init fails
/// cleanly and the host falls back to ffmpeg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
	H264,
	H265,
	Av1,
}

/// Owns the capture+encode thread. `stop()` or `Drop` tears everything down deterministically
/// (releases the NVENC session + DXGI duplication by joining the thread).
pub struct CaptureHandle {
	stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
	thread: Option<std::thread::JoinHandle<()>>,
	/// Stage-3 adaptive bitrate: the host controller writes the target kbps here; the capture
	/// thread reads it each tick and applies it live via `Encoder::reconfigure_bitrate` on change.
	requested_kbps: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl CaptureHandle {
	/// Set the live target bitrate (kbps). Lock-free: stores into the atom the capture thread
	/// polls each tick — never blocks the caller or stalls encode. No-op if unchanged.
	pub fn set_bitrate(&self, kbps: u32) {
		self.requested_kbps
			.store(kbps.max(1), std::sync::atomic::Ordering::SeqCst);
	}

	/// Signal the thread, join it (releases the NVENC session + DXGI duplication), return.
	pub fn stop(mut self) {
		self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

impl Drop for CaptureHandle {
	fn drop(&mut self) {
		self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

// Pure-Rust RTP/H.264 packetizer (RFC 6184), byte-for-byte compatible with the
// client depacketizer in `src/lib/h264.ts`. Platform-independent (std::net + rand),
// so it builds and is unit-tested on every target.
pub mod rtp;
pub use rtp::{RtpEgress, RtpSender};

// ===========================================================================
// Windows implementation
// ===========================================================================
#[cfg(windows)]
pub(crate) mod dxgi;
#[cfg(windows)]
mod encode;

// Shared frame type lives here so both dxgi.rs (producer) and encode.rs (consumer) import
// it from the crate root and cannot drift. Only meaningful on Windows (it borrows D3D11/DXGI
// interface types), so it is itself Windows-gated.
#[cfg(windows)]
pub use frame::Frame;

#[cfg(windows)]
mod frame {
	use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
	use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

	/// One captured surface handed from `dxgi.rs` → `encode.rs` each paced tick.
	///
	/// The texture is owned by `dxgi.rs`'s pool and only valid for the duration of the
	/// `on_frame` callback. Because the pacing loop calls `on_frame` synchronously and
	/// `Encoder::submit` runs to completion before the loop advances, the borrow never
	/// outlives the producer's hold on the texture — so in v1 no keyed mutex is required
	/// (capture and encode share one device on one thread). See the locking note in dxgi.rs.
	pub struct Frame<'a> {
		/// Pool BGRA texture (`DEFAULT`, `RENDER_TARGET | SHADER_RESOURCE`); the source for
		/// the BGRA→NV12 `VideoProcessorBlt` that `encode.rs` performs.
		pub texture: &'a ID3D11Texture2D,
		/// `DXGI_FORMAT_B8G8R8A8_UNORM` (SDR). HDR is deferred to the ffmpeg fallback in v1.
		pub format: DXGI_FORMAT,
		/// Post-rotation presented width.
		pub width: u32,
		/// Post-rotation presented height.
		pub height: u32,
		/// `false` on the timeout-reuse path (the desktop was static — we still re-encode the
		/// last surface so the client sees a steady fps and NVENC emits a tiny P-frame).
		pub is_new: bool,
	}
}

/// Spawn the dedicated capture+encode thread.
///
/// All fallible init (DXGI device + duplication + NVENC open + RTP header) happens *inside*
/// the thread, but its `Result` is sent back over a bounded channel that this function blocks
/// on (≤ ~3s). So `Ok(handle)` means "streaming has started"; `Err(e)` means "init failed —
/// the host should fall back to ffmpeg".
#[cfg(windows)]
pub fn start_capture_encode(cfg: CaptureConfig) -> io::Result<CaptureHandle> {
	use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
	use std::sync::Arc;

	let stop = Arc::new(AtomicBool::new(false));
	// Bounded (capacity 1) sync channel carries the one-shot init result back to the caller.
	let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
	let stop_t = stop.clone();
	// Stage-3 adaptive bitrate: shared target the host controller writes (set_bitrate) and the
	// capture thread polls each tick, applying changes live via Encoder::reconfigure_bitrate.
	let requested_kbps = Arc::new(AtomicU32::new(cfg.bitrate_kbps.max(1)));
	let req_kbps_t = requested_kbps.clone();

	let thread = std::thread::Builder::new()
		.name("pulsar-capture".into())
		.spawn(move || unsafe {
			// 1. Thread priority + display-keepalive (both RAII / restored on scope exit).
			//    TIME_CRITICAL keeps the pacing loop off the scheduler's slow path; the
			//    display-keepalive prevents the monitor sleeping mid-stream.
			let _prio = ThreadPriorityGuard::time_critical();
			let _wake = DisplayKeepAlive::engage();

			// 2. Build capture (DXGI). May fail: no display owner / E_ACCESSDENIED on a
			//    locked or secure desktop / session-0. On failure we report and bail so the
			//    host falls back to ffmpeg.
			let mut cap = match dxgi::CaptureDevice::create(cfg.output_idx) {
				Ok(c) => c,
				Err(e) => {
					let _ = init_tx.send(Err(format!("dxgi init: {e:?}")));
					return;
				}
			};
			let (w, h) = cap.target_size(cfg.width, cfg.height);
			// Native (full) capture size — the encoder sizes the cross-adapter bridge +
			// VideoProcessor input to this, scaling native→encode in the Blt when they differ.
			let (cap_w, cap_h) = cap.native_size();
			let rotation = cap.rotation_deg();

			// 3. Build the encoder on the SAME device + immediate context as capture
			//    (Strategy A: the NVENC session is opened on this D3D11 device; the
			//    driver does any cross-adapter copy internally on hybrid GPUs).
			let mut enc = match encode::Encoder::new(
				&cap.device,
				&cap.context,
				&encode::EncParams {
					width: w,
					height: h,
					capture_width: cap_w,
					capture_height: cap_h,
					fps: cfg.fps.max(1),
					bitrate_kbps: cfg.bitrate_kbps,
					dest: cfg.dest.clone(),
					codec: cfg.codec,
					low_latency: cfg.low_latency,
					rotation,
				},
			) {
				Ok(e) => e,
				Err(e) => {
					let _ = init_tx.send(Err(format!("nvenc init: {e}")));
					return;
				}
			};

			// 4. Init OK — unblock the caller. From here, failures only reinit (handled in
			//    dxgi::run) or exit the thread cleanly; the next StreamReq re-runs the branch.
			let _ = init_tx.send(Ok(()));

			// 5. Pacing loop (Sunshine technique). `on_frame` is invoked once per paced client
			//    tick with the CURRENT pool texture (fresh capture OR reused last frame). The
			//    closure converts BGRA→NV12, encodes via NVENC, and HANDS the Annex-B access
			//    unit to the RTP sender thread (`RtpEgress`) — the network send no longer runs
			//    inline here, so capture/encode never blocks on the socket.
			let mut pts: i64 = 0;
			let mut last_kbps = cfg.bitrate_kbps.max(1);
			cap.run(cfg.fps.max(1), cfg.draw_mouse, &stop_t, |frame: &Frame| {
				// Stage-3 adaptive bitrate: apply any pending target BEFORE encoding this
				// tick — a cheap live nvEncReconfigureEncoder (no re-init). Lock-free poll;
				// on a reconfigure error keep the old bitrate so the stream never stalls.
				let want = req_kbps_t.load(Ordering::Relaxed);
				if want != last_kbps {
					match enc.reconfigure_bitrate(want) {
						Ok(()) => last_kbps = want,
						Err(e) => dbg_capture(&format!("reconfigure {want} kbps: {e}")),
					}
				}
				if let Err(e) = enc.submit(frame, pts) {
					dbg_capture(&format!("encode: {e}"));
				}
				pts += 1;
			});

			// 6. Tear down NVENC (no drain needed — SYNC, no B-frames) and release the
			//    AddRef'd D3D11 device LAST. Dropping the Encoder also drops its `RtpEgress`,
			//    which closes the mailbox + joins the `pulsar-rtp-send` thread (≤ the socket
			//    write timeout, so a wedged socket can't hang teardown); raw RTP has no muxer,
			//    so there is no trailer to write.
			enc.flush_and_close();
		})?;

	// Block on the init handshake (bounded). If the thread died before sending, treat as Err.
	match init_rx.recv_timeout(std::time::Duration::from_secs(3)) {
		Ok(Ok(())) => Ok(CaptureHandle {
			stop,
			thread: Some(thread),
			requested_kbps,
		}),
		Ok(Err(msg)) => {
			// Thread reported an init error and is returning; join to reap it.
			let _ = thread.join();
			Err(io::Error::new(io::ErrorKind::Other, msg))
		}
		Err(_) => {
			// Timed out (or the sender was dropped without a value): tell the thread to stop,
			// join it, and report so the host falls back to ffmpeg.
			stop.store(true, std::sync::atomic::Ordering::SeqCst);
			let _ = thread.join();
			Err(io::Error::new(
				io::ErrorKind::TimedOut,
				"capture init timed out",
			))
		}
	}
}

/// One enumerated host monitor: `(idx, name, width, height, primary)`. `idx` is the
/// 0-based position in the attached-to-desktop output list — the same index
/// `CaptureConfig::output_idx` / `CaptureDevice::create` expect, so the host can
/// advertise these and capture the chosen one with no mapping. `name` is the GDI
/// device name (`\\.\DISPLAY1`, trimmed to `DISPLAY1`); `primary` is the output
/// anchored at the virtual-desktop origin (0,0).
pub type DisplayDesc = (u32, String, u32, u32, bool);

/// Enumerate the host's attached monitors in DXGI output order (Windows). Empty on
/// non-Windows or when enumeration fails — the caller then advertises no picker and
/// streams the default output.
#[cfg(windows)]
pub fn list_displays() -> Vec<DisplayDesc> {
	unsafe { dxgi::CaptureDevice::list_outputs().unwrap_or_default() }
}

/// Non-Windows stub (see [`list_displays`]).
#[cfg(not(windows))]
pub fn list_displays() -> Vec<DisplayDesc> {
	Vec::new()
}

/// Non-Windows stub: the native path is Windows-only, so callers `cfg`-gate the call site and
/// this exists purely to keep the symbol present for a clean cross-platform `cargo check`.
#[cfg(not(windows))]
pub fn start_capture_encode(_cfg: CaptureConfig) -> io::Result<CaptureHandle> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"native capture is Windows-only",
	))
}

// ===========================================================================
// Windows helpers (thread priority, display keep-alive, debug logging)
// ===========================================================================

/// Lightweight debug print for the capture thread's encode errors. Writes to stderr only in
/// debug builds (no-op in release); this crate has no dependency on the host binary.
#[cfg(windows)]
#[inline]
fn dbg_capture(msg: &str) {
	#[cfg(debug_assertions)]
	eprintln!("[pulsar-capture] {msg}");
	#[cfg(not(debug_assertions))]
	let _ = msg;
}

/// RAII guard that bumps the calling thread to `THREAD_PRIORITY_TIME_CRITICAL` for the life of
/// the capture loop and restores the previous priority on drop.
///
/// Why: with the default priority, `thread::sleep`/timer waits land on the 15.6ms scheduler
/// tick and the documented "73fps irregular" symptom appears; TIME_CRITICAL keeps the paced
/// wakeups tight. (We also use a high-resolution waitable timer in dxgi.rs — both are needed.)
#[cfg(windows)]
struct ThreadPriorityGuard {
	prev: i32,
}

#[cfg(windows)]
impl ThreadPriorityGuard {
	unsafe fn time_critical() -> Self {
		use windows::Win32::System::Threading::{
			GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
		};
		// GetThreadPriority returns THREAD_PRIORITY_ERROR_RETURN (0x7fffffff) on failure; we
		// store whatever it returns and only restore if it looked valid (see Drop).
		let prev = GetThreadPriority(GetCurrentThread());
		// SetThreadPriority returns windows_core::Result<()> in 0.59; ignore failure (e.g. on a
		// constrained job object) — the loop still runs, just with worse pacing.
		let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
		Self { prev }
	}
}

#[cfg(windows)]
impl Drop for ThreadPriorityGuard {
	fn drop(&mut self) {
		use windows::Win32::System::Threading::{
			GetCurrentThread, SetThreadPriority, THREAD_PRIORITY,
		};
		// THREAD_PRIORITY_ERROR_RETURN sentinel — don't try to restore a bogus value.
		const ERR_RETURN: i32 = 0x7fff_ffffu32 as i32;
		if self.prev != ERR_RETURN {
			unsafe {
				let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(self.prev));
			}
		}
	}
}

/// RAII guard that asks Windows to keep the display on while we capture, and clears the request
/// on drop. Without this the monitor can power-save mid-stream, which both blanks the captured
/// surface and (on some drivers) drops Desktop Duplication.
#[cfg(windows)]
struct DisplayKeepAlive {
	engaged: bool,
}

#[cfg(windows)]
impl DisplayKeepAlive {
	unsafe fn engage() -> Self {
		use windows::Win32::System::Power::{
			SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED,
		};
		// ES_CONTINUOUS makes the state sticky for this thread until we clear it; combined with
		// ES_DISPLAY_REQUIRED it keeps the monitor awake. A 0 return means the call failed.
		let prev = SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED);
		Self {
			engaged: prev.0 != 0,
		}
	}
}

#[cfg(windows)]
impl Drop for DisplayKeepAlive {
	fn drop(&mut self) {
		if self.engaged {
			use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
			// Clear our request (ES_CONTINUOUS alone with no other flags resets to the default
			// power policy) so we don't pin the display awake after the stream ends.
			unsafe {
				let _ = SetThreadExecutionState(ES_CONTINUOUS);
			}
		}
	}
}
