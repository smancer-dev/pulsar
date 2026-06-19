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

/// How to surface the connections window, decided by the incoming connection's mode.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Surface {
	/// Brand-new window opens MINIMIZED (taskbar only); an existing window left as-is.
	/// Used at accept time, before the mode is known.
	Background,
	/// Un-minimize + show but do NOT steal focus — a GAME first-stream: the host gets a
	/// visible indication without yanking focus off a fullscreen game it may be playing.
	Reveal,
	/// Un-minimize + show + focus — a REMOTE first-stream, and the sidebar reveal button.
	Forward,
}

/// Open the connections window (if absent) or, when it already exists, bring it
/// forward only for a Remote connection. `bring_forward == false` (Game) opens a
/// brand-new window **hidden** and leaves an existing one untouched (background).
///
/// All window operations run on the main (event-loop) thread via
/// `run_on_main_thread`: this is called from a tokio worker thread (the serve loop),
/// and on Windows show/focus/close/build on a webview window only take effect on the
/// main thread (off-thread calls silently no-op).
pub(crate) fn open_or_update(app: &AppHandle, surface: Surface) {
	let handle = app.clone();
	let _ = app.run_on_main_thread(move || {
		if let Some(win) = handle.get_webview_window(LABEL) {
			match surface {
				Surface::Forward => {
					let _ = win.unminimize();
					let _ = win.show();
					let _ = win.set_focus();
				}
				// Game first-stream: make it visible (un-minimize + show) WITHOUT focus, so
				// the host sees the active-connection list without losing the game.
				Surface::Reveal => {
					let _ = win.unminimize();
					let _ = win.show();
				}
				Surface::Background => {}
			}
			return;
		}
		match WebviewWindowBuilder::new(&handle, LABEL, WebviewUrl::App("index.html".into()))
			.initialization_script("window.__CONNECTIONS__=true;")
			.title(crate::i18n::t("title.connections"))
			.inner_size(360.0, 440.0)
			.min_inner_size(300.0, 240.0)
			.resizable(true)
			// Always real (in the taskbar + alt-tab so the host can find it), never
			// `visible(false)`. Only a Forward surface takes focus on build.
			.visible(true)
			.focused(surface == Surface::Forward)
			.build()
		{
			Ok(win) => match surface {
				// Remote: bring it forward, like the approval popup.
				Surface::Forward => {
					let _ = win.set_focus();
				}
				// Game first-stream: leave it visible+unfocused as built (a present indication).
				Surface::Reveal => {}
				// Accept time (mode unknown): send it to the taskbar so it doesn't cover the
				// streamed game / steal focus until the first stream upgrades it.
				Surface::Background => {
					let _ = win.minimize();
				}
			},
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

/// One row of the connections window's list — ONE per live SESSION (not per device).
/// A single client device with two concurrent sessions (couch co-op / split panes)
/// produces two rows that share `peer` but differ by `sid`.
#[derive(serde::Serialize)]
pub(crate) struct ConnRow {
	/// This session's id — the key the kick / view-only actions pass back so they
	/// target exactly this session, not a sibling pane from the same device.
	pub(crate) sid: u64,
	/// The connected device's grouping key (relay id or ip:port). Shared across a
	/// device's concurrent sessions, so the UI can group rows by device.
	pub(crate) peer: String,
	pub(crate) since_ms: u64,
	pub(crate) mode: ConnMode,
	pub(crate) view_only: bool,
	/// Pushed identity decorations (`DataMsg::PeerName`/`Avatar`), when the client
	/// sent them — so a window opened AFTER the push still shows who connected.
	pub(crate) name: Option<String>,
	pub(crate) avatar: Option<String>,
	/// The client's own relay device ID (`DataMsg::PeerId`), when pushed — shown
	/// instead of the `peer` ip:port on direct/same-LAN connects.
	pub(crate) client_id: Option<String>,
}

/// The connections window's initial snapshot (it then stays live via `session` events).
/// `active` is keyed by sid now, so this yields one row PER SESSION; identity
/// decorations (`peer_meta`/`peer_ids`) are per-device, looked up by each row's `peer`.
#[tauri::command]
pub(crate) async fn list_connections(state: State<'_, AppState>) -> Result<Vec<ConnRow>, String> {
	let meta = state.peer_meta.lock().unwrap().clone();
	let ids = state.peer_ids.lock().unwrap().clone();
	let g = state.active.lock().unwrap();
	Ok(g.iter()
		.map(|(sid, ci)| {
			let (name, avatar) = meta.get(&ci.peer).cloned().unwrap_or((None, None));
			ConnRow {
				sid: *sid,
				peer: ci.peer.clone(),
				since_ms: ci.since_ms,
				mode: ci.mode,
				view_only: ci.view_only,
				name,
				avatar,
				client_id: ids.get(&ci.peer).cloned(),
			}
		})
		.collect())
}

/// "Sadece izleme" toggle: revoke/restore a connected SESSION's CONTROL — the serve
/// loop's input handler drops its events while set; the stream keeps running. Keyed by
/// the row's `sid` so one pane of a same-host co-op pair can be view-only-d alone.
#[tauri::command]
pub(crate) async fn set_view_only(
	state: State<'_, AppState>,
	sid: u64,
	on: bool,
) -> Result<(), String> {
	if let Some(ci) = state.active.lock().unwrap().get_mut(&sid) {
		ci.view_only = on;
	}
	Ok(())
}

/// Sidebar "Bağlantılar" button → reveal/focus the (possibly hidden) window.
#[tauri::command]
pub(crate) async fn show_connections(app: AppHandle) -> Result<(), String> {
	open_or_update(&app, Surface::Forward);
	Ok(())
}
