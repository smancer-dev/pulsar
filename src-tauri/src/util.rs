//! Small shared helpers + process-wide statics used across the command modules:
//! path resolution, relay/target address parsing, session lookup, fps/rotation
//! helpers, and the one-time CLI/probe statics.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use pulsar_core::proto::DeviceId;
use pulsar_core::service::{DataMsg, InputEvent};
use pulsar_core::Node;
use tauri::{AppHandle, Manager};

use crate::events::AutoConnect;
use crate::state::AppState;

/// Cached result of the one-time ddagrab→CUDA→NVENC zero-copy probe (true only when
/// the display adapter is the NVIDIA GPU). Avoids re-probing every stream.
pub(crate) static DDAGRAB_ZEROCOPY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// CLI auto-connect target: `pulsar --connect <id|ip> [--connect-pw <pw>]` makes the app
/// connect to that device as soon as it's online — for a kiosk/unattended client and for
/// automated end-to-end testing (no manual ID entry / clicking). Set once in `run()`,
/// consumed once by the frontend on startup.
pub(crate) static AUTO_CONNECT: std::sync::OnceLock<Option<AutoConnect>> =
	std::sync::OnceLock::new();

pub(crate) fn config_path(app: &AppHandle) -> PathBuf {
	app.path()
		.app_config_dir()
		.unwrap_or_else(|_| PathBuf::from("."))
		.join("config.json")
}

/// Per-user file holding this device's persistent X25519 identity (32 secret
/// bytes). Lives in the per-user app config dir, so the relay-assigned device ID is
/// stable across restarts and distinct per OS user (ASTER seats stay separate).
pub(crate) fn identity_path(app: &AppHandle) -> PathBuf {
	app.path()
		.app_config_dir()
		.unwrap_or_else(|_| PathBuf::from("."))
		.join("identity.key")
}

/// Resolve a user-entered `host:port` (IP or DNS name) to a socket address.
/// Prefers IPv4 — the relay binds `0.0.0.0`, and `localhost` often resolves to
/// `::1` first, which would never reach an IPv4-only relay.
pub(crate) async fn resolve_relay(addr: &str) -> Option<SocketAddr> {
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

/// Parse an `IP` or `IP:port` target into a socket address; a bare IP gets the
/// default node port. Returns `None` for a non-address (e.g. a 9-digit relay ID).
pub(crate) fn parse_target_addr(s: &str) -> Option<SocketAddr> {
	if let Ok(sa) = s.parse::<SocketAddr>() {
		return Some(sa);
	}
	if let Ok(ip) = s.parse::<IpAddr>() {
		return Some(SocketAddr::new(ip, pulsar_core::proto::DEFAULT_NODE_PORT));
	}
	None
}

/// Resolve a typed target — a 9-digit relay ID, or an IP / IP:port for a direct
/// (relay-less) connect — and open a session. Returns the session + a display
/// label (grouped ID or the address). The ID path is unchanged; an IP routes to
/// `connect_direct` (in-band key exchange), after which OTP auth / serve are
/// byte-for-byte identical.
///
/// `discovery` enables the SAME-LAN FAST PATH for ID targets: when the LAN
/// beacon already knows this device id, connect straight to its LAN endpoint.
/// The relay rendezvous would hand two same-NAT peers their PUBLIC (hairpin)
/// addresses, and consumer routers forward a 15 Mbit hairpin flow with heavy
/// loss (measured 14.7% on this network) — the LAN path sidesteps the router's
/// WAN side entirely. Falls back to the relay rendezvous within 1.5 s.
pub(crate) async fn connect_target(
	node: &Arc<Node>,
	discovery: Option<Arc<pulsar_core::discovery::Discovery>>,
	target: &str,
	network_mode: pulsar_core::config::NetworkMode,
) -> Result<(pulsar_core::Session, String), String> {
	let s = target.trim();
	if let Some(addr) = parse_target_addr(s) {
		// An explicit IP target IS a direct connect by definition — honored even in
		// relay-only mode (there is no relay route to a raw address).
		let sess = node
			.connect_direct(addr, None)
			.await
			.map_err(|e| e.to_string())?;
		return Ok((sess, addr.to_string()));
	}
	let id = DeviceId::parse(s).ok_or("geçersiz kimlik veya IP")?;
	// Relay-only must NOT take the same-LAN direct shortcut — the whole point of
	// that mode is that traffic goes through the relay (policy/diagnostics).
	let lan_allowed = !matches!(
		network_mode,
		pulsar_core::config::NetworkMode::RelayOnly
	);
	if let Some(disc) = discovery.filter(|_| lan_allowed) {
		let lan = disc
			.peers()
			.await
			.into_iter()
			.find(|p| p.id == Some(id))
			.map(|p| p.addr);
		if let Some(addr) = lan {
			match tokio::time::timeout(
				std::time::Duration::from_millis(1500),
				node.connect_direct(addr, None),
			)
			.await
			{
				Ok(Ok(sess)) => {
					tracing::info!(%addr, id = %id.grouped(), "same-LAN fast path: connected directly (discovery match)");
					return Ok((sess, id.grouped()));
				}
				_ => {
					tracing::info!(%addr, "LAN fast path failed — falling back to the relay rendezvous");
				}
			}
		}
	}
	let sess = node.connect(id).await.map_err(|e| e.to_string())?;
	Ok((sess, id.grouped()))
}

/// Look up a play session's side-channel sender (clipboard/chat/file/audio).
pub(crate) fn data_sender(
	state: &AppState,
	id: u64,
) -> Result<tokio::sync::mpsc::Sender<DataMsg>, String> {
	state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.data_tx.clone())
		.ok_or_else(|| crate::i18n::t("err.session").into())
}

