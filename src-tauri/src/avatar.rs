//! Self-identity avatar: resolve a small PNG image representing the local user
//! (account photo → desktop wallpaper fallback), shown in the sidebar's "Sen"
//! chip and pushed to the peer over the session side channel
//! ([`pulsar_core::service::DataMsg::Avatar`]) right after a session starts.

use base64::Engine as _;

use crate::state::AppState;

/// Hard cap for the RAW image bytes on the wire. The session sends ONE UDP datagram
/// per message and `DataMsg` is serde_JSON — a `Vec<u8>` serializes as a number
/// array (~3.7× the raw size), so the datagram budget is what binds: 14 000 raw
/// ≈ 52 KB JSON, safely under the 65 507 UDP max. (A 19.6 KB wallpaper PNG → 72 KB
/// JSON → EMSGSIZE → the hold loop treated the failed send as a dead session and
/// tore it down — the "connected but no video" regression.)
const MAX_AVATAR_BYTES: usize = 14_000;

/// Avatar edge in pixels — big enough for the ~32 px UI chips at 2–3× DPI without
/// approaching the datagram budget above.
#[cfg(target_os = "linux")]
const AVATAR_EDGE: u32 = 96;

/// Wrap raw image bytes as a `data:` URL the webview can drop straight into an
/// `<img src>`. Mime sniffed from the magic bytes — the encoder degrades PNG→JPEG
/// to fit the wire budget, so both arrive here.
pub(crate) fn data_url(img: &[u8]) -> String {
	let mime = if img.starts_with(&[0xFF, 0xD8]) {
		"image/jpeg"
	} else {
		"image/png"
	};
	format!(
		"data:{mime};base64,{}",
		base64::engine::general_purpose::STANDARD.encode(img)
	)
}

/// The local user's avatar as PNG bytes, honoring the configured identity mode
/// (Settings → Genel, `Config::avatar_mode`): `user` = account photo (with the
/// wallpaper as fallback so the chip is rarely empty), `wallpaper` = wallpaper
/// only, `anonymous` = nothing is resolved or pushed to peers.
pub(crate) fn avatar_png(app: &tauri::AppHandle, mode: &str) -> Option<Vec<u8>> {
	match mode {
		"anonymous" => None,
		// Wallpaper formats the `image` crate can't decode (JXL/AVIF — modern distro
		// defaults) fall through to the bundled ffmpeg.
		"wallpaper" => wallpaper_png().or_else(|| wallpaper_via_ffmpeg(app)),
		// "user" (and any unknown value — fail open to the default behavior).
		_ => self_avatar_png().or_else(|| wallpaper_via_ffmpeg(app)),
	}
}

/// Linux: the OS user's image — (a) the AccountsService icon (where GNOME/KDE user
/// managers store the account photo), (b) the classic `~/.face` dotfiles, and if
/// neither exists (c) the main display's wallpaper — center-cropped square and
/// scaled to a 96×96 PNG.
#[cfg(target_os = "linux")]
pub(crate) fn self_avatar_png() -> Option<Vec<u8>> {
	account_image()
		.or_else(wallpaper_image)
		.and_then(encode_avatar)
}

/// Windows/macOS: no account-image lookup yet (Windows needs the AccountPicture
/// shell COM dance, macOS a `dscl … JPEGPhoto` decode). Returning `None` keeps the
/// textual chip locally and simply skips the peer push.
#[cfg(not(target_os = "linux"))]
pub(crate) fn self_avatar_png() -> Option<Vec<u8>> {
	None
}

#[cfg(target_os = "linux")]
fn wallpaper_png() -> Option<Vec<u8>> {
	wallpaper_image().and_then(encode_avatar)
}

/// Non-Linux: no wallpaper lookup either (see `self_avatar_png` stub above).
#[cfg(not(target_os = "linux"))]
fn wallpaper_png() -> Option<Vec<u8>> {
	None
}

#[cfg(target_os = "linux")]
fn load_image(path: &std::path::Path) -> Option<image::DynamicImage> {
	// AccountsService icons carry no extension, so sniff the format from the bytes
	// instead of trusting the path.
	let bytes = std::fs::read(path).ok()?;
	image::load_from_memory(&bytes).ok()
}

