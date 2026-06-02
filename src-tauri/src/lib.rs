//! Tauri command layer — the bridge between the SvelteKit UI and `pulsar-core`.
//!
//! Each `#[tauri::command]` is a thin async wrapper around the core: load/save
//! config, bind a [`Node`] + register with the relay (get an ID), connect to a
//! peer (P2P → relay), and enumerate controllers. The heavy lifting all lives in
//! `pulsar-core`; this file just marshals JSON to/from the UI.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::config::Config;
use pulsar_core::input::{create_virtual_pad, ControllerHub, GamepadKind, VirtualGamepad};
use pulsar_core::pipeline::{self, CaptureMethod, HwEncoder, StreamPlan, VCodec};
use pulsar_core::proto::DeviceId;
use pulsar_core::service::{
	accept, decode_data, gen_password, need_password, recv_auth, recv_client_auth, recv_host_auth,
	reject, request_games, request_launch, request_stream, send_auth, send_data, send_input,
	send_keepalive, serve_with, ClientAuth, DataHandlers, DataMsg, GameInfo, HostAuth, InputEvent,
	StreamReq,
};
use pulsar_core::{Discovery, Node, Transport};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

mod viewer;

#[derive(Default)]
struct AppState {
	node: Mutex<Option<Arc<Node>>>,
	config: Mutex<Config>,
	/// LAN auto-discovery beacon (announces this device + collects peers on the
	/// local network). Started on `go_online`, replaced on reconnect.
	discovery: Mutex<Option<Arc<Discovery>>>,
	/// Games this host publishes to clients (set from the UI via `publish_games`).
	games: Arc<Mutex<Vec<HostGame>>>,
	/// Host stream settings (resolution/fps/bitrate/encoder/display).
	stream_cfg: Arc<Mutex<StreamCfg>>,
	/// Running ffmpeg/ffplay child processes (so they can be stopped).
	procs: Arc<Mutex<Vec<Child>>>,
	/// One-time password a client must present to connect (shown in the host UI).
	/// Generated on `go_online`; empty means "not online yet".
	password: Arc<Mutex<String>>,
	/// Active outbound remote-play sessions, keyed by play id (this client can be
	/// connected to several hosts at once — one per tab).
	plays: Arc<Mutex<HashMap<u64, PlaySession>>>,
	/// Monotonic id for play sessions.
	next_play: Arc<AtomicU64>,
	/// Pending Allow/Deny approval requests (request id → decision sender),
	/// resolved by the approval popup via `respond_request`.
	pending: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
	/// Monotonic id for approval requests / popup windows.
	next_req: Arc<AtomicU64>,
	/// Pending client-side password prompts (req id → password sender), resolved by
	/// the UI via `submit_password`. `None` payload means the user cancelled.
	pw_pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	/// Monotonic id for client password prompts.
	next_auth: Arc<AtomicU64>,
	/// Incoming (host-side) sessions, keyed by the connected peer's id → a signal to
	/// kick them. Lets the host disconnect a connected device from the UI.
	incoming: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
	/// Host → client side-channel senders, keyed by the connected peer's id. Lets
	/// the host push chat replies / clipboard to a connected client from the UI.
	host_out: Arc<Mutex<HashMap<String, tokio::sync::mpsc::Sender<DataMsg>>>>,
	/// Restore token for the Wayland ScreenCast portal, so the "share your screen"
	/// dialog only appears the first time.
	#[cfg(target_os = "linux")]
	restore_token: Arc<Mutex<Option<String>>>,
}

/// One active outbound remote-play session (one connected-host tab): the local
/// video relay, the input forwarding channel, and a flag held open until stopped.
struct PlaySession {
	viewer: viewer::Viewer,
	input_tx: tokio::sync::mpsc::Sender<InputEvent>,
	/// Side-channel sender (clipboard / chat / file / mic audio → host).
	data_tx: tokio::sync::mpsc::Sender<DataMsg>,
	/// Running mic recorder (`parecord`), if the user enabled the microphone.
	mic: Arc<Mutex<Option<Child>>>,
	running: Arc<AtomicBool>,
}

/// Host-side stream settings pushed from the UI.
#[derive(Clone, Deserialize)]
#[serde(default)]
struct StreamCfg {
	width: u32,
	height: u32,
	fps: u32,
	bitrate_kbps: u32,
	/// `auto` / `nvenc` / `vaapi` / `qsv` / `videotoolbox` / `software`
	encoder: String,
	/// `auto` / `x11grab` / `kmsgrab` / `gdigrab` / `avfoundation`
	capture: String,
	display: String,
	vaapi_device: String,
}

impl Default for StreamCfg {
	fn default() -> Self {
		Self {
			width: 1920,
			height: 1080,
			fps: 60,
			bitrate_kbps: 30_000,
			encoder: "auto".into(),
			capture: "auto".into(),
			display: std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into()),
			vaapi_device: "/dev/dri/renderD128".into(),
		}
	}
}

fn capture_from_str(s: &str) -> CaptureMethod {
	match s {
		"x11grab" => CaptureMethod::X11grab,
		"kmsgrab" => CaptureMethod::Kmsgrab,
		"gdigrab" => CaptureMethod::Gdigrab,
		"avfoundation" => CaptureMethod::AvFoundation,
		_ => CaptureMethod::default_for_os(),
	}
}

fn encoder_from_str(s: &str) -> HwEncoder {
	match s {
		"nvenc" => HwEncoder::Nvenc,
		"vaapi" => HwEncoder::Vaapi,
		"qsv" => HwEncoder::Qsv,
		"videotoolbox" => HwEncoder::VideoToolbox,
		"software" => HwEncoder::Software,
		_ => HwEncoder::Auto,
	}
}

fn codec_from_str(s: &str) -> VCodec {
	match s {
		"h265" => VCodec::H265,
		"av1" => VCodec::Av1,
		_ => VCodec::H264,
	}
}