/// Forward an input event to a specific play session's host.
pub(crate) fn forward(state: &AppState, id: u64, ev: InputEvent) {
	if let Some(play) = state.plays.lock().unwrap().get(&id) {
		let _ = play.input_tx.try_send(ev);
	}
}

/// Nearest stream fps the host offers ({30,60,120}) to a display's refresh rate.
pub(crate) fn nearest_fps(hz: u32) -> u32 {
	[30u32, 60, 120]
		.into_iter()
		.min_by_key(|f| (*f as i32 - hz as i32).abs())
		.unwrap_or(60)
}

/// Client display refresh → nearest supported stream fps. "Auto" fps targets the client's own
/// screen so motion is as smooth as the panel allows (a 60 Hz panel → 60, a 120 Hz panel → 120).
/// Linux reads it from GDK on the main thread; other platforms default to 60 for now.
#[cfg(target_os = "linux")]
pub(crate) async fn client_auto_fps(app: &AppHandle) -> u32 {
	use gdk::prelude::*;
	let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
	let posted = app
		.run_on_main_thread(move || {
			let hz = gdk::Display::default()
				.and_then(|d| d.monitor(0))
				.map(|m| m.refresh_rate())
				.filter(|r| *r > 0)
				.map(|r| ((r as f64) / 1000.0).round() as u32)
				.unwrap_or(60);
			let _ = tx.send(hz);
		})
		.is_ok();
	let hz = if posted { rx.await.unwrap_or(60) } else { 60 };
	nearest_fps(hz)
}
#[cfg(not(target_os = "linux"))]
pub(crate) async fn client_auto_fps(_app: &AppHandle) -> u32 {
	60
}

/// Host: this machine's primary display orientation in degrees (0/90/180/270). The captured
/// framebuffer carries this rotation, so the client renders the video rotated by the inverse to
/// show it upright (e.g. a laptop configured upside-down reports 180). `PULSAR_HOST_ROTATE` forces
/// a value (override if the auto-detect is wrong, or to test). Windows-only auto-detect; other
/// host OSes report 0 for now (X11 could read RandR, macOS CoreGraphics — TODO).
pub(crate) fn display_rotation() -> u32 {
	if let Some(d) = std::env::var("PULSAR_HOST_ROTATE")
		.ok()
		.and_then(|s| s.parse::<u32>().ok())
	{
		return d % 360;
	}
	display_rotation_detect()
}

#[cfg(windows)]
fn display_rotation_detect() -> u32 {
	use windows_sys::Win32::Graphics::Gdi::{
		EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
	};
	unsafe {
		let mut dm: DEVMODEW = std::mem::zeroed();
		dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
		if EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
			// dmDisplayOrientation (display branch of the DEVMODE union): DMDO_90=1/180=2/270=3.
			return match dm.Anonymous1.Anonymous2.dmDisplayOrientation {
				1 => 90,
				2 => 180,
				3 => 270,
				_ => 0,
			};
		}
	}
	0
}
#[cfg(not(windows))]
fn display_rotation_detect() -> u32 {
	0
}

