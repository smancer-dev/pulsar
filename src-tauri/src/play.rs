//! Client remote-play lifecycle: `start_remote_play` opens a session, brings up the
//! local video viewer (embedded WebCodecs or a native renderer), holds the control
//! session open full-duplex, and registers the `PlaySession`. `stop_stream` tears it
//! all down.

use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::input::{ControllerHub, EmulationTarget};
use pulsar_core::service::{
	request_launch, request_stream, DataMsg, InputEvent, QualityPref, StreamReq,
};
use pulsar_core::Transport;
use tauri::{AppHandle, Emitter, State};

use crate::events::{ConnPhase, PlayInfo};
use crate::native_view;
use crate::process;
use crate::state::{AppState, PlaySession, RenderSeed, Restream};
use crate::util::{client_auto_fps, connect_target};
use crate::viewer;

mod hold;

/// Linux-only: the ONE resident `pulsar-render` child kept alive across session boundaries
/// to avoid EGL context destruction on the shared RK3588 Mali display (see
/// `AppState::resident_render`).
pub(crate) struct ResidentRender {
	/// The live renderer process (kept alive; stdin/stdout already taken).
	pub(crate) child: Child,
	/// The renderer's stdin (shared Arc so callers can write lines without holding the lock).
	pub(crate) stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
	/// The GDK container (child GdkWindow) session id under which this renderer lives.
	/// On next connect we re-register it under the new session id so
	/// `create_native_container` is skipped and the existing X window is reused.
	pub(crate) container_id: u64,
	/// Shared session id for the renderer's stdout reader thread (see `start_render_reader`).
	/// The reader uses this to tag `play-vstats`/`play-ready` events with the CURRENT session
	/// id — so when the renderer is reused for a new session, updating this Arc redirects all
	/// events to the new session without restarting the reader thread.
	pub(crate) live_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
	/// The session mode (`game_mode=true` → game, `false` → remote) the renderer was originally
	/// spawned with. Tracked so the resident-reuse path can detect a cross-session mode change
	/// and send a `mode game|remote` command to apply the correct overlay/pace-ceiling on reconnect.
	pub(crate) game_mode: bool,
}

/// Whether this client advertises the cursor side-channel ([`StreamReq::cursor_external`]):
/// it tells the host the client can draw the host pointer itself, so the host may use the
/// cursorless KMS zero-copy capture and stream the pointer out-of-band. Opt-in behind
/// `PULSAR_CURSOR_SC=1` while it's being proven on the Pi; default OFF preserves the
/// embedded-cursor behavior. Only meaningful with the native renderer (the only client
/// that can draw a side-channel cursor over the video).
fn cursor_external_enabled() -> bool {
	matches!(
		std::env::var("PULSAR_CURSOR_SC").as_deref(),
		Ok("1") | Ok("on") | Ok("true")
	)
}

/// Format the host's monitors for the renderer's `caps` line `displays=` field, as
/// `idx:name:w:h:primary` comma-joined — NO spaces (the caps line is whitespace-tokenized,
/// and session_cmds patches it token-wise, so a spaced value would corrupt both). The
/// egui overlay parses this back into its Display-section monitor picker.
fn fmt_displays(displays: &[pulsar_core::service::DisplayInfo]) -> String {
	displays
		.iter()
		.map(|d| {
			format!(
				"{}:{}:{}:{}:{}",
				d.idx,
				d.name,
				d.width,
				d.height,
				if d.primary { 1 } else { 0 }
			)
		})
		.collect::<Vec<_>>()
		.join(",")
}

