//! Linux rkmpp video decode + zero-copy GL present, ported from `pulsar-vidsink.c`. Runs in the
//! SAME process/GL context as the egui overlay so the overlay is a child of the app window
//! (moves/clips/stacks with it — no separate top-level, no compositor desync). Decode on a
//! worker thread → DRM_PRIME mailbox; the main thread imports the newest frame as an
//! `EGL_LINUX_DMA_BUF_EXT` EGLImage → `GL_TEXTURE_EXTERNAL_OES` → draws a letterboxed quad.

use ffmpeg_sys_next as ff;
use std::collections::VecDeque;
use std::ffi::{c_void, CString};
use std::os::raw::c_int;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

// --- EGL/GL extension constants (not in khronos-egl's typed API) ---
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const FD_EXT: [i32; 3] = [0x3272, 0x3275, 0x3278];
const OFFSET_EXT: [i32; 3] = [0x3273, 0x3276, 0x3279];
const PITCH_EXT: [i32; 3] = [0x3274, 0x3277, 0x327A];
const MOD_LO_EXT: [i32; 3] = [0x3443, 0x3445, 0x3447];
const MOD_HI_EXT: [i32; 3] = [0x3444, 0x3446, 0x3448];
const EGL_NONE_I: i32 = 0x3038;
pub const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

// --- DRM_PRIME frame queue (decode thread → render thread) ---
struct FramePtr(*mut ff::AVFrame);
unsafe impl Send for FramePtr {}

/// Max frames buffered between decode and present (env `PULSAR_QCAP`, default 3, min 2). With
/// pacing ON the present side drains this FIFO one frame per vsync, so a clumped/bursty arrival
/// (network/receive jitter, a periodic delivery gap) is spread smoothly over the next vsyncs
/// instead of collapsing to one update — Moonlight-style frame pacing. Costs ≤ QCAP-1 frames of
/// latency (~16 ms @120 fps at the default 3); raise it to absorb a larger arrival gap at a
/// proportional latency cost. Pacing OFF keeps only the newest frame each present (lowest latency).
static QCAP: AtomicUsize = AtomicUsize::new(3);

static MBX: Mutex<VecDeque<FramePtr>> = Mutex::new(VecDeque::new());
static STOP: AtomicBool = AtomicBool::new(false);
/// Live demuxer/decoder reopen (codec switch). On RK3588 the renderer PROCESS must survive a
/// codec change (killing it corrupts WebKit's shared Mali GL state — the desktop/Windows
/// backends respawn instead), so the app rewrites the SDP and sends `reopen <path>` over stdin:
/// the decode loop tears the old demuxer+decoder down and reopens in place.
static REOPEN: AtomicBool = AtomicBool::new(false);
static REOPEN_SDP: Mutex<Option<String>> = Mutex::new(None);
/// True from the moment a reopen is requested (monitor/codec switch) until the new
/// stream's first keyframe decodes — the overlay draws a "switching…" indicator over
/// the held last frame so the user sees the change is in progress, not a hang. Only set
/// on a REOPEN (not the initial connect, which has its own connecting screen).
pub static SWITCHING: AtomicBool = AtomicBool::new(false);
/// Monotonic-millis deadline by which the switch (reopen) must produce its first keyframe,
/// or 0 = no switch in progress. If a monitor/codec switch fails silently (the host can't
/// capture the new output, or its restarted encoder never sends a keyframe on the same UDP
/// port), nothing else would ever clear SWITCHING or unblock `av_read_frame` — the overlay
/// would spin forever over the held frame. Past the deadline the interrupt callback aborts
/// the blocked read and the decode loop retries the open, so a silent switch self-recovers.
static SWITCH_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
/// How long a switch may take to produce its first keyframe before we give up waiting on the
/// (possibly silent) new stream, clear the spinner and retry the reopen. Covers the host's
/// restream gap + a couple of GOPs of slack.
const SWITCH_TIMEOUT_MS: u64 = 6_000;

/// Monotonic milliseconds since first use (for the switch deadline; no wall-clock jumps).
fn mono_ms() -> u64 {
	use std::sync::OnceLock;
	use std::time::Instant;
	static E: OnceLock<Instant> = OnceLock::new();
	E.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Public alias for linux.rs stall detection (linux.rs cannot call the private `mono_ms`).
pub fn mono_ms_pub() -> u64 {
	mono_ms()
}

/// True once a switch (reopen) has been waiting past `SWITCH_TIMEOUT_MS` without a keyframe.
fn switch_timed_out() -> bool {
	let d = SWITCH_DEADLINE_MS.load(Ordering::Relaxed);
	d != 0 && mono_ms() >= d
}

/// Queue a live reopen on `sdp_path` (called from the stdin reader on a `reopen` line).
pub fn request_reopen(sdp_path: &str) {
	// Set REOPEN_SDP and REOPEN under the SAME lock the decode loop uses to take()+clear them,
	// so a second reopen arriving between the loop's take() and store(false) can't be clobbered
	// back to REOPEN=false (a TOCTOU that dropped the second switch — the new SDP would sit in
	// REOPEN_SDP with REOPEN=false until the next EOF). With both atomics flipped inside the
	// mutex, the decode loop either sees the new SDP on its take() or re-breaks immediately on
	// the still-set REOPEN flag and picks it up on the next pass.
	{
		let mut sdp = REOPEN_SDP.lock().unwrap();
		*sdp = Some(sdp_path.to_string());
		REOPEN.store(true, Ordering::Relaxed);
	}
	SWITCHING.store(true, Ordering::Relaxed);
	// Arm the switch deadline so a silent new stream can't hang the spinner forever.
	SWITCH_DEADLINE_MS.store(mono_ms() + SWITCH_TIMEOUT_MS, Ordering::Relaxed);
}

/// libav interrupt callback: abort a blocked demuxer read on stop/reopen, or once a switch
/// has waited past its deadline, so the decode loop can't hang waiting for packets that will
/// never come (e.g. the host already switched codecs / monitors and the new RTP flow stayed
/// silent on the same port).
unsafe extern "C" fn intr_cb(_: *mut c_void) -> c_int {
	(STOP.load(Ordering::Relaxed) || REOPEN.load(Ordering::Relaxed) || switch_timed_out()) as c_int
}
/// Frame pacing toggle. false = newest-wins (drain all-but-newest each present); true =
/// Moonlight per-vblank metering (present exactly ONE oldest frame per draw/vblank, hold the
/// last frame on underflow, adaptive depth ≤ PACE_CEIL). The startup default is ON (set via
/// `video::set_pace(true)` from linux.rs; `PULSAR_PACE=0` forces off). Flipped live by linux.rs's
/// stdin reader on a `pace 0|1` line.
static PACE: AtomicBool = AtomicBool::new(false);
/// Estimated source frame interval in microseconds (EMA over decoded-frame arrivals). DIAGNOSTIC
/// ONLY now — the Moonlight pacer drives cadence off the real vblank (one frame per draw), NOT
/// this timer (the old EMA-cadence pacer beat-drifted vs the panel). Kept for future HUD use.
static SRC_US: AtomicU64 = AtomicU64::new(16_666);
/// Demuxed video bytes received since the presenter last sampled it (received-stream bitrate,
/// matching mpv's `video-bitrate` notion). The decode thread adds each packet's size; the
/// presenter swaps it to 0 once a second and turns it into Mbit/s for the HUD. Atomic, so the
/// decode thread can poke it without taking the FPS lock the presenter owns.
static VBYTES: AtomicU64 = AtomicU64::new(0);
/// EMA of per-frame decode time in microseconds — how long `avcodec_receive_frame` takes to
/// hand us a decoded frame. Drives the overlay's "Çözme ms" tile (was hardcoded 0 before).
pub static DEC_US: AtomicU64 = AtomicU64::new(0);
pub static FPS: Mutex<[f32; 3]> = Mutex::new([0.0; 3]); // fps, mbit, ms (filled by main on present)
/// True while the stream is stalled (no fresh frame for ≥ STALL_SECS) and video was
/// previously live. Cleared the moment a fresh frame arrives again. The render loop
/// drives this on Windows (per-AU queue); linux.rs drives it from LAST_FRAME_MS.
pub static STALLED: AtomicBool = AtomicBool::new(false);
/// Set when the app reuses the resident renderer for a DIFFERENT host (it sends `show`
/// before `reopen`). The render loop then drops the previous host's held frame so its last
/// view isn't shown while the new stream spins up — Chrome-tab semantics (each connect is a
/// fresh view), not the codec-switch "hold the frame" path (which sends `reopen` alone).
pub static RESET_VIEW: AtomicBool = AtomicBool::new(false);
/// Monotonic-millis timestamp of the LAST fresh decoded frame handed to the GL
/// presenter (updated on every `Presenter::draw()` with a non-empty MBX pop).
/// Used by the Linux render loop to detect a stall without a separate timer thread.
/// 0 = no frame presented yet.
pub static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);
/// Live STREAM pixel dimensions (0 = no video yet) — mirrors the `vidsink-dims` stdout
/// report. The ~1 Hz `vidsink-fps` stats line embeds THESE (not the window size!): the app
/// re-emits them as `play-dims`, and the webview's input letterbox mapping uses them — the
/// window size there made the mapping compute a no-op rect and the remote cursor drifted.
pub static VID_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
pub static VID_H: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Human label of the ACTUAL decoder in use (e.g. "h264_rkmpp (HW)") — the egui
/// overlay displays it read-only; selection is always automatic.
pub static DEC_LABEL: Mutex<String> = Mutex::new(String::new());