/// Host: the primary display's refresh rate in Hz, or `None` if unknown. Used to NEGOTIATE the
/// stream fps so the host never encodes faster than its own panel can scan out — a client that
/// asks for 120 fps on a 60 Hz host gets capped to 60 (no point encoding duplicate frames; and the
/// extra load was the user-visible "120 seçtim, değişmiyor" symptom). `PULSAR_HOST_HZ` forces a
/// value (override if the auto-detect is wrong, or to test). Windows reads `dmDisplayFrequency`
/// (same DEVMODE the rotation detect uses); Linux parses the active `xrandr` mode's `*` rate;
/// other host OSes return `None` and the caller leaves the requested fps as-is.
pub(crate) fn host_panel_hz() -> Option<u32> {
	if let Some(h) = std::env::var("PULSAR_HOST_HZ")
		.ok()
		.and_then(|s| s.parse::<u32>().ok())
		.filter(|h| *h > 0)
	{
		return Some(h);
	}
	host_panel_hz_detect()
}

#[cfg(windows)]
fn host_panel_hz_detect() -> Option<u32> {
	use windows_sys::Win32::Graphics::Gdi::{
		EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
	};
	unsafe {
		let mut dm: DEVMODEW = std::mem::zeroed();
		dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
		if EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
			// dmDisplayFrequency: 0 or 1 mean "default/unknown" (per the Win32 docs) — treat as
			// unknown so the caller doesn't clamp to a bogus 1 Hz.
			let hz = dm.dmDisplayFrequency;
			return (hz > 1).then_some(hz);
		}
	}
	None
}

#[cfg(target_os = "linux")]
fn host_panel_hz_detect() -> Option<u32> {
	let out = std::process::Command::new("xrandr").output().ok()?;
	let text = String::from_utf8_lossy(&out.stdout);
	// The active mode line carries the current rate marked with a trailing `*`, e.g.
	// "   1920x1080     60.00*+   59.94    50.00". Take the first `*`-marked token.
	for line in text.lines() {
		for tok in line.split_whitespace() {
			if let Some(rate) = tok.strip_suffix('*').or_else(|| tok.strip_suffix("*+")) {
				if let Ok(hz) = rate.parse::<f64>() {
					let hz = hz.round() as u32;
					if hz > 1 {
						return Some(hz);
					}
				}
			}
		}
	}
	None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn host_panel_hz_detect() -> Option<u32> {
	None
}

/// Host: the primary display's pixel size `(width, height)`, or `None` if unknown. Used to clamp
/// the ffmpeg capture (`x11grab -video_size`) so the host never asks to grab a region LARGER than
/// its screen — ffmpeg dies with "Capture area WxH … outside the screen size" and sends no video
/// (hit when a 1440p-configured stream targets a 1080p host, e.g. an Orange Pi acting as host).
/// Windows captures via the native DXGI path (scales internally), so this is Linux/X11-only;
/// other OSes return `None` and the caller leaves the requested size as-is.
#[cfg(target_os = "linux")]
pub(crate) fn display_size(display: &str) -> Option<(u32, u32)> {
	let disp = if display.is_empty() { ":0" } else { display };
	let out = std::process::Command::new("xrandr")
		.env("DISPLAY", disp)
		.output()
		.ok()?;
	let text = String::from_utf8_lossy(&out.stdout);
	// "Screen 0: minimum 320 x 200, current 1920 x 1080, maximum 16384 x 16384"
	let after = text.split("current").nth(1)?;
	let mut nums = after
		.split(|c: char| !c.is_ascii_digit())
		.filter(|s| !s.is_empty());
	let w: u32 = nums.next()?.parse().ok()?;
	let h: u32 = nums.next()?.parse().ok()?;
	(w > 0 && h > 0).then_some((w, h))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn display_size(_display: &str) -> Option<(u32, u32)> {
	None
}

#[cfg(windows)]
pub(crate) fn is_executable(p: &std::path::Path) -> bool {
	matches!(
		p.extension()
			.and_then(|e| e.to_str())
			.map(|e| e.to_lowercase())
			.as_deref(),
		Some("exe") | Some("bat") | Some("cmd") | Some("lnk")
	)
}

#[cfg(not(windows))]
pub(crate) fn is_executable(p: &std::path::Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	std::fs::metadata(p)
		.map(|m| m.permissions().mode() & 0o111 != 0)
		.unwrap_or(false)
}