/// Tear down the viewer relay + any native renderer child spawned before the play
/// session was registered. Called on the `request_launch`/`request_stream` early
/// returns so a connect that fails after auth (but before `state.plays` insert)
/// doesn't orphan the viewer's UDP/WS tasks or the native renderer process — the
/// same orphaned-renderer class that causes the Pi input-stutter (see MEMORY).
/// Children go through `stop_render_child` (SIGTERM + grace), never a bare kill:
/// SIGKILLing a renderer mid-EGL-bind wedges WebKitGTK's shared Mali GL input on
/// RK3588 (see `stop_render_child`). Also drops the per-id GTK state created
/// earlier (in-app container / single-surface GL renderer) — ids are never
/// reused, so skipping that leaks a hidden child window per failed connect.
///
/// `preserve_native_container`: when `true` (Linux only, resident renderer re-parked)
/// the GDK container for `id` is NOT destroyed — the resident renderer's `--wid`
/// X parent window must remain valid for the next reconnect.
async fn teardown_partial(
	app: &AppHandle,
	id: u64,
	single_surface: bool,
	preserve_native_container: bool,
	view: viewer::Viewer,
	children: Vec<Option<Child>>,
) {
	view.stop();
	// Offload SIGTERM-grace polls to blocking threads so this async fn never parks a
	// Tokio worker for up to ~600 ms × N children (matches the discipline in stop_stream
	// and session_cmds; see `stop_render_child_blocking`).
	let handles: Vec<_> = children
		.into_iter()
		.flatten()
		.map(stop_render_child_blocking)
		.collect();
	for h in handles {
		let _ = h.await;
	}
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		if single_surface {
			crate::render::teardown_single_surface(app, id).await;
		}
		if !preserve_native_container {
			crate::render::destroy_native_container(app, id);
		}
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	let _ = (app, id, single_surface, preserve_native_container);
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
	quality: Option<String>,
	touchpad_as_mouse: bool,
) -> Result<PlayInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or(crate::i18n::t("err.online"))?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());

	// Testing override: `PULSAR_FORCE_CODEC=h265|av1|h264` forces the requested codec without
	// the session-menu UI (the host still validates + degrades if it can't encode it).
	let codec = std::env::var("PULSAR_FORCE_CODEC").unwrap_or(codec);

	let disc = state.discovery.lock().unwrap().clone();
	let (net_mode, relay) = {
		let cfg = state.config.lock().unwrap();
		(cfg.network_mode, cfg.relay.clone())
	};
	let (mut sess, peer_label) = connect_target(&app, &node, disc, &target, net_mode, &relay).await?;
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
	// Timeout on the auth handshake: if the host never answers (e.g. it opened the
	// session but its Allow/Deny popup was dismissed, or the link silently dropped
	// mid-handshake after connect), the recv_host_auth loop inside client_authenticate
	// has no deadline and would park forever. 40 s < the 45 s JS UI timeout so the
	// Rust future fails first and the JS catch sees a real error (not the JS timer's
	// synthetic CONNECT_TIMEOUT string). When the timeout fires the future is
	// cancelled; `sess` then falls out of scope when we return Err, which drops the
	// Session and closes the connection — the host's recv_client_auth sees Gone and
	// tears down its Allow/Deny state cleanly.
	const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);
	let auth_result = tokio::time::timeout(
		AUTH_TIMEOUT,
		crate::auth::client_authenticate(&mut sess, &app, &pw_pending, &next_auth, &peer_label),
	)
	.await
	// The JS friendlyConnectError checks for the literal substring "connect-timed-out"
	// (the same sentinel the UI-side CONNECT_TIMEOUT constant uses) — returning it here
	// lets the JS route both the Rust-side and JS-side timeouts to the same friendly
	// connErr.timeout translation instead of showing the raw English string.
	.map_err(|_| "connect-timed-out".to_string())?;
	if !auth_result? {
		return Err(crate::i18n::t("err.denied").into());
	}
	// Default codec ("auto"): prefer H.265, but only when the HOST can actually encode
	// it — asked over the session (validated, hardware-only caps) BEFORE this client
	// writes its decoder SDP, so the SDP and the stream the host starts can never
	// disagree. Timeout/old host/empty caps → H.264 (universally encodable).
	let host_caps = tokio::time::timeout(
		std::time::Duration::from_secs(2),
		pulsar_core::service::query_stream_caps(&mut sess),
	)
	.await
	.ok()
	.and_then(|r| r.ok())
	.unwrap_or_default();
	tracing::info!(codecs = ?host_caps.codecs, encoders = ?host_caps.encoders, features = ?host_caps.features, "host caps received");
	// Media-over-session: carry the RTP inside the encrypted session (ONE external
	// socket — the same hole the control channel punched; symmetric NAT then works
	// via the relay). Only when the host advertises it; old hosts stream direct.
	let mos = host_caps
		.features
		.iter()
		.any(|f| f == pulsar_core::service::media::FEAT_MOS);
	let host_nack = host_caps
		.features
		.iter()
		.any(|f| f == pulsar_core::service::media::FEAT_NACK);
	// This client's DECODE caps (startup probe). Unknown (probe still running /
	// macOS stub failure) → assume the universal software pair. `incompat` maps a codec
	// to the host encoder families whose REAL bitstream this client can't decode even
	// though the codec validated against a conformant sample (the diagnosed rkmpp-HEVC ×
	// native-NVENC case) — used to prune the negotiated set below.
	let probed_decoders = state
		.local_caps
		.lock()
		.unwrap()
		.clone()
		.map(|lc| lc.decoders)
		.unwrap_or_default();
	let client_codecs: Vec<String> = if !probed_decoders.is_empty() {
		probed_decoders
			.iter()
			.filter(|d| d.ok)
			.map(|d| d.codec.clone())
			.collect()
	} else {
		vec!["h264".to_string(), "h265".to_string()]
	};
	let incompat = |codec: &str| -> Vec<String> {
		probed_decoders
			.iter()
			.find(|d| d.ok && d.codec == codec)
			.map(|d| d.incompatible_with.clone())
			.unwrap_or_default()
	};
	// Negotiated set = host-encodable ∩ client-decodable, MINUS any codec whose client
	// decoder is known-incompatible with an encoder family the host actually has (it WILL
	// use its HW encoder for that codec — e.g. host nvenc → HEVC over native NVENC, which
	// the Pi's rkmpp can't decode, so HEVC is dropped and "auto" lands on h264). "auto"
	// picks by quality (av1 > h265 > h264). The SDP is written from this AFTER the pick,
	// so the codec on the wire and the client's decoder can never disagree.
	let allowed: Vec<String> = host_caps
		.codecs
		.iter()
		.filter(|c| client_codecs.iter().any(|d| d == *c))
		.filter(|c| {
			let bad = incompat(c);
			let blocked = bad.iter().any(|f| host_caps.encoders.iter().any(|e| e == f));
			if blocked {
				tracing::info!(codec = %c, ?bad, "codec dropped: client decoder incompatible with host encoder family");
			}
			!blocked
		})
		.cloned()
		.collect();
	let codec = if codec.is_empty() || codec == "auto" {
		["av1", "h265", "h264"]
			.iter()
			.find(|c| allowed.iter().any(|a| a == **c))
			.map(|c| c.to_string())
			.unwrap_or_else(|| "h264".to_string())
	} else {
		codec
	};
	tracing::info!(%codec, ?allowed, "stream codec resolved");

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
	let mut view = viewer::start(mos)
		.await
		.map_err(|e| format!("{}: {e}", crate::i18n::t("err.videoRecv")))?;
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
	// True when `render_child` is a REUSED resident renderer (Linux only).  On a failed
	// reconnect teardown_partial must NOT call stop_render_child on it — doing so destroys
	// the EGL context on the shared RK3588 Mali display and wedges WebKitGTK input (the
	// very wedge the resident model exists to prevent).  Instead we re-park it.
	#[allow(unused_mut, unused_assignments, unused_variables)]
	let mut render_child_is_resident: bool = false;
	// Shared live session id for the resident renderer's stdout reader (Linux only; None on
	// other platforms where the reader uses a fixed id for the renderer's single lifetime).
	#[allow(unused_mut, unused_assignments)]
	let mut render_live_id: Option<std::sync::Arc<std::sync::atomic::AtomicU64>> = None;
	let overlay_stdin: Arc<Mutex<Option<std::process::ChildStdin>>> = Arc::new(Mutex::new(None));
	let caps_line: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
	// Stdin-only renderer state remembered for a codec-switch respawn re-push.
	let render_seed: Arc<Mutex<RenderSeed>> = Arc::new(Mutex::new(RenderSeed::default()));
	// Every SDP temp file written for this session — tracked so stop_stream can delete
	// them (they're never overwritten because the port-based name is effectively unique
	// per session, and codec/monitor switches write a fresh one each time).
	let sdp_files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
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
		// TODO(port-toctou): free_udp_port() binds an ephemeral UDP port, reads it, and
		// drops the socket — the renderer child below re-binds it moments later, so a
		// racing process can grab the port in the gap. A clean fix can't live here (the
		// CHILD owns the bind, not us, so we can't hold the socket open across the spawn
		// without blocking the child's bind); it belongs in native_view/spawn.rs (out of
		// scope), e.g. passing the bound socket fd to the child or retrying on bind failure.
		if let Some(vport) = native_view::free_udp_port() {
			match native_view::write_sdp(vport, &codec) {
				Ok(sdp) => {
					// Track for teardown on every platform (Windows ffplay/render path
					// below never stores `mpv_sdp`, so this is the only handle to it).
					sdp_files.lock().unwrap().push(sdp.clone());
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
								crate::i18n::lang(),
							),
							None => {
								tracing::warn!("native render skipped: main window HWND unavailable");
								None
							}
						};
						if let Some(c) = rc.as_mut() {
							if let Some(out) = c.stdout.take() {
								crate::render_stats::start_render_reader(&app, id, out, None);
							}
							if let Some(si) = c.stdin.take() {
								*overlay_stdin.lock().unwrap() = Some(si);
							}
						}
						if let Some(c) = rc {
							tracing::info!(pid = c.id(), port = vport, "native renderer (pulsar-render) spawned");
							render_child = Some(c);
							video_port = vport;
							// Seed the egui overlay (same as the Linux branch): host caps filter
							// the codec/encoder rows; the audio line seeds the Ses toggles.
							{
								use std::io::Write as _;
								let enc = if encoder.is_empty() { "auto" } else { &encoder };
								let line = format!(
									"caps codecs={} encoders={} codec={} encoder={} conn={} displays={}",
									allowed.join(","),
									host_caps.encoders.join(","),
									codec,
									enc,
									if transport == "relay" { "Relay" } else { "P2P" },
									fmt_displays(&host_caps.displays)
								);
								if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
									let _ = writeln!(si, "{line}");
									let _ = writeln!(
										si,
										"audio tx=1 mute={} mic=0",
										if game_mode { 1 } else { 0 }
									);
								}
								*caps_line.lock().unwrap() = line;
								render_seed.lock().unwrap().audio = Some((true, game_mode, false));
							}
						} else if let Some(c) =
							native_view::spawn_ffplay(&process::ffplay_bin(&app), &sdp)
						{
							// Fallback: separate fullscreen ffplay window.
							tracing::warn!(pid = c.id(), port = vport, "pulsar-render failed → ffplay fallback window");
							native_child = Some(c);
							video_port = vport;
						} else {
							tracing::error!("both pulsar-render and ffplay failed to spawn — no video renderer");
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
									let _ = tx.send(crate::render::install_single_surface(
										&app2, id, sdp_s,
									));
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
							// Resident renderer: if a previous session's `pulsar-render` is parked
							// (hidden but EGL-context alive), reuse it instead of spawning a new
							// child. Spawning a new one would destroy-and-recreate the EGL context
							// (the old child's eglDestroyContext fires when the process exits), which
							// on RK3588 corrupts WebKitGTK's shared Mali GL (wedges click input).
							// Reuse: take one child from the pool, re-register its GDK container
							// under the new session id, send `show` + `reopen <new-sdp>` + new caps.
							// The pool may hold several parked renderers when multiple concurrent tabs
							// ended — pop the first entry (LIFO is fine; all are equivalent).
							let resident = state.resident_render.lock().unwrap().pop();
							// Shared session id for the reader thread: when reusing the resident,
							// update this Arc so the existing reader tags events with the new id;
							// when spawning fresh, pass it to start_render_reader so the new reader
							// uses it (allowing future reuse on the NEXT reconnect).
							let cur_live_id = {
								use std::sync::atomic::AtomicU64;
								resident
									.as_ref()
									.map(|r| {
										// Update in-place so the RUNNING reader picks up the new id.
										r.live_id.store(id, std::sync::atomic::Ordering::Relaxed);
										r.live_id.clone()
									})
									.unwrap_or_else(|| std::sync::Arc::new(AtomicU64::new(id)))
							};
							render_live_id = Some(cur_live_id.clone());
							let toplevel = crate::render::window_xid(&app).await;
							// In-app container: a pass-through child GdkWindow the renderer embeds
							// into. The frontend positions it over the session tab's content area
							// (`native_view_rect`), so the video renders INSIDE the app — tabs and
							// chrome stay visible/clickable — instead of covering the whole window.
							// Falls back to the toplevel XID (old full-window embed) and then to a
							// standalone renderer window if there's no XID at all.
							// When reusing the resident, the existing container (kept alive at session
							// end) is re-registered under the new id — no new X window is created.
							let wid: Option<u64> = if let Some(ref res) = resident {
								// Re-register the kept container under the new session id.
								crate::render::rename_native_container(
									&app,
									res.container_id,
									id,
								);
								// The renderer is still embedded in that container (it has been
								// idle-looping with the window unmapped). The wid for reconnect
								// re-use is whatever the container's X11 XID already is — the
								// renderer's `--wid` arg was set at spawn and doesn't change; we
								// just need `mpv_wid` bookkeeping for any future codec-switch respawn.
								// Re-read it so codec-switch respawn uses the correct XID.
								crate::render::container_xid(&app, id).await
							} else {
								let container = match toplevel {
									Some(_) => crate::render::create_native_container(&app, id).await,
									None => None,
								};
								container.or(toplevel)
							};
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
							// Try the resident renderer first; fall back to a fresh spawn.
							// Belt-and-suspenders liveness check: even if stop_stream's guard
							// should have caught a dead renderer at park time, probe again here
							// so a race (child dies between stop_stream's try_wait and now) or
							// an unexpected code path cannot poison this session with a dead
							// process. If the child has exited, drop the resident and fall
							// through to the fresh-spawn path.
							let resident = resident.and_then(|mut res| {
								match res.child.try_wait() {
									Ok(None) => Some(res), // still running — safe to reuse
									Ok(Some(status)) => {
										tracing::warn!(
											pid = res.child.id(),
											?status,
											"parked pulsar-render already exited — dropping dead resident, spawning fresh"
										);
										let _ = res.child.wait();
										None
									}
									Err(e) => {
										tracing::warn!(
											pid = res.child.id(),
											err = %e,
											"try_wait on parked pulsar-render failed — treating as dead, spawning fresh"
										);
										None
									}
								}
							});
							let mut rc = if let Some(res) = resident {
								use std::io::Write as _;
								// Activate the resident: show the window + switch to the new SDP.
								// If the session mode changed (game→remote or remote→game) send a
								// `mode` command so the renderer updates its overlay personality and
								// pace ceiling — the `--mode` arg was set at original spawn and never
								// changes otherwise, causing the wrong menu/look/ceiling on reconnect.
								if let Some(si) = res.stdin.lock().unwrap().as_mut() {
									if res.game_mode != game_mode {
										let _ = writeln!(
											si,
											"mode {}",
											if game_mode { "game" } else { "remote" }
										);
									}
									let _ = writeln!(si, "reopen {}", sdp.display());
									let _ = writeln!(si, "show");
									let _ = si.flush();
								}
								tracing::info!(
									pid = res.child.id(),
									resident_game_mode = res.game_mode,
									new_game_mode = game_mode,
									"reusing resident pulsar-render for reconnect"
								);
								// Reconstitute as a Child for the session bookkeeping path below.
								// The stdin was already taken into render_stdin at the previous spawn;
								// here we restore the shared Arc so the session's stat/pace writers
								// find it in the same Arc they already hold.
								*overlay_stdin.lock().unwrap() = res.stdin.lock().unwrap().take();
								// Mark as reused resident so teardown_partial re-parks instead of
								// killing if request_launch / request_stream fails below.
								render_child_is_resident = true;
								Some(res.child)
							} else if std::env::var_os("PULSAR_USE_MPV").is_some() {
								None
							} else {
								native_view::spawn_render(
									&rbin,
									&sdp,
									wid,
									game_mode,
									pace_default,
									crate::i18n::lang(),
								)
							};
							if let Some(c) = rc.as_mut() {
								// Only take stdout/stdin for a freshly-spawned renderer
								// (resident had them taken at the original spawn).
								if c.stdout.is_some() {
									if let Some(out) = c.stdout.take() {
										// Pass the live_id Arc so the reader can have its id
										// updated on the next reconnect (resident model).
										crate::render_stats::start_render_reader(
											&app,
											id,
											out,
											Some(cur_live_id.clone()),
										);
									}
								}
								// Capture the renderer's stdin so set_frame_pacing (and the HUD
								// stat writer) can push `pace 0|1` / `stat …` lines to it live.
								// (Resident path: stdin was already placed in overlay_stdin above.)
								if c.stdin.is_some() {
									if let Some(si) = c.stdin.take() {
										*overlay_stdin.lock().unwrap() = Some(si);
									}
								}
							}
							if let Some(c) = rc {
								render_child = Some(c);
								video_port = vport;
								mpv_sdp = Some(sdp.clone());
								mpv_wid = wid;
								// Seed the egui overlay: host caps (filters its codec/encoder
								// rows) + the active request (so it doesn't show "Otomatik"
								// while an explicit codec/encoder is live).
								{
									use std::io::Write as _;
									let enc = if encoder.is_empty() { "auto" } else { &encoder };
									let line = format!(
										"caps codecs={} encoders={} codec={} encoder={} conn={} displays={}",
										allowed.join(","),
										host_caps.encoders.join(","),
										codec,
										enc,
										if transport == "relay" { "Relay" } else { "P2P" },
										fmt_displays(&host_caps.displays)
									);
									if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
										let _ = writeln!(si, "{line}");
										// Seed the overlay's Ses section with the session's
										// starting audio policy (game mode mutes the host).
										let _ = writeln!(
											si,
											"audio tx=1 mute={} mic=0",
											if game_mode { 1 } else { 0 }
										);
									}
									*caps_line.lock().unwrap() = line;
									render_seed.lock().unwrap().audio =
										Some((true, game_mode, false));
								}
							} else {
								// mpv fallback (no overlay). Deterministic per-id IPC socket.
								let ipc =
									std::env::temp_dir().join(format!("pulsar-mpv-{id}.sock"));
								if let Some(c) = native_view::spawn_mpv(&sdp, wid, &ipc) {
									native_child = Some(c);
									video_port = vport;
									mpv_ipc_sock = Some(ipc.clone());
									mpv_sdp = Some(sdp.clone());
									mpv_wid = wid;
									crate::render::start_mpv_ipc_stats(
										&app,
										id,
										ipc,
										wid.is_none(),
									);
								}
							}
						}
					}
					#[cfg(target_os = "macos")]
					{
						// macOS video: the native mpv child (VideoToolbox HW decode → zero-copy
						// GL), in its OWN window for now — the embedded zero-copy Metal renderer is
						// a later phase (needs a Mac to build). This already removes the webview
						// video path here.
						let ipc = std::env::temp_dir().join(format!("pulsar-mpv-{id}.sock"));
						match native_view::spawn_mpv(&sdp, None, &ipc) {
							Some(c) => {
								tracing::info!(pid = c.id(), port = vport, "macOS mpv video renderer spawned");
								native_child = Some(c);
								video_port = vport;
								mpv_ipc_sock = Some(ipc);
							}
							// mpv missing/unrunnable used to be SILENTLY swallowed (the old
							// `Err(_) => {}`), leaving the user on a black session with no idea why.
							// Surface it now: a clear `tracing::error` AND a `host-stats` label line
							// (the channel the session UI already renders as a status string — see
							// play/hold.rs's `host-stats` emit) telling the user to install mpv
							// (`brew install mpv`). The session still proceeds — the overlay below +
							// the control channel come up; only the video is missing — so this is a
							// degraded-not-fatal notice, NOT a `play-ended` teardown.
							None => {
								tracing::error!("macOS: mpv failed to spawn — video unavailable (install mpv: brew install mpv)");
								// Turkish UI copy (project default). Kept inline rather than as an i18n
								// key so this stays within the renderer-wiring change set; if an EN
								// catalog entry is wanted later, swap to crate::i18n::t.
								let _ = app.emit(
									"host-stats",
									crate::events::PlayStats {
										id,
										label: "Video yok — mpv kurulu değil (brew install mpv)"
											.to_string(),
									},
								);
							}
						}

						// ALSO bring up the native OVERLAY renderer (`pulsar-render`, overlay-only on
						// macOS) alongside mpv, exactly like Windows/Linux: it draws the same egui
						// overlay (menu + closed-state HUD/button/hint) in a transparent always-on-top
						// eframe window. Overlay-only here (the video is the mpv child above), so no
						// `--wid`; open/close arrive over stdin from session_cmds::set_overlay (its
						// non-Linux branch writes `open`/`close` lines).
						let rbin = process::render_bin(&app);
						let pace_default = std::env::var("PULSAR_PACE")
							.map(|v| v == "1" || v == "on" || v == "true")
							.unwrap_or(true);
						// No mac-specific spawn helper (spawn_render is Linux-only, spawn_render_win
						// Windows-only); build the overlay-only command inline — same flags as the
						// other backends minus `--wid` (nothing to embed into).
						let mut cmd = std::process::Command::new(&rbin);
						cmd.arg(&*sdp)
							.arg("--mode")
							.arg(if game_mode { "game" } else { "remote" })
							.arg("--pace")
							.arg(if pace_default { "on" } else { "off" })
							.arg("--lang")
							.arg(crate::i18n::lang());
						cmd.stdin(std::process::Stdio::piped());
						cmd.stdout(std::process::Stdio::piped());
						match cmd.spawn() {
							Ok(mut c) => {
								tracing::info!(pid = c.id(), "macOS native overlay (pulsar-render) spawned");
								// Read the overlay-only renderer's stdout (`ov …` interaction lines; it
								// emits no `vidsink-fps` video stats). start_render_reader matches an
								// overlay-only process and tolerates the missing stats.
								if let Some(out) = c.stdout.take() {
									crate::render_stats::start_render_reader(&app, id, out, None);
								}
								// Capture its stdin into the SHARED render_stdin slot the other
								// platforms use, so set_overlay (`open`/`close`), the HUD `stat …`
								// writer, render_chat/fsjson/audio etc. all reach it.
								if let Some(si) = c.stdin.take() {
									*overlay_stdin.lock().unwrap() = Some(si);
								}
								render_child = Some(c);
								// Seed the egui overlay: host caps filter the codec/encoder rows; the
								// audio line seeds the Ses toggles (same seed as the Win/Linux paths).
								{
									use std::io::Write as _;
									let enc = if encoder.is_empty() { "auto" } else { &encoder };
									let line = format!(
										"caps codecs={} encoders={} codec={} encoder={} conn={} displays={}",
										allowed.join(","),
										host_caps.encoders.join(","),
										codec,
										enc,
										if transport == "relay" { "Relay" } else { "P2P" },
										fmt_displays(&host_caps.displays)
									);
									if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
										let _ = writeln!(si, "{line}");
										let _ = writeln!(
											si,
											"audio tx=1 mute={} mic=0",
											if game_mode { 1 } else { 0 }
										);
									}
									*caps_line.lock().unwrap() = line;
									render_seed.lock().unwrap().audio = Some((true, game_mode, false));
								}
							}
							// The overlay binary may be missing (e.g. a dev build that didn't bundle
							// it) — the session still works without it (video + control are
							// independent), so log it and carry on rather than failing the connect.
							Err(e) => {
								tracing::warn!(error = %e, "macOS: pulsar-render overlay failed to spawn — overlay unavailable");
							}
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
			// Park ANY live pulsar-render child instead of killing it — regardless of whether
			// it was a reused resident or a freshly-spawned one. On RK3588 the EGL context is
			// shared with WebKitGTK's Mali display: destroying it (even via clean SIGTERM) wedges
			// WebKit click input with no in-session recovery. The reuse guard previously checked
			// `render_child_is_resident` but a fresh renderer on an empty pool carries the same
			// risk — it is live and rendering into the same shared Mali EGL surface.
			#[cfg(all(unix, not(target_os = "macos")))]
			let render_child_parked = if let Some(child) = render_child.take() {
				use std::io::Write as _;
				if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
					let _ = writeln!(si, "hide");
					let _ = si.flush();
				}
				let live_id = render_live_id.clone().unwrap_or_else(|| {
					std::sync::Arc::new(std::sync::atomic::AtomicU64::new(id))
				});
				state.resident_render.lock().unwrap().push(ResidentRender {
					child,
					stdin: overlay_stdin.clone(),
					container_id: id,
					live_id,
					game_mode,
				});
				// Cap the pool at 1 — reap any excess idle residents now that we pushed.
				reap_excess_resident_pool(&app, &*state, 1);
				tracing::info!(
					session_id = id,
					was_resident = render_child_is_resident,
					"request_launch failed: parked renderer (EGL context preserved)",
				);
				true
			} else {
				false
			};
			#[cfg(not(all(unix, not(target_os = "macos"))))]
			let render_child_parked = false;
			// Clean up the viewer + native renderer we already brought up before bailing.
			// preserve_native_container: if we parked the render child above, its GDK container
			// must stay alive so the renderer's --wid X parent remains valid for the next connect.
			teardown_partial(
				&app,
				id,
				single_surface,
				render_child_parked,
				view,
				vec![native_child, render_child],
			)
			.await;
			return Err(e.to_string());
		}
	}
	// Held copies so the session menu can re-request the stream at a new resolution.
	let codec_h = codec.clone();
	let encoder_h = encoder.clone();
	// HDR: read from the UI setting persisted in stream_cfg. The PULSAR_HDR env var is a
	// debug override that wins if set. The UI value is the normal production path.
	let req_hdr = state.stream_cfg.lock().unwrap().hdr || std::env::var_os("PULSAR_HDR").is_some();
	// Quality bias from the Settings → Display 'Varsayılan kalite' control (ui.quality).
	// "hq" → Quality, "fast" → Latency; "auto" (or absent) defers to the mode-natural default
	// (game=Latency, remote=Quality). Also seeded into hold_session so re-requests (adaptive
	// bitrate steps + session-menu changes) preserve the user's chosen bias.
	let req_quality = match quality.as_deref() {
		Some("hq") => QualityPref::Quality,
		Some("fast") => QualityPref::Latency,
		_ => if game_mode { QualityPref::Latency } else { QualityPref::Quality },
	};
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
		let g = |k: &str, d: u32| {
			std::env::var(k)
				.ok()
				.and_then(|v| v.parse().ok())
				.unwrap_or(d)
		};
		if render_child.is_some() {
			// Native zero-copy single-surface renderer (rkmpp→DRM_PRIME→EGL): sustains a full
			// stream easily. Default 1080p; fps follows the client's display refresh (auto).
			(
				g("PULSAR_W", 1920),
				g("PULSAR_H", 1080),
				g("PULSAR_FPS", auto_fps),
				g("PULSAR_KBPS", 15_000),
			)
		} else if vidsink_bin_path.is_some() {
			// Native zero-copy vidsink (rkmpp→DRM_PRIME→EGL): proven 468 fps @1080p / 264 @1440p
			// on this Pi, so it easily sustains a full stream. Default 1080p; auto fps.
			(
				g("PULSAR_W", 1920),
				g("PULSAR_H", 1080),
				g("PULSAR_FPS", auto_fps),
				g("PULSAR_KBPS", 15_000),
			)
		} else {
			// mpv fallback (no DRM_PRIME→EGL interop → HW-downloads every frame): keep the light
			// 540p30 cap so it can keep up / not overflow the socket.
			(
				g("PULSAR_W", 960),
				g("PULSAR_H", 540),
				g("PULSAR_FPS", 30),
				g("PULSAR_KBPS", 6_000),
			)
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
		// Session-menu audio defaults: transmit on; request host-silent in GAME mode (the
		// Moonlight/Sunshine default — "play on host" is OFF). The host satisfies host-silent by
		// REDIRECTING its default render endpoint to a sinkless virtual sink and capturing that —
		// NOT by muting the captured endpoint (muting taps post-mute on common codecs and would
		// stream pure silence; that was the dead-silent bug). Because the host now uses redirect,
		// requesting host-silent in game mode is safe again. Desktop mode leaves the host playing;
		// a future play-on-host toggle flips this off in game mode. The menu mirrors these live.
		transmit_audio: true,
		mute_host: game_mode,
		game_mode,
		// 0 = host default; quality: "hq"→Quality, "fast"→Latency, "auto"/missing→mode-derived default.
		bitrate_kbps: req_kbps,
		quality: req_quality,
		hdr: req_hdr,
		yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
		// PRUNED set (host-encoder-aware), not raw `client_codecs`: the host clamps its
		// auto-degraded codec to this, so it can never land on a codec the client dropped
		// because its decoder is incompatible with the host's encoder family (e.g. the Pi's
		// rkmpp can't decode native-NVENC HEVC — HEVC is in `client_codecs` but not `allowed`).
		decode_codecs: allowed.clone(),
		media_over_session: mos,
		// Cursor side-channel: the native renderer can draw the host pointer itself
		// (so the host may use the cursorless KMS zero-copy capture and stream the
		// pointer out-of-band). Opt-in behind PULSAR_CURSOR_SC=1 while it's proven on
		// the Pi; default OFF keeps the embedded-cursor behavior. The webview client
		// never sets it (no native overlay to draw the pointer into).
		cursor_external: cursor_external_enabled(),
		// First stream always targets the host's primary monitor; the session menu's
		// monitor picker changes it live via Restream::Display (see hold.rs).
		display_idx: 0,
		// Requested audio channel layout. Default Stereo — the universally-decodable
		// layout the native/webview audio paths expect (the client's SDP is opus/48000/2
		// per RFC 7587 and ffmpeg auto-detects multichannel from the bitstream). The host
		// negotiates this down against its own captured endpoint. A surround picker can
		// raise it later; carried across re-requests in hold.rs.
		audio_layout: pulsar_core::audio::ChannelLayout::Stereo,
	};
	if let Err(e) = request_stream(&mut sess, &req).await {
		// Park ANY live pulsar-render child instead of killing it — regardless of whether
		// it was a reused resident or a freshly-spawned one. Same rationale as the
		// request_launch guard above: destroying the EGL context on the shared RK3588 Mali
		// display (even via clean SIGTERM) wedges WebKitGTK click input. A fresh renderer
		// on an empty pool carries the same risk as a reused one.
		#[cfg(all(unix, not(target_os = "macos")))]
		let render_child_parked = if let Some(child) = render_child.take() {
			use std::io::Write as _;
			if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
				let _ = writeln!(si, "hide");
				let _ = si.flush();
			}
			let live_id = render_live_id.clone().unwrap_or_else(|| {
				std::sync::Arc::new(std::sync::atomic::AtomicU64::new(id))
			});
			state.resident_render.lock().unwrap().push(ResidentRender {
				child,
				stdin: overlay_stdin.clone(),
				container_id: id,
				live_id,
				game_mode,
			});
			// Cap the pool at 1 — reap any excess idle residents now that we pushed.
			reap_excess_resident_pool(&app, &*state, 1);
			tracing::info!(
				session_id = id,
				was_resident = render_child_is_resident,
				"request_stream failed: parked renderer (EGL context preserved)",
			);
			true
		} else {
			false
		};
		#[cfg(not(all(unix, not(target_os = "macos"))))]
		let render_child_parked = false;
		// Clean up the viewer + native renderer we already brought up before bailing.
		// preserve_native_container: if we parked the render child above, its GDK container
		// must stay alive so the renderer's --wid X parent remains valid for the next connect.
		teardown_partial(
			&app,
			id,
			single_surface,
			render_child_parked,
			view,
			vec![native_child, render_child],
		)
		.await;
		return Err(e.to_string());
	}

	// Linux native client: play the host's Opus/RTP audio NATIVELY (ffmpeg→PulseAudio),
	// because WebKitGTK can't decode it via WebCodecs (the webview audio path is silent there).
	// The viewer forwards the received audio datagrams to a loopback port ffmpeg listens on.
	#[cfg(target_os = "linux")]
	let audio_native: Option<Child> = if native && req.transmit_audio && audio_port > 0 {
		match std::net::UdpSocket::bind("127.0.0.1:0")
			.and_then(|s| s.local_addr().map(|a| a.port()))
		{
			Ok(lp) => {
				let ff = process::ffmpeg_bin(&app);
				// The host derives the real channel layout from its own endpoint/config
				// (it isn't carried in StreamReq), and the client negotiates stereo by
				// default, so pass the stereo channel count here — the SDP stays
				// stereo-correct and ffmpeg's Opus decoder still outputs the stream's
				// true channel count if the host ever sends multistream surround.
				match native_view::spawn_native_audio(&ff, lp, pulsar_core::audio::CHANNELS) {
					Some(c) => {
						// Track the audio SDP temp file (named by spawn_native_audio after
						// the loopback port) so stop_stream removes it on teardown.
						sdp_files
							.lock()
							.unwrap()
							.push(std::env::temp_dir().join(format!("pulsar-audio-{lp}.sdp")));
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
		// Clones the live controller_order Arc so slot reorders apply each tick without
		// requiring a reconnect (the UI writes via set_controller_order → AppState).
		let reader_flag = running.clone();
		let gtx = input_tx.clone();
		let order_arc = state.controller_order.clone();
		let emul_arc = state.controller_emulation.clone();
		// Clone the overlay stdin so the reader can emit `ctrls` lines (game mode only).
		let ctrls_stdin = overlay_stdin.clone();
		let _ctrls_game_mode = game_mode;
		tokio::task::spawn_blocking(move || {
			let Ok(mut hub) = ControllerHub::new() else {
				return;
			};
			// Track which uuids were connected last tick so we can detect disconnects.
			let mut prev_uuids: std::collections::HashSet<String> =
				std::collections::HashSet::new();
			// Remember each uuid → slot mapping from last tick for disconnect events.
			let mut prev_slot: std::collections::HashMap<String, u8> =
				std::collections::HashMap::new();
			// Last-emitted `ctrls` payload — only re-send when it changes (hotplug /
			// order change). Also throttled to ~1 Hz (every 60 ticks × 16 ms) so a
			// stale renderer that missed the initial line eventually gets it.
			let mut prev_ctrls_line = String::new();
			let mut ctrls_tick: u32 = 0;
			while reader_flag.load(Ordering::SeqCst) {
				let pads = hub.snapshot_with_keys();
				// Snapshot the current order Vec (cheap clone of the Vec).
				let order: Vec<String> = order_arc.lock().unwrap().clone();
				// Snapshot the per-uuid emulation target map (cheap clone each tick).
				let emul: std::collections::HashMap<String, String> =
					emul_arc.lock().unwrap().clone();
				let mut cur_uuids: std::collections::HashSet<String> =
					std::collections::HashSet::new();
				let mut append_idx = order.len() as u8;
				// Build the ctrls payload while computing slots (avoids a second pass).
				// Each entry: (slot, kind, uuid, target_str) where target_str is 'auto'/'xbox360'/'ds4'.
				let mut ctrls_entries: Vec<(u8, pulsar_core::input::GamepadKind, String, String)> =
					Vec::new();
				for (uuid, kind, state) in &pads {
					// Compute slot: position in order Vec, or next append slot (capped at 3).
					let slot = order
						.iter()
						.position(|k| k == uuid)
						.map(|i| i as u8)
						.unwrap_or_else(|| {
							let s = append_idx.min(3);
							append_idx += 1;
							s
						});
					// Resolve emulation target from the per-uuid map ("xbox"/"ds4"/absent→Auto).
					let target = match emul.get(uuid).map(|s| s.as_str()) {
						Some("xbox") => EmulationTarget::Xbox360,
						Some("ds4") => EmulationTarget::Ds4,
						_ => EmulationTarget::Auto,
					};
					// Keep the token as-is for the ctrls line (auto/xbox360/ds4 = serde lowercase).
					let target_str = match target {
						EmulationTarget::Xbox360 => "xbox360",
						EmulationTarget::Ds4 => "ds4",
						EmulationTarget::Auto => "auto",
					};
					cur_uuids.insert(uuid.clone());
					prev_slot.insert(uuid.clone(), slot);
					let _ = gtx.blocking_send(InputEvent::GamepadSlot {
						slot,
						kind: *kind,
						target,
						state: *state,
					});
					ctrls_entries.push((slot, *kind, uuid.clone(), target_str.to_string()));
				}
				// Emit GamepadDisconnect for any uuid that was present last tick but is gone now.
				for uuid in &prev_uuids {
					if !cur_uuids.contains(uuid) {
						if let Some(&slot) = prev_slot.get(uuid) {
							let _ = gtx.blocking_send(InputEvent::GamepadDisconnect { slot });
						}
						prev_slot.remove(uuid);
					}
				}
				prev_uuids = cur_uuids;
				// Build `ctrls slot:kind:name,...` line and push it to the overlay whenever
				// the set changes or every ~1 Hz (so a late-spawned renderer gets the list).
				// Format: NO spaces in the payload; name spaces become underscores. Kind is
				// the GamepadKind label() string with spaces replaced by underscores.
				ctrls_tick = ctrls_tick.wrapping_add(1);
				// Fetch names from hub.list() (cheap: same gilrs context, no extra alloc
				// beyond the list itself; called ~1Hz or on change, not every 16 ms tick).
				let name_map: std::collections::HashMap<String, String> = if !ctrls_entries.is_empty() {
					hub.list()
						.into_iter()
						.filter(|c| c.connected)
						.map(|c| (c.uuid, c.name))
						.collect()
				} else {
					std::collections::HashMap::new()
				};
				let ctrls_payload: String = {
					let mut entries = ctrls_entries.clone();
					entries.sort_by_key(|(s, _, _, _)| *s);
					entries
						.iter()
						.map(|(slot, kind, uuid, target_str)| {
							let kind_tag = kind.label().replace(' ', "_");
							let name = name_map
								.get(uuid)
								.map(|n| n.replace(' ', "_"))
								.unwrap_or_else(|| uuid.clone());
							// 5-field form: slot:kind_tag:name:uuid:target
							// Stale renderers (3-field parsers) tolerate the extra tokens via splitn(3).
							format!("{slot}:{kind_tag}:{name}:{uuid}:{target_str}")
						})
						.collect::<Vec<_>>()
						.join(",")
				};
				// Emit `ctrls` in BOTH modes — the controller overlay view (🎮 Kollar)
				// is available in remote mode too, so it needs the live list there.
				let changed = ctrls_payload != prev_ctrls_line;
				let periodic = ctrls_tick % 60 == 0;
				if changed || periodic {
					use std::io::Write as _;
					if let Some(si) = ctrls_stdin.lock().unwrap().as_mut() {
						let _ = writeln!(si, "ctrls {ctrls_payload}");
						let _ = si.flush();
					}
					if changed {
						prev_ctrls_line = ctrls_payload;
					}
				}
				std::thread::sleep(std::time::Duration::from_millis(16));
			}
		});
	}

	// DS4/DS5 touchpad-as-mouse: Linux only. Synthesizes PointerRelative /
	// PointerButton events from the physical touchpad surface and sends them
	// through the SAME input channel as the gamepad and keyboard readers.
	// No-ops silently when the device isn't found or isn't accessible.
	#[cfg(target_os = "linux")]
	if touchpad_as_mouse {
		pulsar_core::input::touchpad_linux::spawn_touchpad_reader(
			input_tx.clone(),
			running.clone(),
			None,
		);
	}
	#[cfg(not(target_os = "linux"))]
	let _ = touchpad_as_mouse; // suppress unused-variable warning on Windows/macOS

	// Side-channel queue (clipboard / chat / file / mic audio → host).
	let (data_tx, data_rx) = tokio::sync::mpsc::channel::<DataMsg>(512);
	// Push our identity image to the host (client → host direction; the host's
	// connections list shows it next to our id). Queued here and drained by
	// hold_session's data_rx → send_data like any side-channel message, so it goes
	// out right after the session is up. On a blocking thread — resolving the
	// avatar may decode a full-size wallpaper, too slow for this async fn. Honors
	// the avatar_mode setting (anonymous = nothing sent); best-effort, no error path.
	{
		let tx = data_tx.clone();
		let app_av = app.clone();
		let (mode, name) = {
			let cfg = state.config.lock().unwrap();
			let n = cfg.device_name.trim();
			let name = if n.is_empty() || n == "Pulsar Cihazı" {
				pulsar_core::discovery::os_display_name()
			} else {
				n.to_string()
			};
			(cfg.avatar_mode.clone(), name)
		};
		// Name first (tiny, instant); the avatar may take a blocking decode.
		let _ = tx.try_send(DataMsg::PeerName(name));
		tokio::task::spawn_blocking(move || {
			if let Some(png) = crate::avatar::avatar_png(&app_av, &mode) {
				let _ = tx.try_send(DataMsg::Avatar(png));
			}
		});
	}
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
		// PRUNED set: the hold-loop re-requests (adaptive bitrate / menu restream) carry this
		// as `decode_codecs`, so the host's clamp keeps excluding host-encoder-incompatible
		// codecs across the whole session, not just the first request (see `allowed` above).
		allowed.clone(),
		overlay_stdin.clone(),
		mos,
		host_nack,
		req_w,
		req_h,
		req_fps,
		req_kbps,
		req.cursor_external,
		req_hdr,
		req_quality,
	));

	let audio_is_native = audio_native.is_some();
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
			sdp_files,
			mpv_sdp,
			mpv_wid,
			vidsink_bin: vidsink_bin_path,
			vidsink_rotate: vidsink_rotate_init,
			render_child,
			render_stdin: overlay_stdin,
			video_port,
			game_mode,
			caps_line,
			render_seed,
			render_live_id,
			respawn_lock: Arc::new(tokio::sync::Mutex::new(())),
		},
	);
	Ok(PlayInfo {
		id,
		transport,
		ws_port,
		// When the NATIVE audio player runs (Linux ffmpeg→PulseAudio), the webview must
		// NOT also open the audio WebSocket — on WebKits whose WebCodecs CAN decode Opus
		// the same stream played twice (native + WebAudio, offset by the webview's
		// buffering) as a delayed echo. Port 0 = the frontend skips its audio path.
		audio_ws_port: if audio_is_native { 0 } else { audio_ws_port },
		local,
		native,
		embedded: single_surface,
		// The UI gates codec options on the NEGOTIATED set (host ∩ client).
		host_codecs: allowed,
		host_displays: host_caps.displays,
		host_encoders: host_caps.encoders,
		client_codecs,
	})
}

