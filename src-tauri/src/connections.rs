//! The dedicated "active connections" management window — a separate, reusable
//! Tauri window (label `connections`) that lists all inbound connections and lets
//! the host disconnect them. Mirrors the Allow/Deny approval window (`auth.rs`):
//! it loads `index.html` with `window.__CONNECTIONS__=true` injected so the Svelte
//! app renders the `Connections` screen instead of the main shell.
//!
//! Focus policy is decided by the *incoming connection's mode* (set on the
//! `StreamReq`): a **Remote** connection brings the window forward (like the approval
//! popup); a **Game** connection opens it **hidden** so it neither steals focus from
//! the fullscreen game running on this host for the client nor appears in the client's
//! game stream. The host can reveal a hidden window via the sidebar button
//! (`show_connections`).

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::state::{AppState, ConnMode};

const LABEL: &str = "connections";

/// Open the connections window (if absent) or, when it already exists, bring it
/// forward only for a Remote connection. `bring_forward == false` (Game) opens a
/// brand-new window **hidden** and leaves an existing one untouched (background).
///
/// All window operations run on the main (event-loop) thread via
/// `run_on_main_thread`: this is called from a tokio worker thread (the serve loop),
/// and on Windows show/focus/close/build on a webview window only take effect on the
/// main thread (off-thread calls silently no-op).
pub(crate) fn open_or_update(app: &AppHandle, bring_forward: bool) {
	let handle = app.clone();
	let _ = app.run_on_main_thread(move || {
		if let Some(win) = handle.get_webview_window(LABEL) {
			if bring_forward {
				let _ = win.unminimize();
				let _ = win.show();
				let _ = win.set_focus();
			}
			return;
		}
		match WebviewWindowBuilder::new(&handle, LABEL, WebviewUrl::App("index.html".into()))
			.initialization_script("window.__CONNECTIONS__=true;")
			.title("Pulsar — Bağlantılar")
			.inner_size(360.0, 440.0)
			.min_inner_size(300.0, 240.0)
			.resizable(true)
			// Always real (in the taskbar + alt-tab so the host can find it), never
			// `visible(false)` (that hides it from both). `focused(false)` for Game so it
			// doesn't steal focus; it's then minimized below so it doesn't cover the game.
			.visible(true)
			.focused(bring_forward)
			.build()
		{
			Ok(win) => {
				if bring_forward {
					// Remote: bring it forward, like the approval popup.
					let _ = win.set_focus();
				} else {
					// Game: send it to the taskbar (minimized, unfocused) — present but not
					// covering the streamed game / stealing focus.
					let _ = win.minimize();
				}
			}
			Err(e) => tracing::warn!(%e, "connections window failed to open"),
		}
	});
}

/// Close the connections window if it exists (called when the last connection ends).
/// Runs on the main thread (see `open_or_update`).
pub(crate) fn close(app: &AppHandle) {
	let handle = app.clone();
	let _ = app.run_on_main_thread(move || {
		if let Some(win) = handle.get_webview_window(LABEL) {
			let _ = win.close();
		}
	});
}

/// One row of the connections window's list.
#[derive(serde::Serialize)]
pub(crate) struct ConnRow {
	pub(crate) peer: String,
	pub(crate) since_ms: u64,
	pub(crate) mode: ConnMode,
}

/// The connections window's initial snapshot (it then stays live via `session` events).
#[tauri::command]
pub(crate) async fn list_connections(state: State<'_, AppState>) -> Result<Vec<ConnRow>, String> {
	let g = state.active.lock().unwrap();
	Ok(g
		.iter()
		.map(|(peer, ci)| ConnRow {
			peer: peer.clone(),
			since_ms: ci.since_ms,
			mode: ci.mode,
		})
		.collect())
}

/// Sidebar "Bağlantılar" button → reveal/focus the (possibly hidden) window.
#[tauri::command]
pub(crate) async fn show_connections(app: AppHandle) -> Result<(), String> {
	open_or_update(&app, true);
	Ok(())
}
