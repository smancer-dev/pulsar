//! Client remote-play lifecycle: `start_remote_play` opens a session, brings up the
//! local video viewer (embedded WebCodecs or a native renderer), holds the control
//! session open full-duplex, and registers the `PlaySession`. `stop_stream` tears it
//! all down.

use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::input::ControllerHub;
use pulsar_core::service::{
	request_launch, request_stream, DataMsg, InputEvent, QualityPref, StreamReq,
};
use pulsar_core::Transport;
use tauri::{AppHandle, Emitter, State};

use crate::events::{ConnPhase, PlayInfo};
use crate::native_view;
use crate::process;
use crate::state::{AppState, PlaySession, Restream};
use crate::util::{client_auto_fps, connect_target};
use crate::viewer;

mod hold;

/// Tear down the viewer relay + any native renderer child spawned before the play
/// session was registered. Called on the `request_launch`/`request_stream` early
/// returns so a connect that fails after auth (but before `state.plays` insert)
/// doesn't orphan the viewer's UDP/WS tasks or the native renderer process — the
/// same orphaned-renderer class that causes the Pi input-stutter (see MEMORY).
fn teardown_partial(view: viewer::Viewer, children: Vec<Option<Child>>) {
	view.stop();
	for mut c in children.into_iter().flatten() {
		let _ = c.kill();
		let _ = c.wait();
	}
}

