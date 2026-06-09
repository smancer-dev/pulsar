//! Assorted Tauri commands: config load/save, encoder enumeration, host stream
//! settings, the connect/list/launch flows, LAN discovery, controllers, local IP,
//! Steam/folder scanning, host-side prep commands, and the password helpers.

use pulsar_core::config::Config;
use pulsar_core::input::ControllerHub;
use pulsar_core::pipeline::HwEncoder;
use pulsar_core::service::{gen_password, request_games, request_launch, GameInfo};
use pulsar_core::Transport;
use tauri::{AppHandle, State};

use crate::events::{AutoConnect, ConnInfo, ControllerInfo, LanDevice, ScannedApp};
#[cfg(windows)]
use crate::process::no_window;
use crate::process::{detect_encoders, ffmpeg_bin, HostGame};
use crate::state::{AppState, StreamCfg};
use crate::util::{config_path, connect_target, is_executable, AUTO_CONNECT};

#[tauri::command]
pub(crate) async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
	tracing::info!("get_config invoked (frontend JS is running)");
	Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
pub(crate) async fn set_config(
	app: AppHandle,
	state: State<'_, AppState>,
	config: Config,
) -> Result<(), String> {
	*state.config.lock().unwrap() = config.clone();
	config.save(config_path(&app)).map_err(|e| e.to_string())
}

/// Available hardware encoders detected via ffmpeg (kebab-case values).
#[tauri::command]
pub(crate) async fn available_encoders(app: AppHandle) -> Vec<String> {
	detect_encoders(&ffmpeg_bin(&app))
		.into_iter()
		.map(|e| {
			match e {
				HwEncoder::Nvenc => "nvenc",
				HwEncoder::Amf => "amf",
				HwEncoder::Vaapi => "vaapi",
				HwEncoder::Qsv => "qsv",
				HwEncoder::VideoToolbox => "videotoolbox",
				HwEncoder::Vulkan => "vulkan",
				HwEncoder::MediaFoundation => "mediafoundation",
				HwEncoder::Software => "software",
				HwEncoder::Auto => "auto",
			}
			.to_string()
		})
		.collect()
}

/// Set the host's stream settings (resolution/fps/bitrate/encoder/display).
#[tauri::command]
pub(crate) async fn set_stream_settings(state: State<'_, AppState>, cfg: StreamCfg) -> Result<(), String> {
	*state.stream_cfg.lock().unwrap() = cfg;
	Ok(())
}

/// The one-time password a client must enter to connect to this host (empty until
/// online, or when unattended access is enabled).
#[tauri::command]
pub(crate) async fn session_password(state: State<'_, AppState>) -> Result<String, String> {
	Ok(state.password.lock().unwrap().clone())
}

/// Generate a fresh one-time password, invalidating the previous one.
#[tauri::command]
pub(crate) async fn new_password(state: State<'_, AppState>) -> Result<String, String> {
	let pw = gen_password();
	*state.password.lock().unwrap() = pw.clone();
	Ok(pw)
}

/// Publish the host's games so connecting clients can list/launch them.
#[tauri::command]
pub(crate) async fn publish_games(state: State<'_, AppState>, games: Vec<HostGame>) -> Result<(), String> {
	*state.games.lock().unwrap() = games;
	Ok(())
}

/// Client: list the games published by the host at `target`.
#[tauri::command]
pub(crate) async fn list_remote_games(
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
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let (mut sess, peer_label) = connect_target(&node, &target).await?;
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
	request_games(&mut sess).await.map_err(|e| e.to_string())
}

