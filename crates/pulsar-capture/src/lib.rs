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
	/// Live MONITOR switch: the host writes the desired output index here; the capture thread
	/// polls it each tick and, on change, rebuilds capture+encode on the new monitor in THIS
	/// thread (the new monitor may be on a different GPU — see start_capture_encode), instead of
	/// the host tearing the whole pipeline + audio down and re-spawning a fresh process-side
	/// thread. `u32::MAX` = no pending request (the sentinel).
	requested_output: std::sync::Arc<std::sync::atomic::AtomicU32>,
	/// The output index the capture thread is ACTUALLY streaming right now, written by the thread
	/// after each successful build (including reverts). The host reads this to know the confirmed
	/// capture output, which may differ from the REQUESTED output when a switch-build failed and
	/// the thread reverted to `prev_good_output`. Used by the host to keep `cur_display`
	/// (input-mapping) and `last_native_req.display_idx` (fast-path baseline) in sync with
	/// reality so input lands on the right monitor and a switch retry is not a silent no-op.
	current_output: std::sync::Arc<std::sync::atomic::AtomicU32>,
	/// Monotonic build counter: incremented by the capture thread after EVERY successful build
	/// (including same-index resolution-change rebuilds). The host's input path tracks this to
	/// detect a host-side resolution change and re-resolve the monitor geometry via
	/// `display_rect()` / `set_monitor()` even when the monitor INDEX is unchanged (C8).
	build_gen: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl CaptureHandle {
	/// Set the live target bitrate (kbps). Lock-free: stores into the atom the capture thread
	/// polls each tick — never blocks the caller or stalls encode. No-op if unchanged.
	pub fn set_bitrate(&self, kbps: u32) {
		self.requested_kbps
			.store(kbps.max(1), std::sync::atomic::Ordering::SeqCst);
	}

	/// Switch which host monitor is captured, live (session-menu picker). Lock-free: stores the
	/// output index the capture thread picks up next tick and rebuilds capture+encode on it in
	/// the SAME thread (correct across GPUs — see the field doc) — no new OS thread, no init-
	/// handshake wait, and the host leaves audio + forwarders running. No-op if it equals the
	/// current output.
	pub fn switch_output(&self, idx: u32) {
		self.requested_output
			.store(idx, std::sync::atomic::Ordering::SeqCst);
	}

	/// The output index the capture thread is ACTUALLY streaming right now (written by the thread
	/// after every confirmed build, including reverts). May lag the last `switch_output` request
	/// by one build cycle while the thread is rebuilding. The host uses this to keep its
	/// input-mapping (`cur_display`) and fast-path baseline (`last_native_req.display_idx`) in
	/// sync with the actual streamed monitor rather than the optimistically-requested one.
	pub fn current_output(&self) -> u32 {
		self.current_output
			.load(std::sync::atomic::Ordering::SeqCst)
	}

	/// A clone of the Arc<AtomicU32> the capture thread writes after every confirmed build
	/// (including reverts). Callers that need to track the live confirmed output across a
	/// switch — in particular the input-mapping path — can clone this Arc once and then
	/// poll it directly without locking the CaptureHandle or the native_slot mutex. The
	/// atom's value is always the monitor the thread is ACTUALLY streaming, never the
	/// optimistically-requested one, so reading it from on_input gives the correct rect for
	/// absolute-pointer injection even during a mid-session monitor switch.
	pub fn current_output_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicU32> {
		self.current_output.clone()
	}

	/// A clone of the build-generation Arc. The capture thread increments this after every
	/// successful build — including same-index resolution-change rebuilds (C8). The host's
	/// input closure tracks this alongside the output index: when the generation advances the
	/// closure re-calls `display_rect(idx)` / `set_monitor()` to pick up the new monitor
	/// geometry even though the monitor INDEX has not changed.
	pub fn build_gen_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicU32> {
		self.build_gen.clone()
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
		/// Set on the first frame after a SAME-resolution duplication reinit (host Hz change /
		/// transient ACCESS_LOST). The encoder forces an IDR (+ SPS/PPS) for this frame so a
		/// client mid-GOP re-syncs immediately instead of freezing until the next safety GOP.
		pub force_idr: bool,
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
	// Live monitor switch: u32::MAX = no pending request. The host writes a new output index
	// via CaptureHandle::switch_output; the capture thread re-points DXGI in place (see run).
	let requested_output = Arc::new(AtomicU32::new(u32::MAX));
	let req_out_t = requested_output.clone();
	// Actual output the thread is streaming — written after each confirmed build (including
	// reverts). The host reads this via CaptureHandle::current_output() to reconcile its
	// input-mapping and fast-path baseline after a failed switch revert.
	let current_output = Arc::new(AtomicU32::new(cfg.output_idx));
	let cur_out_t = current_output.clone();
	// Monotonic build counter: incremented after every successful build (including same-index
	// resolution-change rebuilds so the host input path can detect geometry changes — C8).
	let build_gen = Arc::new(AtomicU32::new(0));
	let build_gen_t = build_gen.clone();

	let thread = std::thread::Builder::new()
		.name("pulsar-capture".into())
		.spawn(move || unsafe {
			// 1. Thread priority + display-keepalive (both RAII / restored on scope exit).
			//    TIME_CRITICAL keeps the pacing loop off the scheduler's slow path; the
			//    display-keepalive prevents the monitor sleeping mid-stream.
			let _prio = ThreadPriorityGuard::time_critical();
			let _wake = DisplayKeepAlive::engage();

			// Live MONITOR switch is handled by REBUILDING capture+encode on the new output in
			// THIS thread (the outer loop below), NOT by mutating the live device in place. A
			// monitor can sit on a DIFFERENT GPU (MUX laptops: each panel on the iGPU or the
			// dGPU), and a D3D11 device can only DuplicateOutput a monitor on its OWN adapter —
			// so `CaptureDevice::create` must re-pick the adapter per output. Rebuilding here
			// still skips everything the host-side full restart pays (new OS thread, the init
			// handshake wait, the audio-pipeline restart, the ffmpeg encoder re-probe), so a
			// switch is much faster than a fresh `start_capture_encode` and works across GPUs.
			let mut output_idx = cfg.output_idx;
			let mut announced = false;
			// pts is monotonic across switches (the rebuilt encoder forces an IDR on its first
			// frame, so the client re-syncs regardless); last_kbps is re-seeded per build.
			let mut pts: i64 = 0;
			let mut build_no: u32 = 0;
			// The last output index that successfully built+streamed — a failed SWITCH build
			// reverts here so the stream survives (instead of the thread dying → packet drought).
			let mut prev_good_output = cfg.output_idx;
			loop {
				build_no += 1;
				let build_t0 = std::time::Instant::now();
				cap_log(&format!("=== build #{build_no} output={output_idx} ==="));
				// 2. Build capture (DXGI) on the CURRENT output. `create` enumerates the adapter
				//    that owns this monitor and makes the D3D11 device there — so a cross-GPU
				//    switch lands on the right adapter. May fail (no display owner / locked
				//    desktop / session-0). On the FIRST build a failure is reported so the host
				//    falls back to ffmpeg; on a later (switch) build it tears the stream down (the
				//    host can re-request).
				// `announced` is the "already streaming" flag — true ⇒ this is an in-session SWITCH
				// (or revert) build, so normally we use the SHORT transient-retry budget: a switch to
				// a fullscreen monitor must not freeze video for ~5 s (and the revert would pay it
				// again). The initial build (announced=false) keeps the long budget. (B30)
				//
				// EXCEPTION (C3): a same-index rebuild triggered by RunExit::Switch(output_idx) is a
				// RESOLUTION CHANGE on the SAME monitor (pacing.rs:147-150), NOT a cross-monitor
				// switch. There is nowhere to revert (prev_good_output == output_idx), so the short
				// budget converts a transient >600ms unavailability (e.g. a fullscreen-exclusive game
				// taking the output) into permanent thread death. Use the LONG budget for same-index
				// rebuilds so the mode transition resolves within ~5 s and capture recovers.
				let fast_transient = announced && output_idx != prev_good_output;
				let mut cap = match dxgi::CaptureDevice::create(output_idx, stop_t.clone(), fast_transient) {
					Ok(c) => c,
					Err(e) => {
						if !announced {
							cap_log(&format!("BUILD #{build_no} cap FAILED output={output_idx}: {e:?} — initial, fall to ffmpeg"));
							let _ = init_tx.send(Err(format!("dxgi init: {e:?}")));
							return;
						}
						// A SWITCH-build failed (e.g. target monitor mid-fullscreen, DuplicateOutput
						// still unavailable after the long retry). DON'T kill the stream — revert to
						// the last good monitor so video keeps flowing; the user can retry the switch.
						if output_idx != prev_good_output {
							cap_log(&format!("BUILD #{build_no} cap FAILED output={output_idx}: {e:?} — REVERT to {prev_good_output}"));
							output_idx = prev_good_output;
							continue;
						}
						cap_log(&format!("BUILD #{build_no} cap FAILED output={output_idx} (last-good too): {e:?} — THREAD DYING"));
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
				//    driver does any cross-adapter copy internally on hybrid GPUs). Seed the
				//    bitrate from the LIVE target so a switch preserves any adaptive-bitrate step.
				let mut last_kbps = req_kbps_t.load(Ordering::Relaxed).max(1);
				let mut enc = match encode::Encoder::new(
					&cap.device,
					&cap.context,
					&encode::EncParams {
						width: w,
						height: h,
						capture_width: cap_w,
						capture_height: cap_h,
						fps: cfg.fps.max(1),
						bitrate_kbps: last_kbps,
						dest: cfg.dest.clone(),
						codec: cfg.codec,
						low_latency: cfg.low_latency,
						rotation,
					},
				) {
					Ok(e) => e,
					Err(e) => {
						if !announced {
							cap_log(&format!("BUILD #{build_no} enc FAILED output={output_idx}: {e} — initial, fall to ffmpeg"));
							let _ = init_tx.send(Err(format!("nvenc init: {e}")));
							return;
						}
						if output_idx != prev_good_output {
							cap_log(&format!("BUILD #{build_no} enc FAILED output={output_idx}: {e} — REVERT to {prev_good_output}"));
							output_idx = prev_good_output;
							continue;
						}
						cap_log(&format!("BUILD #{build_no} enc FAILED output={output_idx} (last-good too): {e} — THREAD DYING"));
						return;
					}
				};
				prev_good_output = output_idx;
				// Publish the confirmed output so the host can read current_output() and
				// reconcile cur_display / last_native_req after a failed switch revert.
				cur_out_t.store(output_idx, Ordering::SeqCst);
				// Bump the build generation so the host input path re-resolves monitor geometry
				// even when the index is unchanged (same-index resolution-change rebuild — C8).
				build_gen_t.fetch_add(1, Ordering::SeqCst);
				cap_log(&format!(
					"build #{build_no} STREAMING output={output_idx} {w}x{h} kbps={last_kbps} rebuilt_in={}ms",
					build_t0.elapsed().as_millis(),
				));

				// 4. First build OK — unblock the caller (Ok ⇒ streaming started). Later (switch)
				//    builds skip this; the handshake is a one-shot.
				if !announced {
					let _ = init_tx.send(Ok(()));
					announced = true;
				}

				// 4b. Drain a switch requested DURING this rebuild's blind window (between the prior
				//     run() exit and now) — `run` won't poll the atom until its first tick, so a
				//     rapid second click would otherwise wait a whole rebuild+GOP. Rebuild straight
				//     onto it. A request for the monitor we just built is consumed (no-op).
				let pending = req_out_t.swap(u32::MAX, Ordering::AcqRel);
				if pending != u32::MAX && pending != output_idx {
					enc.flush_and_close();
					drop(enc);
					drop(cap);
					output_idx = pending;
					continue;
				}

				// 5. Pacing loop (Sunshine technique). `on_frame` converts BGRA→NV12, encodes via
				//    NVENC, and hands the Annex-B AU to the RTP sender thread. `run` returns either
				//    Stop (teardown) or Switch(idx) when the host asked for a new monitor.
				let mut submit_errs: u64 = 0;
				let mut frames_sent: u64 = 0;
				let exit = cap.run(
					cfg.fps.max(1),
					cfg.draw_mouse,
					&req_out_t,
					&stop_t,
					|frame: &Frame| {
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
						// First frame after a same-res reinit (Hz change / transient ACCESS_LOST):
						// force a keyframe so the client re-syncs instead of waiting a safety GOP.
						if frame.force_idr {
							enc.request_idr();
						}
						if let Err(e) = enc.submit(frame, pts) {
							submit_errs += 1;
							if submit_errs <= 3 || submit_errs % 240 == 0 {
								cap_log(&format!("build #{build_no} submit err #{submit_errs}: {e}"));
							}
						} else {
							frames_sent += 1;
						}
						pts += 1;
					},
				);
				cap_log(&format!(
					"build #{build_no} run exited ({}) frames_sent={frames_sent} submit_errs={submit_errs}",
					match exit { dxgi::RunExit::Stop => "stop".to_string(), dxgi::RunExit::Switch(i) => format!("switch->{i}") },
				));

				// 6. Tear down NVENC (no drain — SYNC, no B-frames) + release the AddRef'd device
				//    LAST. Dropping the Encoder drops its RtpEgress (closes the socket, joins the
				//    sender thread). Then drop `cap` (releases the DXGI duplication + device).
				enc.flush_and_close();
				drop(enc);
				drop(cap);
				match exit {
					dxgi::RunExit::Stop => break,
					// Rebuild on the requested monitor (its adapter may differ — see above).
					dxgi::RunExit::Switch(idx) => output_idx = idx,
				}
			}
		})?;

	// Block on the init handshake (bounded). If the thread died before sending, treat as Err.
	match init_rx.recv_timeout(std::time::Duration::from_secs(3)) {
		Ok(Ok(())) => Ok(CaptureHandle {
			stop,
			thread: Some(thread),
			requested_kbps,
			requested_output,
			current_output,
			build_gen,
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

/// Geometry needed to map a normalized (0..1) pointer onto the captured monitor's
/// place in the Windows virtual desktop: the chosen monitor's rect and the bounding
/// box of ALL attached monitors (== `SM_*VIRTUALSCREEN`). All in virtual-desktop
/// pixels. `mon_*` is the output at `output_idx` (DXGI order, same as `list_displays`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayRect {
	pub mon_left: i32,
	pub mon_top: i32,
	pub mon_width: i32,
	pub mon_height: i32,
	pub virt_left: i32,
	pub virt_top: i32,
	pub virt_width: i32,
	pub virt_height: i32,
}

/// Resolve the virtual-desktop geometry of the captured output (`output_idx`, DXGI
/// order) plus the full virtual-screen extent, so absolute-pointer injection can target
/// the streamed (possibly non-primary) monitor. `None` if enumeration fails or the index
/// is out of range. Windows-only (the native multi-monitor capture path).
#[cfg(windows)]
pub fn display_rect(output_idx: u32) -> Option<DisplayRect> {
	unsafe { dxgi::CaptureDevice::output_rect(output_idx) }
}

/// Non-Windows stub (see [`display_rect`]).
#[cfg(not(windows))]
pub fn display_rect(_output_idx: u32) -> Option<DisplayRect> {
	None
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

/// Capture-thread rebuild lifecycle log. Debug-builds only (no-op in release).
/// Previously wrote to `C:\Users\Public\pulsar-capture-dbg.txt` unconditionally;
/// that was temporary instrumentation for the "switch works N times then stuck"
/// diagnosis and must not ship to release (world-readable fixed path, unbounded
/// growth, file I/O on the pacing-critical path).
#[cfg(windows)]
#[inline]
fn cap_log(msg: &str) {
	#[cfg(debug_assertions)]
	{
		use std::io::Write;
		if let Ok(mut f) = std::fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open("C:\\Users\\Public\\pulsar-capture-dbg.txt")
		{
			let _ = writeln!(f, "[lib] {msg}");
		}
	}
	#[cfg(not(debug_assertions))]
	let _ = msg;
}

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