/// Client: connect to a host, start receiving its video (embedded WebCodecs
/// viewer, no separate window), and (optionally) stream our controller input —
/// all over a single session held open until `stop_stream`. Asks the host to
/// launch `game_id` (if any) and stream RTP/H.264 to our local viewer.
#[tauri::command]
pub(crate) async fn start_remote_play(
	app: AppHandle,
	state: State<'_, AppState>,
	target: String,
	game_id: String,
	_port: u16,
	codec: String,
	encoder: String,
	gamepad: bool,
	game_mode: bool,
) -> Result<PlayInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());

	// Testing override: `PULSAR_FORCE_CODEC=h265|av1|h264` forces the requested codec without
	// the session-menu UI (the host still validates + degrades if it can't encode it).
	let codec = std::env::var("PULSAR_FORCE_CODEC").unwrap_or(codec);

	let (mut sess, peer_label) = connect_target(&node, &target).await?;
	// Real connection phase: the transport is now actually established (direct P2P or
	// relay). Tell the Connecting screen so it reflects the truth instead of guessing.
	let transport = match sess.transport() {
		Transport::Direct => "direct",
		Transport::Relay => "relay",
	}
	.to_string();
	let _ = app.emit(
		"conn-phase",
		ConnPhase {
			target: target.clone(),
			transport: transport.clone(),
		},
	);
	if !crate::auth::client_authenticate(
		&mut sess,
		&app,
		&pw_pending,
		&next_auth,
		&peer_label,
	)
	.await?
	{
		return Err("Bağlantı reddedildi.".into());
	}
	// Same machine? (loopback P2P) → control would feed back, so flag it.
	let local = matches!(sess.transport(), Transport::Direct)
		&& sess
			.peer_addr()
			.await
			.map(|a| a.ip().is_loopback())
			.unwrap_or(false);

	// Start the local RTP→WebSocket viewer only after auth (don't bind ports for a
	// rejected connection). The host streams to the viewer's ephemeral UDP port.
	// `mut` is used on Linux (forward_audio_to_loopback for the native audio player).
	#[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
	let mut view = viewer::start()
		.await
		.map_err(|e| format!("video alıcı başlatılamadı: {e}"))?;
	let ws_port = view.ws_port;
	let media_port = view.media_port;
	// Audio flows as a second RTP stream to its own port; the host streams to it
	// only when its audio policy says transmit (game mode always does).
	let audio_port = view.audio_port;
	let audio_ws_port = view.audio_ws_port;

	// Native external player (opt-in): a hardware-decoded fullscreen player (ffplay on
	// Windows, mpv on Linux/macOS) fed by its own RTP port + SDP, instead of the in-webview
	// WebCodecs canvas; the host then streams video to that port. The DEFAULT is the
	// embedded webview canvas — on Windows WebView2 WebCodecs is GPU-accelerated, and on
	// Linux WebKitGTK's WebCodecs hardware-decodes via the installed GStreamer decoder
	// (Rockchip `mppvideodec` on RK3588), so the video stays embedded + controllable.
	// Falls back to the webview on any spawn failure.
	// Allocate the play id early — the Linux single-surface renderer keys its GL state by it.
	let id = state.next_play.fetch_add(1, Ordering::SeqCst);
	let mut native_child: Option<Child> = None;
	let mut video_port = media_port;
	#[allow(unused_mut)]
	let mut single_surface = false;
	// JSON-IPC socket for the embedded `--wid` mpv child (Faz 3 pause + Faz 4 stats share
	// ONE deterministic per-id path). Set only when that mpv child is actually spawned;
	// stays None for Windows ffplay and the single-surface renderer.
	#[allow(unused_mut)]
	let mut mpv_ipc_sock: Option<PathBuf> = None;
	// Faz 3 overlay: SDP + window id so set_overlay can kill+respawn the `--wid` mpv child
	// (revealing the webview menu). Set only by the default Linux `--wid` path below.
	#[allow(unused_mut, unused_assignments)]
	let mut mpv_sdp: Option<PathBuf> = None;
	#[allow(unused_mut, unused_assignments)]
	let mut mpv_wid: Option<u64> = None;
	// The `pulsar-vidsink` binary path when the native zero-copy renderer (not mpv) is in use.
	#[allow(unused_mut, unused_assignments)]
	let mut vidsink_bin_path: Option<String> = None;
	// Current vidsink display-rotation (degrees CW); refined by the host's DisplayRotation.
	#[allow(unused_mut, unused_assignments)]
	let mut vidsink_rotate_init: u32 = 0;
	// Native overlay renderer (`pulsar-render`) child + its stdin (shared with the stats thread
	// which feeds live `stat …` lines). Spawned alongside the vidsink on the Linux native path.
	#[allow(unused_mut, unused_assignments)]
	let mut render_child: Option<Child> = None;
	let overlay_stdin: Arc<Mutex<Option<std::process::ChildStdin>>> = Arc::new(Mutex::new(None));
	// Linux/X11: the embedded webview (WebKitGTK WebCodecs) can't hardware-decode here — it
	// would software-decode + glitch AND add a webview hop to the video path. So the native
	// mpv renderer (rkmpp) is the DEFAULT. We first try the moonlight-style SINGLE SURFACE
	// (libmpv render API → a GtkGLArea with the webview composited transparently on top, in
	// one window); on any failure we fall back to an embedded `--wid` mpv child. Elsewhere
	// (Windows WebView2 / macOS WKWebView hardware-decode fine in-webview) it stays opt-in.
	// All platforms render natively now (Moonlight-style, NO webview video — it was too slow).
	// Linux: pulsar-render (rkmpp). Windows: pulsar-render (Media Foundation + D3D11, embedded).
	// macOS: native mpv (VideoToolbox HW decode) — the embedded zero-copy Metal renderer is a
	// later phase (needs a Mac to build), but this already removes the webview video path there.
	let use_native = true;
	if use_native {
		if let Some(vport) = native_view::free_udp_port() {
			match native_view::write_sdp(vport, &codec) {
				Ok(sdp) => {
					#[cfg(windows)]
					{
						// DEFAULT Windows renderer: `pulsar-render` — Media Foundation HW decode +
						// D3D11 present in a CHILD HWND of the app window (`--wid`), replacing the
						// webview WebCodecs path (too slow). stdout: `vidsink-fps …`/`ov …`; stdin:
						// `stat …`/`pace 0|1`. Falls back to ffplay (separate window) if the binary
						// is missing. Embedding needs the Tauri window's HWND.
						let hwnd = process::window_hwnd(&app);
						let pace_default = std::env::var("PULSAR_PACE")
							.map(|v| v == "1" || v == "on" || v == "true")
							.unwrap_or(true);
						let mut rc = match hwnd {
							Some(h) => native_view::spawn_render_win(
								&process::render_bin(&app),
								&sdp,
								h,
								game_mode,
								pace_default,
							),
							None => None,
						};
						if let Some(c) = rc.as_mut() {
							if let Some(out) = c.stdout.take() {
								crate::render_stats::start_render_reader(&app, id, out);
							}
							if let Some(si) = c.stdin.take() {
								*overlay_stdin.lock().unwrap() = Some(si);
							}
						}
						if let Some(c) = rc {
							render_child = Some(c);
							video_port = vport;
						} else if let Some(c) =
							native_view::spawn_ffplay(&process::ffplay_bin(&app), &sdp)
						{
							// Fallback: separate fullscreen ffplay window.
							native_child = Some(c);
							video_port = vport;
						}
					}
					#[cfg(all(unix, not(target_os = "macos")))]
					{
						// The moonlight-style SINGLE SURFACE (libmpv→GtkGLArea + webview overlay)
						// is OPT-IN (`PULSAR_SINGLE_SURFACE=1`) while the final webview-over-GLArea
						// transparency compositing is sorted; it renders rkmpp video + control
						// already, but the webview composites opaque over the GLArea on this GTK3
						// stack. The DEFAULT Linux path is the proven embedded `--wid` mpv child.
						let installed = if std::env::var_os("PULSAR_SINGLE_SURFACE").is_some() {
							let sdp_s = sdp.to_string_lossy().into_owned();
							let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
							let app2 = app.clone();
							let posted = app
								.run_on_main_thread(move || {
									let _ = tx.send(crate::render::install_single_surface(&app2, id, sdp_s));
								})
								.is_ok();
							match (posted, rx.await) {
								(true, Ok(Ok(()))) => true,
								(true, Ok(Err(_))) => false,
								_ => false,
							}
						} else {
							false
						};
						if installed {
							single_surface = true;
							video_port = vport;
						} else {
							let wid = crate::render::window_xid(&app).await;
							// DEFAULT Linux renderer: `pulsar-render` — a SINGLE-SURFACE native
							// renderer doing rkmpp video + the egui overlay in ONE child window of the
							// app (`--wid`). The overlay is a child of the app window, so it moves/
							// clips/stacks WITH it (the earlier separate vidsink + override-redirect
							// overlay desynced on move and floated above other apps). stdout carries
							// `vidsink-fps …` (HUD) + `ov …` (interaction). Falls back to the old
							// vidsink/mpv if the binary is missing or PULSAR_USE_MPV=1.
							let rbin = process::render_bin(&app);
							// Frame-pacing startup default (env; the frontend re-applies the
							// persisted ui.framePacing live once the session is up).
							let pace_default = std::env::var("PULSAR_PACE")
								.map(|v| v == "1" || v == "on" || v == "true")
								.unwrap_or(true);
							let mut rc = if std::env::var_os("PULSAR_USE_MPV").is_some() {
								None
							} else {
								native_view::spawn_render(&rbin, &sdp, wid, game_mode, pace_default)
							};
							if let Some(c) = rc.as_mut() {
								if let Some(out) = c.stdout.take() {
									crate::render_stats::start_render_reader(&app, id, out);
								}
								// Capture the renderer's stdin so set_frame_pacing (and the HUD
								// stat writer) can push `pace 0|1` / `stat …` lines to it live.
								if let Some(si) = c.stdin.take() {
									*overlay_stdin.lock().unwrap() = Some(si);
								}
							}
							if let Some(c) = rc {
								render_child = Some(c);
								video_port = vport;
								mpv_sdp = Some(sdp.clone());
								mpv_wid = wid;
							} else {
								// mpv fallback (no overlay). Deterministic per-id IPC socket.
								let ipc = std::env::temp_dir().join(format!("pulsar-mpv-{id}.sock"));
								if let Some(c) = native_view::spawn_mpv(&sdp, wid, &ipc) {
									native_child = Some(c);
									video_port = vport;
									mpv_ipc_sock = Some(ipc.clone());
									mpv_sdp = Some(sdp.clone());
									mpv_wid = wid;
									crate::render::start_mpv_ipc_stats(&app, id, ipc);
								}
							}
						}
					}
					#[cfg(target_os = "macos")]
					{
						let ipc = std::env::temp_dir().join(format!("pulsar-mpv-{id}.sock"));
						if let Some(c) = native_view::spawn_mpv(&sdp, None, &ipc) {
							native_child = Some(c);
							video_port = vport;
							mpv_ipc_sock = Some(ipc);
						}
					}
				}
				Err(_) => {}
			}
		}
	}
	let native = native_child.is_some() || single_surface || render_child.is_some();

	if !game_id.is_empty() {
		if let Err(e) = request_launch(&mut sess, &game_id).await {
			// Clean up the viewer + native renderer we already brought up before bailing.
			teardown_partial(view, vec![native_child, render_child]);
			return Err(e.to_string());
		}
	}
	// Held copies so the session menu can re-request the stream at a new resolution.
	let codec_h = codec.clone();
	let encoder_h = encoder.clone();
	// Linux/Pi native renderer caps. mpv 0.34 here decodes with rkmpp but the gpu VO has
	// no DRM_PRIME→EGL interop on this build, so every frame is HW-DOWNLOADED (GPU→CPU)
	// and re-uploaded for Panfrost GL — far too slow to drain a 1440p60 high-bitrate
	// stream. The UDP socket then overflows (measured ~1.7k dropped pkt/s → ~90% loss →
	// ~3 fps + seconds of latency). So the Linux native path asks the host for a stream
	// it can actually keep up with (720p60 @ 10 Mbit default), instead of deferring to the
	// host's heavy native resolution. Webview clients (Win/macOS WebCodecs) and the menu's
	// own resolution selector are unaffected (0 = host default). Env-overridable for tuning:
	// PULSAR_W / PULSAR_H / PULSAR_FPS / PULSAR_KBPS.
	// mpv 0.34 on the Pi sustains only ~25 Mpx/s through its rkmpp→HW-download→Panfrost-GL
	// path, so a 960x540@30 (≈16 Mpx/s) default stays inside that envelope: mpv keeps up, the
	// socket buffer doesn't fill, latency stays low. (720p60 = 55 Mpx/s overran it → the
	// 3 fps + multi-second lag.) Bump via the in-app resolution menu or PULSAR_W/H/FPS/KBPS
	// once the Pi runs a zero-copy mpv build. Webview clients defer to the host (0).
	// "Auto" fps targets the client's display refresh (nearest of 30/60/120).
	let auto_fps = client_auto_fps(&app).await;
	let (req_w, req_h, req_fps, req_kbps) = if native && cfg!(target_os = "linux") {
		let g = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
		if render_child.is_some() {
			// Native zero-copy single-surface renderer (rkmpp→DRM_PRIME→EGL): sustains a full
			// stream easily. Default 1080p; fps follows the client's display refresh (auto).
			(g("PULSAR_W", 1920), g("PULSAR_H", 1080), g("PULSAR_FPS", auto_fps), g("PULSAR_KBPS", 15_000))
		} else if vidsink_bin_path.is_some() {
			// Native zero-copy vidsink (rkmpp→DRM_PRIME→EGL): proven 468 fps @1080p / 264 @1440p
			// on this Pi, so it easily sustains a full stream. Default 1080p; auto fps.
			(g("PULSAR_W", 1920), g("PULSAR_H", 1080), g("PULSAR_FPS", auto_fps), g("PULSAR_KBPS", 15_000))
		} else {
			// mpv fallback (no DRM_PRIME→EGL interop → HW-downloads every frame): keep the light
			// 540p30 cap so it can keep up / not overflow the socket.
			(g("PULSAR_W", 960), g("PULSAR_H", 540), g("PULSAR_FPS", 30), g("PULSAR_KBPS", 6_000))
		}
	} else {
		(0, 0, 0, 0) // defer to the host config
	};
	let req = StreamReq {
		port: video_port,
		codec,
		encoder,
		width: req_w,
		height: req_h,
		fps: req_fps,
		audio_port,
		// Session-menu audio defaults: transmit on; mute the host only in game mode (so
		// the sound moves entirely to the player). The client menu mirrors these and can
		// change them live.
		transmit_audio: true,
		mute_host: game_mode,
		game_mode,
		// 0 = host default; quality defers to game_mode-natural bias until the menu changes it.
		bitrate_kbps: req_kbps,
		quality: if game_mode {
			QualityPref::Latency
		} else {
			QualityPref::Quality
		},
		// HDR / 4:4:4 are opt-in; default off. Env overrides let us exercise the host path
		// before the session-menu toggles are wired in the frontend. The host validates and
		// degrades if the chosen encoder+codec can't actually do them.
		hdr: std::env::var_os("PULSAR_HDR").is_some(),
		yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
	};
	if let Err(e) = request_stream(&mut sess, &req).await {
		// Clean up the viewer + native renderer we already brought up before bailing.
		teardown_partial(view, vec![native_child, render_child]);
		return Err(e.to_string());
	}

	// Linux native client: play the host's Opus/RTP audio NATIVELY (ffmpeg→PulseAudio),
	// because WebKitGTK can't decode it via WebCodecs (the webview audio path is silent there).
	// The viewer forwards the received audio datagrams to a loopback port ffmpeg listens on.
	#[cfg(target_os = "linux")]
	let audio_native: Option<Child> = if native && req.transmit_audio && audio_port > 0 {
		match std::net::UdpSocket::bind("127.0.0.1:0").and_then(|s| s.local_addr().map(|a| a.port())) {
			Ok(lp) => {
				let ff = process::ffmpeg_bin(&app);
				match native_view::spawn_native_audio(&ff, lp) {
					Some(c) => {
						view.forward_audio_to_loopback(lp);
						Some(c)
					}
					None => None,
				}
			}
			Err(_) => None,
		}
	} else {
		None
	};
	#[cfg(not(target_os = "linux"))]
	let audio_native: Option<Child> = None;

	// Register this play session (one per connected-host tab). `id` was allocated above.
	let running = Arc::new(AtomicBool::new(true));
	let (input_tx, input_rx) = tokio::sync::mpsc::channel::<InputEvent>(256);

	if gamepad {
		// Read controllers on a blocking thread (gilrs isn't async/Send-friendly).
		let reader_flag = running.clone();
		let gtx = input_tx.clone();
		tokio::task::spawn_blocking(move || {
			let Ok(mut hub) = ControllerHub::new() else {
				return;
			};
			while reader_flag.load(Ordering::SeqCst) {
				if let Some((_, st)) = hub.snapshot().into_iter().next() {
					let _ = gtx.blocking_send(InputEvent::Gamepad(st));
				}
				std::thread::sleep(std::time::Duration::from_millis(16));
			}
		});
	}

	// Side-channel queue (clipboard / chat / file / mic audio → host).
	let (data_tx, data_rx) = tokio::sync::mpsc::channel::<DataMsg>(512);
	// Live stream changes from the session menu (resolution / encoder) → re-request.
	let (restream_tx, restream_rx) = tokio::sync::mpsc::channel::<Restream>(8);
	let mic = Arc::new(Mutex::new(None));

	// Hold the control session open full-duplex: forward input + side-channel data,
	// keepalive every ~2s (UDP has no disconnect signal), and receive the host's
	// chat/clipboard pushes — surfacing them to the UI.
	let send_flag = running.clone();
	tokio::spawn(hold::hold_session(
		sess,
		app.clone(),
		send_flag,
		input_rx,
		data_rx,
		restream_rx,
		id,
		video_port,
		audio_port,
		encoder_h,
		codec_h,
		game_mode,
	));

	state.plays.lock().unwrap().insert(
		id,
		PlaySession {
			viewer: view,
			input_tx,
			data_tx,
			mic,
			running,
			restream_tx,
			ffplay: native_child,
			audio_native,
			mpv_ipc: mpv_ipc_sock,
			mpv_sdp,
			mpv_wid,
			vidsink_bin: vidsink_bin_path,
			vidsink_rotate: vidsink_rotate_init,
			render_child,
			render_stdin: overlay_stdin,
		},
	);
	Ok(PlayInfo {
		id,
		transport,
		ws_port,
		audio_ws_port,
		local,
		native,
		embedded: single_surface,
	})
}

