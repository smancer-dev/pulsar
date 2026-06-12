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
mod avatar;
mod caps;
mod commands;
mod connections;
mod events;
mod files;
mod files_window;
mod i18n;
mod fs_browse;
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
use avatar::{device_user_name, self_avatar};
use commands::{
	auto_connect_target, available_encoders, connect, controllers, get_config, lan_devices,
	launch_remote_game, list_audio_sources, list_remote_games, local_ip, new_password, node_port,
	publish_games,
	relaunch_to_home, run_command, scan_folder, session_password, set_config, set_language,
	set_stream_settings,
	steam_path,
};
use connections::{list_connections, set_view_only, show_connections};
use files_window::open_files_window;
use fs_browse::local_ls;
use host::go_online;
use io_cmds::{
	chat_log, host_send_chat, input_button, input_key, input_pointer, input_scroll,
	kbd_capture_start, kbd_capture_stop, kbd_engage, mic_start, mic_stop, native_view_rect,
	read_clipboard_text, send_chat, send_clipboard, send_file, send_file_path,
	set_window_fullscreen, write_clipboard_text,
};
use play::{start_remote_play, stop_stream};
use session_cmds::{
	fs_get, fs_list, render_chat, render_fs, render_hint, render_kin, render_toast, reverse_play,
	set_frame_pacing, set_overlay, set_overlay_button, set_overlay_button_pos, set_play_audio,
	set_play_bitrate, set_play_codec, set_play_encoder, set_play_fps, set_play_quality,
	set_play_resolution, set_stats_hud,
};

// Headless `pulsar --relay` mode lives in its own module to keep this file focused.
mod relay_mode;
pub use relay_mode::run_relay;

/// Per-window focus map driving the global input-capture gate (`kbdhook::set_focused`),
/// OR-ed across ALL Pulsar windows so an app-internal focus handoff (main → approval
/// popup / connections window) isn't treated as "unfocused" (that cleared the ENGAGED
/// latch mid-session). Module-scoped because BOTH the `Focused` and `Destroyed` arms
/// below must maintain it: tao's GTK backend doesn't guarantee a final Focused(false)
/// for a window destroyed while focused (the approve popup is closed programmatically
/// right after its Allow click), and a stale `true` would pin the evdev grab/combos on
/// after Pulsar loses focus — while unique approve-N labels grew the map forever.
static WIN_FOCUS: std::sync::Mutex<Option<std::collections::HashMap<String, bool>>> =
	std::sync::Mutex::new(None);

/// Recompute the OR of all per-window focus states and push it to the capture gate.
fn refresh_focus_gate(map: &std::collections::HashMap<String, bool>) {
	kbdhook::set_focused(map.values().any(|f| *f));
}

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

			// Startup capability probe (encode + decode), Moonlight-style: every launch,
			// in the background; the frontend splash waits for the `local-caps` event.
			crate::caps::spawn_startup_probe(app.handle().clone());

			// Kiosk auto-engage is ONE-SHOT: arm it only when THIS launch will actually
			// auto-connect. AUTO_CONNECT alone is the wrong key — app.restart() preserves
			// argv, so it stays Some for every later manual session of a `--connect`
			// process — and the `.skip-autoconnect` marker (relaunch_to_home; consumed
			// later by auto_connect_target) means the frontend will NOT auto-connect.
			{
				let auto = util::AUTO_CONNECT.get().map_or(false, |t| t.is_some());
				let skip = util::config_path(app.handle())
					.with_file_name(".skip-autoconnect")
					.exists();
				if auto && !skip {
					kbdhook::arm_kiosk_engage();
				}
			}

			// Load persisted config (relay endpoint, network mode, etc.).
			let cfg = pulsar_core::config::Config::load(util::config_path(app.handle()));
			tracing::info!(relay = %cfg.relay, "config loaded");
			crate::i18n::set_lang(&cfg.language);
			*app.state::<AppState>().config.lock().unwrap() = cfg;

			// System tray: once launched, Pulsar stays resident in the tray. Closing
			// the window hides it (see on_window_event); the only full exit is the
			// tray's quit item. (Tray labels pick the language at startup — a language
			// change applies to them after a restart.)
			let show = MenuItem::with_id(app, "show", crate::i18n::t("tray.show"), true, None::<&str>)?;
			let quit = MenuItem::with_id(app, "quit", crate::i18n::t("tray.quit"), true, None::<&str>)?;
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
				// Main window only — the approval popup (auth.rs) is always-on-top by design
				// and must not have its flag cleared here. Outside fullscreen this actively
				// clears topmost, self-healing any path that left the flag set.
				if window.label() == "main" {
					let fs = window.state::<AppState>().fs_geom.lock().unwrap().is_some();
					let _ = window.set_always_on_top(*focused && fs);
				}
				// Capture/forward keyboard+mouse + the overlay/leave combos ONLY while
				// SOME Pulsar window is focused (Linux evdev grab is otherwise global).
				// See WIN_FOCUS for why the map is OR-ed across all windows.
				{
					let mut g = WIN_FOCUS.lock().unwrap();
					let map = g.get_or_insert_with(Default::default);
					map.insert(window.label().to_string(), *focused);
					refresh_focus_gate(map);
				}
				// Losing focus while the overlay is open would otherwise STRAND it: the combo is
				// now focus-gated off, so it couldn't be closed. Auto-close it on blur so the
				// state resets — refocusing then re-enables the combo cleanly.
				if !*focused {
					let _ = window.emit("window-blur", ());
				}
			}
			// A destroyed window must leave the focus map (no Focused(false) is
			// guaranteed first — see WIN_FOCUS): drop its entry and recompute the
			// gate so a popup that died focused can't hold the global grab on.
			WindowEvent::Destroyed => {
				let mut g = WIN_FOCUS.lock().unwrap();
				if let Some(map) = g.as_mut() {
					if map.remove(window.label()).is_some() {
						refresh_focus_gate(map);
					}
				}
			}
			_ => {}
		})
		.invoke_handler(tauri::generate_handler![
			get_config,
			set_config,
			set_language,
			go_online,
			connect,
			lan_devices,
			controllers,
			local_ip,
			node_port,
			auto_connect_target,
			relaunch_to_home,
			steam_path,
			scan_folder,
			run_command,
			publish_games,
			list_remote_games,
			launch_remote_game,
			available_encoders,
			list_audio_sources,
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
			set_stats_hud,
			set_overlay_button,
			set_overlay_button_pos,
			render_hint,
			render_toast,
			render_chat,
			render_fs,
			render_kin,
			set_overlay,
			set_play_audio,
			reverse_play,
			set_window_fullscreen,
			kbd_engage,
			native_view_rect,
			crate::caps::local_caps,
			self_avatar,
			device_user_name,
			session_password,
			new_password,
			respond_request,
			submit_password,
			disconnect_peer,
			list_connections,
			show_connections,
			open_files_window,
			set_view_only,
			input_pointer,
			input_button,
			input_scroll,
			input_key,
			kbd_capture_start,
			kbd_capture_stop,
			send_clipboard,
			read_clipboard_text,
			write_clipboard_text,
			send_chat,
			host_send_chat,
			chat_log,
			send_file,
			send_file_path,
			fs_list,
			fs_get,
			local_ls,
			mic_start,
			mic_stop
		])
		.run(tauri::generate_context!())
		.expect("error while running Pulsar");
}
