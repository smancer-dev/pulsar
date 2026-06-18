//! Assorted Tauri commands: config load/save, encoder enumeration, host stream
//! settings, the connect/list/launch flows, LAN discovery, controllers, local IP,
//! Steam/folder scanning, host-side prep commands, and the password helpers.

use pulsar_core::config::Config;
// Controller reading + rumble are via crate::controllers (SDL3) now, not gilrs.
use pulsar_core::pipeline::HwEncoder;
use pulsar_core::service::{gen_password, request_games, request_launch, GameInfo};
use pulsar_core::Transport;
use tauri::{AppHandle, Emitter, State};

use crate::events::{AutoConnect, ConnInfo, ControllerInfo, LanDevice, ScannedApp};
#[cfg(windows)]
use crate::process::no_window;
use crate::process::{detect_encoders, ffmpeg_bin, HostGame};
use crate::state::{AppState, StreamCfg};
use crate::util::{config_path, connect_target, forget_peer_key, is_executable, AUTO_CONNECT};

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
	crate::i18n::set_lang(&config.language);
	*state.config.lock().unwrap() = config.clone();
	config.save(config_path(&app)).map_err(|e| e.to_string())
}

/// Set the UI language from the frontend's switcher. The webview's i18n lives in
/// localStorage and was never reflected here, so everything Rust renders (tray,
/// host strings, the native overlay's `--lang`) stayed on `Config.language`'s
/// default — an English UI still got a Turkish in-session overlay. Persisted into
/// the config so child processes and the next launch agree.
#[tauri::command]
pub(crate) async fn set_language(
	app: AppHandle,
	state: State<'_, AppState>,
	lang: String,
) -> Result<(), String> {
	let language = if lang == "en" {
		pulsar_core::config::Language::En
	} else {
		pulsar_core::config::Language::Tr
	};
	crate::i18n::set_lang(&language);
	let cfg = {
		let mut g = state.config.lock().unwrap();
		if g.language == language {
			return Ok(()); // no change — skip the disk write
		}
		g.language = language;
		g.clone()
	};
	cfg.save(config_path(&app)).map_err(|e| e.to_string())
}

/// Remove the TOFU-pinned key for a peer id from `known_peers.json`. Called when
/// the user acknowledges an `IdentityChanged` error and wants to accept the peer's
/// new identity — the next connect will re-pin via TOFU. `id` is the target in any
/// format `DeviceId::parse` accepts ("641724395" or "641 724 395"). The pin is
/// scoped to the current relay endpoint so that pins on other relays are unaffected.
#[tauri::command]
pub(crate) async fn forget_peer(
	app: AppHandle,
	state: State<'_, AppState>,
	id: String,
) -> Result<(), String> {
	let device_id =
		pulsar_core::proto::DeviceId::parse(&id).ok_or_else(|| crate::i18n::t("err.badTarget").to_string())?;
	let relay = state.config.lock().unwrap().relay.clone();
	forget_peer_key(&app, &relay, &device_id);
	Ok(())
}

/// Available hardware encoders detected via ffmpeg (kebab-case values).
#[tauri::command]
pub(crate) async fn available_encoders(app: AppHandle) -> Vec<String> {
	// `detect_encoders` shells out to ffmpeg (`-encoders`), which can take a second
	// or more — run it off the async runtime so it doesn't block a tokio worker.
	let ffmpeg = ffmpeg_bin(&app);
	tokio::task::spawn_blocking(move || {
		let mut ids: Vec<String> = detect_encoders(&ffmpeg)
			.into_iter()
			.map(|e| crate::process::encoder_wire_id(e).to_string())
			.collect();
		// Families served by the GStreamer backend (e.g. Rockchip MPP encode on RK3588,
		// where ffmpeg has no rkmpp encoders) count as available too.
		#[cfg(target_os = "linux")]
		for (enc, _codecs) in crate::process::validated_gst_encoders() {
			let id = enc.wire_id().to_string();
			if !ids.contains(&id) {
				ids.push(id);
			}
		}
		ids
	})
	.await
	.unwrap_or_default()
}