/// Client: ask the host at `target` to launch one of its games.
#[tauri::command]
pub(crate) async fn launch_remote_game(
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
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let (mut sess, peer_label) = connect_target(&node, &target).await?;
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
	request_launch(&mut sess, &game_id)
		.await
		.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn connect(state: State<'_, AppState>, target: String) -> Result<ConnInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or("önce çevrimiçi ol")?;
	let (sess, peer_label) = connect_target(&node, &target).await?;
	let transport = match sess.transport() {
		Transport::Direct => "direct",
		Transport::Relay => "relay",
	};
	Ok(ConnInfo {
		transport: transport.to_string(),
		peer: peer_label,
	})
}

/// Devices auto-discovered on the local network. Empty until `go_online` starts
/// the beacon. Polled by the Devices screen.
#[tauri::command]
pub(crate) async fn lan_devices(state: State<'_, AppState>) -> Result<Vec<LanDevice>, String> {
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
pub(crate) async fn controllers() -> Result<Vec<ControllerInfo>, String> {
	match ControllerHub::new() {
		Ok(mut hub) => Ok(hub
			.list()
			.into_iter()
			.map(|c| ControllerInfo {
				index: c.index,
				name: c.name,
				kind: format!("{:?}", c.kind),
				label: c.kind.label().to_string(),
				connected: c.connected,
			})
			.collect()),
		Err(_) => Ok(Vec::new()),
	}
}

/// This machine's primary LAN IPv4 (so a peer can connect to us by IP). Best-effort
/// + offline-safe: "connects" a UDP socket toward a public address (no packets are
/// sent) and reads back the local address the OS would route through. Empty on
/// failure (e.g. no network).
#[tauri::command]
pub(crate) async fn local_ip() -> Result<String, String> {
	let sock = match std::net::UdpSocket::bind("0.0.0.0:0") {
		Ok(s) => s,
		Err(_) => return Ok(String::new()),
	};
	// 8.8.8.8 is just a routing hint; connect() on UDP sends nothing.
	if sock.connect("8.8.8.8:80").is_err() {
		return Ok(String::new());
	}
	Ok(sock
		.local_addr()
		.map(|a| a.ip().to_string())
		.unwrap_or_default())
}

/// Path to an installed Steam launcher, or empty if Steam isn't found — lets the UI
/// offer a deletable "Steam" default only when it's actually installed.
#[tauri::command]
pub(crate) async fn steam_path() -> Result<String, String> {
	let candidates: &[&str] = if cfg!(windows) {
		&[
			"C:\\Program Files (x86)\\Steam\\steam.exe",
			"C:\\Program Files\\Steam\\steam.exe",
		]
	} else if cfg!(target_os = "macos") {
		&["/Applications/Steam.app/Contents/MacOS/steam_osx"]
	} else {
		&["/usr/bin/steam", "/usr/games/steam", "/var/lib/flatpak/exports/bin/com.valvesoftware.Steam"]
	};
	for p in candidates {
		if std::path::Path::new(p).exists() {
			return Ok((*p).to_string());
		}
	}
	Ok(String::new())
}

/// Scan a folder (one level deep) for launchable apps so the Oyunlar tab can list
/// them. Cross-platform: Windows matches common executable extensions, Unix the
/// executable bit.
#[tauri::command]
pub(crate) async fn scan_folder(path: String) -> Result<Vec<ScannedApp>, String> {
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

/// Run a host-side prep command (e.g. a per-game session start/stop hook).
/// Fire-and-forget; runs through the platform shell.
#[tauri::command]
pub(crate) async fn run_command(command: String) -> Result<(), String> {
	let command = command.trim().to_string();
	if command.is_empty() {
		return Ok(());
	}
	#[cfg(windows)]
	let spawn = {
		let mut c = std::process::Command::new("cmd");
		c.args(["/C", &command]);
		no_window(&mut c); // host prep commands must not flash a console either
		c.spawn()
	};
	#[cfg(not(windows))]
	let spawn = std::process::Command::new("sh")
		.args(["-c", &command])
		.spawn();
	spawn.map(|_| ()).map_err(|e| e.to_string())
}

/// The CLI `--connect` auto-connect target (id/ip + optional password), for the frontend
/// to initiate a session on startup. `None` unless `--connect` was passed.
///
/// A one-shot `.skip-autoconnect` marker (written by `relaunch_to_home`) suppresses the
/// auto-connect for exactly the next launch and is consumed here: after the user disconnects
/// from a direct-connect (kiosk) session the app relaunches to a fresh, usable home WITHOUT
/// reconnecting (the new process gives WebKitGTK a healthy webview again — see `relaunch_to_home`).
#[tauri::command]
pub(crate) fn auto_connect_target(app: AppHandle) -> Option<AutoConnect> {
	let marker = config_path(&app).with_file_name(".skip-autoconnect");
	if marker.exists() {
		let _ = std::fs::remove_file(&marker);
		return None;
	}
	AUTO_CONNECT.get().cloned().flatten()
}

/// Relaunch the app to a fresh home after the user disconnects from a direct-connect (kiosk)
/// session. On Linux the native video renderer leaves WebKitGTK unable to process clicks once it
/// tears down on this headless path (the webview is covered from boot and never warmed by a real
/// click), so the only reliable way back to a usable UI is a new process. We drop a one-shot
/// `.skip-autoconnect` marker (consumed by `auto_connect_target`) so the relaunched instance lands
/// on the Connect screen instead of reconnecting, then restart. No-op on Windows/macOS, where the
/// WebView2/WKWebView webview stays interactive after a session ends.
#[tauri::command]
pub(crate) fn relaunch_to_home(app: AppHandle) {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		let marker = config_path(&app).with_file_name(".skip-autoconnect");
		if let Some(dir) = marker.parent() {
			let _ = std::fs::create_dir_all(dir);
		}
		let _ = std::fs::write(&marker, b"1");
		app.restart();
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	let _ = app;
}
