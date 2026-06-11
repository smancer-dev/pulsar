//! Pulsar native renderer (separate process; see Cargo.toml for why).
//!
//! Phase 1a (this file): Linux only — create a child X11 window of the Tauri window
//! (`--wid`), bring up an EGL/GLES context, and paint the shared egui overlay
//! (`overlay.rs`) with egui_glow. No video yet — the background is cleared dark so the
//! overlay is verifiable on-device. Next: import the rkmpp DRM_PRIME frame as the
//! background texture (port the proven `pulsar-vidsink.c` path) and wire decode.
//!
//! Usage: pulsar-render <stream.sdp> --wid 0x<parent-xid> [--mode game|remote]
//! SIGUSR1/2 toggle the overlay (open/close), like the C vidsink.

mod i18n;
mod overlay;

/// `--lang tr|en` (from the app's `Config.language`) — picks the native UI language
/// for the overlay/hints (the renderer can't share the webview's i18n tables; the
/// catalogs live in `lang/tr.json` + `lang/en.json`, see `i18n.rs`).
fn apply_lang() {
	let mut args = std::env::args();
	while let Some(a) = args.next() {
		if a == "--lang" {
			if let Some(v) = args.next() {
				i18n::set_english(v.starts_with("en"));
			}
		}
	}
}

#[cfg(all(unix, not(target_os = "macos")))]
mod decode;
#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
mod video;

// Shared streaming types + RTP depacketizer for the native-decode backends (Win MF, mac VT).
// Linux uses ffmpeg's own RTP demux, so it doesn't need this.
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod stream;

// Windows: native zero-copy renderer (child HWND + D3D11 + Media Foundation decode).
// Module is `win` (NOT `windows`) so it never shadows the `windows` crate in `use` paths.
#[cfg(target_os = "windows")]
mod win;

// macOS: overlay-only eframe stub for now (native VideoToolbox→Metal video is Task 10).
#[cfg(target_os = "macos")]
mod desktop;

#[cfg(all(unix, not(target_os = "macos")))]
fn main() {
	// `--probe`: headless capability probe (per-codec decoder selection with REAL
	// canned-frame decodes) printing JSON for the app's startup detection.
	if std::env::args().any(|a| a == "--probe") {
		println!("{}", decode::probe_json());
		return;
	}
	apply_lang();
	linux::run();
}

#[cfg(target_os = "windows")]
fn main() {
	apply_lang();
	win::run();
}

#[cfg(target_os = "macos")]
fn main() {
	apply_lang();
	desktop::run();
}

#[cfg(not(any(
	target_os = "windows",
	target_os = "macos",
	all(unix, not(target_os = "macos"))
)))]
fn main() {
	eprintln!("pulsar-render: native backend for this platform not built yet");
	std::process::exit(1);
}