/// Drain excess entries from `AppState::resident_render` so the pool never exceeds `cap`
/// entries. Called immediately after every `.push()` at the three park sites.
///
/// # Why this is safe
/// Only idle (hidden) renderers are ever in the pool — each was sent `hide\n` before being
/// parked, so its X window is already unmapped and its EGL surface is dormant. A SIGTERM
/// causes the renderer to run its full clean teardown sequence (eglDestroyContext →
/// XDestroyWindow → XCloseDisplay) and then exit. This is the SAME sequence as a normal
/// session end and does NOT corrupt WebKitGTK's shared Mali GL state. The wedge only occurs
/// when a SIGKILL abandons an active EGL context mid-frame; an idle renderer hit with SIGTERM
/// exits cleanly.
///
/// Excess entries are drained from the FRONT of the Vec (oldest entries first); the most
/// recently parked entry (at the back, which `.pop()` will take on the next connect) is kept.
#[cfg(all(unix, not(target_os = "macos")))]
fn reap_excess_resident_pool(app: &AppHandle, state: &AppState, cap: usize) {
	let excess: Vec<ResidentRender> = {
		let mut pool = state.resident_render.lock().unwrap();
		if pool.len() > cap {
			let n_excess = pool.len() - cap;
			pool.drain(..n_excess).collect()
		} else {
			return;
		}
	};
	for excess_r in excess {
		tracing::info!(
			pid = excess_r.child.id(),
			container_id = excess_r.container_id,
			"resident_render pool over cap — reaping excess idle renderer (SIGTERM, EGL-safe)"
		);
		// SIGTERM the idle renderer: clean EGL teardown, no Mali wedge.
		stop_render_child_blocking(excess_r.child);
		// The renderer's X window is destroyed by its own teardown, but the GDK container
		// (the parent GdkWindow we created for `--wid` embedding) is owned by this process
		// and must be explicitly released so it doesn't leak in the GTK widget tree.
		crate::render::destroy_native_container(app, excess_r.container_id);
	}
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
pub(crate) fn stop_render_child(child: &mut std::process::Child) {
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
pub(crate) fn stop_render_child(child: &mut std::process::Child) {
	let _ = child.kill();
	let _ = child.wait();
}

/// Async wrapper: runs `stop_render_child` on a blocking thread so the SIGTERM-grace
/// poll (up to ~600 ms on unix) never occupies a tokio worker thread.  Takes ownership
/// of the `Child` so the closure is `'static`.  Callers that must wait for the port to
/// be released (e.g. respawn_render_for_codec) should `.await` the returned handle;
/// callers that are fire-and-forget (e.g. session teardown) may drop it.
pub(crate) fn stop_render_child_blocking(
	mut child: std::process::Child,
) -> tokio::task::JoinHandle<()> {
	tokio::task::spawn_blocking(move || stop_render_child(&mut child))
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
	// Drop this session's per-id monitor-picker debounce entry so the map doesn't
	// accumulate stale entries across reconnects.
	crate::session_cmds::forget_monitor_debounce(id);
	// A session torn down with its overlay still open must release the global
	// SUSPENDED latch (see AppState::overlay_open) — otherwise the next session
	// starts permanently un-engageable.
	// `overlay_was_open` is used below (Linux resident path) to send SIGUSR2 so
	// the parked renderer's OPEN flag is reset before it is reused.
	#[allow(unused_variables)]
	let overlay_was_open;
	{
		let mut set = state.overlay_open.lock().unwrap();
		overlay_was_open = set.remove(&id);
		if overlay_was_open {
			crate::kbdhook::overlay_suspend(!set.is_empty());
		}
	}
	// Set to true when the Linux `pulsar-render` child is parked as a resident (kept alive)
	// rather than killed — in that case the GDK container must NOT be destroyed here.
	#[allow(unused_mut, unused_assignments, unused_variables)]
	let mut resident_container_kept = false;
	if let Some(mut play) = play {
		play.running.store(false, Ordering::SeqCst);
		play.viewer.stop();
		if let Some(mut mic) = play.mic.lock().unwrap().take() {
			let _ = mic.kill();
			let _ = mic.wait(); // reap — kill alone leaves a unix zombie until app exit
		}
		// Close the ffplay fallback renderer (Windows/Linux mpv-fallback), if any. GRACEFUL
		// (SIGTERM-first, see stop_render_child) — kills a separate window process cleanly.
		// The pulsar-render child is handled separately below (Linux: kept resident).
		// Fire-and-forget: teardown does not need to complete before stop_stream returns.
		if let Some(child) = play.ffplay.take() {
			stop_render_child_blocking(child);
		}
		// Linux `pulsar-render` (embedded `--wid` renderer): keep it RESIDENT between sessions
		// to avoid destroying its EGL context. On RK3588 destroying the EGL context of an
		// embedded renderer that shares the Mali display with WebKitGTK corrupts WebKit's shared
		// Mali GL state — the webview stops processing clicks (hover works, nothing is clickable)
		// with no in-session recovery short of a reboot. A clean SIGTERM teardown runs the same
		// EGL context destruction (XDestroyWindow + eglDestroyContext + XCloseDisplay) as a
		// SIGKILL, so it carries the same risk. The safe model: send `hide\n` (the renderer
		// unmaps its window, revealing the WebKitGTK webview underneath, but keeps its EGL
		// context alive and idle-loops), park the child in AppState::resident_render, and on the
		// next connect reuse it by sending `show\n` + `reopen <new-sdp>\n` + new caps lines.
		// The GDK container (child GdkWindow) is ALSO kept alive (skipping destroy_native_container
		// below) so the renderer's `--wid` X parent window remains valid; the container is
		// re-registered under the new session id on reconnect.
		// Non-Linux / mpv-fallback: fall through to stop_render_child as before (Windows/macOS
		// have no shared-Mali-GL issue).
		#[cfg(all(unix, not(target_os = "macos")))]
		if let Some(mut child) = play.render_child.take() {
			use std::io::Write as _;
			// If the overlay was open when the session ended, force-close it before parking:
			// send SIGUSR2 so the renderer's OPEN AtomicBool is reset to false and it stops
			// drawing the egui menu. Without this the renderer parks with OPEN=true and
			// the next reused session immediately renders the overlay over the fresh video
			// (the frontend's overlayOpen is false for the new session, so nothing re-closes it).
			// Also restore the container's input pass-through so the new session's video
			// window receives pointer events by default (not blocked by the stale egui overlay).
			if overlay_was_open {
				unsafe {
					libc::kill(child.id() as i32, libc::SIGUSR2);
				}
				crate::render::set_container_pass_through(&app, id, true);
			}
			// Before parking, verify the child is still alive. If it exited during the session
			// (crash, OOM-kill, external signal), parking a dead Child would poison the next
			// reconnect (the reuse path would write to a broken stdin and produce a black
			// screen with no recovery). Reap it instead and leave resident_render empty so the
			// next connect falls through to a fresh spawn.
			let child_alive = match child.try_wait() {
				Ok(None) => true,        // still running — safe to park
				Ok(Some(status)) => {
					tracing::warn!(pid = child.id(), ?status, "pulsar-render exited mid-session — dropping dead resident, will spawn fresh on next connect");
					let _ = child.wait(); // reap the zombie
					false
				}
				Err(e) => {
					tracing::warn!(pid = child.id(), err = %e, "try_wait failed on pulsar-render — treating as dead, will spawn fresh on next connect");
					false
				}
			};
			if child_alive {
				// Signal the renderer to hide (unmap video window, idle-loop, keep EGL alive).
				if let Some(si) = play.render_stdin.lock().unwrap().as_mut() {
					let _ = writeln!(si, "hide");
					let _ = si.flush();
				}
				// Park in AppState pool. Cap the pool at 1 after pushing: excess idle
				// parked renderers (e.g. from a prior multi-tab session) are drained from
				// the front and SIGTERM'd. SIGTERM is safe for idle (hidden-window)
				// renderers — they run their own EGL teardown cleanly. Only SIGKILL of an
				// active renderer corrupts the shared Mali GL; idle renderers are not
				// actively rendering and exit cleanly on SIGTERM (see
				// `reap_excess_resident_pool` for the full rationale). Capping at 1
				// prevents the pool from growing to the multi-tab high-water mark and
				// leaking CPU/GPU/socket resources indefinitely (the orphan-pile-up fix).
				let live_id = play
					.render_live_id
					.clone()
					.unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicU64::new(id)));
				state.resident_render.lock().unwrap().push(ResidentRender {
					child,
					stdin: play.render_stdin.clone(),
					container_id: id,
					live_id,
					game_mode: play.game_mode,
				});
				// Reap excess idle residents beyond the cap of 1.
				reap_excess_resident_pool(&app, &*state, 1);
				resident_container_kept = true; // container must NOT be destroyed — kept for next session
			}
			// If the child was dead, resident_container_kept stays false → the container is
			// destroyed below (it will be re-created on the next fresh spawn).
		}
		// Non-Linux (Windows/macOS) or mpv/ffplay fallback on Linux (render_child is None
		// there; the ffplay child is handled via play.ffplay above):
		// Fire-and-forget: teardown does not need to complete before stop_stream returns.
		#[cfg(not(all(unix, not(target_os = "macos"))))]
		if let Some(child) = play.render_child.take() {
			stop_render_child_blocking(child);
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
		// Remove every SDP temp file this session wrote (video + Linux audio, plus one per
		// codec/monitor switch). Their port-based names are effectively unique per session,
		// so without this they accumulate in temp_dir for the life of the machine.
		for sdp in play.sdp_files.lock().unwrap().drain(..) {
			let _ = std::fs::remove_file(&sdp);
		}
	}
	// Tear down the Linux single-surface renderer (libmpv→GLArea), if this session used it.
	#[cfg(all(unix, not(target_os = "macos")))]
	crate::render::teardown_single_surface(&app, id).await;
	// Drop the in-app video container — UNLESS the renderer was kept resident (the container
	// is still the renderer's valid `--wid` parent and will be re-registered on reconnect).
	#[cfg(all(unix, not(target_os = "macos")))]
	if !resident_container_kept {
		crate::render::destroy_native_container(&app, id);
	}
	let _ = &app; // used by teardown on Linux; silence unused elsewhere
	Ok(())
}