/// The video's on-screen letterbox rectangle in framebuffer PIXELS — `[x, y, w, h]`, updated
/// every `draw()`. The cursor side-channel overlay (linux.rs) reads it to map a normalized host
/// pointer position (0..1 in the streamed screen) to a screen point: the video may be
/// letterboxed/cropped, so the cursor must follow the SAME rect the frame is drawn into. `w==0`
/// means "no frame presented yet" (don't draw the cursor).
pub static VIDEO_RECT: Mutex<[i32; 4]> = Mutex::new([0; 4]);

/// The source frame dimensions in HOST pixels — `[w, h]`, updated every `draw()`. The cursor
/// side-channel overlay (linux.rs) divides the displayed rect (`VIDEO_RECT`) by this to get the
/// host-pixel → displayed-pixel scale, so the cursor bitmap/hotspot (which arrive in raw host
/// pixels) are drawn at the right size and the tip stays aligned when the video isn't shown 1:1.
/// `[0, 0]` means "no frame presented yet".
pub static VIDEO_SRC: Mutex<[i32; 2]> = Mutex::new([0; 2]);

/// View-fit mode for presenting the video in the window (AnyDesk-style): 0 = FIT
/// (letterbox, keep aspect — default), 1 = STRETCH (fill the window, may distort),
/// 2 = ORIGINAL (1:1 source pixels, centered; larger streams crop). Set by the
/// overlay's Görüntü section / the frontend over stdin (`fit fit|stretch|original`).
static FIT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_fit(mode: &str) {
	let v = match mode {
		"stretch" => 1,
		"original" => 2,
		_ => 0,
	};
	FIT_MODE.store(v, Ordering::Relaxed);
}

pub fn fit_label() -> &'static str {
	match FIT_MODE.load(Ordering::Relaxed) {
		1 => "stretch",
		2 => "original",
		_ => "fit",
	}
}

/// Live-toggle frame pacing (called from the stdin reader and the startup default).
pub fn set_pace(on: bool) {
	PACE.store(on, Ordering::Relaxed);
}

/// Current pacing state — the overlay reads this each frame so its toggle highlight tracks
/// whatever set it last (the egui click OR a `pace 0|1` line pushed from the frontend).
pub fn pace_on() -> bool {
	PACE.load(Ordering::Relaxed)
}

/// Soft adaptive pacing ceiling: the MOST frames the Moonlight pacer will buffer when the
/// queue has recently drained (so it can absorb the next burst). Within the QCAP hard cap.
/// Set from `--mode` at startup — game=2 (min latency), remote=3 (smoother). The pacer trims
/// toward 1 frame at steady state and only fills toward this ceiling after a recent drain.
static PACE_CEIL: AtomicUsize = AtomicUsize::new(3);

/// Set the adaptive pacing ceiling (clamped ≥1). Called from linux.rs once `--mode` is known.
pub fn set_pace_ceiling(n: usize) {
	PACE_CEIL.store(n.max(1), Ordering::Relaxed);
}

/// Software formats the presenter can display: 3-plane 8-bit YUV (any subsampling)
/// or 2-plane NV12 (what `av_hwframe_transfer_data` readback typically yields).
pub fn is_displayable_sw(f: c_int) -> bool {
	is_planar_yuv(f) || f == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int
}

/// Planar 8-bit YUV formats the software presenter can upload (3 separate planes; the
/// chroma plane size is derived per format in `Presenter::draw`). A High-4:4:4 stream
/// (e.g. a host that forgot to pin 4:2:0) offers ONLY yuv444p here — rejecting it is a
/// guaranteed black screen, so all common subsamplings are accepted.
fn is_planar_yuv(f: c_int) -> bool {
	use ff::AVPixelFormat::*;
	f == AV_PIX_FMT_YUV420P as c_int
		|| f == AV_PIX_FMT_YUVJ420P as c_int
		|| f == AV_PIX_FMT_YUV422P as c_int
		|| f == AV_PIX_FMT_YUVJ422P as c_int
		|| f == AV_PIX_FMT_YUV444P as c_int
		|| f == AV_PIX_FMT_YUVJ444P as c_int
}

/// Chroma plane dimensions for a planar 8-bit YUV format (Y is full `w`×`h`).
fn chroma_dims(f: c_int, w: i32, h: i32) -> (i32, i32) {
	use ff::AVPixelFormat::*;
	if f == AV_PIX_FMT_YUV444P as c_int || f == AV_PIX_FMT_YUVJ444P as c_int {
		(w, h)
	} else if f == AV_PIX_FMT_YUV422P as c_int || f == AV_PIX_FMT_YUVJ422P as c_int {
		((w + 1) / 2, h)
	} else {
		((w + 1) / 2, (h + 1) / 2) // 4:2:0
	}
}

/// Open the SDP (RTP/H.264, H.265 or AV1 — codec read from the SDP), decode with the matching
/// rkmpp HW decoder (or a software fallback for AV1) into DRM_PRIME, publish newest to the mailbox.
pub fn start_decode(sdp_path: &str) {
	if let Some(n) = std::env::var("PULSAR_QCAP")
		.ok()
		.and_then(|v| v.parse::<usize>().ok())
	{
		QCAP.store(n.max(2), Ordering::Relaxed);
	}
	let sdp = CString::new(sdp_path).unwrap();
	std::thread::spawn(move || {
		let mut sdp = sdp;
		loop {
			unsafe { decode_once(&sdp) };
			if STOP.load(Ordering::Relaxed) {
				return;
			}
			// A pending reopen (live codec switch): drop the stale old-codec frames and
			// run another decode pass on the rewritten SDP. No pending path → the demuxer
			// hit EOF / a fatal open error: keep the old end-of-thread behavior.
			// Take the pending SDP and clear REOPEN under the SAME lock request_reopen sets
			// them under, so a concurrent reopen can't land between take() and store(false)
			// and have its REOPEN=true clobbered to false (the dropped-second-switch TOCTOU).
			let mut next = {
				let mut sdp = REOPEN_SDP.lock().unwrap();
				let n = sdp.take();
				REOPEN.store(false, Ordering::Relaxed);
				n
			};
			// A switch was still in progress when this pass ended without a pending reopen —
			// i.e. the reopen's open/decoder validation failed (`break 'decode`) before any
			// keyframe. Clear the spinner and fall through to the park loop below so the
			// thread stays alive waiting for the next `request_reopen` (e.g. the user
			// switches back to H.264/H.265 after an AV1 failure). The renderer process MUST
			// survive on RK3588 — killing it corrupts the shared Mali GL state.
			if next.is_none() && SWITCHING.load(Ordering::Relaxed) {
				SWITCHING.store(false, Ordering::Relaxed);
				SWITCH_DEADLINE_MS.store(0, Ordering::Relaxed);
			}
			match next {
				Some(p) => {
					let mut q = MBX.lock().unwrap();
					while let Some(f) = q.pop_front() {
						let mut o = f.0;
						unsafe { ff::av_frame_free(&mut o) };
					}
					drop(q);
					match CString::new(p) {
						Ok(c) => sdp = c,
						Err(_) => return,
					}
				}
				None => {
					// No pending SDP and not a reopen — this was a plain EOF or a fatal open
					// error on a non-switch path. Return and let the caller decide (STOP
					// → process exit; otherwise the session is over).
					// Exception: if STOP is NOT set, we may be on RK3588 where the process
					// must persist. Park here waiting for the next request_reopen() instead
					// of exiting, so a subsequent codec/monitor switch can revive video
					// without a full disconnect/reconnect.
					loop {
						if STOP.load(Ordering::Relaxed) {
							return;
						}
						std::thread::sleep(std::time::Duration::from_millis(200));
						let mut pending = REOPEN_SDP.lock().unwrap();
						if pending.is_some() {
							// A new reopen arrived while we were parked — take it and
							// re-enter the decode loop, exactly as the normal reopen path.
							let p = pending.take().unwrap();
							REOPEN.store(false, Ordering::Relaxed);
							drop(pending);
							eprintln!("pulsar-render: decode thread revived by new reopen request");
							match CString::new(p) {
								Ok(c) => {
									sdp = c;
									break; // back to the outer `loop { decode_once … }`
								}
								Err(_) => return,
							}
						}
					}
				}
			}
		}
	});
}