/// Gracefully stop a native renderer child that owns a GL/EGL context + an X window sharing
/// the display with WebKitGTK (Linux `pulsar-render` / vidsink / mpv). A hard SIGKILL abandons
/// the EGL context mid-bind and WEDGES the webview's input on RK3588 — after the session ends the
/// home screen renders but stops processing clicks (you can hover but nothing is clickable). A
/// SIGTERM lets the renderer run its clean teardown (release the EGL context, XDestroyWindow,
/// XCloseDisplay) so WebKit's shared GL/input state stays healthy; SIGKILL is the fallback only
/// if it doesn't exit promptly. No-op-different on non-unix (plain kill — Windows has no shared-GL
/// wedge and the renderer there is a separate top-level).
#[cfg(unix)]
fn stop_render_child(child: &mut std::process::Child) {
	unsafe {
		libc::kill(child.id() as i32, libc::SIGTERM);
	}
	// Poll up to ~600 ms for a clean exit before forcing it.
	for _ in 0..60 {
		match child.try_wait() {
			Ok(Some(_)) => return,
			Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
			Err(_) => break,
		}
	}
	let _ = child.kill();
	let _ = child.wait();
}
#[cfg(not(unix))]
fn stop_render_child(child: &mut std::process::Child) {
	let _ = child.kill();
	let _ = child.wait();
}