/// The audio capture devices this host can record from, for the Settings dropdown.
/// The stored value (`Config.audio_input`) is the device-name string the user picks
/// (empty = platform default). The list is platform-specific and can change at any
/// time (USB mics unplugged), so the UI re-queries on mount and polls periodically.
#[tauri::command]
pub(crate) async fn list_audio_sources(app: AppHandle) -> Vec<String> {
	let _ = &app;
	// Device enumeration shells out (ffmpeg `-list_devices` / `pactl`) and can take a
	// second or more, so run it off the async runtime to avoid blocking a tokio worker.
	#[cfg(windows)]
	let ffmpeg = ffmpeg_bin(&app);
	tokio::task::spawn_blocking(move || {
		#[cfg(windows)]
		{
			audio_sources_dshow(&ffmpeg)
		}
		#[cfg(target_os = "linux")]
		{
			audio_sources_pactl()
		}
		#[cfg(target_os = "macos")]
		{
			Vec::new()
		}
	})
	.await
	.unwrap_or_default()
}

/// Windows: enumerate DirectShow audio capture devices via the bundled ffmpeg.
/// `ffmpeg -list_devices true -f dshow -i dummy` prints the devices to STDERR; the
/// audio devices follow a `DirectShow audio devices` header, each on its own line as
/// a quoted name. We collect those quoted names.
#[cfg(windows)]
fn audio_sources_dshow(ffmpeg: &str) -> Vec<String> {
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args([
		"-hide_banner",
		"-list_devices",
		"true",
		"-f",
		"dshow",
		"-i",
		"dummy",
	]);
	no_window(&mut cmd);
	let out = match cmd.output() {
		Ok(out) => out,
		Err(_) => return Vec::new(),
	};
	let text = String::from_utf8_lossy(&out.stderr);
	// Two stderr dialects: old ffmpeg groups devices under a "DirectShow audio
	// devices" header; ffmpeg ≥6 prints one line per device suffixed "(audio)" /
	// "(video)" with no headers. Handle both.
	let mut in_audio = false;
	let mut names = Vec::new();
	for line in text.lines() {
		let lower = line.to_ascii_lowercase();
		if lower.contains("directshow") && lower.contains("audio") && lower.contains("devices") {
			in_audio = true;
			continue;
		}
		if lower.contains("directshow") && lower.contains("video") && lower.contains("devices") {
			in_audio = false;
			continue;
		}
		let trimmed = line.trim_end();
		let tagged_audio = trimmed.ends_with("(audio)");
		let tagged_video = trimmed.ends_with("(video)");
		if !(in_audio && !tagged_video) && !tagged_audio {
			continue;
		}
		// A device line carries the name in double quotes; the following
		// "Alternative name" line also has quotes — skip it (it's not user-facing).
		if line.contains("Alternative name") {
			continue;
		}
		if let Some(start) = line.find('"') {
			let rest = &line[start + 1..];
			if let Some(end) = rest.find('"') {
				let name = rest[..end].trim();
				if !name.is_empty() {
					names.push(name.to_string());
				}
			}
		}
	}
	names
}

/// Linux: list PulseAudio/PipeWire sources via `pactl list short sources`. The source
/// name is the second tab-separated column; capturing a sink's playback uses its
/// `.monitor` source, which appears here. Empty if `pactl` is missing.
#[cfg(target_os = "linux")]
fn audio_sources_pactl() -> Vec<String> {
	let out = match std::process::Command::new("pactl")
		.args(["list", "short", "sources"])
		.output()
	{
		Ok(out) => out,
		Err(_) => return Vec::new(),
	};
	String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter_map(|line| line.split('\t').nth(1))
		.map(|name| name.trim().to_string())
		.filter(|name| !name.is_empty())
		.collect()
}