/// One demux+decode pass over `sdp` — runs until stop, reopen, EOF or a fatal error.
/// (The body of the old inline decode thread, unchanged except the loop condition and
/// the interrupt callback; extracted so a live reopen can run it again on a new SDP.)
unsafe fn decode_once(sdp: &CString) {
	{
		let mut fmt: *mut ff::AVFormatContext = ptr::null_mut();
		let mut opts: *mut ff::AVDictionary = ptr::null_mut();
		let set = |o: &mut *mut ff::AVDictionary, k: &str, v: &str| {
			let k = CString::new(k).unwrap();
			let v = CString::new(v).unwrap();
			ff::av_dict_set(o, k.as_ptr(), v.as_ptr(), 0);
		};
		set(&mut opts, "protocol_whitelist", "file,rtp,udp");
		set(&mut opts, "fflags", "nobuffer+discardcorrupt");
		// RTP reorder window (Moonlight's RtpVideoQueue reassembles fragments in ascending seq
		// before depacketizing). 0 = ZERO tolerance: one reordered UDP fragment corrupts the whole
		// access unit → discardcorrupt drops it → the P-frame reference chain breaks → rkmpp can't
		// decode the next ~10-12 frames until a keyframe → a ~200 ms freeze (the cursor/typing
		// "teleport"). On a 1440p stream each AU spans many fragments, so even a little LAN reorder
		// hits often (measured ~1 lost AU/s). 16 packets covered LAN reorder, but a NACK
		// retransmit re-enters ~1 RTT later — at 15 Mbit/1200 B that's dozens of packets — so the
		// window must hold the in-flight stretch or the retransmit lands "too late" and is dropped.
		// 64 packets ≈ 40 ms of 15 Mbit stream — measured TOO SMALL against a phone host:
		// a single IDR spans ~90 packets at 8 Mbit/1100 B, so any gap inside an IDR burst
		// overflowed the queue ("jitter buffer full") and force-flushed past the hole before
		// the NACK retransmit landed ("RTP: dropping old packet received too late") →
		// mosaic/smear every time. 512 packets (~0.5 s @ 8 Mbit, a few hundred KB) holds the
		// whole burst while a retransmit is in flight; in-order streams are unaffected (the
		// queue drains immediately when there is no gap). (env PULSAR_REORDER overrides.)
		// Envs are parsed as numbers (default on garbage): a non-numeric value on a KNOWN
		// option would fail av_opt_set_dict inside avformat_open_input → permanent black screen.
		let reorder = std::env::var("PULSAR_REORDER")
			.ok()
			.and_then(|v| v.parse::<u64>().ok())
			.unwrap_or(512);
		set(&mut opts, "reorder_queue_size", &reorder.to_string());
		// UDP socket SO_RCVBUF headroom. NOT a latency delay-line: with nobuffer+low_delay the
		// decode thread reads packets immediately, so the socket stays drained at steady state
		// (RecvQ≈0); this only absorbs transient bursts (an IDR spread over many packets).
		// 4 MiB (env PULSAR_BUFSZ); Pi rmem_max is 16 MiB (sysctl).
		let bufsz = std::env::var("PULSAR_BUFSZ")
			.ok()
			.and_then(|v| v.parse::<u64>().ok())
			.unwrap_or(4_194_304);
		set(&mut opts, "buffer_size", &bufsz.to_string());
		// How long the demuxer waits on a SEQ GAP for the missing packet before skipping ahead
		// (µs). 0 made the NACK retransmit path useless: on any loss the queue was flushed
		// instantly ("max delay reached"), and when the host's resend arrived ~1 RTT later the
		// demuxer had moved past it → "RTP: dropping old packet received too late" → broken
		// reference chain → green/mosaic smear until the next keyframe (gop=120 ≈ 2 s). 40 ms
		// covers the LAN-relay/hairpin NACK round-trip; in-order packets are still delivered
		// immediately (this delays ONLY when a gap is being waited out), so steady-state latency
		// is unchanged. Raised 40 → 100 ms: against a phone host over Wi-Fi (+ possibly a
		// remote relay) the detect→NACK→retransmit→arrive loop measured past 40 ms, so every
		// retransmit was already "too late" and the picture stayed corrupt until the next
		// keyframe. 100 ms still only bites while a hole is being waited out.
		// (env PULSAR_MAXDELAY overrides, µs.)
		let maxdelay = std::env::var("PULSAR_MAXDELAY")
			.ok()
			.and_then(|v| v.parse::<u64>().ok())
			.unwrap_or(100_000);
		set(&mut opts, "max_delay", &maxdelay.to_string());
		// Make avformat_find_stream_info (below) return ASAP instead of sitting in libav's
		// default ~5 s analyze window. The codec is ALREADY known from the SDP (the m=video
		// rtpmap sets codec_id) and SPS/PPS arrive in-band on every IDR (host repeatSPSPPS),
		// so there is nothing to probe — without this, every monitor/codec-switch REOPEN
		// blocked here for ~5 s on top of the host's restream gap (the "switch takes 5-8 s"
		// even though the host already resumed). analyzeduration=0 + fpsprobesize=0 (don't
		// burn frames measuring an fps we don't use — the renderer is untimed) collapses the
		// probe; a tiny probesize bounds the byte budget. (envs override for diagnosis.)
		let analyzedur = std::env::var("PULSAR_ANALYZEDUR")
			.ok()
			.and_then(|v| v.parse::<u64>().ok())
			.unwrap_or(0);
		set(&mut opts, "analyzeduration", &analyzedur.to_string());
		let probesize = std::env::var("PULSAR_PROBESIZE")
			.ok()
			.and_then(|v| v.parse::<u64>().ok())
			.unwrap_or(100_000);
		set(&mut opts, "probesize", &probesize.to_string());
		set(&mut opts, "fpsprobesize", "0");
		// Pre-allocate the context to install the interrupt callback (a blocked RTP read
		// must abort on stop/reopen). avformat_open_input frees it on failure as usual.
		//
		// Retry on open failure for up to ~800 ms: on a codec/monitor switch the spawner
		// (respawn_render_for_codec) kills the old renderer via stop_render_child (SIGTERM
		// + up to 600 ms wait, then SIGKILL+wait) before launching this new process, so
		// the UDP port should already be free. But there can be a brief kernel delay
		// between the child's death and the bound UDP socket being released. Retrying
		// here turns that transient race into a non-event instead of a permanent black
		// screen for the rest of the session.
		let opened = {
			let mut ok = false;
			for attempt in 0..8u32 {
				if attempt > 0 {
					std::thread::sleep(std::time::Duration::from_millis(100));
				}
				if STOP.load(Ordering::Relaxed) || REOPEN.load(Ordering::Relaxed) {
					break; // stop/reopen requested — exit the retry loop and let decode_once return
				}
				// avformat_alloc_context returns a context we own; avformat_open_input frees
				// it on failure, so we re-allocate on every retry.
				fmt = ff::avformat_alloc_context();
				(*fmt).interrupt_callback.callback = Some(intr_cb);
				(*fmt).interrupt_callback.opaque = ptr::null_mut();
				// Re-build the dict for each attempt: avformat_open_input consumes and
				// frees consumed entries; calling it again on the same dict after a failure
				// would re-use already-freed memory.
				let mut retry_opts: *mut ff::AVDictionary = ptr::null_mut();
				let set2 = |o: &mut *mut ff::AVDictionary, k: &str, v: &str| {
					let k = CString::new(k).unwrap();
					let v = CString::new(v).unwrap();
					ff::av_dict_set(o, k.as_ptr(), v.as_ptr(), 0);
				};
				set2(&mut retry_opts, "protocol_whitelist", "file,rtp,udp");
				set2(&mut retry_opts, "fflags", "nobuffer+discardcorrupt");
				let reorder2 = std::env::var("PULSAR_REORDER")
					.ok()
					.and_then(|v| v.parse::<u64>().ok())
					.unwrap_or(64);
				set2(&mut retry_opts, "reorder_queue_size", &reorder2.to_string());
				let bufsz2 = std::env::var("PULSAR_BUFSZ")
					.ok()
					.and_then(|v| v.parse::<u64>().ok())
					.unwrap_or(4_194_304);
				set2(&mut retry_opts, "buffer_size", &bufsz2.to_string());
				let maxdelay2 = std::env::var("PULSAR_MAXDELAY")
					.ok()
					.and_then(|v| v.parse::<u64>().ok())
					.unwrap_or(40_000);
				set2(&mut retry_opts, "max_delay", &maxdelay2.to_string());
				let analyzedur2 = std::env::var("PULSAR_ANALYZEDUR")
					.ok()
					.and_then(|v| v.parse::<u64>().ok())
					.unwrap_or(0);
				set2(&mut retry_opts, "analyzeduration", &analyzedur2.to_string());
				let probesize2 = std::env::var("PULSAR_PROBESIZE")
					.ok()
					.and_then(|v| v.parse::<u64>().ok())
					.unwrap_or(100_000);
				set2(&mut retry_opts, "probesize", &probesize2.to_string());
				set2(&mut retry_opts, "fpsprobesize", "0");
				let r =
					ff::avformat_open_input(&mut fmt, sdp.as_ptr(), ptr::null_mut(), &mut retry_opts);
				ff::av_dict_free(&mut retry_opts);
				if r >= 0 {
					ok = true;
					break;
				}
				if attempt > 0 {
					eprintln!(
						"pulsar-render: avformat_open_input failed (attempt {attempt}), retrying…"
					);
				}
			}
			// The original opts dict was already consumed/freed via set() above; free it
			// now in case we break out early (STOP/REOPEN) before reaching avformat_open_input.
			ff::av_dict_free(&mut opts);
			ok
		};
		if !opened {
			eprintln!("pulsar-render: avformat_open_input failed after retries");
			return;
		}
		// Every exit from here on funnels through the cleanup tail below the labeled block
		// (this renderer stays resident after session end — the `hide` idle path — so an
		// early-return leak of the format/codec contexts would persist).
		let mut dc: *mut ff::AVCodecContext = ptr::null_mut();
		'decode: {
			ff::avformat_find_stream_info(fmt, ptr::null_mut());
			let vs = ff::av_find_best_stream(
				fmt,
				ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
				-1,
				-1,
				ptr::null_mut(),
				0,
			);
			if vs < 0 {
				eprintln!("pulsar-render: no video stream");
				break 'decode;
			}
			// Moonlight-style tiered selection (decode.rs): candidates per codec, each
			// VALIDATED by really decoding a canned keyframe — zero-copy SoC decoders
			// (DRM_PRIME by capability, e.g. rkmpp on RK3588) → generic hwaccels
			// (VAAPI/CUDA/Vulkan, zero-copy map or readback) → software. The SDP (written
			// by spawn.rs::write_sdp from the negotiated codec) sets codec_id.
			let st = *(*fmt).streams.add(vs as usize);
			let codec_id = (*(*st).codecpar).codec_id;
			let sel = match crate::decode::select(codec_id) {
				Some(s) => s.sel,
				None => {
					eprintln!("pulsar-render: no decoder validated for this codec");
					break 'decode;
				}
			};
			let dec = crate::decode::find_decoder(&sel, codec_id);
			if dec.is_null() {
				eprintln!("pulsar-render: selected decoder disappeared");
				break 'decode;
			}
			dc = ff::avcodec_alloc_context3(dec);
			ff::avcodec_parameters_to_context(dc, (*st).codecpar);
			// Decoder low-delay: emit frames without reorder buffering when the SPS lacks
			// bitstream_restriction. This is a codec-context flag — an AVFormatContext has
			// no "flags" AVOption, so it can't ride the demuxer dict above.
			(*dc).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
			crate::decode::set_wanted_hw_fmt(if sel.tier == crate::decode::Tier::HwAccel {
				sel.hw_fmt
			} else {
				ff::AVPixelFormat::AV_PIX_FMT_NONE
			});
			(*dc).get_format = Some(crate::decode::get_format);
			(*dc).extra_hw_frames = 8;
			if let Some(dev) = sel.hwdev {
				let hwctx = crate::decode::create_hwdevice(dev);
				if hwctx.is_null() {
					eprintln!("pulsar-render: hw device ctx failed at stream open");
					break 'decode;
				}
				(*dc).hw_device_ctx = hwctx;
			}
			// Software decoders (desktop x86, AV1): slice threading only — frame threading would
			// add a frame of latency per extra thread. No-op for HW decoders.
			(*dc).thread_count = 0;
			(*dc).thread_type = ff::FF_THREAD_SLICE as c_int;
			if ff::avcodec_open2(dc, dec, ptr::null_mut()) < 0 {
				eprintln!("pulsar-render: avcodec_open2 failed");
				break 'decode;
			}
			let dec_name = sel.name.clone();
			let hw = if sel.tier == crate::decode::Tier::Software {
				"sw"
			} else {
				"hw"
			};
			eprintln!(
				"pulsar-render: decoder={dec_name} tier={}",
				sel.tier.as_str()
			);
			// Tell the app which decoder is REALLY in use (the UI shows it read-only —
			// there is no decoder picker; selection is always automatic).
			{
				use std::io::Write;
				println!("vidsink-dec {dec_name} {hw} {}", sel.tier.as_str());
				let _ = std::io::stdout().flush();
				*DEC_LABEL.lock().unwrap() =
					format!("{dec_name} ({})", if hw == "hw" { "HW" } else { "SW" });
			}
			// Tier-1 zero-copy: try av_hwframe_map → DRM_PRIME once; on the first failure
			// fall back to readback (av_hwframe_transfer_data → NV12 upload) permanently.
			//
			// VAAPI is forced onto the readback path: AMD's Mesa VAAPI exports NV12 as a
			// MULTI-LAYER DRM_PRIME descriptor (layer0 = R8 luma, layer1 = GR88 chroma),
			// unlike rkmpp's single composed NV12 layer. The EGLImage import only reads
			// layers[0], so it grabbed the Y plane as an R8 image → the external sampler
			// returned (Y,0,0,1) → a red grayscale screen. Readback (vaGetImage → NV12 CPU
			// buffer → the known-good NV12 upload path) sidesteps the multi-layer import and
			// still HW-decodes (the H.264 work stays on the GPU; only a per-frame NV12 copy
			// is added, sub-ms at 1080p). Zero-copy stays on for rkmpp/others.
			let force_readback =
				sel.hwdev == Some(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI);
			let mut map_failed = false;
			let mut readback_warned = false;
			let pkt = ff::av_packet_alloc();
			let frame = ff::av_frame_alloc();
			let mut last_pub_pace = std::time::Instant::now();
			// Wait for the first KEYFRAME before feeding the decoder. A fresh pass (initial start
			// OR a reopen after a monitor/codec switch) joins the stream at an arbitrary point;
			// decoding P-frames with no reference yet makes rkmpp emit GREEN/mosaic frames until
			// the next IDR (GOP≈2 s). Dropping pre-keyframe packets makes the presenter HOLD the
			// last good frame across the switch instead of flashing green.
			let mut seen_key = false;
			while !STOP.load(Ordering::Relaxed) && !REOPEN.load(Ordering::Relaxed) {
				let r = ff::av_read_frame(fmt, pkt);
				// A switch that never produced its first keyframe within SWITCH_TIMEOUT_MS:
				// intr_cb has aborted the (otherwise indefinite) read. Stop spinning, clear the
				// "switching…" veil and requeue THIS sdp so start_decode's loop reopens it from
				// scratch instead of letting the decode thread die — a silent host that later
				// resumes is then picked up on the retry. (When the host is gone for good this
				// just retries every ~SWITCH_TIMEOUT_MS, which is recoverable, not a hard hang.)
				if r < 0 && !seen_key && switch_timed_out() {
					ff::av_packet_unref(pkt);
					SWITCHING.store(false, Ordering::Relaxed);
					SWITCH_DEADLINE_MS.store(0, Ordering::Relaxed);
					if let Ok(s) = sdp.to_str() {
						*REOPEN_SDP.lock().unwrap() = Some(s.to_string());
					}
					break;
				}
				if r == ff::AVERROR_EOF {
					break;
				}
				if r < 0 {
					ff::av_packet_unref(pkt);
					continue;
				}
				// Gate on the first keyframe (video stream only) — kills the pre-IDR green smear.
				if (*pkt).stream_index == vs && !seen_key {
					if (*pkt).flags & ff::AV_PKT_FLAG_KEY != 0 {
						seen_key = true;
						SWITCHING.store(false, Ordering::Relaxed); // new stream live
						SWITCH_DEADLINE_MS.store(0, Ordering::Relaxed); // switch completed
					} else {
						ff::av_packet_unref(pkt);
						continue;
					}
				}
				// Time the decode work for this packet (send + frame drain), EMA'd per frame into the
				// overlay's "Çözme ms" tile. rkmpp is async HW: timing avcodec_receive_frame alone
				// reads ~0 (the frame is already decoded), so span the whole send→drain instead.
				let dec_t0 = if (*pkt).stream_index == vs {
					// Tally received video bytes for the HUD bitrate (presenter reads this 1×/s).
					VBYTES.fetch_add((*pkt).size.max(0) as u64, Ordering::Relaxed);
					let t0 = std::time::Instant::now();
					ff::avcodec_send_packet(dc, pkt);
					Some(t0)
				} else {
					None
				};
				ff::av_packet_unref(pkt);
				let mut got_frames: u64 = 0;
				while ff::avcodec_receive_frame(dc, frame) == 0 {
					got_frames += 1;
					// Route by output class: DRM_PRIME → zero-copy present; tier-1 hwaccel
					// frames → map to DRM_PRIME (zero-copy) or read back to NV12; software
					// planar/NV12 → GL upload. Anything else is undisplayable → skip.
					let f = (*frame).format;
					let nf: *mut ff::AVFrame;
					if f == ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int || is_displayable_sw(f)
					{
						nf = ff::av_frame_alloc();
						ff::av_frame_move_ref(nf, frame);
					} else if sel.tier == crate::decode::Tier::HwAccel && f == sel.hw_fmt as c_int {
						// Zero-copy first: a VAAPI/DRM frame usually maps straight to a
						// DRM_PRIME dmabuf the EGL path imports with no copy.
						let mut mapped: *mut ff::AVFrame = ptr::null_mut();
						if !map_failed && !force_readback {
							let mf = ff::av_frame_alloc();
							(*mf).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
							if ff::av_hwframe_map(mf, frame, ff::AV_HWFRAME_MAP_READ as c_int) == 0
							{
								mapped = mf;
							} else {
								let mut mf = mf;
								ff::av_frame_free(&mut mf);
								map_failed = true;
								eprintln!(
                                "pulsar-render: hwframe map to DRM_PRIME unavailable — using readback"
                            );
							}
						}
						if mapped.is_null() {
							// Readback (the GenericHwAccel analog): GPU→CPU transfer, NV12 upload.
							// NV12 is REQUESTED explicitly: a 10-bit stream (host HDR → P010)
							// would otherwise read back in a format the presenter routes to its
							// 3-plane branch, dereferencing P010's NULL data[2] → segfault. If
							// the hwaccel refuses NV12, retry letting libav pick — and only
							// queue the frame when the result is really displayable.
							let sw = ff::av_frame_alloc();
							(*sw).format = ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
							let mut ok = ff::av_hwframe_transfer_data(sw, frame, 0) >= 0;
							if !ok {
								ff::av_frame_unref(sw); // resets format to NONE → libav picks
								ok = ff::av_hwframe_transfer_data(sw, frame, 0) >= 0;
							}
							if !ok || !is_displayable_sw((*sw).format) {
								if ok && !readback_warned {
									readback_warned = true;
									eprintln!(
                                    "pulsar-render: readback format {} not displayable — skipping frames",
                                    (*sw).format
                                );
								}
								let mut sw = sw;
								ff::av_frame_free(&mut sw);
								ff::av_frame_unref(frame);
								continue;
							}
							mapped = sw;
						}
						ff::av_frame_unref(frame);
						nf = mapped;
					} else {
						ff::av_frame_unref(frame);
						continue;
					}
					{
						let mut q = MBX.lock().unwrap();
						q.push_back(FramePtr(nf));
						// Bound the queue: if the consumer falls behind, drop the OLDEST frame so
						// latency stays capped (pacing ON) and newest-wins still works (pacing OFF).
						while q.len() > QCAP.load(Ordering::Relaxed) {
							if let Some(old) = q.pop_front() {
								let mut o = old.0;
								ff::av_frame_free(&mut o);
							}
						}
					}
					// Update the source-rate estimate (EMA) so the pacer presents at the host's
					// frame rate, not the display refresh rate.
					{
						let pubt = std::time::Instant::now();
						let dt = pubt.duration_since(last_pub_pace).as_micros() as u64;
						if (1_000..1_000_000).contains(&dt) {
							let prev = SRC_US.load(Ordering::Relaxed);
							SRC_US.store((prev * 7 + dt) / 8, Ordering::Relaxed);
						}
						last_pub_pace = pubt;
					}
				}
				if let Some(t0) = dec_t0 {
					if got_frames > 0 {
						let us = t0.elapsed().as_micros() as u64 / got_frames;
						let prev = DEC_US.load(Ordering::Relaxed);
						DEC_US.store(
							if prev == 0 { us } else { (prev * 7 + us) / 8 },
							Ordering::Relaxed,
						);
					}
				}
			}
			let mut pkt = pkt;
			let mut frame = frame;
			ff::av_packet_free(&mut pkt);
			ff::av_frame_free(&mut frame);
		}
		// Cleanup tail for EVERY exit: the codec context (frees hw_device_ctx with it)
		// and the demuxer. Both calls are no-ops on still-null pointers.
		ff::avcodec_free_context(&mut dc);
		ff::avformat_close_input(&mut fmt);
	}
}

