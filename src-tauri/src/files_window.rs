//! Per-session file-manager window — one separate Tauri window per remote-play
//! session (label `files-<playId>`), mirroring the connections window pattern:
//! it loads `index.html` with `window.__FILES__={id,peer}` injected so the Svelte
//! app renders the standalone Files screen instead of the main shell. The title
//! carries the peer so multiple sessions stay distinguishable; the window is
//! force-closed when its session ends (`play-ended` in `play/hold.rs`).

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

fn label(id: u64) -> String {
	format!("files-{id}")
}

/// Open (or focus) the file-manager window for play session `id`. `peer` is the
/// display name shown in the title + header so the user knows whose files these are.
#[tauri::command]
pub(crate) async fn open_files_window(app: AppHandle, id: u64, peer: String) -> Result<(), String> {
	let handle = app.clone();
	let _ = app.run_on_main_thread(move || {
		let lbl = label(id);
		if let Some(win) = handle.get_webview_window(&lbl) {
			let _ = win.unminimize();
			let _ = win.show();
			let _ = win.set_focus();
			return;
		}
		// JSON-encode the peer name so quotes/backslashes can't break the script.
		let peer_js = serde_json::to_string(&peer).unwrap_or_else(|_| "\"\"".into());
		let title = format!("{} — {}", crate::i18n::t("title.files"), peer);
		match WebviewWindowBuilder::new(&handle, &lbl, WebviewUrl::App("index.html".into()))
			.initialization_script(format!("window.__FILES__={{id:{id},peer:{peer_js}}};"))
			.title(&title)
			.inner_size(780.0, 540.0)
			.min_inner_size(520.0, 360.0)
			.resizable(true)
			.build()
		{
			Ok(win) => {
				let _ = win.set_focus();
			}
			Err(e) => tracing::warn!(%e, "files window failed to open"),
		}
	});
	Ok(())
}

/// Close session `id`'s file window if it exists — called when the session ends so
/// a dead session never leaves an orphaned file manager. Main-thread like the rest
/// (off-thread window ops silently no-op on Windows).
pub(crate) fn close(app: &AppHandle, id: u64) {
	let handle = app.clone();
	let _ = app.run_on_main_thread(move || {
		if let Some(win) = handle.get_webview_window(&label(id)) {
			let _ = win.close();
		}
	});
}