/// Run `ffmpeg -encoders` (the bundled binary) and return the hardware encoders available.
fn detect_encoders(ffmpeg: &str) -> Vec<HwEncoder> {
	match std::process::Command::new(ffmpeg)
		.args(["-hide_banner", "-encoders"])
		.output()
	{
		Ok(out) => {
			let text = String::from_utf8_lossy(&out.stdout);
			pipeline::detect(&text)
		}
		Err(_) => Vec::new(),
	}
}

/// Spawn a process and remember it so it can be stopped later.
fn spawn_tracked(
	procs: &Arc<Mutex<Vec<Child>>>,
	program: &str,
	args: &[String],
) -> Result<(), String> {
	match std::process::Command::new(program).args(args).spawn() {
		Ok(child) => {
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("{program} başlatılamadı: {e}")),
	}
}

/// A host game/app, as sent from the UI's games store.
#[derive(Clone, Deserialize)]
struct HostGame {
	id: String,
	title: String,
	#[serde(rename = "type")]
	kind: String,
	#[serde(default)]
	path: String,
	#[serde(default)]
	args: String,
	#[serde(default)]
	command: String,
	#[serde(rename = "cmdStart", default)]
	cmd_start: String,
	#[allow(dead_code)]
	#[serde(rename = "cmdStop", default)]
	cmd_stop: String,
}

/// Run a command through the platform shell (fire-and-forget).
fn spawn_shell(cmd: &str) {
	let cmd = cmd.trim();
	if cmd.is_empty() {
		return;
	}
	#[cfg(windows)]
	let _ = std::process::Command::new("cmd").args(["/C", cmd]).spawn();
	#[cfg(not(windows))]
	let _ = std::process::Command::new("sh").args(["-c", cmd]).spawn();
}

/// Launch a host game: its start hook, then the program/command itself.
fn launch_host_game(g: &HostGame) {
	spawn_shell(&g.cmd_start);
	match g.kind.as_str() {
		"program" if !g.path.is_empty() => spawn_shell(&format!("\"{}\" {}", g.path, g.args)),
		"command" if !g.command.is_empty() => spawn_shell(&g.command),
		_ => {}
	}
}

#[derive(Serialize)]
struct ConnInfo {
	transport: String,
	peer: String,
}

#[derive(Serialize)]
struct ControllerInfo {
	kind: String,
	label: String,
}

fn config_path(app: &AppHandle) -> PathBuf {
	app.path()
		.app_config_dir()
		.unwrap_or_else(|_| PathBuf::from("."))
		.join("config.json")
}

/// Resolve the ffmpeg binary the host uses to capture + encode the screen. Pulsar
/// **bundles** ffmpeg so streaming works out of the box — nothing for the user to
/// install, and it works offline. Prefers the bundled copy (the installed app's
/// resource dir, or next to the executable for portable / `tauri dev` builds), and
/// only falls back to a system `ffmpeg` on PATH if no bundled copy is present.
fn ffmpeg_bin(app: &AppHandle) -> String {
	let name = if cfg!(windows) {
		"ffmpeg.exe"
	} else {
		"ffmpeg"
	};
	if let Ok(dir) = app.path().resource_dir() {
		for cand in [dir.join(name), dir.join("resources").join(name)] {
			if cand.is_file() {
				return cand.to_string_lossy().into_owned();
			}
		}
	}
	if let Ok(exe) = std::env::current_exe() {
		if let Some(p) = exe.parent().map(|d| d.join(name)) {
			if p.is_file() {
				return p.to_string_lossy().into_owned();
			}
		}
	}
	"ffmpeg".to_string()
}

/// Resolve a user-entered `host:port` (IP or DNS name) to a socket address.
/// Prefers IPv4 — the relay binds `0.0.0.0`, and `localhost` often resolves to
/// `::1` first, which would never reach an IPv4-only relay.
async fn resolve_relay(addr: &str) -> Option<SocketAddr> {
	if let Ok(parsed) = addr.parse::<SocketAddr>() {
		return Some(parsed);
	}
	let resolved: Vec<SocketAddr> = tokio::net::lookup_host(addr).await.ok()?.collect();
	resolved
		.iter()
		.copied()
		.find(SocketAddr::is_ipv4)
		.or_else(|| resolved.first().copied())
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
	tracing::info!("get_config invoked (frontend JS is running)");
	Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
async fn set_config(
	app: AppHandle,
	state: State<'_, AppState>,
	config: Config,
) -> Result<(), String> {
	*state.config.lock().unwrap() = config.clone();
	config.save(config_path(&app)).map_err(|e| e.to_string())
}

/// An event about a client session, emitted to the host UI as `session`.
#[derive(Clone, Serialize)]
struct SessionEvent {
	kind: String,
	peer: String,
	detail: String,
}

/// A side-channel text payload (clipboard / chat) tagged with the peer it came
/// from. Emitted to the host UI (`clipboard` / `host-chat`) or, with `peer`
/// holding the play id, to the client UI (`data-clip` / `chat-msg`).
#[derive(Clone, Serialize)]
struct DataPayload {
	peer: String,
	text: String,
}

/// Emitted to the host UI (`file-recv`) when a file transfer from a client
/// finishes (or fails the gap check).
#[derive(Clone, Serialize)]
struct FilePayload {
	peer: String,
	name: String,
	bytes: u64,
	ok: bool,
}

/// Strip path separators so a peer can't write outside the received-files dir.
fn sanitize_filename(name: &str) -> String {
	let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
	let cleaned: String = base
		.chars()
		.filter(|c| !matches!(c, '\0'..='\u{1f}'))
		.collect();
	if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
		"dosya".into()
	} else {
		cleaned
	}
}

