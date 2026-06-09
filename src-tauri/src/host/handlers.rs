//! Per-session handler factories extracted from `go_online`'s serve loop. Each
//! `make_*` builds and returns the same inline closure the host wired before, so
//! behavior is identical; `spawn_loopback_audio` is the Windows WASAPI helper
//! `make_on_stream` calls within this module.

use super::*;

/// Windows: stream the host's system audio via **WASAPI loopback** (no
/// `virtual-audio-capturer` / Stereo Mix device required). Spawns the encoder ffmpeg
/// reading raw PCM on stdin (Opus/RTP to `dest`) plus a capture thread that feeds it
/// the default render endpoint's loopback. The ffmpeg is tracked in `procs`, so a
/// (re-)stream or session teardown kills it — which ends the capture thread via the
/// broken pipe. Returns whether it started (callers fall back to the dshow path).
#[cfg(windows)]
pub(super) fn spawn_loopback_audio(procs: &Arc<Mutex<Vec<Child>>>, ffmpeg: &str, dest: &str) -> bool {
	use std::process::Stdio;
	let fmt = match pulsar_core::audio::loopback_format() {
		Ok(f) => f,
		Err(_) => {
			return false;
		}
	};
	let mut args: Vec<String> = vec![
		"-hide_banner".into(),
		"-loglevel".into(),
		"error".into(),
		"-f".into(),
		fmt.ffmpeg_sample_fmt().into(),
		"-ar".into(),
		fmt.rate.to_string(),
		"-ac".into(),
		fmt.channels.to_string(),
		"-i".into(),
		"pipe:0".into(),
	];
	args.extend(pulsar_core::audio::opus_rtp_output(dest));
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args(&args).stdin(Stdio::piped());
	no_window(&mut cmd);
	let mut child = match cmd.spawn() {
		Ok(c) => c,
		Err(_) => {
			return false;
		}
	};
	let stdin = match child.stdin.take() {
		Some(s) => s,
		None => {
			let _ = child.kill();
			return false;
		}
	};
	crate::job::assign(&child); // tie ffmpeg to Pulsar's lifetime (job.rs), like spawn_tracked
	procs.lock().unwrap().push(child);
	std::thread::spawn(move || {
		// Runs until the pipe breaks (ffmpeg killed on teardown) or WASAPI errors — both expected.
		let _ = pulsar_core::audio::run_loopback_capture(stdin);
	});
	true
}

/// Start the host→client audio stream (Opus/RTP) and apply the requested host-mute.
/// Shared by the X11/Windows fall-through path and the Wayland branch so a Wayland
/// host streams audio + honors game-mode mute exactly like the X11 path. Synchronous:
/// it only spawns tracked children (`spawn_tracked`/`spawn_loopback_audio`) and makes
/// the blocking `set_host_muted` call — it must run in the closure body, not the async
/// portal-capture task. Re-evaluated on every (re-)stream so live toggles take effect.
fn start_audio_and_mute(
	procs: &Arc<Mutex<Vec<Child>>>,
	ffmpeg: &str,
	app_h: &AppHandle,
	addr: SocketAddr,
	req: &StreamReq,
	host_muted: &Arc<AtomicBool>,
) {
	// Audio: a second ffmpeg streams Opus/RTP to the client's audio port.
	// Transmit + host-mute are driven by the session-menu toggles in the
	// request (game mode defaults both on, client-side).
	let acfg = pulsar_core::Config::load(config_path(app_h));
	if req.transmit_audio && req.audio_port > 0 {
		let dest = format!("rtp://{}:{}", addr.ip(), req.audio_port);
		// Windows: prefer WASAPI loopback — it taps whatever is playing on the
		// default output, so it works with NO virtual-audio-capturer / Stereo
		// Mix device installed. Falls back to the dshow command if it can't
		// start or a specific capture device name was configured.
		#[cfg(windows)]
		let started_audio = acfg.audio_loopback() && spawn_loopback_audio(procs, ffmpeg, &dest);
		#[cfg(not(windows))]
		let started_audio = false;
		if !started_audio {
			let (_, aargs) = pulsar_core::audio::audio_command(&acfg.audio_input(), &dest);
			let _ = spawn_tracked(procs, ffmpeg, &aargs);
		}
	}
	// Apply the requested host-mute state (re-evaluated on every
	// (re-)stream so a live toggle takes effect).
	let want_mute = req.mute_host;
	if want_mute != host_muted.load(Ordering::SeqCst) {
		match pulsar_core::audio::set_host_muted(want_mute) {
			Ok(()) => host_muted.store(want_mute, Ordering::SeqCst),
			Err(e) => tracing::warn!("host mute toggle failed: {e}"),
		}
	}
}

