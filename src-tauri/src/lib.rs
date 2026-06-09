//! Tauri command layer — the bridge between the SvelteKit UI and `pulsar-core`.
//!
//! Each `#[tauri::command]` is a thin async wrapper around the core: load/save
//! config, bind a [`Node`] + register with the relay (get an ID), connect to a
//! peer (P2P → relay), and enumerate controllers. The heavy lifting all lives in
//! `pulsar-core`; this file just marshals JSON to/from the UI.
//!
//! The command implementations and their supporting types live in submodules:
//! `state` (the managed `AppState` + session value types), `events` (serde
//! payloads), `util` (shared helpers + statics), `auth` (connection authorization),
//! `host` (`go_online` serving loop), `play` (client remote-play lifecycle),
//! `render` (Linux native renderer plumbing), `session_cmds` (live in-session
//! commands), and `commands` (the assorted remaining commands).

use tauri::{AppHandle, Emitter, Manager, WindowEvent};

mod audio_io;
mod auth;
mod commands;
mod connections;
mod events;
mod files;
mod host;
mod io_cmds;
#[cfg(windows)]
mod job;
mod kbdhook;
mod native_view;
mod play;
mod process;
#[cfg(all(unix, not(target_os = "macos")))]
mod render;
#[cfg(any(all(unix, not(target_os = "macos")), target_os = "windows"))]
mod render_stats;
mod session_cmds;
mod state;
mod util;
mod viewer;

use state::AppState;

// `native_view::spawn` calls `crate::no_window` — keep it reachable at the crate
// root (it lives in `process`), preserving that path after the split.
pub(crate) use process::no_window;

// Re-export every `#[tauri::command]` so `tauri::generate_handler!` below resolves
// them by bare name (they're defined across the submodules above).
use auth::{disconnect_peer, respond_request, submit_password};
use connections::{list_connections, show_connections};
use commands::{
	auto_connect_target, available_encoders, connect, controllers, get_config, lan_devices,
	launch_remote_game, list_remote_games, local_ip, new_password, publish_games, relaunch_to_home,
	run_command, scan_folder, session_password, set_config, set_stream_settings, steam_path,
};
use host::go_online;
use io_cmds::{
	host_send_chat, input_button, input_key, input_pointer, input_scroll, kbd_capture_start,
	kbd_capture_stop, mic_start, mic_stop, send_chat, send_clipboard, send_file,
	set_window_fullscreen,
};
use play::{start_remote_play, stop_stream};
use session_cmds::{
	reverse_play, set_frame_pacing, set_overlay, set_play_audio, set_play_bitrate, set_play_codec,
	set_play_encoder, set_play_fps, set_play_quality, set_play_resolution,
};

// Headless `pulsar --relay` mode lives in its own module to keep this file focused.
mod relay_mode;
pub use relay_mode::run_relay;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Acquire a per-user/per-session single-instance lock. Returns false if another
/// Pulsar is already running **for this user/seat**, in which case the caller
/// should exit. ASTER seats / different OS users are separate sessions, so each
/// still gets its own instance.
#[cfg(windows)]
fn acquire_single_instance() -> bool {
	use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
	use windows_sys::Win32::System::Threading::CreateMutexW;
	// "Local\\" namespace → scoped to the logon session (per ASTER seat / per user
	// with fast-user-switching), not machine-global.
	let name: Vec<u16> = "Local\\PulsarSingletonMutex\0".encode_utf16().collect();
	let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
	if h.is_null() {
		return true; // couldn't create the lock — never block startup over it
	}
	// Held for the whole process (never closed) so the OS releases it only on exit.
	unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
}

/// Name of the per-session "show yourself" event used to wake a tray-hidden instance
/// when the user launches Pulsar again. `Local\` → scoped to the logon session (ASTER).
#[cfg(windows)]
const SHOW_EVENT_NAME: &str = "Local\\PulsarShowEvent\0";