/// Async-signal-safe stop request: ONLY stores the STOP atomic (no lock, no libav free), so it
/// can be called from a SIGINT/SIGTERM handler. The decode loop checks STOP and exits; the actual
/// drain+free (`stop_decode`) must be done from the main thread after the render loop ends, where
/// MBX is not held — calling stop_decode() from the signal handler can deadlock on the MBX mutex
/// the main thread holds every frame in `Presenter::draw`.
pub fn signal_stop() {
	STOP.store(true, Ordering::Relaxed);
}

pub fn stop_decode() {
	STOP.store(true, Ordering::Relaxed);
	// Drain + free any queued frames so none leak on teardown.
	let mut q = MBX.lock().unwrap();
	while let Some(f) = q.pop_front() {
		let mut o = f.0;
		unsafe { ff::av_frame_free(&mut o) };
	}
}

// --- GL video presenter (runs on the main/GL thread) ---
type EglCreateImage =
	unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImage = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type GlEglImageTargetTexture2DOES = unsafe extern "C" fn(u32, *mut c_void);

pub struct Presenter {
	dpy: *mut c_void,
	create_image: EglCreateImage,
	destroy_image: EglDestroyImage,
	image_target: GlEglImageTargetTexture2DOES,
	prog: glow::Program,
	vbo: glow::Buffer,
	tex: glow::Texture,
	// Software present path (non-DRM_PRIME frames: desktop decoders, AV1 fallback):
	// planar YUV 4:2:0 uploaded as three LUMINANCE textures + a YUV→RGB shader.
	sw_prog: glow::Program,
	nv_prog: glow::Program,
	sw_tex: [glow::Texture; 3],
	sw_dims: (i32, i32, c_int), // allocated Y-plane size + pixel format; (0,0,-1) = unallocated
	vid_dims: (i32, i32), // last STREAM size reported on stdout (`vidsink-dims`); (0,0) = none yet
	sw_scratch: Vec<u8>,  // row-repack buffer when an AVFrame linesize is padded
	last: *mut ff::AVFrame, // most-recent frame, RE-PRESENTED every vsync (no flicker on empty)
	last_t: f64,
	frames: u64,
	last_frames: u64,
	last_fresh_t: f64, // time of last FRESH-frame take (present-side cadence probe)
	max_gap_ms: f64,   // worst gap between fresh presents in the current window
	// Moonlight-style adaptive pacer: a rolling window of recent MBX depth samples (one per
	// draw/vblank). The pacer trims the backlog toward 1 frame but tolerates up to PACE_CEIL
	// when the recent min depth shows the queue keeps draining (so it can absorb the next burst).
	depth_hist: [u16; 32],
	depth_idx: usize,
}