/// Stop one remote-play session (tab): closes its control session (the host sees a
/// disconnect) and tears down its video relay.
#[tauri::command]
pub(crate) async fn stop_stream(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
) -> Result<(), String> {
	// Remove from the map and DROP the guard before the blocking kill()/wait() below —
	// otherwise every other state.plays user (forward() on each mouse-move/keystroke,
	// the setters, mic/kbd commands) stalls until the closed session's children exit.
	let play = state.plays.lock().unwrap().remove(&id);
	if let Some(mut play) = play {
		play.running.store(false, Ordering::SeqCst);
		play.viewer.stop();
		if let Some(mut mic) = play.mic.lock().unwrap().take() {
			let _ = mic.kill();
		}
		// Close the native renderer's fullscreen/embedded `--wid` window, if any. GRACEFUL
		// (SIGTERM-first, see stop_render_child) so the GL/EGL teardown runs and WebKit's input
		// doesn't wedge after the session ends.
		if let Some(mut child) = play.ffplay.take() {
			stop_render_child(&mut child);
		}
		// Keep the native renderer (`pulsar-render`) ALIVE on disconnect — do NOT kill it. Killing
		// destroys its EGL context, which corrupts the WebKitGTK webview's shared Mali GL on RK3588
		// and wedges the webview (clicks dead) with no in-session recovery (needs a reboot). Instead
		// tell it to `hide` (unmap its window → the webview shows through) and idle; its
		// `PR_SET_PDEATHSIG` reaps it when the app exits. We `forget` the handle so Child's drop
		// doesn't close its stdout pipe (the idle render writes no stats, so nothing fills). If
		// there's no stdin to send `hide` (shouldn't happen for pulsar-render), fall back to a kill.
		if let Some(mut child) = play.render_child.take() {
			// Linux: hide + keep resident (don't destroy the EGL context → no WebKit GL corruption).
			#[cfg(all(unix, not(target_os = "macos")))]
			{
				let hidden = {
					use std::io::Write as _;
					match play.render_stdin.lock().unwrap().as_mut() {
						Some(si) => writeln!(si, "hide").is_ok(),
						None => false,
					}
				};
				if hidden {
					std::mem::forget(child);
				} else {
					stop_render_child(&mut child);
				}
			}
			// Windows/macOS: WebView2/WKWebView don't share GL with the renderer like WebKitGTK does
			// on Mali, so a normal teardown is fine (and the renderer there doesn't handle `hide`).
			#[cfg(not(all(unix, not(target_os = "macos"))))]
			stop_render_child(&mut child);
		}
		// Stop the Linux native audio player (ffmpeg→PulseAudio), if any.
		if let Some(mut child) = play.audio_native.take() {
			let _ = child.kill();
			let _ = child.wait();
		}
		// Remove the embedded mpv's IPC socket file (deterministic per-id path → at most
		// one stale file, overwritten on reuse anyway).
		#[cfg(unix)]
		if let Some(sock) = play.mpv_ipc.take() {
			let _ = std::fs::remove_file(&sock);
		}
	}
	// Tear down the Linux single-surface renderer (libmpv→GLArea), if this session used it.
	#[cfg(all(unix, not(target_os = "macos")))]
	crate::render::teardown_single_surface(&app, id).await;
	let _ = &app; // used by teardown on Linux; silence unused elsewhere
	Ok(())
}