/// The OS account photo, if the user has one set.
#[cfg(target_os = "linux")]
fn account_image() -> Option<image::DynamicImage> {
	// (a) AccountsService — the freedesktop store GNOME/KDE user settings write to
	// (a raw image file named exactly after the user). World-readable by design.
	if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) {
		let p = std::path::Path::new("/var/lib/AccountsService/icons").join(user);
		if let Some(img) = load_image(&p) {
			return Some(img);
		}
	}
	// (b) the classic home-dir dotfiles (SDDM/KDM and older setups).
	let home = std::env::var("HOME").ok()?;
	for name in [".face", ".face.icon"] {
		if let Some(img) = load_image(&std::path::Path::new(&home).join(name)) {
			return Some(img);
		}
	}
	None
}

/// The main display's wallpaper: KDE Plasma's appletsrc first (the desktop this
/// project is developed on), then gsettings (GNOME and schema-followers). A
/// candidate the `image` crate can't decode (JXL/AVIF defaults on modern distros)
/// falls through to the bundled-ffmpeg path in `wallpaper_via_ffmpeg`.
#[cfg(target_os = "linux")]
fn wallpaper_candidates() -> Vec<std::path::PathBuf> {
	let mut out = Vec::new();
	// KDE Plasma: ~/.config/plasma-org.kde.plasma.desktop-appletsrc holds the per-
	// containment wallpaper as `Image=file:///…` lines; the LAST one is the most
	// recently configured screen. Plain INI grep — no KDE libs needed.
	if let Ok(home) = std::env::var("HOME") {
		let rc =
			std::path::Path::new(&home).join(".config/plasma-org.kde.plasma.desktop-appletsrc");
		if let Ok(text) = std::fs::read_to_string(rc) {
			for line in text.lines().rev() {
				if let Some(v) = line.trim().strip_prefix("Image=") {
					let p = percent_decode(v.strip_prefix("file://").unwrap_or(v));
					out.push(std::path::PathBuf::from(p));
					break;
				}
			}
		}
	}
	// GNOME (light key first, then the dark variant).
	for key in ["picture-uri", "picture-uri-dark"] {
		let Ok(o) = std::process::Command::new("gsettings")
			.args(["get", "org.gnome.desktop.background", key])
			.output()
		else {
			break; // gsettings missing entirely → no GNOME candidates
		};
		if !o.status.success() {
			continue;
		}
		// Output is a quoted GVariant string like 'file:///usr/share/…/bg.png'.
		let raw = String::from_utf8_lossy(&o.stdout);
		let uri = raw.trim().trim_matches('\'');
		out.push(std::path::PathBuf::from(percent_decode(
			uri.strip_prefix("file://").unwrap_or(uri),
		)));
	}
	out.retain(|p| p.is_file());
	out
}

#[cfg(target_os = "linux")]
fn wallpaper_image() -> Option<image::DynamicImage> {
	wallpaper_candidates()
		.into_iter()
		.find_map(|p| load_image(&p))
}

/// Decode + square-crop a wallpaper the `image` crate can't read (JXL/AVIF…) with
/// the BUNDLED ffmpeg — it ships libjxl etc., so any wallpaper a desktop can set,
/// we can avatar. Produces the final 96×96 PNG directly (scale+crop in-filter).
#[cfg(target_os = "linux")]
fn wallpaper_via_ffmpeg(app: &tauri::AppHandle) -> Option<Vec<u8>> {
	let ffmpeg = crate::process::ffmpeg_bin(app);
	for p in wallpaper_candidates() {
		// JPEG output (mjpeg) so a photographic wallpaper fits the wire budget; try
		// two quality steps (lower q number = higher quality in mjpeg's 2–31 scale).
		let tmp = std::env::temp_dir().join("pulsar-avatar.jpg");
		for q in ["6", "14"] {
			let ok = std::process::Command::new(&ffmpeg)
				.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
				.arg(&p)
				.args([
					"-vf",
					&format!(
						"scale={AVATAR_EDGE}:{AVATAR_EDGE}:force_original_aspect_ratio=increase,crop={AVATAR_EDGE}:{AVATAR_EDGE}"
					),
					"-frames:v",
					"1",
					"-q:v",
					q,
				])
				.arg(&tmp)
				.status()
				.map(|s| s.success())
				.unwrap_or(false);
			if ok {
				if let Ok(jpg) = std::fs::read(&tmp) {
					let _ = std::fs::remove_file(&tmp);
					if jpg.len() <= MAX_AVATAR_BYTES {
						return Some(jpg);
					}
				}
			}
		}
	}
	None
}