const VERT: &str = "attribute vec2 pos;\nattribute vec2 uvin;\nvarying vec2 uv;\nvoid main(){ uv=uvin; gl_Position=vec4(pos,0.0,1.0); }\n";
const FRAG: &str = "#extension GL_OES_EGL_image_external : require\nprecision mediump float;\nvarying vec2 uv;\nuniform samplerExternalOES tex;\nvoid main(){ gl_FragColor=vec4(texture2D(tex,uv).rgb,1.0); }\n";
// Planar YUV → RGB, BT.709 limited range (the stream is HD H.264/HEVC/AV1).
const FRAG_YUV: &str = "precision mediump float;\nvarying vec2 uv;\nuniform sampler2D ty;\nuniform sampler2D tu;\nuniform sampler2D tv;\nvoid main(){\n  float y = (texture2D(ty, uv).r - 0.0627) * 1.1644;\n  float u = texture2D(tu, uv).r - 0.5;\n  float v = texture2D(tv, uv).r - 0.5;\n  gl_FragColor = vec4(y + 1.5748*v, y - 0.1873*u - 0.4681*v, y + 1.8556*u, 1.0);\n}\n";
// NV12 (2-plane, interleaved UV) → RGB, BT.709 limited. The UV plane uploads as
// LUMINANCE_ALPHA: U lands in .r (=L), V in .a.
const FRAG_NV12: &str = "precision mediump float;\nvarying vec2 uv;\nuniform sampler2D ty;\nuniform sampler2D tuv;\nvoid main(){\n  float y = (texture2D(ty, uv).r - 0.0627) * 1.1644;\n  float u = texture2D(tuv, uv).r - 0.5;\n  float v = texture2D(tuv, uv).a - 0.5;\n  gl_FragColor = vec4(y + 1.5748*v, y - 0.1873*u - 0.4681*v, y + 1.8556*u, 1.0);\n}\n";