/// Directory incoming files are written to (`~/Pulsar Alınanlar`, created on
/// demand). Falls back to the system temp dir if `$HOME` is unset.
fn received_dir() -> PathBuf {
	let base = std::env::var("HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|_| std::env::temp_dir());
	let dir = base.join("Pulsar Alınanlar");
	let _ = std::fs::create_dir_all(&dir);
	dir
}

/// Write received bytes to the received-files dir, avoiding clobbering an
/// existing file by suffixing ` (n)`. Returns the final path on success.
fn save_received_file(name: &str, data: &[u8]) -> Option<PathBuf> {
	let dir = received_dir();
	let mut path = dir.join(name);
	let (stem, ext) = match name.rsplit_once('.') {
		Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
		_ => (name.to_string(), String::new()),
	};
	let mut n = 1;
	while path.exists() {
		path = dir.join(format!("{stem} ({n}){ext}"));
		n += 1;
	}
	std::fs::write(&path, data).ok().map(|_| path)
}

/// Raw-PCM audio format used for the mic side channel (s16le, 48kHz, mono).
const AUDIO_ARGS: &[&str] = &["--rate=48000", "--channels=1", "--format=s16le", "--raw"];

/// Spawn a real-time PCM player on the host (`paplay`/`pw-cat`/`aplay`), returning
/// the child + its stdin to pipe frames into. `None` if no player is available.
fn spawn_audio_player() -> Option<(Child, std::process::ChildStdin)> {
	let candidates: [(&str, Vec<String>); 3] = [
		("paplay", AUDIO_ARGS.iter().map(|s| s.to_string()).collect()),
		(
			"pw-cat",
			[
				"--playback",
				"--rate",
				"48000",
				"--channels",
				"1",
				"--format",
				"s16",
				"-",
			]
			.iter()
			.map(|s| s.to_string())
			.collect(),
		),
		(
			"aplay",
			["-q", "-f", "S16_LE", "-r", "48000", "-c", "1"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
	];
	for (prog, args) in candidates {
		if let Ok(mut child) = std::process::Command::new(prog)
			.args(&args)
			.stdin(Stdio::piped())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
		{
			if let Some(stdin) = child.stdin.take() {
				return Some((child, stdin));
			}
			let _ = child.kill();
		}
	}
	None
}

/// Spawn a mic recorder (`parecord`/`pw-record`/`arecord`) producing raw PCM on
/// stdout. `None` if no recorder is available.
fn spawn_mic_recorder() -> Option<Child> {
	let candidates: [(&str, Vec<String>); 3] = [
		(
			"parecord",
			AUDIO_ARGS.iter().map(|s| s.to_string()).collect(),
		),
		(
			"pw-record",
			["--rate", "48000", "--channels", "1", "--format", "s16", "-"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
		(
			"arecord",
			["-q", "-f", "S16_LE", "-r", "48000", "-c", "1"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
	];
	for (prog, args) in candidates {
		if let Ok(child) = std::process::Command::new(prog)
			.args(&args)
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
		{
			return Some(child);
		}
	}
	None
}

/// Emitted to the CLIENT UI (`auth-prompt`) when a host asks for a password — the
/// UI shows a prompt and replies via `submit_password(req, ...)`.
#[derive(Clone, Serialize)]
struct AuthPrompt {
	req: u64,
	peer: String,
}

/// Spawn the Allow/Deny popup as a separate, focused, always-on-top window that
/// requests the user's attention (they may be in another app).
fn open_approval_window(app: &AppHandle, id: u64, peer: &str, pw_status: &str) {
	let peer_q: String = peer.chars().filter(|c| c.is_ascii_digit()).collect();
	// Inject the request details before the page loads (more reliable than a query
	// string surviving the asset URL).
	let init = format!("window.__APPROVE__={{id:{id},peer:\"{peer_q}\",pw:\"{pw_status}\"}};");
	match WebviewWindowBuilder::new(
		app,
		format!("approve-{id}"),
		WebviewUrl::App("index.html".into()),
	)
	.initialization_script(&init)
	.title("Pulsar — Bağlantı isteği")
	.inner_size(400.0, 300.0)
	.resizable(false)
	.always_on_top(true)
	.center()
	.focused(true)
	.build()
	{
		Ok(win) => {
			let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
		}
		Err(e) => tracing::warn!(%e, "approval window failed to open"),
	}
}

/// The approval popup's Allow/Deny buttons call this to resolve the request.
#[tauri::command]
async fn respond_request(state: State<'_, AppState>, id: u64, allow: bool) -> Result<(), String> {
	if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
		let _ = tx.send(allow);
	}
	Ok(())
}

/// Host: open the Allow/Deny popup AND, at the same time, race it against a correct
/// password arriving over the session. Accept on whichever lands first — so the
/// host can approve passwordlessly while the client is still being asked for one.
async fn race_host_auth(
	session: &mut pulsar_core::Session,
	app: &AppHandle,
	pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
	next_req: &Arc<AtomicU64>,
	peer: &str,
	host_pw: &str,
) -> bool {
	let id = next_req.fetch_add(1, Ordering::SeqCst);
	let (tx, mut rx) = oneshot::channel::<bool>();
	pending.lock().unwrap().insert(id, tx);
	let _ = app.emit(
		"session",
		SessionEvent {
			kind: "request".into(),
			peer: peer.into(),
			detail: "wait".into(),
		},
	);
	open_approval_window(app, id, peer, "wait");

	let result = loop {
		tokio::select! {
			biased;
			d = &mut rx => break matches!(d, Ok(true)),
			msg = recv_client_auth(session) => match msg {
				ClientAuth::Password(pw) => {
					if !host_pw.is_empty() && pw == host_pw {
						break true; // correct password → accept
					}
					let _ = need_password(session).await; // wrong → ask client to retry
				}
				ClientAuth::Keepalive => {}
				ClientAuth::Gone => break false,
			}
		}
	};
	pending.lock().unwrap().remove(&id);
	if let Some(win) = app.get_webview_window(&format!("approve-{id}")) {
		let _ = win.close();
	}
	result
}

/// Client: open a password prompt on the UI; returns the receiver for the answer.
fn open_pw_prompt(
	app: &AppHandle,
	pw_pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	next_auth: &Arc<AtomicU64>,
	peer: &str,
) -> (u64, oneshot::Receiver<Option<String>>) {
	let id = next_auth.fetch_add(1, Ordering::SeqCst);
	let (tx, rx) = oneshot::channel::<Option<String>>();
	pw_pending.lock().unwrap().insert(id, tx);
	let _ = app.emit(
		"auth-prompt",
		AuthPrompt {
			req: id,
			peer: peer.into(),
		},
	);
	(id, rx)
}

/// Client: authenticate over the session. Sends an empty request first (which makes
/// the host show its Allow/Deny popup + ask us to prompt), then races the host's
/// approval against the user typing the password. Returns `Ok(true)` if accepted.
async fn client_authenticate(
	sess: &mut pulsar_core::Session,
	app: &AppHandle,
	pw_pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	next_auth: &Arc<AtomicU64>,
	peer: &str,
) -> Result<bool, String> {
	send_auth(sess, "").await.map_err(|e| e.to_string())?;
	let mut pw_rx: Option<oneshot::Receiver<Option<String>>> = None;
	let mut cur_id: u64 = 0;
	let cleanup = |id: u64| {
		pw_pending.lock().unwrap().remove(&id);
	};
	loop {
		match pw_rx.take() {
			// Waiting for both the host's reply and the user's password.
			Some(mut rx) => {
				tokio::select! {
					biased;
					pw = &mut rx => {
						cleanup(cur_id);
						match pw {
							Ok(Some(p)) => send_auth(sess, &p).await.map_err(|e| e.to_string())?,
							_ => return Ok(false), // user cancelled
						}
					}
					out = recv_host_auth(sess) => match out {
						HostAuth::Ok => { cleanup(cur_id); return Ok(true); }
						HostAuth::Denied | HostAuth::Gone => { cleanup(cur_id); return Ok(false); }
						HostAuth::NeedPassword => {
							cleanup(cur_id);
							let (id, rx2) = open_pw_prompt(app, pw_pending, next_auth, peer);
							cur_id = id;
							pw_rx = Some(rx2);
						}
						HostAuth::Other => pw_rx = Some(rx), // keepalive: keep waiting
					}
				}
			}
			// Not prompting yet: just read the host's reply.
			None => match recv_host_auth(sess).await {
				HostAuth::Ok => return Ok(true),
				HostAuth::Denied | HostAuth::Gone => return Ok(false),
				HostAuth::NeedPassword => {
					let (id, rx) = open_pw_prompt(app, pw_pending, next_auth, peer);
					cur_id = id;
					pw_rx = Some(rx);
				}
				HostAuth::Other => {}
			},
		}
	}
}

/// The client password prompt replies here (`null` = cancelled).
#[tauri::command]
async fn submit_password(
	state: State<'_, AppState>,
	req: u64,
	password: Option<String>,
) -> Result<(), String> {
	if let Some(tx) = state.pw_pending.lock().unwrap().remove(&req) {
		let _ = tx.send(password);
	}
	Ok(())
}

/// Host: forcibly disconnect a connected client by its peer id.
#[tauri::command]
async fn disconnect_peer(state: State<'_, AppState>, peer: String) -> Result<(), String> {
	if let Some(tx) = state.incoming.lock().unwrap().remove(&peer) {
		let _ = tx.send(());
	}
	Ok(())
}

/// Bind the node and register with the configured relay; returns this device's
/// grouped ID. Fails (so the UI shows "offline") when the relay is unreachable.
#[tauri::command]
async fn go_online(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
	let cfg = state.config.lock().unwrap().clone();
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
	let node = Node::bind_named(local, relay, cfg.network_mode, announce_name.clone())
		.await
		.map_err(|e| e.to_string())?;

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
		gen_password()
	} else {
		String::new()
	};
	*state.password.lock().unwrap() = password;

	// Host role: serve published games, start streams, and surface activity.
	let games = state.games.clone();
	let stream_cfg = state.stream_cfg.clone();
	let procs = state.procs.clone();
	// Read the live password per connection (so `new_password` takes effect).
	let password_store = state.password.clone();
	let pending = state.pending.clone();
	let next_req = state.next_req.clone();
	let incoming = state.incoming.clone();
	let host_out = state.host_out.clone();
	#[cfg(target_os = "linux")]
	let restore_token = state.restore_token.clone();
	let serve_node = node.clone();
	let app_h = app.clone();
	tokio::spawn(async move {
		while let Some(session) = serve_node.next_incoming().await {
			let games = games.clone();
			let stream_cfg = stream_cfg.clone();
			let procs = procs.clone();
			let password_store = password_store.clone();
			let pending = pending.clone();
			let next_req = next_req.clone();
			let incoming = incoming.clone();
			let host_out = host_out.clone();
			#[cfg(target_os = "linux")]
			let restore_token = restore_token.clone();
			let app_h = app_h.clone();
			let peer = session.peer().grouped();
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
						race_host_auth(&mut session, &app_h, &pending, &next_req, &peer, &host_pw)
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
				// Allow the host UI to kick this client (`disconnect_peer`).
				let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
				incoming.lock().unwrap().insert(peer.clone(), stop_tx);

				// Side channels: a queue the host UI drains to push chat/clipboard back
				// to this client (registered by peer id so `host_send_*` can find it).
				let (out_tx, out_rx) = tokio::sync::mpsc::channel::<DataMsg>(256);
				host_out.lock().unwrap().insert(peer.clone(), out_tx);

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
						let found = games.lock().unwrap().iter().find(|h| h.id == id).cloned();
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
				let on_stream = {
					let stream_cfg = stream_cfg.clone();
					let procs = procs.clone();
					let app_h = app_h.clone();
					let peer = peer.clone();
					#[cfg(target_os = "linux")]
					let restore_token = restore_token.clone();
					#[cfg(target_os = "linux")]
					let cap_slot = cap_slot.clone();
					move |req: StreamReq, addr: SocketAddr| {
						let cfg = stream_cfg.lock().unwrap().clone();

						// Wayland: x11grab of rootless Xwayland is black, so capture the
						// real screen (and inject input) through the desktop portals.
						#[cfg(target_os = "linux")]
						if pulsar_core::capture::is_wayland() {
							let ip = addr.ip().to_string();
							let (port, codec) = (req.port, req.codec.clone());
							let (bitrate, fps) = (cfg.bitrate_kbps, cfg.fps);
							let token = restore_token.lock().unwrap().clone();
							let restore_token = restore_token.clone();
							let cap_slot = cap_slot.clone();
							let app_h = app_h.clone();
							let peer = peer.clone();
							tokio::spawn(async move {
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
										let _ = app_h.emit(
											"session",
											SessionEvent {
												kind: "stream".into(),
												peer,
												detail: "Wayland · ekran + kontrol".into(),
											},
										);
									}
									Err(e) => {
										let _ = app_h.emit(
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
							return;
						}

						let ffmpeg = ffmpeg_bin(&app_h);
						let encoder = pipeline::resolve(
							encoder_from_str(&cfg.encoder),
							&detect_encoders(&ffmpeg),
						);
						let plan = StreamPlan {
							encoder,
							codec: codec_from_str(&req.codec),
							width: cfg.width,
							height: cfg.height,
							fps: cfg.fps,
							bitrate_kbps: cfg.bitrate_kbps,
							capture: capture_from_str(&cfg.capture),
							display: cfg.display.clone(),
							vaapi_device: cfg.vaapi_device.clone(),
							dest: format!("rtp://{}:{}", addr.ip(), req.port),
						};
						// Use the bundled ffmpeg rather than relying on PATH. For the NVENC
						// `prime-run` wrapper, swap its inner "ffmpeg" arg for the bundled path.
						let (program, mut args) = pipeline::encode_command(&plan);
						let (program, args) = if program == "ffmpeg" {
							(ffmpeg.clone(), args)
						} else {
							if let Some(first) = args.first_mut() {
								if first == "ffmpeg" {
									*first = ffmpeg.clone();
								}
							}
							(program, args)
						};
						let started = spawn_tracked(&procs, &program, &args).is_ok();
						let _ = app_h.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer: peer.clone(),
								detail: format!("{} · {}p", encoder.label(), cfg.height)
									+ if started { "" } else { " (ffmpeg başlamadı)" },
							},
						);
					}
				};
				// Route the client's input: controllers into a virtual gamepad, and
				// mouse/keyboard into a uinput desktop injector — both created lazily.
				let on_input = {
					let mut pad: Option<Box<dyn VirtualGamepad>> = None;
					let mut desktop: Option<pulsar_core::input::DesktopInput> = None;
					let mut tried = false;
					move |ev: InputEvent| match ev {
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
									InputEvent::PointerMotion { x, y } => d.pointer(x, y),
									InputEvent::PointerButton { button, down } => {
										d.button(button, down)
									}
									InputEvent::Scroll { dx, dy } => d.scroll(dx, dy),
									InputEvent::Key { code, down } => d.key(code, down),
									InputEvent::Gamepad(_) => {}
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
				let on_file = {
					let app_h = app_h.clone();
					let peer = peer.clone();
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
				};
				let on_audio = {
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
				};
				let handlers = DataHandlers {
					outbound: Some(out_rx),
					on_clipboard: Box::new(on_clipboard),
					on_chat: Box::new(on_chat),
					on_file: Box::new(on_file),
					on_audio: Box::new(on_audio),
				};
				tokio::select! {
					_ = serve_with(session, provider, on_launch, on_stream, on_input, handlers) => {}
					_ = &mut stop_rx => {} // host kicked this client from the UI
				}
				incoming.lock().unwrap().remove(&peer);
				host_out.lock().unwrap().remove(&peer);
				tracing::info!(%peer, "session disconnected");
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
	Ok(id.grouped())
}

/// Available hardware encoders detected via ffmpeg (kebab-case values).
#[tauri::command]
async fn available_encoders(app: AppHandle) -> Vec<String> {
	detect_encoders(&ffmpeg_bin(&app))
		.into_iter()
		.map(|e| {
			match e {
				HwEncoder::Nvenc => "nvenc",
				HwEncoder::Vaapi => "vaapi",
				HwEncoder::Qsv => "qsv",
				HwEncoder::VideoToolbox => "videotoolbox",
				HwEncoder::Software => "software",
				HwEncoder::Auto => "auto",
			}
			.to_string()
		})
		.collect()
}

/// Set the host's stream settings (resolution/fps/bitrate/encoder/display).
#[tauri::command]
async fn set_stream_settings(state: State<'_, AppState>, cfg: StreamCfg) -> Result<(), String> {
	*state.stream_cfg.lock().unwrap() = cfg;
	Ok(())
}

/// Returned to the UI after a successful connect: the play id (for this tab), how
/// the link was made, and the loopback WebSocket port the webview renders from.
#[derive(Serialize)]
struct PlayInfo {
	id: u64,
	transport: String,
	ws_port: u16,
	/// True when the host is this same machine (loopback P2P) — control would be a
	/// cursor feedback loop, so the UI disables it.
	local: bool,
}

/// Client: connect to a host, start receiving its video (embedded WebCodecs
/// viewer, no separate window), and (optionally) stream our controller input —
/// all over a single session held open until `stop_stream`. Asks the host to
/// launch `game_id` (if any) and stream RTP/H.264 to our local viewer.
#[tauri::command]
async fn start_remote_play(
	app: AppHandle,
	state: State<'_, AppState>,
	target: String,
	game_id: String,
	_port: u16,
	codec: String,
	encoder: String,
	gamepad: bool,
) -> Result<PlayInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let target_id = DeviceId::parse(&target).ok_or("geçersiz kimlik")?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());

	let mut sess = node.connect(target_id).await.map_err(|e| e.to_string())?;
	if !client_authenticate(
		&mut sess,
		&app,
		&pw_pending,
		&next_auth,
		&target_id.grouped(),
	)
	.await?
	{
		return Err("Bağlantı reddedildi.".into());
	}
	let transport = match sess.transport() {
		Transport::Direct => "direct",
		Transport::Relay => "relay",
	}
	.to_string();
	// Same machine? (loopback P2P) → control would feed back, so flag it.
	let local = matches!(sess.transport(), Transport::Direct)
		&& sess
			.peer_addr()
			.await
			.map(|a| a.ip().is_loopback())
			.unwrap_or(false);

	// Start the local RTP→WebSocket viewer only after auth (don't bind ports for a
	// rejected connection). The host streams to the viewer's ephemeral UDP port.
	let view = viewer::start()
		.await
		.map_err(|e| format!("video alıcı başlatılamadı: {e}"))?;
	let ws_port = view.ws_port;
	let media_port = view.media_port;

	if !game_id.is_empty() {
		request_launch(&mut sess, &game_id)
			.await
			.map_err(|e| e.to_string())?;
	}
	let req = StreamReq {
		port: media_port,
		codec,
		encoder,
	};
	request_stream(&mut sess, &req)
		.await
		.map_err(|e| e.to_string())?;

	// Register this play session (one per connected-host tab).
	let id = state.next_play.fetch_add(1, Ordering::SeqCst);
	let running = Arc::new(AtomicBool::new(true));
	let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<InputEvent>(256);

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
	let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<DataMsg>(512);
	let mic = Arc::new(Mutex::new(None));

	// Hold the control session open full-duplex: forward input + side-channel data,
	// keepalive every ~2s (UDP has no disconnect signal), and receive the host's
	// chat/clipboard pushes — surfacing them to the UI.
	let send_flag = running.clone();
	let app_ev = app.clone();
	tokio::spawn(async move {
		let mut keep = tokio::time::interval(std::time::Duration::from_secs(2));
		keep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
		loop {
			if !send_flag.load(Ordering::SeqCst) {
				break;
			}
			tokio::select! {
				ev = input_rx.recv() => match ev {
					Some(ev) => if send_input(&mut sess, &ev).await.is_err() { break },
					None => break,
				},
				d = data_rx.recv() => match d {
					Some(dm) => if send_data(&sess, &dm).await.is_err() { break },
					None => {}
				},
				inbound = sess.recv() => match inbound {
					Some(bytes) => if let Some(dm) = decode_data(&bytes) {
						match dm {
							DataMsg::Clipboard(text) => {
								let _ = app_ev.emit("data-clip", DataPayload { peer: id.to_string(), text });
							}
							DataMsg::Chat(text) => {
								let _ = app_ev.emit("chat-msg", DataPayload { peer: id.to_string(), text });
							}
							_ => {}
						}
					},
					None => break, // host closed the session
				},
				_ = keep.tick() => if send_keepalive(&mut sess).await.is_err() { break },
			}
		}
		drop(sess);
	});

	state.plays.lock().unwrap().insert(
		id,
		PlaySession {
			viewer: view,
			input_tx,
			data_tx,
			mic,
			running,
		},
	);
	Ok(PlayInfo {
		id,
		transport,
		ws_port,
		local,
	})
}

/// Stop one remote-play session (tab): closes its control session (the host sees a
/// disconnect) and tears down its video relay.
#[tauri::command]
async fn stop_stream(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	if let Some(play) = state.plays.lock().unwrap().remove(&id) {
		play.running.store(false, Ordering::SeqCst);
		play.viewer.stop();
		if let Some(mut mic) = play.mic.lock().unwrap().take() {
			let _ = mic.kill();
		}
	}
	Ok(())
}

/// Look up a play session's side-channel sender (clipboard/chat/file/audio).
fn data_sender(state: &AppState, id: u64) -> Result<tokio::sync::mpsc::Sender<DataMsg>, String> {
	state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.data_tx.clone())
		.ok_or_else(|| "oturum bulunamadı".into())
}

/// Client → host: push clipboard text (read from the webview) to the remote.
#[tauri::command]
async fn send_clipboard(state: State<'_, AppState>, id: u64, text: String) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Clipboard(text))
		.await
		.map_err(|_| "pano gönderilemedi".to_string())
}

/// Client → host: send a chat line.
#[tauri::command]
async fn send_chat(state: State<'_, AppState>, id: u64, text: String) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Chat(text))
		.await
		.map_err(|_| "mesaj gönderilemedi".to_string())
}

/// Host → client: reply to a connected peer's chat.
#[tauri::command]
async fn host_send_chat(
	state: State<'_, AppState>,
	peer: String,
	text: String,
) -> Result<(), String> {
	let tx = state.host_out.lock().unwrap().get(&peer).cloned();
	tx.ok_or_else(|| "cihaz bağlı değil".to_string())?
		.send(DataMsg::Chat(text))
		.await
		.map_err(|_| "mesaj gönderilemedi".to_string())
}

/// Client → host: send a file (chunked over the session, saved on the host).
#[tauri::command]
async fn send_file(
	state: State<'_, AppState>,
	id: u64,
	name: String,
	data: Vec<u8>,
) -> Result<(), String> {
	const CHUNK: usize = 16 * 1024;
	let tx = data_sender(&state, id)?;
	let chunks = data.len().div_ceil(CHUNK) as u32;
	tx.send(DataMsg::FileBegin {
		name,
		size: data.len() as u64,
		chunks,
	})
	.await
	.map_err(|_| "dosya gönderilemedi".to_string())?;
	for (i, ch) in data.chunks(CHUNK).enumerate() {
		tx.send(DataMsg::FileChunk {
			index: i as u32,
			data: ch.to_vec(),
		})
		.await
		.map_err(|_| "dosya gönderilemedi".to_string())?;
	}
	tx.send(DataMsg::FileEnd)
		.await
		.map_err(|_| "dosya gönderilemedi".to_string())
}

/// Client: start streaming the microphone to the host (raw PCM over the session).
#[tauri::command]
async fn mic_start(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	let (tx, mic_slot) = {
		let plays = state.plays.lock().unwrap();
		let p = plays.get(&id).ok_or("oturum bulunamadı")?;
		(p.data_tx.clone(), p.mic.clone())
	};
	if mic_slot.lock().unwrap().is_some() {
		return Ok(()); // already on
	}
	let mut child =
		spawn_mic_recorder().ok_or("mikrofon kaydedici bulunamadı (parecord/pw-record/arecord)")?;
	let stdout = child.stdout.take().ok_or("mikrofon çıkışı alınamadı")?;
	*mic_slot.lock().unwrap() = Some(child);
	// Blocking read loop on a dedicated thread; killing the child ends it.
	std::thread::spawn(move || {
		let mut rdr = stdout;
		let mut buf = [0u8; 3840]; // ~20 ms @ 48 kHz mono s16
		loop {
			match rdr.read(&mut buf) {
				Ok(0) | Err(_) => break,
				Ok(n) => {
					if tx.blocking_send(DataMsg::Audio(buf[..n].to_vec())).is_err() {
						break;
					}
				}
			}
		}
	});
	Ok(())
}

/// Client: stop streaming the microphone.
#[tauri::command]
async fn mic_stop(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	let (tx, mic_slot) = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else {
			return Ok(());
		};
		(p.data_tx.clone(), p.mic.clone())
	};
	if let Some(mut c) = mic_slot.lock().unwrap().take() {
		let _ = c.kill();
	}
	let _ = tx.send(DataMsg::AudioEnd).await;
	Ok(())
}

/// Forward an input event to a specific play session's host.
fn forward(state: &AppState, id: u64, ev: InputEvent) {
	if let Some(play) = state.plays.lock().unwrap().get(&id) {
		let _ = play.input_tx.try_send(ev);
	}
}

/// Client: forward absolute pointer motion (normalized 0..1) to the host.
#[tauri::command]
async fn input_pointer(state: State<'_, AppState>, id: u64, x: f64, y: f64) -> Result<(), String> {
	forward(&state, id, InputEvent::PointerMotion { x, y });
	Ok(())
}

/// Client: forward a mouse button (0=left, 1=right, 2=middle) press/release.
#[tauri::command]
async fn input_button(
	state: State<'_, AppState>,
	id: u64,
	button: u8,
	down: bool,
) -> Result<(), String> {
	forward(&state, id, InputEvent::PointerButton { button, down });
	Ok(())
}

/// Client: forward a scroll delta.
#[tauri::command]
async fn input_scroll(state: State<'_, AppState>, id: u64, dx: f64, dy: f64) -> Result<(), String> {
	forward(&state, id, InputEvent::Scroll { dx, dy });
	Ok(())
}

/// Client: forward a keyboard evdev keycode press/release.
#[tauri::command]
async fn input_key(
	state: State<'_, AppState>,
	id: u64,
	code: u32,
	down: bool,
) -> Result<(), String> {
	forward(&state, id, InputEvent::Key { code, down });
	Ok(())
}

/// The one-time password a client must enter to connect to this host (empty until
/// online, or when unattended access is enabled).
#[tauri::command]
async fn session_password(state: State<'_, AppState>) -> Result<String, String> {
	Ok(state.password.lock().unwrap().clone())
}

/// Generate a fresh one-time password, invalidating the previous one.
#[tauri::command]
async fn new_password(state: State<'_, AppState>) -> Result<String, String> {
	let pw = gen_password();
	*state.password.lock().unwrap() = pw.clone();
	Ok(pw)
}

/// Publish the host's games so connecting clients can list/launch them.
#[tauri::command]
async fn publish_games(state: State<'_, AppState>, games: Vec<HostGame>) -> Result<(), String> {
	*state.games.lock().unwrap() = games;
	Ok(())
}

/// Client: list the games published by the host at `target`.
#[tauri::command]
async fn list_remote_games(
	app: AppHandle,
	state: State<'_, AppState>,
	target: String,
) -> Result<Vec<GameInfo>, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let target_id = DeviceId::parse(&target).ok_or("geçersiz kimlik")?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let mut sess = node.connect(target_id).await.map_err(|e| e.to_string())?;
	if !client_authenticate(
		&mut sess,
		&app,
		&pw_pending,
		&next_auth,
		&target_id.grouped(),
	)
	.await?
	{
		return Err("Bağlantı reddedildi.".into());
	}
	request_games(&mut sess).await.map_err(|e| e.to_string())
}

/// Client: ask the host at `target` to launch one of its games.
#[tauri::command]
async fn launch_remote_game(
	app: AppHandle,
	state: State<'_, AppState>,
	target: String,
	game_id: String,
) -> Result<(), String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let target_id = DeviceId::parse(&target).ok_or("geçersiz kimlik")?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let mut sess = node.connect(target_id).await.map_err(|e| e.to_string())?;
	if !client_authenticate(
		&mut sess,
		&app,
		&pw_pending,
		&next_auth,
		&target_id.grouped(),
	)
	.await?
	{
		return Err("Bağlantı reddedildi.".into());
	}
	request_launch(&mut sess, &game_id)
		.await
		.map_err(|e| e.to_string())
}

#[tauri::command]
async fn connect(state: State<'_, AppState>, target: String) -> Result<ConnInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let target_id = DeviceId::parse(&target).ok_or("geçersiz kimlik")?;
	let sess = node.connect(target_id).await.map_err(|e| e.to_string())?;
	let transport = match sess.transport() {
		Transport::Direct => "direct",
		Transport::Relay => "relay",
	};
	Ok(ConnInfo {
		transport: transport.to_string(),
		peer: target_id.grouped(),
	})
}

/// A Pulsar device found on the local network (via the multicast beacon).
#[derive(Serialize)]
struct LanDevice {
	/// Grouped relay id (e.g. `482 913 056`), or empty if the peer is relay-less.
	id: String,
	/// Whether `id` is usable to connect via the normal flow.
	has_id: bool,
	name: String,
	/// `ip:port` the peer announced.
	addr: String,
	/// `windows` / `linux` / `macos`.
	platform: String,
}

/// Devices auto-discovered on the local network. Empty until `go_online` starts
/// the beacon. Polled by the Devices screen.
#[tauri::command]
async fn lan_devices(state: State<'_, AppState>) -> Result<Vec<LanDevice>, String> {
	let disc = state.discovery.lock().unwrap().clone();
	let Some(disc) = disc else {
		return Ok(Vec::new());
	};
	Ok(disc
		.peers()
		.await
		.into_iter()
		.map(|p| LanDevice {
			id: p.id.map(|d| d.grouped()).unwrap_or_default(),
			has_id: p.id.is_some(),
			name: p.name,
			addr: p.addr.to_string(),
			platform: p.platform,
		})
		.collect())
}

/// Detected physical controllers (DS3/DS4/DS5/Xbox/standard). Best-effort: an
/// empty list when no gamepad subsystem is available.
#[tauri::command]
async fn controllers() -> Result<Vec<ControllerInfo>, String> {
	match ControllerHub::new() {
		Ok(mut hub) => Ok(hub
			.snapshot()
			.into_iter()
			.map(|(kind, _state)| ControllerInfo {
				kind: format!("{kind:?}"),
				label: kind.label().to_string(),
			})
			.collect()),
		Err(_) => Ok(Vec::new()),
	}
}

#[derive(Serialize)]
struct ScannedApp {
	name: String,
	path: String,
}

/// Scan a folder (one level deep) for launchable apps so the Oyunlar tab can list
/// them. Cross-platform: Windows matches common executable extensions, Unix the
/// executable bit.
#[tauri::command]
async fn scan_folder(path: String) -> Result<Vec<ScannedApp>, String> {
	let dir = std::path::PathBuf::from(&path);
	if !dir.is_dir() {
		return Err(format!("klasör bulunamadı: {path}"));
	}
	let mut apps = Vec::new();
	for entry in std::fs::read_dir(&dir)
		.map_err(|e| e.to_string())?
		.flatten()
	{
		let p = entry.path();
		if p.is_file() && is_executable(&p) {
			let name = p
				.file_stem()
				.and_then(|s| s.to_str())
				.unwrap_or("app")
				.to_string();
			apps.push(ScannedApp {
				name,
				path: p.to_string_lossy().into_owned(),
			});
		}
	}
	apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
	Ok(apps)
}

#[cfg(windows)]
fn is_executable(p: &std::path::Path) -> bool {
	matches!(
		p.extension()
			.and_then(|e| e.to_str())
			.map(|e| e.to_lowercase())
			.as_deref(),
		Some("exe") | Some("bat") | Some("cmd") | Some("lnk")
	)
}

#[cfg(not(windows))]
fn is_executable(p: &std::path::Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	std::fs::metadata(p)
		.map(|m| m.permissions().mode() & 0o111 != 0)
		.unwrap_or(false)
}

/// Run a host-side prep command (e.g. a per-game session start/stop hook).
/// Fire-and-forget; runs through the platform shell.
#[tauri::command]
async fn run_command(command: String) -> Result<(), String> {
	let command = command.trim().to_string();
	if command.is_empty() {
		return Ok(());
	}
	#[cfg(windows)]
	let spawn = std::process::Command::new("cmd")
		.args(["/C", &command])
		.spawn();
	#[cfg(not(windows))]
	let spawn = std::process::Command::new("sh")
		.args(["-c", &command])
		.spawn();
	spawn.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	tracing_subscriber::fmt()
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
		)
		.init();
	tracing::info!("Pulsar starting");

	tauri::Builder::default()
		.manage(AppState::default())
		.setup(|app| {
			// Load persisted config (relay endpoint, network mode, etc.).
			let cfg = Config::load(config_path(app.handle()));
			tracing::info!(relay = %cfg.relay, "config loaded");
			*app.state::<AppState>().config.lock().unwrap() = cfg;
			Ok(())
		})
		.invoke_handler(tauri::generate_handler![
			get_config,
			set_config,
			go_online,
			connect,
			lan_devices,
			controllers,
			scan_folder,
			run_command,
			publish_games,
			list_remote_games,
			launch_remote_game,
			available_encoders,
			set_stream_settings,
			start_remote_play,
			stop_stream,
			session_password,
			new_password,
			respond_request,
			submit_password,
			disconnect_peer,
			input_pointer,
			input_button,
			input_scroll,
			input_key,
			send_clipboard,
			send_chat,
			host_send_chat,
			send_file,
			mic_start,
			mic_stop
		])
		.run(tauri::generate_context!())
		.expect("error while running Pulsar");
}