/// First instance: wait (on a background thread) for a second launch's signal and bring
/// the main window to the front — so re-launching a tray-hidden Pulsar reveals it instead
/// of doing nothing.
#[cfg(windows)]
fn spawn_show_watcher(app: AppHandle) {
	use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
	use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
	std::thread::spawn(move || {
		let name: Vec<u16> = SHOW_EVENT_NAME.encode_utf16().collect();
		// Auto-reset, initially non-signaled. CreateEventW returns the existing event if a
		// second instance already created it, so the signal is never lost to a startup race.
		let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
		if ev.is_null() {
			return;
		}
		loop {
			if unsafe { WaitForSingleObject(ev, INFINITE) } != WAIT_OBJECT_0 {
				break;
			}
			if let Some(w) = app.get_webview_window("main") {
				let _ = w.show();
				let _ = w.unminimize();
				let _ = w.set_focus();
			}
		}
	});
}

/// Second instance: tell the already-running Pulsar (this user/seat) to surface its
/// window, then exit. `AllowSetForegroundWindow(ASFW_ANY)` hands our foreground right to
/// it first — without this, Windows' foreground lock would only flash the taskbar icon
/// instead of raising the window.
#[cfg(windows)]
fn signal_existing_instance() {
	use windows_sys::Win32::Foundation::CloseHandle;
	use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
	use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
	const ASFW_ANY: u32 = 0xFFFF_FFFF;
	unsafe {
		AllowSetForegroundWindow(ASFW_ANY);
		let name: Vec<u16> = SHOW_EVENT_NAME.encode_utf16().collect();
		let ev = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
		if !ev.is_null() {
			SetEvent(ev);
			CloseHandle(ev);
		}
	}
}

#[cfg(not(windows))]
fn acquire_single_instance() -> bool {
	// TODO(per-user lock on Linux/macOS): advisory flock on a file in the per-user
	// runtime dir. No-op for now — the Windows/ASTER case is what needs guarding.
	true
}