/// Build the per-session `on_stream` handler. A (re-)stream request restarts
/// capture: it kills any ffmpeg/native capture already running for this session,
/// then spawns the new encode (native DXGI+NVENC on Windows, else ffmpeg), pushes
/// the encode summary + display rotation to the client, starts the audio stream,
/// and applies the requested host-mute. On Wayland it routes through the portals.
#[allow(clippy::too_many_arguments)]
pub(super) fn make_on_stream(
	stream_cfg: Arc<Mutex<crate::state::StreamCfg>>,
	procs: Arc<Mutex<Vec<Child>>>,
	active: Arc<Mutex<std::collections::HashMap<String, crate::state::ConnInfo>>>,
	sid: u64,
	#[cfg(windows)] native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>>,
	host_muted: Arc<AtomicBool>,
	stats_out: tokio::sync::mpsc::Sender<DataMsg>,
	app_h: AppHandle,
	peer: String,
	#[cfg(target_os = "linux")] restore_token: Arc<Mutex<Option<String>>>,
	#[cfg(target_os = "linux")] cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>>,
) -> impl FnMut(StreamReq, SocketAddr) + Send + 'static {
	let mut announced = false;
	move |req: StreamReq, addr: SocketAddr| {
		let cfg = stream_cfg.lock().unwrap().clone();

		// First stream request reveals this connection's mode: record it and open the
		// dedicated connections window — brought forward for a Remote connection, opened
		// hidden for a Game one (so it doesn't disrupt / leak into the streamed game).
		// Done once (not on re-streams) so a live resolution change doesn't re-pop it.
		if !announced {
			announced = true;
			let mode = if req.game_mode {
				crate::state::ConnMode::Game
			} else {
				crate::state::ConnMode::Remote
			};
			if let Some(ci) = active.lock().unwrap().get_mut(&peer) {
				if ci.sid == sid {
					ci.mode = mode;
				}
			}
			crate::connections::open_or_update(&app_h, !req.game_mode);
		}

		// Wayland: x11grab of rootless Xwayland is black, so capture the
		// real screen (and inject input) through the desktop portals.
		#[cfg(target_os = "linux")]
		if pulsar_core::capture::is_wayland() {
			// A (re-)stream restarts capture: kill any audio ffmpeg already running for
			// this session before spawning the new one, so a live re-stream (resolution/
			// codec/fps/audio-toggle change) doesn't stack audio encoders. (The video
			// capture is restarted separately via `cap_slot` in the async task below.)
			for mut child in procs.lock().unwrap().drain(..) {
				let _ = child.kill();
				let _ = child.wait();
			}
			let ip = addr.ip().to_string();
			let (port, codec) = (req.port, req.codec.clone());
			// Client-requested bitrate wins; 0 falls back to the host config.
			let eff_bitrate = if req.bitrate_kbps > 0 {
				req.bitrate_kbps
			} else {
				cfg.bitrate_kbps
			};
			let eff_fps = if req.fps > 0 { req.fps } else { cfg.fps };
			let (bitrate, fps) = (eff_bitrate, eff_fps);
			let token = restore_token.lock().unwrap().clone();
			let restore_token = restore_token.clone();
			let cap_slot = cap_slot.clone();
			// Clone for the spawned capture task; the param `app_h` stays owned so the
			// synchronous audio+host-mute below (and FnMut re-calls) can still use it.
			let app_h_task = app_h.clone();
			let peer = peer.clone();
			tokio::spawn(async move {
				// A (re-)stream restarts capture: stop the prior WaylandCapture FIRST —
				// kill its gst-launch AND close the portal session. WaylandCapture has no
				// Drop and the gst child has PR_SET_PDEATHSIG, so a bare overwrite would
				// neither kill it nor stop the duplicate RTP (and the compositor would keep
				// showing "screen is being shared"). Take the Option in its own statement so
				// the MutexGuard is dropped before the `.await` (std guards are !Send).
				let prev = cap_slot.lock().unwrap().take();
				if let Some(p) = prev {
					p.stop().await;
				}
				match pulsar_core::capture::start(
					&ip, port, &codec, bitrate, fps, token,
				)
				.await
				{
					Ok((cap, new_token)) => {
						if let Some(t) = new_token {
							*restore_token.lock().unwrap() = Some(t);
						}
						*cap_slot.lock().unwrap() = Some(cap);
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: "Wayland · ekran + kontrol".into(),
							},
						);
					}
					Err(e) => {
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: format!("Wayland yakalama başarısız: {e}"),
							},
						);
					}
				}
			});
			// Audio + host-mute on Wayland too (the early return above used to skip both,
			// so a Wayland host streamed silent video and never muted in game mode). Runs
			// synchronously in the closure body, like the X11 path; the Pulse `.monitor`
			// ffmpeg is tracked in `procs` and killed on (re-)stream/teardown.
			let ffmpeg = ffmpeg_bin(&app_h);
			start_audio_and_mute(&procs, &ffmpeg, &app_h, addr, &req, &host_muted);
			return;
		}

		// A (re-)stream request restarts capture: kill any ffmpeg already
		// running for this session before spawning the new one (this is how
		// a live resolution change from the client takes effect).
		for mut child in procs.lock().unwrap().drain(..) {
			let _ = child.kill();
			let _ = child.wait();
		}
		// Same for the native capture thread, if the prior (re)stream used it.
		#[cfg(windows)]
		if let Some(h) = native_slot.lock().unwrap().take() {
			h.stop();
		}
		// Client-requested size/fps/bitrate win; 0 falls back to the host config.
		let eff_w = if req.width > 0 { req.width } else { cfg.width };
		let eff_h = if req.height > 0 { req.height } else { cfg.height };
		let eff_fps = if req.fps > 0 { req.fps } else { cfg.fps };
		let eff_bitrate = if req.bitrate_kbps > 0 {
			req.bitrate_kbps
		} else {
			cfg.bitrate_kbps
		};
		// Clamp the capture resolution to the host's actual screen. ffmpeg's x11grab/gdigrab grab a
		// REGION of size `-video_size`, which must be ≤ the screen or ffmpeg dies ("Capture area …
		// outside the screen size") and streams NO video — hit when a 1440p-configured stream
		// targets a 1080p host (e.g. an Orange Pi acting as host). Windows captures via the native
		// DXGI path (scales internally), so this only guards the ffmpeg capture path.
		#[cfg(not(windows))]
		let (eff_w, eff_h) = match crate::util::display_size(&cfg.display) {
			Some((sw, sh)) if eff_w > sw || eff_h > sh => (sw, sh),
			_ => (eff_w, eff_h),
		};
		let ffmpeg = ffmpeg_bin(&app_h);
		// The viewer picks the encoder live from the session menu (sent in the
		// stream request); an empty request falls back to the host's own setting.
		// `resolve` still degrades gracefully if this host lacks that encoder.
		let enc_pref = if req.encoder.is_empty() {
			cfg.encoder.clone()
		} else {
			req.encoder.clone()
		};
		// Probe the bundled ffmpeg ONCE: which backends exist, and (per backend) which
		// codecs. `resolve` degrades the encoder; `resolve_codec` then degrades the codec
		// to what that encoder can actually emit (requested → H.264 → first available), so a
		// HEVC/AV1 request on a build lacking it falls back instead of failing.
		let enc_text = crate::process::encoders_text(&ffmpeg);
		let encoder = pipeline::resolve(
			encoder_from_str(&enc_pref),
			&pipeline::detect(&enc_text),
		);
		// Off-Windows, ffmpeg is the ONLY encode path, so an encoder ffmpeg merely *lists* (a generic
		// build lists h264_nvenc even with no NVIDIA GPU) but can't initialize here must be dropped,
		// not used — else it fails at spawn and sends no video (the Orange-Pi-as-host case:
		// h264_nvenc → "Cannot load libcuda.so.1"). Validate + degrade to a working encoder
		// (ultimately libx264). Windows keeps its native-NVENC path + hybrid guard (compiled out here).
		#[cfg(not(windows))]
		let encoder = crate::process::resolve_encoder_validated(
			&ffmpeg,
			encoder,
			&enc_text,
			&cfg.vaapi_device,
		);
		// Validate the codec with a real one-frame encode probe (cached) — catches "listed
		// but the GPU/driver can't init it" (e.g. av1_nvenc on Ampere), degrading to a codec
		// that actually works rather than producing a dead stream.
		let codec = crate::process::resolve_codec_validated(
			&ffmpeg,
			encoder,
			codec_from_str(&req.codec),
			&cfg.vaapi_device,
		);
		let capture = capture_from_str(&cfg.capture);
		// NVENC + ddagrab: probe ONCE whether the fully zero-copy
		// D3D11→CUDA→NVENC path works (display on the NVIDIA GPU). On a
		// hybrid box it doesn't, and we use the GPU-scale path instead.
		let gpu_zerocopy = if encoder == HwEncoder::Nvenc
			&& capture == CaptureMethod::Ddagrab
		{
			let ff = ffmpeg.clone();
			*DDAGRAB_ZEROCOPY.get_or_init(|| probe_ddagrab_zerocopy(&ff))
		} else {
			false
		};
		let plan = StreamPlan {
			encoder,
			codec,
			width: eff_w,
			height: eff_h,
			fps: eff_fps,
			bitrate_kbps: eff_bitrate,
			capture,
			display: cfg.display.clone(),
			vaapi_device: cfg.vaapi_device.clone(),
			dest: format!("rtp://{}:{}", addr.ip(), req.port),
			// Quality bias: explicit client preference wins; `Balanced` defers to
			// game_mode (no regression — game mode → lowest latency, remote → quality).
			low_latency: match req.quality {
				QualityPref::Quality => false,
				QualityPref::Latency => true,
				QualityPref::Balanced => req.game_mode,
			},
			gpu_zerocopy,
			hdr: req.hdr,
			yuv444: req.yuv444,
		};
		// NATIVE WINDOWS path: DXGI Desktop Duplication + NVENC SDK → RTP
		// (Sunshine-technique, steady client-fps). Used for NVENC on Windows
		// unless PULSAR_FFMPEG_CAPTURE=1. Init happens inside the capture thread
		// and is reported back synchronously — Ok ⇒ streaming started; Err ⇒ fall
		// back to ffmpeg with zero behaviour change.
		#[cfg(windows)]
		let native_started = if encoder == HwEncoder::Nvenc
			&& capture == CaptureMethod::Ddagrab
			&& !req.hdr
			&& !req.yuv444
			&& std::env::var_os("PULSAR_FFMPEG_CAPTURE").is_none()
		{
			let ncodec = match codec {
				pipeline::VCodec::H264 => pulsar_capture::Codec::H264,
				pipeline::VCodec::H265 => pulsar_capture::Codec::H265,
				pipeline::VCodec::Av1 => pulsar_capture::Codec::Av1,
			};
			match pulsar_capture::start_capture_encode(pulsar_capture::CaptureConfig {
				width: eff_w,
				height: eff_h,
				fps: eff_fps,
				bitrate_kbps: eff_bitrate,
				dest: format!("rtp://{}:{}", addr.ip(), req.port),
				codec: ncodec,
				output_idx: 0,
				low_latency: plan.low_latency,
				draw_mouse: true,
			}) {
				Ok(h) => {
					*native_slot.lock().unwrap() = Some(h);
					true
				}
				Err(_) => {
					false
				}
			}
		} else {
			false
		};
		#[cfg(not(windows))]
		let native_started = false;

		// encode_command always yields ("ffmpeg", args); run the bundled
		// ffmpeg binary directly rather than relying on a system ffmpeg.
		let started = if native_started {
			true
		} else {
			let (_, args) = pipeline::encode_command(&plan);
			spawn_tracked(&procs, &ffmpeg, &args).is_ok()
		};
		// Push the encode summary (codec · encoder · res · fps) to the client
		// for its perf tooltip.
		// Reflect the RESOLVED codec (after `resolve_codec` fallback), not the request —
		// the client uses this to pick its decoder, so it must match what we actually send.
		let codec_label = match codec {
			pipeline::VCodec::H265 => "H.265",
			pipeline::VCodec::Av1 => "AV1",
			pipeline::VCodec::H264 => "H.264",
		};
		let _ = stats_out.try_send(DataMsg::Stats(format!(
			"{} · {} · {}p · {}fps",
			codec_label,
			encoder.label(),
			eff_h,
			eff_fps
		)));
		// Tell the client our display orientation so it can render the video upright even if
		// this host's screen is rotated (e.g. a tent-mode laptop). The NATIVE capture path
		// (pulsar-capture) already BAKES the rotation into the encoded stream via the
		// VideoProcessor Blt, so we report 0 then (avoids double-rotation); the ffmpeg fallback
		// path does NOT rotate, so it reports the real rotation for the client to apply.
		let reported_rotation = if native_started { 0 } else { display_rotation() };
		let _ = stats_out.try_send(DataMsg::DisplayRotation(reported_rotation));
		start_audio_and_mute(&procs, &ffmpeg, &app_h, addr, &req, &host_muted);
		let _ = app_h.emit(
			"session",
			SessionEvent {
				kind: "stream".into(),
				peer: peer.clone(),
				detail: format!("{} · {}p", encoder.label(), eff_h)
					+ if started { "" } else { " (ffmpeg başlamadı)" },
			},
		);
	}
}