impl Presenter {
	/// `get_proc` resolves EGL/GL functions (egl.get_proc_address). `dpy` = raw EGLDisplay.
	pub unsafe fn new(
		gl: &glow::Context,
		dpy: *mut c_void,
		get_proc: &dyn Fn(&str) -> *const c_void,
	) -> Self {
		use glow::HasContext;
		let create_image: EglCreateImage = std::mem::transmute(get_proc("eglCreateImageKHR"));
		let destroy_image: EglDestroyImage = std::mem::transmute(get_proc("eglDestroyImageKHR"));
		let image_target: GlEglImageTargetTexture2DOES =
			std::mem::transmute(get_proc("glEGLImageTargetTexture2DOES"));

		let build = |vert: &str, frag: &str| -> glow::Program {
			let prog = gl.create_program().unwrap();
			for (ty, src) in [(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)] {
				let s = gl.create_shader(ty).unwrap();
				gl.shader_source(s, src);
				gl.compile_shader(s);
				if !gl.get_shader_compile_status(s) {
					eprintln!("pulsar-render: video shader: {}", gl.get_shader_info_log(s));
				}
				gl.attach_shader(prog, s);
			}
			gl.bind_attrib_location(prog, 0, "pos");
			gl.bind_attrib_location(prog, 1, "uvin");
			gl.link_program(prog);
			prog
		};
		let prog = build(VERT, FRAG);
		let sw_prog = build(VERT, FRAG_YUV);
		let nv_prog = build(VERT, FRAG_NV12);
		// Bind the samplers to fixed texture units once — program state persists.
		gl.use_program(Some(sw_prog));
		for (i, name) in ["ty", "tu", "tv"].iter().enumerate() {
			if let Some(loc) = gl.get_uniform_location(sw_prog, name) {
				gl.uniform_1_i32(Some(&loc), i as i32);
			}
		}
		gl.use_program(Some(nv_prog));
		for (i, name) in ["ty", "tuv"].iter().enumerate() {
			if let Some(loc) = gl.get_uniform_location(nv_prog, name) {
				gl.uniform_1_i32(Some(&loc), i as i32);
			}
		}
		gl.use_program(None);

		let vbo = gl.create_buffer().unwrap();
		let tex = gl.create_texture().unwrap();
		gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(tex));
		gl.tex_parameter_i32(
			GL_TEXTURE_EXTERNAL_OES,
			glow::TEXTURE_MIN_FILTER,
			glow::LINEAR as i32,
		);
		gl.tex_parameter_i32(
			GL_TEXTURE_EXTERNAL_OES,
			glow::TEXTURE_MAG_FILTER,
			glow::LINEAR as i32,
		);
		gl.tex_parameter_i32(
			GL_TEXTURE_EXTERNAL_OES,
			glow::TEXTURE_WRAP_S,
			glow::CLAMP_TO_EDGE as i32,
		);
		gl.tex_parameter_i32(
			GL_TEXTURE_EXTERNAL_OES,
			glow::TEXTURE_WRAP_T,
			glow::CLAMP_TO_EDGE as i32,
		);
		let sw_tex = [(); 3].map(|_| {
			let t = gl.create_texture().unwrap();
			gl.bind_texture(glow::TEXTURE_2D, Some(t));
			gl.tex_parameter_i32(
				glow::TEXTURE_2D,
				glow::TEXTURE_MIN_FILTER,
				glow::LINEAR as i32,
			);
			gl.tex_parameter_i32(
				glow::TEXTURE_2D,
				glow::TEXTURE_MAG_FILTER,
				glow::LINEAR as i32,
			);
			gl.tex_parameter_i32(
				glow::TEXTURE_2D,
				glow::TEXTURE_WRAP_S,
				glow::CLAMP_TO_EDGE as i32,
			);
			gl.tex_parameter_i32(
				glow::TEXTURE_2D,
				glow::TEXTURE_WRAP_T,
				glow::CLAMP_TO_EDGE as i32,
			);
			t
		});