pub fn run() {
	tracing_subscriber::fmt()
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
		)
		.init();
	tracing::info!("Pulsar starting");
	{
		// Parse the auto-connect CLI: `--connect <id|ip> [--connect-pw <pw>] [--mode
		// game|remote] [--app <name|id>]`. `--mode` defaults to remote (an unrecognized
		// value silently falls back to remote); `--app` is the host app/game to launch in
		// game mode (empty / "Desktop" = stream the whole desktop, launch nothing).
		let args: Vec<String> = std::env::args().collect();
		let flag = |n: &str| {
			args.iter()
				.position(|a| a == n)
				.and_then(|i| args.get(i + 1))
				.cloned()
		};
		let target = flag("--connect").map(|id| events::AutoConnect {
			id,
			pw: flag("--connect-pw").unwrap_or_default(),
			mode: flag("--mode")
				.map(|m| m.to_ascii_lowercase())
				.filter(|m| m == "game" || m == "remote")
				.unwrap_or_else(|| "remote".into()),
			app: flag("--app").unwrap_or_default(),
		});
		if let Some(ref ac) = target {
			tracing::info!(id = %ac.id, mode = %ac.mode, app = %ac.app, "auto-connect target set from CLI");
		}
		let _ = util::AUTO_CONNECT.set(target);
	}
	if !acquire_single_instance() {
		tracing::info!("another Pulsar instance is already running for this user — surfacing it");
		#[cfg(windows)]
		signal_existing_instance();
		return;
	}

	tauri::Builder::default()
		.manage(AppState::default())
		.setup(|app| {
			use tauri::menu::{Menu, MenuItem};
			use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

			// The main window starts hidden (tauri.conf `visible:false`) so the UI can
			// paint behind a splash before it appears — no white/frame flash on launch.
			// The frontend reveals it once mounted; this is a safety net so a frontend
			// failure can never leave an invisible window.
			{
				let h = app.handle().clone();
				std::thread::spawn(move || {
					std::thread::sleep(std::time::Duration::from_secs(4));
					if let Some(w) = h.get_webview_window("main") {
						if !w.is_visible().unwrap_or(true) {
							let _ = w.show();
						}
					}
				});
			}

			// Re-launching a tray-hidden Pulsar should surface this instance (a second
			// process detects the single-instance lock + signals us).
			#[cfg(windows)]
			spawn_show_watcher(app.handle().clone());

			// Load persisted config (relay endpoint, network mode, etc.).
			let cfg = pulsar_core::config::Config::load(util::config_path(app.handle()));
			tracing::info!(relay = %cfg.relay, "config loaded");
			*app.state::<AppState>().config.lock().unwrap() = cfg;

			// System tray: once launched, Pulsar stays resident in the tray. Closing
			// the window hides it (see on_window_event); the only full exit is the
			// tray's "Çıkış" item.
			let show = MenuItem::with_id(app, "show", "Pulsar'ı Göster", true, None::<&str>)?;
			let quit = MenuItem::with_id(app, "quit", "Çıkış", true, None::<&str>)?;
			let menu = Menu::with_items(app, &[&show, &quit])?;
			let mut tray = TrayIconBuilder::with_id("main")
				.tooltip("Pulsar")
				.menu(&menu)
				.show_menu_on_left_click(false)
				.on_menu_event(|app, event| match event.id.as_ref() {
					"show" => {
						if let Some(w) = app.get_webview_window("main") {
							let _ = w.show();
							let _ = w.unminimize();
							let _ = w.set_focus();
						}
					}
					"quit" => app.exit(0),
					_ => {}
				})
				.on_tray_icon_event(|tray, event| {
					// Left-click toggles the window (show ↔ hide).
					if let TrayIconEvent::Click {
						button: MouseButton::Left,
						button_state: MouseButtonState::Up,
						..
					} = event
					{
						let app = tray.app_handle();
						if let Some(w) = app.get_webview_window("main") {
							if w.is_visible().unwrap_or(false) {
								let _ = w.hide();
							} else {
								let _ = w.show();
								let _ = w.unminimize();
								let _ = w.set_focus();
							}
						}
					}
				});
			if let Some(icon) = app.default_window_icon().cloned() {
				tray = tray.icon(icon);
			}
			tray.build(app)?;
			Ok(())
		})
		.on_window_event(|window, event| match event {
			// Closing the window hides Pulsar to the tray rather than quitting, so a
			// single launch stays resident until the tray's "Çıkış" is chosen.
			WindowEvent::CloseRequested { api, .. } => {
					// Only the MAIN window hides-to-tray; secondary windows (approval popup,
					// connections manager) close NORMALLY so a host-side win.close() works
					// (prevent_close + hide would otherwise leave them stuck hidden).
					if window.label() == "main" {
						api.prevent_close();
						let _ = window.hide();
					}
				}
			// While fullscreen, stay above the taskbar/other apps ONLY when focused;
			// drop topmost when alt-tabbed away so other apps come forward normally.
			WindowEvent::Focused(focused) => {
				if window.state::<AppState>().fs_geom.lock().unwrap().is_some() {
					let _ = window.set_always_on_top(*focused);
				}
				// Capture/forward keyboard+mouse + the overlay/leave combos ONLY while the
				// Pulsar window is focused (Linux evdev grab is otherwise global → the combo
				// fired even when another app had focus). Releases the grab when we lose focus.
				kbdhook::set_focused(*focused);
				// Losing focus while the overlay is open would otherwise STRAND it: the combo is
				// now focus-gated off, so it couldn't be closed. Auto-close it on blur so the
				// state resets — refocusing then re-enables the combo cleanly.
				if !*focused {
					let _ = window.emit("window-blur", ());
				}
			}
			_ => {}
		})
		.invoke_handler(tauri::generate_handler![
			get_config,
			set_config,
			go_online,
			connect,
			lan_devices,
			controllers,
			local_ip,
			auto_connect_target,
			relaunch_to_home,
			steam_path,
			scan_folder,
			run_command,
			publish_games,
			list_remote_games,
			launch_remote_game,
			available_encoders,
			set_stream_settings,
			start_remote_play,
			stop_stream,
			set_play_resolution,
			set_play_encoder,
			set_play_codec,
			set_play_fps,
			set_play_bitrate,
			set_play_quality,
			set_frame_pacing,
			set_overlay,
			set_play_audio,
			reverse_play,
			set_window_fullscreen,
			session_password,
			new_password,
			respond_request,
			submit_password,
			disconnect_peer,
			list_connections,
			show_connections,
			input_pointer,
			input_button,
			input_scroll,
			input_key,
			kbd_capture_start,
			kbd_capture_stop,
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
