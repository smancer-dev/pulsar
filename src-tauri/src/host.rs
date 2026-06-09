//! Host role: bind the node, register with the relay, and serve incoming sessions
//! (auth → games → stream → input → side-channels). `go_online` is the single
//! long-lived entry point; the per-session stream/file/audio handlers (and the
//! Windows WASAPI loopback helper) live in the `handlers` submodule.

use std::net::SocketAddr;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::input::{create_virtual_pad, GamepadKind, VirtualGamepad};
use pulsar_core::pipeline::{self, CaptureMethod, HwEncoder, StreamPlan};
use pulsar_core::proto::DeviceId;
use pulsar_core::service::{
	accept, need_password, recv_auth, reject, serve_with, DataHandlers, DataMsg, GameInfo,
	InputEvent, QualityPref, StreamReq,
};
use pulsar_core::{Discovery, Node};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::audio_io::spawn_audio_player;
use crate::events::{DataPayload, FilePayload, ReverseReq, SessionEvent};
use crate::files::{sanitize_filename, save_received_file};
use crate::process::{
	capture_from_str, codec_from_str, encoder_from_str, ffmpeg_bin,
	launch_host_game, no_window, probe_ddagrab_zerocopy, spawn_tracked,
};
use crate::state::AppState;
use crate::util::{
	config_path, display_rotation, identity_path, resolve_relay, DDAGRAB_ZEROCOPY,
};

mod handlers;
use handlers::{make_on_audio, make_on_file, make_on_stream};

