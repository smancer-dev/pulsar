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
use tauri::{AppHandle, Emitter, Manager as _, State};
use tokio::sync::oneshot;

use crate::audio_io::spawn_audio_player;
use crate::events::{AvatarPayload, DataPayload, FilePayload, ReverseReq, SessionEvent};
use crate::files::{sanitize_filename, save_received_file};
use crate::process::{
	capture_from_str, codec_from_str, encoder_from_str, ffmpeg_bin, launch_host_game, no_window,
	probe_ddagrab_zerocopy, spawn_tracked,
};
use crate::state::AppState;
use crate::util::{config_path, display_rotation, identity_path, resolve_relay, DDAGRAB_ZEROCOPY};

mod handlers;
#[cfg(target_os = "linux")]
pub(crate) mod cursor;
use handlers::{make_on_audio, make_on_file, make_on_stream};

/// Transport features this host advertises in its `StreamCaps` reply: it can carry
/// the RTP media inside the session (single socket) and honors NACK retransmits.
fn media_features() -> Vec<String> {
	use pulsar_core::service::media::{FEAT_MOS, FEAT_NACK};
	vec![FEAT_MOS.to_string(), FEAT_NACK.to_string()]
}

/// Bind the node and register with the configured relay; returns this device's
/// grouped ID. Fails (so the UI shows "offline") when the relay is unreachable.
#[tauri::command]
pub(crate) async fn go_online(
	app: AppHandle,
	state: State<'_, AppState>,
) -> Result<String, String> {
	// Pre-warm ALL encoder probes off the hot path: the first QueryStreamCaps must
	// answer within the client's 2 s window, but a cold probe chain (one-frame ffmpeg
	// encodes per backend×codec + the gst pipelines) takes several seconds. Results
	// are cached per process, so this makes the first caps reply instant. (Verified
	// failure mode on the Pi: cold probes > 2 s → client timed out → auto codec fell
	// back to H.264 even though MPP HEVC was available.)
	{
		let ffmpeg = crate::process::ffmpeg_bin(&app);
		let vaapi = state.stream_cfg.lock().unwrap().vaapi_device.clone();
		std::thread::spawn(move || {
			let _ = crate::process::validated_encoders(&ffmpeg, &vaapi);
			#[cfg(target_os = "linux")]
			let _ = crate::process::validated_gst_encoders();
		});
	}

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
	// The old node (and its port) is gone: clear the advertised port now, so a
	// go_online that fails below doesn't leave Home showing a copyable ip:port
	// that no longer accepts connections. The success path re-publishes the
	// real port further down.
	state
		.node_port
		.store(0, std::sync::atomic::Ordering::SeqCst);
	let _ = app.emit("node-port", 0u16);
	// A previous serve loop's sessions may not have torn down cleanly (independent
	// spawns survive the accept-loop abort) — never carry a stale host-mute over.
	handlers::reset_mute_all();
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
	// Port policy: an explicitly configured port (Settings → Ağ) is binding — if it's
	// already taken, FAIL with a clear error instead of silently sliding to another
	// port (the user pinned it for a firewall rule / port-forward; a silent ephemeral
	// fallback made those rules quietly useless). Unset (0) = a RANDOM ephemeral port
	// every launch — the LAN beacon and the Home screen's "ip:port" always carry the
	// real port, so discovery/direct connects keep working.
	let preferred = SocketAddr::new(local.ip(), cfg.node_port);
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
		Err(e) if cfg.node_port != 0 => {
			return Err(format!(
				"{} ({}): {e}",
				crate::i18n::t("err.portInUse"),
				cfg.node_port
			));
		}
		Err(e) => return Err(e.to_string()),
	};

	// Start LAN discovery BEFORE registering so it works even when the relay is
	// unreachable (offline mode): we announce ourselves (id-less) and find peers on
	// the local network regardless of relay state. Replaces any prior beacon.
	let node_port = node.local_addr().map(|a| a.port()).unwrap_or(0);
	// Surface the live port to the UI (Home shows "ip:port" for direct connects):
	// state for late mounts + an event for screens already up.
	state
		.node_port
		.store(node_port, std::sync::atomic::Ordering::SeqCst);
	let _ = app.emit("node-port", node_port);
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
	// Our display name, pushed to every connecting client (PeerName decoration).
	let self_name = announce_name.clone();
	let serve_handle = tokio::spawn(async move {
		while let Some(session) = serve_node.next_incoming().await {
			let self_name = self_name.clone();
			let games = games.clone();
			let stream_cfg = stream_cfg.clone();
			// ffmpeg children for THIS session live here and are killed on teardown
			// below — never in a global pool, so a client's exit can't orphan them.
			let procs: Arc<Mutex<Vec<Child>>> = Arc::new(Mutex::new(Vec::new()));
			// Native DXGI+NVENC capture handle for this session (Windows), when the native path
			// is used instead of ffmpeg. Stopped at the same drain sites as `procs`.
			#[cfg(windows)]
			let native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>> = Arc::new(Mutex::new(None));
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
				// Bounded wait: a peer that establishes a session but never sends
				// Auth would otherwise pin this task (and its SessionState +
				// unbounded channel) forever — UDP gives no close.
				let provided = match tokio::time::timeout(
					std::time::Duration::from_secs(60),
					recv_auth(&mut session),
				)
				.await
				{
					Ok(Some(p)) => p,
					_ => return,
				};
				// Auth: a correct up-front password is accepted immediately. Otherwise
				// the host's Allow/Deny popup AND the client's password prompt appear
				// at the SAME time; accept on whichever lands first (so the host can
				// approve passwordlessly). Unattended hosts auto-allow. The persistent
				// connect password (Settings → Güvenlik) is accepted alongside the
				// one-time password; wrong attempts are rate-limited per peer, and a
				// locked-out peer is rejected up front WITHOUT an Allow/Deny popup
				// (otherwise repeated connects could spam attention-grabbing windows).
				let approved = if require_auth {
					if let Some(rem) = crate::auth::throttle::locked_out(&peer) {
						tracing::warn!(%peer, secs = rem.as_secs(), "auth throttled: rejecting without prompt");
						false
					} else {
						let host_pw = password_store.lock().unwrap().clone();
						let custom_pw = app_h
							.state::<crate::state::AppState>()
							.config
							.lock()
							.unwrap()
							.connect_password
							.clone();
						let accepted: Vec<String> = [host_pw, custom_pw]
							.into_iter()
							.filter(|p| !p.is_empty())
							.collect();
						if !accepted.is_empty() && accepted.iter().any(|a| provided == *a) {
							true
						} else {
							if !provided.is_empty() {
								// A wrong up-front guess counts toward the throttle too.
								crate::auth::throttle::record_failure(&peer);
							}
							if crate::auth::throttle::locked_out(&peer).is_some() {
								false
							} else {
								let _ = need_password(&mut session).await;
								crate::auth::race_host_auth(
									&mut session,
									&app_h,
									&pending,
									&next_req,
									&peer,
									&accepted,
								)
								.await
							}
						}
					}
				} else {
					true
				};
				if approved {
					crate::auth::throttle::clear(&peer);
				}
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
				// Connection time for the connections window. Registration into
				// `active`/`incoming`/`host_out` is DEFERRED to the first stream request
				// (`make_on_stream`): a short-lived control session from the same peer
				// (Home's "fetch games" while a play session is live) must never clobber
				// the live session's entries — overwriting `incoming` drops the live
				// stop_tx, which instantly tears its stream down. A second STREAMING
				// session from the same peer still takes over (the overwritten stop_tx
				// drop ends the old session — the documented same-peer reconnect path).
				let since_ms = std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.map(|d| d.as_millis() as u64)
					.unwrap_or(0);
				// Allows the host UI to kick this client (`disconnect_peer`) once
				// make_on_stream registers it in `incoming`.
				let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

				// Side channels: a queue the host UI drains to push chat/clipboard back
				// to this client (registered by peer id in `host_out` so `host_send_*`
				// can find it — registration deferred with the rest, see above).
				let (out_tx, out_rx) = tokio::sync::mpsc::channel::<DataMsg>(256);
				// A clone for on_stream to push the encode summary to the client.
				let stats_out = out_tx.clone();
				// A clone for the file-manager handler's replies (FsEntries / file stream).
				let fs_out = out_tx.clone();

				// Media-over-session: a send-only session handle for the RTP forwarder
				// tasks (they transmit concurrently with the serve loop's recv), and the
				// NACK channel slot the active video forwarder registers itself into.
				let media_tx = session.sender();
				let nack_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>> =
					Arc::new(Mutex::new(None));
				// The running RTP forwarder tasks for this session's CURRENT stream; a
				// re-stream aborts + replaces them (same lifecycle as `procs`).
				let fwd_slot: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
					Arc::new(Mutex::new(Vec::new()));

				// Per-session: hold the screen capture so it can be stopped when this
				// client disconnects. (Input injection is via uinput in `on_input`.)
				#[cfg(target_os = "linux")]
				let cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>> =
					Arc::new(Mutex::new(None));
				// Generation guard for the async portal-capture task: capture::start can
				// sit in the portal dialog for seconds, racing teardown and overlapping
				// re-streams. Every (re-)stream bumps + captures it, teardown bumps it,
				// and a task whose generation went stale STOPS its fresh capture instead
				// of storing it into a dead/superseded session (orphaned portal cast).
				#[cfg(target_os = "linux")]
				let cap_gen: Arc<std::sync::atomic::AtomicU64> = Arc::new(std::sync::atomic::AtomicU64::new(0));

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
								.or_else(|| {
									g.iter().find(|h| h.title.eq_ignore_ascii_case(id.trim()))
								})
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
					incoming.clone(),
					host_out.clone(),
					stop_tx,
					out_tx,
					since_ms,
					sid,
					self_name.clone(),
					#[cfg(windows)]
					native_slot.clone(),
					stats_out.clone(),
					app_h.clone(),
					peer.clone(),
					media_tx.clone(),
					nack_slot.clone(),
					fwd_slot.clone(),
					#[cfg(target_os = "linux")]
					restore_token.clone(),
					#[cfg(target_os = "linux")]
					cap_slot.clone(),
					#[cfg(target_os = "linux")]
					cap_gen.clone(),
				);
				// Route the client's input: controllers into a virtual gamepad, and
				// mouse/keyboard into a uinput desktop injector — both created lazily.
				let on_input = {
					let mut pad: Option<Box<dyn VirtualGamepad>> = None;
					let mut desktop: Option<pulsar_core::input::DesktopInput> = None;
					let mut tried = false;
					// "Sadece izleme" gate: read per-event (cheap map lookup) so the
					// Connections-window toggle takes effect mid-session, sid-guarded
					// against a same-peer reconnection's newer entry.
					let view_active = active.clone();
					let view_peer = peer.clone();
					// Input is injected WITHOUT any pointer rotation. The host video is always presented
					// UPRIGHT (the native capture bakes the display rotation into the frame, or a rotated
					// ffmpeg stream is un-rotated by the client), and Windows SendInput addresses the same
					// logical desktop coordinate space the upright video shows — coords inject as-is.
					// (Rotating here would DOUBLE-correct vs the baked-upright video → 180°-mirrored clicks.)
					move |ev: InputEvent| {
						// View-only: drop EVERY input event for this session (gamepad too)
						// while the host user has control revoked.
						if view_active
							.lock()
							.unwrap()
							.get(&view_peer)
							.map(|ci| ci.sid == sid && ci.view_only)
							.unwrap_or(false)
						{
							return;
						}
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
					let chat_log = tauri::Manager::state::<AppState>(&app_h).chat_log.clone();
					move |text: String| {
						// Backlog first (the connections window may be CLOSED — events
						// broadcast only to live windows), then surface the window: the
						// connections window's message modal is the host chat UI now.
						// Capped: the log lives for the (tray-resident) app's lifetime.
						{
							let mut log = chat_log.lock().unwrap();
							log.push((peer.clone(), text.clone(), false));
							let excess = log.len().saturating_sub(500);
							if excess > 0 {
								log.drain(..excess);
							}
						}
						crate::connections::open_or_update(&app_h, true);
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
				// What this host can ACTUALLY stream, best-first — answers the client's
				// `QueryStreamCaps` so its "auto" codec resolves to what we will really
				// send (the client writes its decoder SDP before the stream starts).
				// Wayland captures via the GStreamer x264 path → H.264 only; otherwise
				// run the same validated encoder/codec resolution the stream start uses
				// (probes are cached, so this is cheap after the first call).
				let stream_caps = {
					let stream_cfg = stream_cfg.clone();
					let app_h = app_h.clone();
					move || {
						use pulsar_core::pipeline::{HwEncoder, VCodec};
						use pulsar_core::service::StreamCaps;
						// Startup-probed caps: derive the reply instantly when available
						// (the background probe at launch ran the SAME validation chain).
						let probed = tauri::Manager::state::<AppState>(&app_h)
							.local_caps
							.lock()
							.unwrap()
							.clone();
						if let Some(lc) = probed {
							// `capture` is a Linux-only module in pulsar-core — gate the
							// call (same pattern as the gst probe below).
							#[cfg(target_os = "linux")]
							let wayland = pulsar_core::capture::is_wayland();
							#[cfg(not(target_os = "linux"))]
							let wayland = false;
							// Wayland encodes ONLY through gst: keep gst-backed families
							// (+ software, which gst's x264 covers too).
							let usable = |e: &crate::caps::EncoderCap| {
								!wayland || e.backend == "gst" || e.id == "software"
							};
							let mut encoders: Vec<String> = lc
								.encoders
								.iter()
								.filter(|e| usable(e))
								.map(|e| e.id.clone())
								.collect();
							if encoders.is_empty() {
								encoders.push("software".to_string());
							}
							let hw_h265 = lc.encoders.iter().any(|e| {
								usable(e)
									&& e.id != "software" && e.codecs.iter().any(|c| c == "h265")
							});
							let codecs = if hw_h265 {
								vec!["h265".to_string(), "h264".to_string()]
							} else {
								vec!["h264".to_string()]
							};
							return StreamCaps {
								codecs,
								encoders,
								features: media_features(),
							};
						}
						// Fallback (probe still running): compute inline, same chain.
						// Validated gst families (Linux): the Wayland path encodes through gst
						// exclusively, and on X11 they cover HW encoders ffmpeg lacks (Orange Pi
						// MPP). hw_h265 = any gst HARDWARE family validated for HEVC.
						#[cfg(target_os = "linux")]
						let gst = crate::process::validated_gst_encoders();
						#[cfg(not(target_os = "linux"))]
						let gst: Vec<(pulsar_core::pipeline::gst::GstEncoder, Vec<VCodec>)> = Vec::new();
						let gst_hw_h265 = gst.iter().any(|(e, codecs)| {
							*e != pulsar_core::pipeline::gst::GstEncoder::X264
								&& codecs.contains(&VCodec::H265)
						});
						// Wayland: gst is the ONLY encode path — caps come from it alone.
						#[cfg(target_os = "linux")]
						let wayland = pulsar_core::capture::is_wayland();
						#[cfg(not(target_os = "linux"))]
						let wayland = false;
						if wayland {
							let mut encoders: Vec<String> =
								gst.iter().map(|(e, _)| e.wire_id().to_string()).collect();
							if encoders.is_empty() {
								encoders.push("software".to_string());
							}
							let codecs = if gst_hw_h265 {
								vec!["h265".to_string(), "h264".to_string()]
							} else {
								vec!["h264".to_string()]
							};
							return StreamCaps {
								codecs,
								encoders,
								features: media_features(),
							};
						}
						let cfg = stream_cfg.lock().unwrap().clone();
						let ffmpeg = crate::process::ffmpeg_bin(&app_h);
						// Encoder backends that really work here (cached one-frame probes),
						// merged with the gst HARDWARE families (same wire vocabulary, so e.g.
						// "rkmpp" appears once whether ffmpeg-rockchip or gst serves it).
						let mut encoders: Vec<String> =
							crate::process::validated_encoders(&ffmpeg, &cfg.vaapi_device)
								.into_iter()
								.map(|e| crate::process::encoder_wire_id(e).to_string())
								.collect();
						for (e, _) in gst
							.iter()
							.filter(|(e, _)| *e != pulsar_core::pipeline::gst::GstEncoder::X264)
						{
							let id = e.wire_id().to_string();
							if !encoders.contains(&id) {
								// HW families ahead of the terminal software entry.
								let pos = encoders.len().saturating_sub(1);
								encoders.insert(pos, id);
							}
						}
						// The encoder the host would pick for its configured preference — drives
						// which codecs we can promise. Software realtime HEVC isn't viable on the
						// hosts we target, so H.265 is offered only from a hardware encoder
						// (ffmpeg-validated or a gst HW family).
						let enc_text = crate::process::encoders_text(&ffmpeg);
						let encoder = pulsar_core::pipeline::resolve(
							crate::process::encoder_from_str(&cfg.encoder),
							&pulsar_core::pipeline::detect(&enc_text),
						);
						#[cfg(not(windows))]
						let encoder = crate::process::resolve_encoder_validated(
							&ffmpeg,
							encoder,
							&enc_text,
							&cfg.vaapi_device,
						);
						let ffmpeg_hw = |c: VCodec| {
							!matches!(encoder, HwEncoder::Software)
								&& crate::process::resolve_codec_validated(
									&ffmpeg,
									encoder,
									c,
									&cfg.vaapi_device,
								) == c
						};
						// Quality-descending; H.265/AV1 only from validated HW encoders.
						let mut codecs = Vec::new();
						if ffmpeg_hw(VCodec::Av1) {
							codecs.push("av1".to_string());
						}
						if ffmpeg_hw(VCodec::H265) || gst_hw_h265 {
							codecs.push("h265".to_string());
						}
						codecs.push("h264".to_string());
						tracing::info!(?codecs, ?encoders, "stream caps reply");
						StreamCaps {
							codecs,
							encoders,
							features: media_features(),
						}
					}
				};
				// The client pushed its identity image: surface it to every window — the
				// connections list renders it next to this peer's id — and remember it in
				// peer_meta so a LATER-opened connections window's snapshot still has it.
				let peer_meta = tauri::Manager::state::<AppState>(&app_h).peer_meta.clone();
				let on_avatar = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let peer_meta = peer_meta.clone();
					move |png: Vec<u8>| {
						let url = crate::avatar::data_url(&png);
						peer_meta
							.lock()
							.unwrap()
							.entry(peer.clone())
							.or_insert((None, None))
							.1 = Some(url.clone());
						let _ = app_h.emit(
							"peer-avatar",
							AvatarPayload {
								peer: peer.clone(),
								data_url: url,
							},
						);
					}
				};
				// Same for the pushed display name (DataMsg::PeerName).
				let on_peer_name = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let peer_meta = peer_meta.clone();
					move |name: String| {
						peer_meta
							.lock()
							.unwrap()
							.entry(peer.clone())
							.or_insert((None, None))
							.0 = Some(name.clone());
						let _ = app_h.emit("peer-name", (peer.clone(), name));
					}
				};
				// NACK requests from the client → the active video forwarder's channel.
				let on_nack = {
					let nack_slot = nack_slot.clone();
					move |seqs: Vec<u16>| {
						if let Some(tx) = nack_slot.lock().unwrap().as_ref() {
							let _ = tx.send(seqs);
						}
					}
				};
				let handlers = DataHandlers {
					outbound: Some(out_rx),
					on_clipboard: Box::new(on_clipboard),
					on_chat: Box::new(on_chat),
					on_file: Box::new(on_file),
					on_audio: Box::new(on_audio),
					on_reverse: Box::new(on_reverse),
					stream_caps: Box::new(stream_caps),
					on_nack: Box::new(on_nack),
					on_avatar: Box::new(on_avatar),
					on_peer_name: Box::new(on_peer_name),
					// File manager: FsList/FsGet from this client, answered through the
					// same outbound queue (HOME-jailed; see fs_browse).
					on_fs: Box::new(crate::fs_browse::make_on_fs(fs_out)),
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
				// Stop the media-over-session forwarder tasks (their session is gone).
				for h in fwd_slot.lock().unwrap().drain(..) {
					h.abort();
				}
				// Stop the native capture thread (releases the NVENC session + DXGI duplication).
				#[cfg(windows)]
				if let Some(h) = native_slot.lock().unwrap().take() {
					h.stop();
				}
				// Drop this session's host-mute request (global owner set in handlers:
				// a same-peer reconnect's newer session keeps the host muted through
				// the OLD session's delayed teardown).
				handlers::release_mute(sid);
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
				let (was_mine, conns_emptied) = {
					let mut g = active.lock().unwrap();
					let mine = g.get(&peer).map(|ci| ci.sid) == Some(sid);
					if mine {
						g.remove(&peer);
					}
					(mine, g.is_empty())
				};
				if was_mine {
					// Identity cache too (sid-guarded the same way): keeping a ~50-70 KB
					// avatar data-URL per ever-seen peer just leaks — a reconnect
					// re-pushes name/avatar anyway.
					peer_meta.lock().unwrap().remove(&peer);
				}
				if conns_emptied {
					crate::connections::close(&app_h);
				}
				// Stop this session's screen capture — closes the portal session so
				// KDE/GNOME stops showing "screen is being shared".
				#[cfg(target_os = "linux")]
				{
					// Bump FIRST: an in-flight capture::start (portal dialog can take
					// seconds) then sees the stale generation and stops its fresh
					// capture instead of storing it into this dead session.
					cap_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

	// Node + serve loop go live BEFORE registering: LAN discovery already
	// announces this host (started pre-register, by design), so direct LAN
	// connects must find a consumer behind `next_incoming()` even when the
	// relay is unreachable — otherwise the documented offline-LAN flow hangs
	// every connecting client at auth.
	*state.node.lock().unwrap() = Some(node.clone());
	*state.serve_task.lock().unwrap() = Some(serve_handle);

	// Register with the relay. If it's unreachable we stay "offline" for the UI
	// (the Err) but keep the node + serve loop + LAN discovery running so
	// same-network devices still appear AND can connect.
	let id = match node.register().await {
		Ok(id) => id,
		Err(e) => {
			tracing::info!(error = %e, "relay unreachable — staying offline, LAN discovery + serving still active");
			return Err(e.to_string());
		}
	};
	tracing::info!(%id, "go_online: registered with relay");
	// Now that we have a relay id, advertise it on the LAN too.
	if let Some(d) = &discovery {
		d.set_id(Some(id)).await;
	}
	Ok(id.grouped())
}