		Presenter {
			dpy,
			create_image,
			destroy_image,
			image_target,
			prog,
			vbo,
			tex,
			sw_prog,
			nv_prog,
			sw_tex,
			sw_dims: (0, 0, -1),
			vid_dims: (0, 0),
			sw_scratch: Vec::new(),
			last: ptr::null_mut(),
			last_t: now_s(),
			frames: 0,
			last_frames: 0,
			last_fresh_t: now_s(),
			max_gap_ms: 0.0,
			depth_hist: [0; 32],
			depth_idx: 0,
		}
	}

	/// Upload one packed-tight plane (repacking padded `linesize` rows first).
	/// `channels` = 1 → LUMINANCE (Y/U/V planes), 2 → LUMINANCE_ALPHA (NV12's UV).
	unsafe fn upload_plane(
		&mut self,
		gl: &glow::Context,
		idx: usize,
		w: i32,
		h: i32,
		channels: i32,
		data: *const u8,
		stride: i32,
		alloc: bool,
	) {
		use glow::HasContext;
		gl.bind_texture(glow::TEXTURE_2D, Some(self.sw_tex[idx]));
		let fmt = if channels == 2 {
			glow::LUMINANCE_ALPHA
		} else {
			glow::LUMINANCE
		};
		let row_bytes = (w * channels) as usize;
		let tight: &[u8] = if stride as usize == row_bytes {
			std::slice::from_raw_parts(data, row_bytes * (h as usize))
		} else {
			self.sw_scratch.resize(row_bytes * (h as usize), 0);
			for row in 0..h as usize {
				let src = data.add(row * stride as usize);
				let dst = self.sw_scratch.as_mut_ptr().add(row * row_bytes);
				std::ptr::copy_nonoverlapping(src, dst, row_bytes);
			}
			&self.sw_scratch
		};
		if alloc {
			gl.tex_image_2d(
				glow::TEXTURE_2D,
				0,
				fmt as i32,
				w,
				h,
				0,
				fmt,
				glow::UNSIGNED_BYTE,
				Some(tight),
			);
		} else {
			gl.tex_sub_image_2d(
				glow::TEXTURE_2D,
				0,
				0,
				0,
				w,
				h,
				fmt,
				glow::UNSIGNED_BYTE,
				glow::PixelUnpackData::Slice(tight),
			);
		}
	}

	/// Push one MBX-depth sample (one per vblank) into the rolling window and return the MIN
	/// depth over the window — the Moonlight adaptive-drop signal (pacer.cpp:210-242). A recent
	/// min ≤ 1 means the queue keeps draining (jitter / source ≈ display), so the pacer may
	/// buffer up to PACE_CEIL to absorb the next burst; a recent min > 1 means a sustained
	/// backlog (source faster than present), so it trims hard toward 1 frame. The 32-element
	/// scan is O(1)-bounded and runs while the MBX lock is held — kept trivial on purpose.
	fn push_depth(&mut self, depth: usize) -> usize {
		let n = self.depth_hist.len();
		self.depth_hist[self.depth_idx % n] = depth.min(u16::MAX as usize) as u16;
		self.depth_idx = self.depth_idx.wrapping_add(1);
		self.depth_hist.iter().copied().min().unwrap_or(0) as usize
	}

	/// Draw the newest decoded frame letterboxed into a `w`x`h` viewport. Returns true if a frame
	/// was present (so the caller knows video is live).
	/// Drop the held last frame + any queued frames so the next `draw()` reports NO video
	/// until the NEW stream delivers one. Used on a new-host switch so the previous host's
	/// last frame is never shown under the new session.
	pub unsafe fn reset(&mut self) {
		if !self.last.is_null() {
			let mut old = self.last;
			ff::av_frame_free(&mut old);
			self.last = ptr::null_mut();
		}
		let mut q = MBX.lock().unwrap();
		while let Some(f) = q.pop_front() {
			let mut o = f.0;
			ff::av_frame_free(&mut o);
		}
		drop(q);
		self.vid_dims = (0, 0);
		VID_W.store(0, Ordering::Relaxed);
		VID_H.store(0, Ordering::Relaxed);
	}

	pub unsafe fn draw(&mut self, gl: &glow::Context, w: i32, h: i32) -> bool {
		use glow::HasContext;
		// Take the newest decoded frame if there is one; else RE-PRESENT the last frame. The
		// render loop runs at vsync (~display rate) but video arrives slower (e.g. 30 fps), so most
		// presents have no new frame — without re-presenting the last one those frames would clear
		// to black → flicker. A new frame bumps the fps counter; a re-present does not.
		let fresh = {
			let mut q = MBX.lock().unwrap();
			if PACE.load(Ordering::Relaxed) {
				// Pacing ON — Moonlight per-vblank metering (pacer.cpp:201-260). draw() IS the
				// per-vblank tick (egl.swap_buffers + swap_interval=1 in linux.rs blocks to
				// vblank), so present EXACTLY ONE frame per call: a clumped/bursty arrival is
				// metered out one step per refresh instead of collapsing to a single update (the
				// "teleport"). An empty queue returns None → the re-present path below HOLDS the
				// last frame (no black, no jump), bridging a delivery gap smoothly.
				//
				// Cadence is the REAL vblank, not an SRC_US wall-clock timer (the old pacer beat-
				// drifted against the panel and paid a fixed multi-frame prebuffer). Adaptive
				// depth: trim the backlog toward 1 frame (low latency) but tolerate up to
				// PACE_CEIL when the recent min depth shows the queue keeps draining, so the next
				// burst is absorbed without dropping mid-burst.
				let recent_min = self.push_depth(q.len());
				let target = if recent_min <= 1 {
					PACE_CEIL.load(Ordering::Relaxed).max(1)
				} else {
					1
				};
				// Catch-up: shed the OLDEST down to `target` BEFORE presenting (free each shed
				// frame) so steady-state latency self-corrects to ~1 frame.
				while q.len() > target {
					if let Some(old) = q.pop_front() {
						let mut o = old.0;
						ff::av_frame_free(&mut o);
					}
				}
				q.pop_front() // exactly one (oldest) frame this vblank; None ⇒ underflow HOLD
			} else {
				// Pacing OFF: newest-wins — keep only the most recent frame, free the skipped.
				let mut newest = None;
				while let Some(f) = q.pop_front() {
					if let Some(old) = newest.replace(f) {
						let mut o = old.0;
						ff::av_frame_free(&mut o);
					}
				}
				newest
			}
		};
		if let Some(FramePtr(nf)) = fresh {
			if !self.last.is_null() {
				let mut old = self.last;
				ff::av_frame_free(&mut old);
			}
			self.last = nf;
			self.frames += 1;
			let t = now_s();
			let gap = (t - self.last_fresh_t) * 1000.0;
			if gap > self.max_gap_ms {
				self.max_gap_ms = gap;
			}
			self.last_fresh_t = t;
			// Record the wall-clock of this fresh frame so linux.rs can detect a stall
			// (no frame for ≥ STALL_SECS) and clear STALLED as soon as frames resume.
			LAST_FRAME_MS.store(mono_ms(), Ordering::Relaxed);
			STALLED.store(false, Ordering::Relaxed);
		}
		let cur = self.last;
		if cur.is_null() {
			return false;
		}

		let vw = (*cur).width;
		let vh = (*cur).height;
		// First frame (or a live resolution switch): report the STREAM size on stdout so
		// the app can size the session window to the host's aspect ratio.
		if (vw, vh) != self.vid_dims && vw > 0 && vh > 0 {
			self.vid_dims = (vw, vh);
			VID_W.store(vw, Ordering::Relaxed);
			VID_H.store(vh, Ordering::Relaxed);
			println!("vidsink-dims {vw}x{vh}");
			use std::io::Write as _;
			let _ = std::io::stdout().flush();
		}
		let is_prime = (*cur).format == ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;

		// Bind the right source: DRM_PRIME → zero-copy EGLImage on the external sampler;
		// anything else → planar YUV 4:2:0 plane upload (desktop decoders, AV1 fallback).
		let mut img: *mut c_void = ptr::null_mut();
		if is_prime {
			let d = (*cur).data[0] as *const ff::AVDRMFrameDescriptor;
			let layer = &(*d).layers[0];

			let mut a: Vec<i32> = Vec::with_capacity(64);
			a.push(EGL_LINUX_DRM_FOURCC_EXT);
			a.push(layer.format as i32);
			a.push(EGL_WIDTH);
			a.push(vw);
			a.push(EGL_HEIGHT);
			a.push(vh);
			// Crash hardening: the EXT attribute tables are fixed [i32; 3] and the
			// descriptor's object array is fixed-capacity, so a malformed/exotic DRM
			// descriptor (>3 planes, or a plane whose object_index points past the
			// reported objects) would index out of bounds and PANIC. Clamp the plane
			// count to the 3 attribute slots we have, and skip any plane with a bad
			// object_index instead of dereferencing it. Log once if we ever clamp so a
			// real multi-plane format isn't silently truncated unnoticed.
			let nb_planes = layer.nb_planes as usize;
			if nb_planes > FD_EXT.len() {
				static WARNED: AtomicBool = AtomicBool::new(false);
				if !WARNED.swap(true, Ordering::Relaxed) {
					eprintln!(
						"pulsar-render: DRM descriptor has {nb_planes} planes; clamping to {} (rest ignored)",
						FD_EXT.len()
					);
				}
			}
			let nb_objects = (*d).nb_objects as usize;
			for i in 0..nb_planes.min(FD_EXT.len()) {
				let p = &layer.planes[i];
				let oi = p.object_index as usize;
				if oi >= nb_objects || oi >= (*d).objects.len() {
					// Bad object_index → skip this plane rather than OOB-deref.
					continue;
				}
				let o = &(*d).objects[oi];
				a.push(FD_EXT[i]);
				a.push(o.fd);
				a.push(OFFSET_EXT[i]);
				a.push(p.offset as i32);
				a.push(PITCH_EXT[i]);
				a.push(p.pitch as i32);
				if o.format_modifier != DRM_FORMAT_MOD_INVALID {
					a.push(MOD_LO_EXT[i]);
					a.push((o.format_modifier & 0xFFFF_FFFF) as i32);
					a.push(MOD_HI_EXT[i]);
					a.push((o.format_modifier >> 32) as i32);
				}
			}
			a.push(EGL_NONE_I);

			img = (self.create_image)(
				self.dpy,
				ptr::null_mut(),
				EGL_LINUX_DMA_BUF_EXT,
				ptr::null_mut(),
				a.as_ptr(),
			);
			if img.is_null() {
				let mut cur = cur; // cur is *mut, already a copy of self.last
				ff::av_frame_free(&mut cur);
				self.last = ptr::null_mut(); // critical: drop the dangling pointer so it can't be re-freed/derefed next frame
				return false;
			}
			gl.use_program(Some(self.prog));
			gl.active_texture(glow::TEXTURE0);
			gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.tex));
			(self.image_target)(GL_TEXTURE_EXTERNAL_OES, img);
		} else if (*cur).format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
			// NV12 (hwaccel readback): full-res Y + half-res interleaved UV.
			let fmt = (*cur).format;
			let (cw, ch) = ((vw + 1) / 2, (vh + 1) / 2);
			gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
			let alloc = self.sw_dims != (vw, vh, fmt);
			gl.active_texture(glow::TEXTURE0);
			self.upload_plane(
				gl,
				0,
				vw,
				vh,
				1,
				(*cur).data[0] as *const u8,
				(*cur).linesize[0],
				alloc,
			);
			gl.active_texture(glow::TEXTURE0 + 1);
			self.upload_plane(
				gl,
				1,
				cw,
				ch,
				2,
				(*cur).data[1] as *const u8,
				(*cur).linesize[1],
				alloc,
			);
			if alloc {
				self.sw_dims = (vw, vh, fmt);
			}
			gl.use_program(Some(self.nv_prog));
		} else {
			// Planar YUV: full-res Y + per-format chroma planes (half for 4:2:0, full for
			// 4:4:4 …). (Re)allocate on size/format change, then per-frame tex_sub uploads.
			let fmt = (*cur).format;
			let (cw, ch) = chroma_dims(fmt, vw, vh);
			gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
			let alloc = self.sw_dims != (vw, vh, fmt);
			let planes = [
				((*cur).data[0] as *const u8, (*cur).linesize[0], vw, vh),
				((*cur).data[1] as *const u8, (*cur).linesize[1], cw, ch),
				((*cur).data[2] as *const u8, (*cur).linesize[2], cw, ch),
			];
			for (i, (data, stride, pw, ph)) in planes.into_iter().enumerate() {
				gl.active_texture(glow::TEXTURE0 + i as u32);
				self.upload_plane(gl, i, pw, ph, 1, data, stride, alloc);
			}
			if alloc {
				self.sw_dims = (vw, vh, fmt);
			}
			gl.use_program(Some(self.sw_prog));
			// upload_plane left each plane bound on its unit (0/1/2), matching the sampler
			// uniforms set at init.
		}

		// View fit (AnyDesk-style): FIT letterboxes (keep aspect), STRETCH fills the
		// window (distorts), ORIGINAL presents 1:1 source pixels centered (crops when
		// the stream is larger than the window — GL clips negative-origin viewports).
		let (rw, rh) = match FIT_MODE.load(Ordering::Relaxed) {
			1 => (w, h),
			2 => (vw, vh),
			_ => {
				let r2 = (w as i64 * vh as i64 / vw as i64) as i32;
				if r2 > h {
					((h as i64 * vw as i64 / vh as i64) as i32, h)
				} else {
					(w, r2)
				}
			}
		};
		let (vx, vy) = ((w - rw) / 2, (h - rh) / 2);
		// Publish the letterbox rect so the cursor side-channel overlay maps the host pointer
		// into the SAME rect the frame fills. Note the GL viewport origin is BOTTOM-left; the
		// overlay (egui) is TOP-left, so it flips Y itself from `[x, y_bottom, w, h]`.
		*VIDEO_RECT.lock().unwrap() = [vx, vy, rw, rh];
		// Source (host-pixel) dims alongside the rect — the cursor overlay needs both to derive
		// the host→displayed scale for sizing the side-channel pointer (see VIDEO_SRC).
		*VIDEO_SRC.lock().unwrap() = [vw, vh];
		gl.viewport(vx, vy, rw, rh);
		let quad: [f32; 24] = [
			-1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, -1.0, -1.0, 0.0, 1.0,
			1.0, 1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 0.0,
		];
		gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
		gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_cast(&quad), glow::STREAM_DRAW);
		gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
		gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
		gl.enable_vertex_attrib_array(0);
		gl.enable_vertex_attrib_array(1);
		gl.draw_arrays(glow::TRIANGLES, 0, 6);

		if !img.is_null() {
			(self.destroy_image)(self.dpy, img);
		}
		// Restore the egui-expected active unit (it manages TEXTURE0 itself, but be tidy).
		gl.active_texture(glow::TEXTURE0);

		let t = now_s();
		if t - self.last_t >= 1.0 {
			let win = t - self.last_t;
			let f = (self.frames - self.last_frames) as f32 / win as f32;
			// Received-stream bitrate: drain the decode thread's byte tally over this window.
			let bytes = VBYTES.swap(0, Ordering::Relaxed);
			let mbit = (bytes as f64 * 8.0 / win / 1e6) as f32;
			*FPS.lock().unwrap() = [f, mbit, self.max_gap_ms as f32];
			self.last_t = t;
			self.last_frames = self.frames;
			self.max_gap_ms = 0.0;
		}
		true
	}
}

fn now_s() -> f64 {
	let t = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap_or_default();
	t.as_secs_f64()
}

fn bytemuck_cast(f: &[f32]) -> &[u8] {
	unsafe { std::slice::from_raw_parts(f.as_ptr() as *const u8, f.len() * 4) }
}
