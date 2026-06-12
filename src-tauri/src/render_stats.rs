//! Linux native-renderer stdout readers: parse the `vidsink-fps …` perf-HUD lines
//! into `play-vstats` events and the `ov …` overlay-interaction lines into the
//! frontend overlay events. Split out of `render` to keep each file cohesive.
//!
//! Linux/X11 + Windows (the native `pulsar-render` stdout uses the same `vidsink-fps`/`ov …`
//! protocol on both) — see `render` for the rest of the Linux renderer plumbing.
#![cfg(any(all(unix, not(target_os = "macos")), target_os = "windows"))]

use std::sync::{Arc, Mutex};

use tauri::AppHandle;

use crate::events::PlayVStats;

/// Read `pulsar-vidsink`'s stdout (`vidsink-fps <fps> <w>x<h>` lines, ~1 Hz) and emit real
/// client fps to the overlay HUD. This is the vidsink's stats channel (it replaces mpv and
/// has no JSON-IPC socket). Unlike mpv 0.34, the vidsink reports a TRUE client fps. Runs on a
/// blocking thread (line-buffered pipe reads); exits when the child closes stdout (killed on
/// overlay-open or session end).
pub(crate) fn start_vidsink_stats(
	app: &AppHandle,
	id: u64,
	stdout: std::process::ChildStdout,
	overlay_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
) {
	use std::io::{BufRead, Write};
	use tauri::Emitter;
	let app = app.clone();
	std::thread::spawn(move || {
		let reader = std::io::BufReader::new(stdout);
		for line in reader.lines() {
			let Ok(line) = line else { break };
			// "vidsink-fps <fps> <w>x<h> <mbit> <ms>"
			if let Some(rest) = line.strip_prefix("vidsink-fps ") {
				let mut it = rest.split_whitespace();
				let fps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let _dims = it.next();
				let mbps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let ms = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let _ = app.emit(
					"play-vstats",
					PlayVStats {
						id,
						fps,
						drops: 0,
						mbps,
						decode_ms: ms,
					},
				);
				// Feed the native overlay HUD: stat <fps> <latency> <decode> <mbps>. The vidsink's
				// real metric is the pipeline-buffer depth (ms) — use it for both latency + decode.
				if let Some(si) = overlay_stdin.lock().unwrap().as_mut() {
					let _ = writeln!(si, "stat {fps:.0} {ms:.0} {ms:.0} {mbps:.1}");
				}
			}
		}
	});
}