/// Bind the node and register with the configured relay; returns this device's
/// grouped ID. Fails (so the UI shows "offline") when the relay is unreachable.
#[tauri::command]
pub(crate) async fn go_online(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
	let cfg = state.config.lock().unwrap().clone();
	// go_online is re-runnable (startup, manual retry, relay/network settings change).
	// Tear down any previous serve loop + node FIRST so we don't leak a stale node
	// (its UDP socket, relay heartbeat, serve task, LAN beacon) on every reconnect.
	// Aborting the serve loop drops its Arc<Node> clone; taking state.node drops ours,
	// so the old node reaches strong-count 0 and its recv_loop/heartbeat_loop exit.
	if let Some(h) = state.serve_task.lock().unwrap().take() {
		h.abort();
	}
	let _ = state.node.lock().unwrap().take();
	tracing::info!(relay = %cfg.relay, "go_online: resolving relay");
	let relay = resolve_relay(&cfg.relay)
		.await
		.ok_or_else(|| format!("relay çözümlenemedi: {}", cfg.relay))?;
	tracing::info!(%relay, "go_online: binding node + registering");
	let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
	// Identity advertised on the network: the user's chosen device name, or — when
	// it's the generic default — the OS user's name, so relay-less peers are still
	// recognizable ("Ahmet Enes Duruer" instead of "Pulsar Cihazı").
	let announce_name = {
		let n = cfg.device_name.trim();
		if n.is_empty() || n == "Pulsar Cihazı" {
			pulsar_core::discovery::os_display_name()
		} else {
			n.to_string()
		}
	};
	// Prefer the user's configured node port (Settings → Ağ), else the well-known
	// port so bare-IP direct connects can reach us; fall back to an ephemeral port if
	// it's already taken (e.g. a 2nd instance/seat).
	let want_port = if cfg.node_port != 0 {
		cfg.node_port
	} else {
		pulsar_core::proto::DEFAULT_NODE_PORT
	};
	let preferred = SocketAddr::new(local.ip(), want_port);
	// Persisted per-user identity → the relay hands back the SAME 9-digit ID every
	// launch (stable device ID). Different OS users keep separate identity files.
	let identity = pulsar_core::crypto::Identity::load_or_create(identity_path(&app));
	let node = match Node::bind_with_identity(
		preferred,
		relay,
		cfg.network_mode,
		announce_name.clone(),
		identity.clone(),
	)
	.await
	{
		Ok(n) => n,
		Err(_) => {
			Node::bind_with_identity(local, relay, cfg.network_mode, announce_name.clone(), identity)
				.await
				.map_err(|e| e.to_string())?
		}
	};

	// Start LAN discovery BEFORE registering so it works even when the relay is
	// unreachable (offline mode): we announce ourselves (id-less) and find peers on
	// the local network regardless of relay state. Replaces any prior beacon.
	let node_port = node.local_addr().map(|a| a.port()).unwrap_or(0);
	let discovery =
		match Discovery::start(announce_name.clone(), node_port, node.public_key(), None).await {
			Ok(d) => {
				tracing::info!(port = node_port, name = %announce_name, "LAN discovery beacon started");
				*state.discovery.lock().unwrap() = Some(d.clone());
				Some(d)
			}
			Err(e) => {
				tracing::warn!(%e, "LAN discovery failed to start");
				None
			}
		};

	// Register with the relay. If it's unreachable we stay "offline" but keep the
	// node + LAN discovery running so same-network devices still appear.
	let id = match node.register().await {
		Ok(id) => id,
		Err(e) => {
			tracing::info!(error = %e, "relay unreachable — staying offline, LAN discovery still active");
			*state.node.lock().unwrap() = Some(node);
			return Err(e.to_string());
		}
	};
	tracing::info!(%id, "go_online: registered with relay");
	// Now that we have a relay id, advertise it on the LAN too.
	if let Some(d) = &discovery {
		d.set_id(Some(id)).await;
	}

	// Issue a fresh one-time password for this online session (unless unattended
	// access is on, in which case no password is required).
	let require_auth = !cfg.unattended_access;
	let password = if require_auth {
		pulsar_core::service::gen_password()
	} else {
		String::new()
	};
	*state.password.lock().unwrap() = password;

	// Host role: serve published games, start streams, and surface activity.
	let games = state.games.clone();
	let stream_cfg = state.stream_cfg.clone();
	// Read the live password per connection (so `new_password` takes effect).
	let password_store = state.password.clone();
	let pending = state.pending.clone();
	let next_req = state.next_req.clone();
	let incoming = state.incoming.clone();
	let host_out = state.host_out.clone();
	let active = state.active.clone();
	#[cfg(target_os = "linux")]
	let restore_token = state.restore_token.clone();
	let serve_node = node.clone();
	let app_h = app.clone();
	let serve_handle = tokio::spawn(async move {
		while let Some(session) = serve_node.next_incoming().await {
			let games = games.clone();
			let stream_cfg = stream_cfg.clone();
			// ffmpeg children for THIS session live here and are killed on teardown
			// below — never in a global pool, so a client's exit can't orphan them.
			let procs: Arc<Mutex<Vec<Child>>> = Arc::new(Mutex::new(Vec::new()));
			// Native DXGI+NVENC capture handle for this session (Windows), when the native path
			// is used instead of ffmpeg. Stopped at the same drain sites as `procs`.
			#[cfg(windows)]
			let native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>> =
				Arc::new(Mutex::new(None));
				// True once this session muted the host's local output (game mode / the
				// mute setting), so teardown only un-mutes what we muted.
				let host_muted = Arc::new(AtomicBool::new(false));
			let password_store = password_store.clone();
			let pending = pending.clone();
			let next_req = next_req.clone();
			let incoming = incoming.clone();
			let host_out = host_out.clone();
			let active = active.clone();
			#[cfg(target_os = "linux")]
			let restore_token = restore_token.clone();
			let app_h = app_h.clone();
			let peer = {
				let id = session.peer();
				if id.0 >= DeviceId::MIN {
					id.grouped()
				} else {
					// Direct (relay-less) connect has no relay id — key by the address.
					session
						.peer_addr()
						.await
						.map(|a| a.to_string())
						.unwrap_or_else(|| "direct".into())
				}
			};
			// This session's id: used so a same-peer reconnection that replaced our
			// `incoming`/`host_out` entries isn't evicted when THIS (older) session tears
			// down (both maps are keyed by `peer`, which collides across reconnects).
			let sid = session.id();
			tokio::spawn(async move {
				let mut session = session;
				// The client's first message is its access request (password may be
				// empty). Auto-allow no-auth hosts or a correct password; otherwise
				// pop an attention-grabbing Allow/Deny window for the host user.
				let provided = match recv_auth(&mut session).await {
					Some(p) => p,
					None => return,
				};
				// Auth: a correct up-front password is accepted immediately. Otherwise
				// the host's Allow/Deny popup AND the client's password prompt appear
				// at the SAME time; accept on whichever lands first (so the host can
				// approve passwordlessly). Unattended hosts auto-allow.
				let approved = if require_auth {
					let host_pw = password_store.lock().unwrap().clone();
					if !host_pw.is_empty() && provided == host_pw {
						true
					} else {
						let _ = need_password(&mut session).await;
						crate::auth::race_host_auth(
							&mut session,
							&app_h,
							&pending,
							&next_req,
							&peer,
							&host_pw,
						)
						.await
					}
				} else {
					true
				};
				if !approved {
					let _ = reject(&mut session).await;
					tracing::info!(%peer, "connection rejected");
					let _ = app_h.emit(
						"session",
						SessionEvent {
							kind: "rejected".into(),
							peer: peer.clone(),
							detail: String::new(),
						},
					);
					return;
				}
				let _ = accept(&mut session).await;
				tracing::info!(%peer, "incoming session connected");
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "connected".into(),
						peer: peer.clone(),
						detail: String::new(),
					},
				);
				// Track this connection for the dedicated connections window. The mode is
				// provisional (Remote) until the client's stream request reveals game_mode
				// (`make_on_stream`), which also brings the window forward / keeps it hidden.
				{
					let now_ms = std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.map(|d| d.as_millis() as u64)
						.unwrap_or(0);
					active.lock().unwrap().insert(
						peer.clone(),
						crate::state::ConnInfo {
							sid,
							since_ms: now_ms,
							mode: crate::state::ConnMode::Remote,
						},
					);
				}
				// Allow the host UI to kick this client (`disconnect_peer`).
				let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
				incoming.lock().unwrap().insert(peer.clone(), (sid, stop_tx));

				// Side channels: a queue the host UI drains to push chat/clipboard back
				// to this client (registered by peer id so `host_send_*` can find it).
				let (out_tx, out_rx) = tokio::sync::mpsc::channel::<DataMsg>(256);
				// A clone for on_stream to push the encode summary to the client.
				let stats_out = out_tx.clone();
				host_out.lock().unwrap().insert(peer.clone(), (sid, out_tx));

				// Per-session: hold the screen capture so it can be stopped when this
				// client disconnects. (Input injection is via uinput in `on_input`.)
				#[cfg(target_os = "linux")]
				let cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>> =
					Arc::new(Mutex::new(None));

				let provider = {
					let games = games.clone();
					move || {
						games
							.lock()
							.unwrap()
							.iter()
							.map(|h| GameInfo {
								id: h.id.clone(),
								title: h.title.clone(),
								kind: h.kind.clone(),
							})
							.collect::<Vec<_>>()
					}
				};
				let on_launch = {
					let games = games.clone();
					let app_h = app_h.clone();
					let peer = peer.clone();
					move |id: String| {
						// Match by id first, then tolerantly by title (case-insensitive) so a
						// CLI `--app <name>` works. An unmatched app (incl. "Desktop"/"Masaüstü")
						// launches nothing — the host still streams the whole desktop.
						let found = {
							let g = games.lock().unwrap();
							g.iter()
								.find(|h| h.id == id)
								.or_else(|| g.iter().find(|h| h.title.eq_ignore_ascii_case(id.trim())))
								.cloned()
						};
						if let Some(g) = found {
							let _ = app_h.emit(
								"session",
								SessionEvent {
									kind: "launch".into(),
									peer: peer.clone(),
									detail: g.title.clone(),
								},
							);
							launch_host_game(&g);
						}
					}
				};
				let on_stream = make_on_stream(
					stream_cfg.clone(),
					procs.clone(),
					active.clone(),
					sid,
					#[cfg(windows)]
					native_slot.clone(),
					host_muted.clone(),
					stats_out.clone(),
					app_h.clone(),
					peer.clone(),
					#[cfg(target_os = "linux")]
					restore_token.clone(),
					#[cfg(target_os = "linux")]
					cap_slot.clone(),
				);
				// Route the client's input: controllers into a virtual gamepad, and
				// mouse/keyboard into a uinput desktop injector — both created lazily.
				let on_input = {
					let mut pad: Option<Box<dyn VirtualGamepad>> = None;
					let mut desktop: Option<pulsar_core::input::DesktopInput> = None;
					let mut tried = false;
					// Input is injected WITHOUT any pointer rotation. The host video is always presented
					// UPRIGHT (the native capture bakes the display rotation into the frame, or a rotated
					// ffmpeg stream is un-rotated by the client), and Windows SendInput addresses the same
					// logical desktop coordinate space the upright video shows — coords inject as-is.
					// (Rotating here would DOUBLE-correct vs the baked-upright video → 180°-mirrored clicks.)
					move |ev: InputEvent| {
						match ev {
						InputEvent::Gamepad(state) => {
							pad.get_or_insert_with(|| create_virtual_pad(GamepadKind::Xbox))
								.apply(&state);
						}
						other => {
							if !tried {
								tried = true;
								match pulsar_core::input::DesktopInput::new() {
									Ok(d) => desktop = Some(d),
									Err(e) => tracing::warn!("desktop input unavailable: {e}"),
								}
							}
							if let Some(d) = desktop.as_mut() {
								match other {
									InputEvent::PointerMotion { x, y } => {
										let (rx, ry) = (x, y);
										d.pointer(rx, ry)
									}
									InputEvent::PointerRelative { dx, dy } => {
										let (rdx, rdy) = (dx, dy);
										d.pointer_relative(rdx, rdy)
									}
									InputEvent::PointerButton { button, down } => {
										d.button(button, down)
									}
									InputEvent::Scroll { dx, dy } => d.scroll(dx, dy),
									InputEvent::Key { code, down } => d.key(code, down),
									InputEvent::Char(c) => d.type_char(c),
									InputEvent::Gamepad(_) => {}
								}
							}
						}
						}
					}
				};
				// Side channels (clipboard / chat / file / mic audio) from this client.
				let on_clipboard = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					move |text: String| {
						let _ = app_h.emit(
							"clipboard",
							DataPayload {
								peer: peer.clone(),
								text,
							},
						);
					}
				};
				let on_chat = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					move |text: String| {
						let _ = app_h.emit(
							"host-chat",
							DataPayload {
								peer: peer.clone(),
								text,
							},
						);
					}
				};
				let on_file = make_on_file(app_h.clone(), peer.clone());
				let on_audio = make_on_audio();
				let on_reverse = {
					let app_h = app_h.clone();
					move |id: String| {
						// The controlling peer asked us to reverse roles: surface it so the
						// host UI can connect back to `id` (it must be online/serving).
						let _ = app_h.emit("reverse-request", ReverseReq { id });
					}
				};
				let handlers = DataHandlers {
					outbound: Some(out_rx),
					on_clipboard: Box::new(on_clipboard),
					on_chat: Box::new(on_chat),
					on_file: Box::new(on_file),
					on_audio: Box::new(on_audio),
					on_reverse: Box::new(on_reverse),
				};
				tokio::select! {
					_ = serve_with(session, provider, on_launch, on_stream, on_input, handlers) => {}
					_ = &mut stop_rx => {} // host kicked this client from the UI
				}
				// Session ended (peer gone or host kicked): kill this session's ffmpeg
				// so capture/encode stops at once and the GPU is freed. Held mouse
				// buttons / modifier keys are released by DesktopInput's Drop (the
				// on_input closure is dropped when serve_with's future ends above).
				for mut child in procs.lock().unwrap().drain(..) {
					let _ = child.kill();
					let _ = child.wait();
				}
				// Stop the native capture thread (releases the NVENC session + DXGI duplication).
				#[cfg(windows)]
				if let Some(h) = native_slot.lock().unwrap().take() {
					h.stop();
				}
				// Restore the host's local audio if this session muted it.
				if host_muted.swap(false, Ordering::SeqCst) {
					let _ = pulsar_core::audio::set_host_muted(false);
				}
				// Compare-and-remove: only drop the entries if they still belong to THIS
				// session. A same-peer reconnection may have already overwritten them with
				// its own (newer) sid; removing unconditionally would kill the live one.
				{
					let mut g = incoming.lock().unwrap();
					if g.get(&peer).map(|(id, _)| *id) == Some(sid) {
						g.remove(&peer);
					}
				}
				{
					let mut g = host_out.lock().unwrap();
					if g.get(&peer).map(|(id, _)| *id) == Some(sid) {
						g.remove(&peer);
					}
				}
				tracing::info!(%peer, "session disconnected");
				// Drop from the connections window's list (sid-guarded like incoming/host_out,
				// so a same-peer reconnection's newer entry survives); close the window once
				// the last connection ends.
				let conns_emptied = {
					let mut g = active.lock().unwrap();
					if g.get(&peer).map(|ci| ci.sid) == Some(sid) {
						g.remove(&peer);
					}
					g.is_empty()
				};
				if conns_emptied {
					crate::connections::close(&app_h);
				}
				// Stop this session's screen capture — closes the portal session so
				// KDE/GNOME stops showing "screen is being shared".
				#[cfg(target_os = "linux")]
				{
					let cap = cap_slot.lock().unwrap().take();
					if let Some(cap) = cap {
						cap.stop().await;
					}
				}
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "disconnected".into(),
						peer,
						detail: String::new(),
					},
				);
			});
		}
	});

	*state.node.lock().unwrap() = Some(node);
	*state.serve_task.lock().unwrap() = Some(serve_handle);
	Ok(id.grouped())
}