#[cfg(not(target_os = "linux"))]
fn wallpaper_via_ffmpeg(_app: &tauri::AppHandle) -> Option<Vec<u8>> {
	None
}

/// Minimal %XX decoding for file URIs — gsettings percent-encodes spaces and
/// non-ASCII (e.g. `duvar%20ka%C4%9F%C4%B1d%C4%B1.jpg`); a URL crate isn't
/// warranted for this one call site. Byte-wise so malformed input can't panic.
#[cfg(target_os = "linux")]
fn percent_decode(s: &str) -> String {
	fn hex(b: u8) -> Option<u8> {
		match b {
			b'0'..=b'9' => Some(b - b'0'),
			b'a'..=b'f' => Some(b - b'a' + 10),
			b'A'..=b'F' => Some(b - b'A' + 10),
			_ => None,
		}
	}
	let bytes = s.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' && i + 2 < bytes.len() {
			if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
				out.push(hi << 4 | lo);
				i += 3;
				continue;
			}
		}
		out.push(bytes[i]);
		i += 1;
	}
	String::from_utf8_lossy(&out).into_owned()
}

/// Center-crop square + scale to [`AVATAR_EDGE`]² and encode WITHIN the wire budget:
/// PNG first (crisp for flat account icons); photographic wallpapers blow the budget
/// there, so degrade to JPEG at falling quality until it fits.
#[cfg(target_os = "linux")]
fn encode_avatar(img: image::DynamicImage) -> Option<Vec<u8>> {
	// resize_to_fill = scale preserving aspect + center-crop the overflow, i.e.
	// exactly the square chip crop in one pass.
	let img = img.resize_to_fill(
		AVATAR_EDGE,
		AVATAR_EDGE,
		image::imageops::FilterType::Lanczos3,
	);
	let mut buf = std::io::Cursor::new(Vec::new());
	if img.write_to(&mut buf, image::ImageFormat::Png).is_ok()
		&& buf.get_ref().len() <= MAX_AVATAR_BYTES
	{
		return Some(buf.into_inner());
	}
	let rgb = img.to_rgb8();
	for q in [80u8, 60, 40] {
		let mut jbuf = Vec::new();
		let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jbuf, q);
		if rgb.write_with_encoder(enc).is_ok() && jbuf.len() <= MAX_AVATAR_BYTES {
			return Some(jbuf);
		}
	}
	None
}

/// The local user's avatar as a `data:image/png;base64,…` URL for the UI's "Sen"
/// chip; `None` when nothing resolves (or mode is `anonymous`) — the UI keeps its
/// textual fallback then. Async + spawn_blocking because resolving may decode a
/// full-size wallpaper (hundreds of ms), which must not block the main thread.
#[tauri::command]
pub(crate) async fn self_avatar(
	app: tauri::AppHandle,
	state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
	let mode = state.config.lock().unwrap().avatar_mode.clone();
	Ok(
		tokio::task::spawn_blocking(move || avatar_png(&app, &mode).map(|png| data_url(&png)))
			.await
			.unwrap_or(None),
	)
}

/// The OS user's display name (e.g. "Ahmet Enes Duruer") for the sidebar identity
/// chip — replaces the generic "Bu cihaz" label with who is actually here.
#[tauri::command]
pub(crate) fn device_user_name() -> String {
	pulsar_core::discovery::os_display_name()
}