/// Set the host's stream settings (resolution/fps/bitrate/encoder/display).
#[tauri::command]
pub(crate) async fn set_stream_settings(
	state: State<'_, AppState>,
	cfg: StreamCfg,
) -> Result<(), String> {
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

/// Rotate the one-time password after it has successfully authenticated a
/// connection (single-use security): mint a fresh code, store it, and emit
/// `session-password` so the Home screen's displayed code updates without a
/// reconnect. NOT called for the persistent connect password (that one is
/// intentionally reusable) — only when the rotating OTP was the credential that
/// matched. No-op when unattended access made the OTP empty (no code to rotate).
pub(crate) fn rotate_session_password(app: &AppHandle) {
	use tauri::{Emitter, Manager};
	let state = app.state::<AppState>();
	let fresh = {
		let mut g = state.password.lock().unwrap();
		if g.is_empty() {
			return; // unattended (no OTP in use) — nothing to rotate
		}
		let pw = gen_password();
		*g = pw.clone();
		pw
	};
	// Reuse the existing UI refresh channel: the Home screen polls `session_password`,
	// and this event lets a live screen update immediately on rotation.
	let _ = app.emit("session-password", fresh);
}

/// Atomically compare-and-consume the one-time password: if `provided` matches
/// the live OTP, rotate it under the same lock so that a concurrent task cannot
/// also match the same value (eliminating the read→compare→rotate TOCTOU race).
/// Returns `true` only for the one caller that actually performed the swap.
/// The persistent connect password is NOT handled here — only the rotating OTP.
pub(crate) fn try_consume_otp(app: &AppHandle, provided: &str) -> bool {
	use tauri::{Emitter, Manager};
	let state = app.state::<AppState>();
	let fresh = {
		let mut g = state.password.lock().unwrap();
		if g.is_empty() || !crate::auth::secret_eq(provided, &g) {
			return false;
		}
		let pw = gen_password();
		*g = pw.clone();
		pw
	};
	let _ = app.emit("session-password", fresh);
	true
}

/// Publish the host's games so connecting clients can list/launch them.
#[tauri::command]
pub(crate) async fn publish_games(
	state: State<'_, AppState>,
	games: Vec<HostGame>,
) -> Result<(), String> {
	*state.games.lock().unwrap() = games;
	Ok(())
}

/// Timeout applied to every `client_authenticate` call that is NOT inside
/// `start_remote_play` (which has its own inline constant for the same reason).
/// Must be less than the JS-side CONNECT_TIMEOUT (45 s) so the Rust future
/// fails first and the frontend sees a real error string rather than the JS
/// timer's synthetic "connect-timed-out" sentinel.  When the deadline fires,
/// `sess` drops on return → `Session::drop` closes the connection → the host's
/// `recv_client_auth` sees `Gone` and tears down its Allow/Deny state cleanly.
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);

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
		.ok_or(crate::i18n::t("err.online"))?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let disc = state.discovery.lock().unwrap().clone();
	let (net_mode, relay) = {
		let cfg = state.config.lock().unwrap();
		(cfg.network_mode, cfg.relay.clone())
	};
	let (mut sess, peer_label) = connect_target(&app, &node, disc, &target, net_mode, &relay).await?;
	// Timeout on the auth handshake: a host that never returns a definitive
	// auth result (Allow/Deny/NeedPassword) would park this future indefinitely,
	// holding a half-open Session and accumulating stuck futures on repeated
	// attempts.  Mirror the same guard already applied in start_remote_play.
	let auth_result = tokio::time::timeout(
		AUTH_TIMEOUT,
		crate::auth::client_authenticate(&mut sess, &app, &pw_pending, &next_auth, &peer_label),
	)
	.await
	.map_err(|_| "connect-timed-out".to_string())?;
	if !auth_result? {
		return Err(crate::i18n::t("err.denied").into());
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
		.ok_or(crate::i18n::t("err.online"))?;
	let (pw_pending, next_auth) = (state.pw_pending.clone(), state.next_auth.clone());
	let disc = state.discovery.lock().unwrap().clone();
	let (net_mode, relay) = {
		let cfg = state.config.lock().unwrap();
		(cfg.network_mode, cfg.relay.clone())
	};
	let (mut sess, peer_label) = connect_target(&app, &node, disc, &target, net_mode, &relay).await?;
	// Same auth-timeout guard as list_remote_games and start_remote_play.
	let auth_result = tokio::time::timeout(
		AUTH_TIMEOUT,
		crate::auth::client_authenticate(&mut sess, &app, &pw_pending, &next_auth, &peer_label),
	)
	.await
	.map_err(|_| "connect-timed-out".to_string())?;
	if !auth_result? {
		return Err(crate::i18n::t("err.denied").into());
	}
	request_launch(&mut sess, &game_id)
		.await
		.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn connect(
	app: AppHandle,
	state: State<'_, AppState>,
	target: String,
) -> Result<ConnInfo, String> {
	let node = state
		.node
		.lock()
		.unwrap()
		.clone()
		.ok_or(crate::i18n::t("err.online"))?;
	let disc = state.discovery.lock().unwrap().clone();
	let (net_mode, relay) = {
		let cfg = state.config.lock().unwrap();
		(cfg.network_mode, cfg.relay.clone())
	};
	let (sess, peer_label) = connect_target(&app, &node, disc, &target, net_mode, &relay).await?;
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
	// gilrs enumeration touches the OS gamepad subsystem and can block briefly; run
	// it off the async runtime so it doesn't stall a tokio worker.
	let list = tokio::task::spawn_blocking(|| {
		crate::controllers::manager()
			.map(|m| {
				m.snapshot()
					.into_iter()
					.enumerate()
					.map(|(i, p)| ControllerInfo {
						index: i as u32,
						uuid: p.uuid,
						name: p.name,
						kind: format!("{:?}", p.kind),
						label: p.kind.label().to_string(),
						connected: true,
					})
					.collect()
			})
			.unwrap_or_default()
	})
	.await
	.unwrap_or_default();
	Ok(list)
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

/// The node's ACTUAL bound UDP port (0 = not online yet) — pairs with `local_ip` so
/// the Home screen can show the full direct-connect target ("ip:port"). Kept current
/// by `go_online` (which also emits the `node-port` event for live screens).
#[tauri::command]
pub(crate) fn node_port(state: State<'_, AppState>) -> u16 {
	state.node_port.load(std::sync::atomic::Ordering::SeqCst)
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
		&[
			"/usr/bin/steam",
			"/usr/games/steam",
			"/var/lib/flatpak/exports/bin/com.valvesoftware.Steam",
		]
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
	// `read_dir` + per-entry stat can hit a slow/network filesystem, so run the scan
	// off the async runtime to avoid blocking a tokio worker.
	tokio::task::spawn_blocking(move || {
		let dir = std::path::PathBuf::from(&path);
		if !dir.is_dir() {
			return Err(format!("{}: {path}", crate::i18n::t("err.folder")));
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
	})
	.await
	.map_err(|e| e.to_string())?
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
	spawn
		.map(|mut c| {
			// Reap off-thread: dropping the Child would leave one Unix zombie per
			// hook invocation for the (tray-resident) app's lifetime.
			std::thread::spawn(move || {
				let _ = c.wait();
			});
		})
		.map_err(|e| e.to_string())
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

/// Whether an in-app self-update can actually replace the *running* binary on this platform.
///
/// On Linux the Tauri updater self-replaces the file pointed to by `$APPIMAGE`. When the AppImage
/// is launched WITHOUT FUSE (e.g. `--appimage-extract-and-run`, or the raw `--no-bundle` dev binary,
/// or any non-AppImage launch) the runtime never sets `$APPIMAGE`, so the updater falls back to
/// `current_exe()` — which points at a throwaway extracted temp file. Installing would then rewrite
/// that temp copy (or error), never the deployed AppImage, so the appliance keeps booting the old
/// version forever while the updater appears to "run". Detect that here and let the frontend skip
/// the update with a clear warning instead of silently no-op'ing. Windows/macOS always update the
/// installed app in place, so they're always capable.
#[tauri::command]
pub(crate) fn self_update_possible() -> bool {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		// Self-update only works from a FUSE-mounted AppImage, which exports $APPIMAGE.
		std::env::var_os("APPIMAGE").is_some()
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	{
		true
	}
}

/// Sync the "run in system tray" preference from the UI. When `enabled` is true,
/// closing the main window hides Pulsar to the tray (existing behavior). When false,
/// closing quits the app. The flag is read by the `CloseRequested` handler in lib.rs.
#[tauri::command]
pub(crate) fn set_tray(state: State<'_, AppState>, enabled: bool) {
	state
		.tray_disabled
		.store(!enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Enable/disable this device's HOST role from the UI. `serving = false` (set when the
/// app enters gaming mode — a pure-client personality) makes the host serve loop reject
/// every inbound connection at auth time, before any Allow/Deny popup, so nobody can
/// connect to this machine. Registration with the relay is untouched, so outbound
/// connects still work. Default (never called) = serving, matching `hosting_disabled`'s
/// `false` zero-value.
#[tauri::command]
pub(crate) fn set_host_serving(state: State<'_, AppState>, serving: bool) {
	state
		.hosting_disabled
		.store(!serving, std::sync::atomic::Ordering::Relaxed);
}

/// A controller-navigation snapshot emitted to the gaming-mode UI. Booleans (not raw
/// axes) so the webview needs no knowledge of gilrs/SDL conventions: `up/down/left/right`
/// fold the D-pad AND the left stick; `a/b/x` are the face buttons (A=select, B=back,
/// X=delete); `lb/rb` are the bumpers (section jump).
#[derive(serde::Serialize, PartialEq, Clone, Default, Debug)]
struct NavInput {
	up: bool,
	down: bool,
	left: bool,
	right: bool,
	a: bool,
	b: bool,
	x: bool,
	lb: bool,
	rb: bool,
}

/// Start the gilrs→webview controller-nav bridge. Reads the first connected pad on a
/// dedicated OS thread (gilrs isn't Send/async — same model as the in-session reader in
/// `play.rs`) and emits `gamepad-nav` events. This is the menu-nav input path: on Linux
/// the webview Gamepad API is absent (WebKitGTK has no libmanette), and gilrs gives clean
/// SDL-mapped input everywhere. Idempotent — a second call while running is a no-op.
#[tauri::command]
pub(crate) fn gamepad_nav_start(app: AppHandle, state: State<'_, AppState>) {
	use std::sync::atomic::Ordering;
	// Bump the generation epoch BEFORE the swap so any thread from the previous
	// start that is mid-sleep wakes to a mismatched epoch and exits.
	let my_gen = state
		.nav_gamepad_gen
		.fetch_add(1, Ordering::SeqCst)
		.wrapping_add(1);
	if state.nav_gamepad_on.swap(true, Ordering::SeqCst) {
		return; // already running
	}
	let flag = state.nav_gamepad_on.clone();
	let gen = state.nav_gamepad_gen.clone();
	std::thread::spawn(move || {
		let Some(mgr) = crate::controllers::manager() else {
			flag.store(false, Ordering::SeqCst);
			return;
		};
		use pulsar_core::input::button;
		const T: i16 = 16_000; // ~0.5 stick deflection
		let mut prev: Option<NavInput> = None;
		let mut beat: u32 = 0;
		while flag.load(Ordering::SeqCst) && gen.load(Ordering::SeqCst) == my_gen {
			// Mimic the old (kind, state) snapshot shape so the nav mapping below is unchanged.
			let pads: Vec<(pulsar_core::input::GamepadKind, pulsar_core::input::GamepadState)> =
				mgr.snapshot().into_iter().map(|p| (p.kind, p.state)).collect();
			// `left_y` is up-positive (see input/hub.rs), so up = positive deflection.
			let nav = pads
				.first()
				.map(|(_, st)| NavInput {
					up: st.is_pressed(button::DPAD_UP) || st.left_y > T,
					down: st.is_pressed(button::DPAD_DOWN) || st.left_y < -T,
					left: st.is_pressed(button::DPAD_LEFT) || st.left_x < -T,
					right: st.is_pressed(button::DPAD_RIGHT) || st.left_x > T,
					a: st.is_pressed(button::A),
					b: st.is_pressed(button::B),
					x: st.is_pressed(button::X),
					lb: st.is_pressed(button::LB),
					rb: st.is_pressed(button::RB),
				})
				.unwrap_or_default();
			// Emit on change, plus a ~4 Hz heartbeat so a late subscriber still syncs.
			beat = beat.wrapping_add(1);
			if prev.as_ref() != Some(&nav) || beat % 16 == 0 {
				let _ = app.emit("gamepad-nav", &nav);
				prev = Some(nav);
			}
			std::thread::sleep(std::time::Duration::from_millis(16));
		}
	});
}

/// Stop the gilrs→webview controller-nav bridge (the reader thread exits on the next tick).
#[tauri::command]
pub(crate) fn gamepad_nav_stop(state: State<'_, AppState>) {
	use std::sync::atomic::Ordering;
	// Bump the generation epoch first so a thread mid-sleep exits on wake (even if
	// a concurrent start races in and sets nav_gamepad_on back to true before the
	// thread checks flag — the epoch mismatch is the authoritative exit signal).
	state.nav_gamepad_gen.fetch_add(1, Ordering::SeqCst);
	state.nav_gamepad_on.store(false, Ordering::SeqCst);
}

/// Persist the controller slot permutation from the UI. `order[n]` is the gilrs uuid
/// hex of the pad assigned to player-slot `n`; unknown/new pads append at the end.
/// The play.rs gilrs reader (T6) clones this Arc and reads it each tick so reorders
/// apply live without reconnect.
#[tauri::command]
pub(crate) async fn set_controller_order(
	state: State<'_, AppState>,
	order: Vec<String>,
) -> Result<(), String> {
	*state.controller_order.lock().unwrap() = order;
	Ok(())
}

/// Persist the per-controller emulation target from the UI. Map key = gilrs uuid hex,
/// value = "auto"|"xbox"|"ds4". The play.rs gilrs reader clones this Arc and reads it each
/// tick so changes apply live without reconnect. Absent/"auto" lets the host resolve from
/// the detected pad kind.
#[tauri::command]
pub(crate) async fn set_controller_emulation(
	state: State<'_, AppState>,
	map: std::collections::HashMap<String, String>,
) -> Result<(), String> {
	*state.controller_emulation.lock().unwrap() = map;
	Ok(())
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