/// Read the single-surface `pulsar-render`'s stdout: BOTH the perf-HUD stats (`vidsink-fps <fps>
/// <wxh> <mbit> <ms>`) → `play-vstats`, AND the overlay interactions (`ov set/end/close`) →
/// frontend events. One process, one stdout, both line kinds.
pub(crate) fn start_render_reader(app: &AppHandle, id: u64, stdout: std::process::ChildStdout) {
	use std::io::BufRead;
	use tauri::Emitter;
	let app = app.clone();
	std::thread::spawn(move || {
		let reader = std::io::BufReader::new(stdout);
		// First REAL frames (fps/bitrate > 0) ⇒ the stream is actually up — the UI keeps
		// its Connecting screen until this fires (one-shot per renderer process).
		let mut ready_sent = false;
		let mut first_line_logged = false;
		for line in reader.lines() {
			let Ok(line) = line else { break };
			if !first_line_logged {
				first_line_logged = true;
				tracing::info!(%line, "renderer first stdout line");
			}
			if let Some(rest) = line.strip_prefix("vidsink-fps ") {
				let mut it = rest.split_whitespace();
				let fps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let _dims = it.next();
				let mbps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let ms = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				tracing::debug!(fps, mbps, decode_ms = ms, "render stats");
				let _ = app.emit(
					"play-vstats",
					PlayVStats {
						id,
						fps,
						drops: 0,
						mbps,
						decode_ms: ms,
					},
				);
				if !ready_sent && (fps > 0.0 || mbps > 0.0) {
					ready_sent = true;
					let _ = app.emit("play-ready", id);
				}
			} else if let Some(rest) = line.strip_prefix("vidsink-dims ") {
				// The STREAM's pixel size ("<w>x<h>", first frame / live res switch) — the
				// frontend sizes the windowed session to the host's aspect ratio from this.
				if let Some((w, h)) = rest.trim().split_once('x') {
					if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
						let _ = app.emit("play-dims", (id, w, h));
					}
				}
			} else if let Some(rest) = line.strip_prefix("vidsink-dec ") {
				// The renderer's ACTUAL decoder ("vidsink-dec <name> <hw|sw|na>") — the UI
				// shows it read-only; there is no decoder picker (selection is automatic).
				let mut it = rest.split_whitespace();
				if let Some(name) = it.next() {
					let hw = it.next().unwrap_or("na").to_string();
					let _ = app.emit("play-decoder", (id, name.to_string(), hw));
				}
			} else {
				let mut it = line.split_whitespace();
				match (it.next(), it.next()) {
					(Some("ov"), Some("set")) => {
						if let (Some(field), Some(val)) = (it.next(), it.next()) {
							let _ =
								app.emit("overlay-cmd", (id, field.to_string(), val.to_string()));
						}
					}
					(Some("ov"), Some("end")) => {
						let _ = app.emit("overlay-end", id);
					}
					(Some("ov"), Some("close")) => {
						let _ = app.emit("overlay-close", id);
					}
					// The renderer's own overlay-open button was clicked (platforms where
					// the closed-state renderer receives pointer events).
					(Some("ov"), Some("toggle")) => {
						let _ = app.emit("overlay-toggle", ());
					}
					// Native Chat: the user sent a line from the overlay composer
					// (rest-of-line payload; the frontend forwards it to the host AND
					// echoes it back over stdin so the renderer's log stays canonical).
					(Some("ov"), Some("chat")) => {
						let text = line.splitn(3, ' ').nth(2).unwrap_or("").trim();
						if !text.is_empty() {
							let _ = app.emit("overlay-chat", (id, text.to_string()));
						}
					}
					// The overlay's Files box: open the per-session file-manager window
					// (the frontend supplies the peer label and invokes the command).
					(Some("ov"), Some("files")) => {
						let _ = app.emit("overlay-files", id);
					}
					// Native Files (remote pane): list / download / upload requests.
					(Some("ov"), Some(op @ ("fsls" | "fsget" | "fssend"))) => {
						let path = line.splitn(3, ' ').nth(2).unwrap_or("").trim();
						// fsls "" = the host's HOME — an empty path is valid there.
						if op == "fsls" || !path.is_empty() {
							let _ = app.emit("overlay-fs", (id, op.to_string(), path.to_string()));
						}
					}
					// Standalone render window (no --wid embed, e.g. Wayland client): it is a
					// separate toplevel, so its focus + video clicks arrive here and feed the
					// evdev capture's focus/engage gates (see kbdhook::linux).
					(Some("ov"), Some("focus")) => {
						crate::kbdhook::set_render_focused(it.next() == Some("1"));
					}
					(Some("ov"), Some("engage")) => {
						crate::kbdhook::engage_render(&app);
					}
					_ => {}
				}
			}
		}
	});
}

/// Read `pulsar-render`'s stdout — the user's overlay interactions — and forward them to the
/// frontend (which calls the existing setters → host) so the overlay drives the real session:
/// `ov set <field> <val>` → `overlay-cmd`; `ov end` → `overlay-end`; `ov close` → `overlay-close`.
pub(crate) fn start_overlay_cmd_reader(
	app: &AppHandle,
	id: u64,
	stdout: std::process::ChildStdout,
) {
	use std::io::BufRead;
	use tauri::Emitter;
	let app = app.clone();
	std::thread::spawn(move || {
		let reader = std::io::BufReader::new(stdout);
		for line in reader.lines() {
			let Ok(line) = line else { break };
			let mut it = line.split_whitespace();
			match (it.next(), it.next()) {
				(Some("ov"), Some("set")) => {
					if let (Some(field), Some(val)) = (it.next(), it.next()) {
						let _ = app.emit("overlay-cmd", (id, field.to_string(), val.to_string()));
					}
				}
				(Some("ov"), Some("end")) => {
					let _ = app.emit("overlay-end", id);
				}
				(Some("ov"), Some("close")) => {
					let _ = app.emit("overlay-close", id);
				}
				_ => {}
			}
		}
	});
}
