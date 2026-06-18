// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
	// Linux: GTK derives WM_CLASS from g_get_prgname() (= argv[0]); force the brand
	// name so taskbars/Alt-Tab show "Pulsar" regardless of how the binary was named
	// or launched (plain `cargo run`, symlinks, renamed AppImage). Must run before
	// tao opens the GTK display. NB: `gdk::set_program_class` would panic here (the
	// binding asserts GDK is initialized) — unnecessary anyway, GTK derives the
	// class half of WM_CLASS by capitalizing the prgname ("pulsar" → "Pulsar").
	#[cfg(all(unix, not(target_os = "macos")))]
	gtk::glib::set_prgname(Some("pulsar"));
	// Linux: run the GTK stack under X11 (XWayland on a Wayland session) — the native
	// video renderer embeds INSIDE the app window via X11 `--wid` child windows, which
	// is impossible on a native Wayland surface (no XID → the video opened a separate
	// toplevel). Host-side capture is unaffected (the Wayland portal path keys off
	// XDG_SESSION_TYPE/WAYLAND_DISPLAY, not the GDK backend). An explicit env wins.
	#[cfg(target_os = "linux")]
	if std::env::var_os("GDK_BACKEND").is_none() {
		std::env::set_var("GDK_BACKEND", "x11");
	}
	// ARM Linux (RK3588/Mali): WebKitGTK's accelerated compositing intermittently
	// stops PRESENTING frames at startup — JS runs, state updates, but the window
	// shows a stale frame forever (blank white / stuck splash / half-painted UI).
	// Forcing the non-AC software path is fully stable (3/3 cold-launch verified on
	// the Orange Pi 5; the host UI is light, so software rendering is fine). The
	// DMABUF-disable knob is NOT used — it segfaults this stack. An explicit env wins.
	#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
	if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
		std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
	}
	// Windows: the native video child fully OCCLUDES the webview during a session, and
	// WebView2's native-window occlusion detection then THROTTLES the page (timers +
	// event handling lag by seconds) — Ctrl+Shift combos, the session top bar and the
	// viewrect reports all arrived late. Disable the occlusion calculation; the session
	// UI must keep running at full speed behind the video. An existing env wins.
	#[cfg(windows)]
	{
		const FLAG: &str = "--disable-features=CalculateNativeWinOcclusion";
		match std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
			Ok(cur) if !cur.contains("CalculateNativeWinOcclusion") => {
				std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", format!("{cur} {FLAG}"));
			}
			Err(_) => std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", FLAG),
			_ => {}
		}
	}
	// `pulsar --relay …` runs a headless relay/rendezvous server instead of the GUI,
	// so the same install can self-host a relay.
	let args: Vec<String> = std::env::args().collect();
	if args.iter().any(|a| a == "--relay") {
		pulsar_tauri::run_relay(&args);
		return;
	}
	// Diagnostic: exercise the SDL3 client rumble path standalone (no host session).
	if std::env::var_os("PULSAR_RUMBLE_TEST").is_some() {
		pulsar_tauri::rumble_selftest();
		return;
	}
	pulsar_tauri::run()
}
