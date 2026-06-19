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
	// APP-UI hardware acceleration (the WebKitGTK webview's accelerated/GPU compositing) — NOT
	// the video stream's encode/decode (those are separate per-session codec settings). Decided
	// ONCE here, before the webview is created (the WebKitGTK env can't change at runtime → the
	// `ui_hardware_accel` setting needs an app restart to apply).
	//
	// Default: ON everywhere EXCEPT the Orange Pi 5 (RK3588/Mali), where WebKitGTK's accelerated
	// compositing has an UNRECOVERABLE "stops presenting" freeze (verified: not recoverable by a
	// window resize or fullscreen toggle). So opi5 defaults to the software path — stable, and the
	// per-row blurred-shadow paint cost is stripped in gaming mode (app.css) so heavy pages stay
	// smooth without the GPU. The `Config.ui_hardware_accel` setting (Some(true)/Some(false))
	// overrides the per-device default. On opi5-with-AC-on we also set PULSAR_FORCE_AC so the
	// frontend runs the present-keepalive (a best-effort freeze reducer).
	#[cfg(target_os = "linux")]
	{
		let is_rk3588 = std::fs::read("/proc/device-tree/compatible")
			.map(|b| b.windows(6).any(|w| w == b"rk3588"))
			.unwrap_or(false);
		let ac_on = ui_hwaccel_pref().unwrap_or(!is_rk3588);
		if ac_on {
			if is_rk3588 && std::env::var_os("PULSAR_FORCE_AC").is_none() {
				std::env::set_var("PULSAR_FORCE_AC", "1");
			}
		} else if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
			std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
		}
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
	// Windows: re-launch ELEVATED (UAC) before the GUI starts, so this machine can act as a full
	// host — injecting keyboard/mouse into elevated app windows (Task Manager, installers, any
	// "Run as administrator" window) is blocked for a non-elevated process (UIPI). Controlled by
	// the `request_admin` setting (default ON; user can disable it in Settings → Güvenlik);
	// `PULSAR_NO_ELEVATE=1` bypasses for dev. Done HERE — before `run()` takes the single-instance
	// mutex — so the unelevated process exits cleanly and the elevated one acquires the lock.
	#[cfg(windows)]
	if std::env::var_os("PULSAR_NO_ELEVATE").is_none()
		&& !is_elevated()
		&& request_admin_pref()
		&& relaunch_elevated(&args)
	{
		return;
	}

	pulsar_tauri::run()
}

/// (Windows) Read the persisted `request_admin` preference BEFORE Tauri starts. Defaults to `true`
/// (the Config default) when the file/field is absent, so a fresh install elevates by default.
#[cfg(windows)]
fn request_admin_pref() -> bool {
	let Some(appdata) = std::env::var_os("APPDATA") else {
		return true;
	};
	let path = std::path::PathBuf::from(appdata)
		.join("dev.pulsar.app")
		.join("config.json");
	pulsar_core::config::Config::load(&path).request_admin
}

/// (Windows) Whether THIS process is already running elevated (admin token).
#[cfg(windows)]
fn is_elevated() -> bool {
	use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
	use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
	use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
	unsafe {
		let mut token: HANDLE = std::ptr::null_mut();
		if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
			return false;
		}
		let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
		let mut len = 0u32;
		let ok = GetTokenInformation(
			token,
			TokenElevation,
			&mut elev as *mut _ as *mut _,
			std::mem::size_of::<TOKEN_ELEVATION>() as u32,
			&mut len,
		);
		CloseHandle(token);
		ok != 0 && elev.TokenIsElevated != 0
	}
}

/// (Windows) Spawn a fresh, ELEVATED copy of this exe via ShellExecuteW("runas") — pops the UAC
/// prompt. Re-passes the original args (so `--connect …` kiosk launches keep working). Returns
/// `true` if the elevated process was launched (the caller then exits); `false` if the user
/// declined UAC or it errored (the caller keeps running unelevated as a best effort).
#[cfg(windows)]
fn relaunch_elevated(args: &[String]) -> bool {
	use windows_sys::Win32::UI::Shell::ShellExecuteW;
	use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
	let Ok(exe) = std::env::current_exe() else {
		return false;
	};
	let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
	let verb = wide("runas");
	let file = wide(&exe.to_string_lossy());
	// Re-pass argv[1..], quoting each to survive spaces.
	let params_str = args
		.iter()
		.skip(1)
		.map(|a| format!("\"{a}\""))
		.collect::<Vec<_>>()
		.join(" ");
	let params = wide(&params_str);
	let h = unsafe {
		ShellExecuteW(
			std::ptr::null_mut(),
			verb.as_ptr(),
			file.as_ptr(),
			if params_str.is_empty() {
				std::ptr::null()
			} else {
				params.as_ptr()
			},
			std::ptr::null(),
			SW_SHOWNORMAL,
		)
	};
	// ShellExecuteW returns a value > 32 on success.
	h as isize > 32
}

/// Read the persisted `ui_hardware_accel` preference from the app config file BEFORE Tauri starts
/// (so the WebKitGTK compositing env is set before the webview is created). Returns `None` (auto)
/// if the file/field is absent — the per-device default then applies.
#[cfg(target_os = "linux")]
fn ui_hwaccel_pref() -> Option<bool> {
	let base = std::env::var_os("XDG_CONFIG_HOME")
		.map(std::path::PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
	let path = base.join("dev.pulsar.app").join("config.json");
	pulsar_core::config::Config::load(&path).ui_hardware_accel
}