/// Build the per-session `on_file` handler: reassemble an inbound file transfer
/// (Begin → buffer, Chunk → append + detect gaps, End → save) and surface the
/// result to the host UI.
pub(super) fn make_on_file(app_h: AppHandle, peer: String) -> impl FnMut(DataMsg) + Send + 'static {
	// Reassemble: Begin → buffer, Chunk → append (detect gaps), End → save.
	let mut name = String::new();
	let mut buf: Vec<u8> = Vec::new();
	let mut next = 0u32;
	let mut expected = 0u32;
	let mut gap = false;
	move |m: DataMsg| match m {
		DataMsg::FileBegin {
			name: n,
			size,
			chunks,
		} => {
			name = sanitize_filename(&n);
			buf = Vec::with_capacity(size as usize);
			next = 0;
			expected = chunks;
			gap = false;
		}
		DataMsg::FileChunk { index, data } => {
			if index != next {
				gap = true;
			}
			next = index.wrapping_add(1);
			buf.extend_from_slice(&data);
		}
		DataMsg::FileEnd => {
			let complete = !gap && next == expected;
			let saved = if complete {
				save_received_file(&name, &buf)
			} else {
				None
			};
			let ok = saved.is_some();
			let _ = app_h.emit(
				"file-recv",
				FilePayload {
					peer: peer.clone(),
					name: name.clone(),
					bytes: buf.len() as u64,
					ok,
				},
			);
			if ok {
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "file".into(),
						peer: peer.clone(),
						detail: format!("{} · {} B", name, buf.len()),
					},
				);
			}
			buf = Vec::new();
		}
		_ => {}
	}
}

/// Build the per-session `on_audio` handler: lazily spawn an audio player and
/// pipe received PCM frames to it, tearing it down on `AudioEnd` / write error.
pub(super) fn make_on_audio() -> impl FnMut(DataMsg) + Send + 'static {
	use std::io::Write;
	// Lazily spawn an audio player and pipe received PCM frames to it.
	let mut sink: Option<std::process::ChildStdin> = None;
	let mut player: Option<Child> = None;
	move |m: DataMsg| match m {
		DataMsg::Audio(frame) => {
			if sink.is_none() {
				if let Some((c, s)) = spawn_audio_player() {
					player = Some(c);
					sink = Some(s);
				}
			}
			if let Some(s) = sink.as_mut() {
				if s.write_all(&frame).is_err() {
					sink = None;
					if let Some(mut c) = player.take() {
						let _ = c.kill();
					}
				}
			}
		}
		DataMsg::AudioEnd => {
			sink = None;
			if let Some(mut c) = player.take() {
				let _ = c.kill();
			}
		}
		_ => {}
	}
}
