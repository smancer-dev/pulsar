//! Linux native-renderer stdout readers: parse the `vidsink-fps …` perf-HUD lines
//! into `play-vstats` events and the `ov …` overlay-interaction lines into the
//! frontend overlay events. Split out of `render` to keep each file cohesive.
//!
//! Linux/X11 + Windows + macOS (the native `pulsar-render` stdout uses the same `vidsink-fps`/
//! `ov …` protocol on all three; on macOS the renderer is overlay-only, so it emits the `ov …`
//! interaction lines but no `vidsink-fps` video stats — both readers tolerate that) — see
//! `render` for the rest of the Linux renderer plumbing. The functions only use portable std
//! APIs (`ChildStdout` + `Emitter`), so they compile on every platform.
#![cfg(any(unix, target_os = "windows"))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::AppHandle;

use crate::events::PlayVStats;

/// Read `pulsar-vidsink`'s stdout (`vidsink-fps <fps> <w>x<h>` lines, ~1 Hz) and emit real
/// client fps to the overlay HUD. This is the vidsink's stats channel (it replaces mpv and
/// has no JSON-IPC socket). Unlike mpv 0.34, the vidsink reports a TRUE client fps. Runs on a
/// blocking thread (line-buffered pipe reads); exits when the child closes stdout (killed on
/// overlay-open or session end).
/// Linux-only: the legacy standalone-vidsink stats channel (the single-surface `pulsar-render`
/// path uses `start_render_reader` instead). Gated to Linux so Windows/macOS — which never call
/// it — don't trip a dead-code warning now that this module compiles on every platform.
#[cfg(all(unix, not(target_os = "macos")))]
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
///
/// `live_id`: when `Some`, the reader uses it to look up the CURRENT session id on every line —
/// so a resident renderer (kept alive across reconnects) can have its stats attributed to the new
/// session by storing the new id into the Arc before the next session starts. When `None`, `id`
/// is fixed for the lifetime of the reader (freshly-spawned, single-session renderer).
pub(crate) fn start_render_reader(
	app: &AppHandle,
	id: u64,
	stdout: std::process::ChildStdout,
	live_id: Option<Arc<AtomicU64>>,
) {
	use std::io::BufRead;
	use tauri::Emitter;
	let app = app.clone();
	std::thread::spawn(move || {
		let reader = std::io::BufReader::new(stdout);
		// First REAL frames (fps/bitrate > 0) ⇒ the stream is actually up — the UI keeps
		// its Connecting screen until this fires (one-shot per renderer process / session).
		// Reset on each reconnect (live_id path): the resident renderer starts streaming fresh
		// data, so play-ready must fire again for the new session id.
		let mut ready_sent_for: Option<u64> = None;
		let mut first_line_logged = false;
		'lines: for line in reader.lines() {
			let Ok(line) = line else { break };
			// Resolve the current session id: either live (resident reconnect) or fixed.
			let cur_id = live_id
				.as_ref()
				.map(|a| a.load(Ordering::Relaxed))
				.unwrap_or(id);
			if !first_line_logged {
				first_line_logged = true;
				tracing::info!(%line, "renderer first stdout line");
			}
			if let Some(rest) = line.strip_prefix("vidsink-fps ") {
				let mut it = rest.split_whitespace();
				let fps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				// The stats line carries the live stream dims ("<w>x<h>") — re-emit them as
				// play-dims EVERY tick (1 Hz, a dozen bytes). The one-shot vidsink-dims print —
				// and any change-deduped variant — races listener registration: a webview that
				// (re)mounts after the frame arrived (session restart, HMR, slow load) would
				// never get dims, leaving the input letterbox mapping at 0×0 = whole-canvas
				// coords → the remote cursor lands way off / the bars count as video. An
				// unconditional 1 Hz emit guarantees any listener has dims within a second,
				// and a rotating phone host's dim change propagates the same way.
				let dims = it.next().and_then(|s| {
					let (w, h) = s.split_once('x')?;
					Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
				});
				if let Some((w, h)) = dims {
					if w > 1 && h > 1 {
						let _ = app.emit("play-dims", (cur_id, w, h));
					}
				}
				let mbps = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				let ms = it.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
				tracing::debug!(fps, mbps, decode_ms = ms, "render stats");
				let _ = app.emit(
					"play-vstats",
					PlayVStats {
						id: cur_id,
						fps,
						drops: 0,
						mbps,
						decode_ms: ms,
					},
				);
				// Emit play-ready once per session id (reset when cur_id changes).
				if ready_sent_for != Some(cur_id) && (fps > 0.0 || mbps > 0.0) {
					ready_sent_for = Some(cur_id);
					let _ = app.emit("play-ready", cur_id);
				}
			} else if let Some(rest) = line.strip_prefix("vidsink-dims ") {
				// The STREAM's pixel size ("<w>x<h>", first frame / live res switch) — the
				// frontend sizes the windowed session to the host's aspect ratio from this.
				if let Some((w, h)) = rest.trim().split_once('x') {
					if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
						let _ = app.emit("play-dims", (cur_id, w, h));
					}
				}
			} else if let Some(rest) = line.strip_prefix("vidsink-dec ") {
				// The renderer's ACTUAL decoder ("vidsink-dec <name> <hw|sw|na>") — the UI
				// shows it read-only; there is no decoder picker (selection is automatic).
				let mut it = rest.split_whitespace();
				if let Some(name) = it.next() {
					let hw = it.next().unwrap_or("na").to_string();
					let _ = app.emit("play-decoder", (cur_id, name.to_string(), hw));
				}
			} else {
				let mut it = line.split_whitespace();
				match (it.next(), it.next()) {
					(Some("ov"), Some("set")) => {
						if let (Some(field), Some(val)) = (it.next(), it.next()) {
							tracing::info!(field, val, "overlay-cmd from renderer");
							// Mirror stream-selection fields into RenderSeed so a codec-switch
							// respawn can replay the user's choices onto the fresh renderer
							// (C14: the overlay would otherwise snap back to defaults).
							match field {
								"res" | "fps" | "bitrate" | "quality" | "display" => {
									use tauri::Manager;
									let state = app.state::<crate::state::AppState>();
									let plays = state.plays.lock().unwrap();
									if let Some(p) = plays.get(&cur_id) {
										let mut seed = p.render_seed.lock().unwrap();
										match field {
											"res" => seed.res = Some(val.to_string()),
											"fps" => seed.fps_sel = Some(val.to_string()),
											"bitrate" => seed.bitrate = Some(val.to_string()),
											"quality" => seed.quality = Some(val.to_string()),
											"display" => {
												if let Ok(idx) = val.parse::<u32>() {
													seed.display_idx = Some(idx);
												}
											}
											_ => {}
										}
									}
								}
								// "Dosya gönder" from the egui overlay Tools box: the click
								// happened in the renderer process, so there is no webview
								// user-activation and both WebView2 and WebKitGTK silently
								// block a programmatic <input type=file>.click(). Open the
								// native OS file picker Rust-side (rfd) and stream the chosen
								// file directly over the session data channel — no webview
								// gesture needed (C7). The rfd GTK backend needs the GTK
								// main thread (macOS: NSOpenPanel likewise), so this Rust-side
								// path is restricted to the two confirmed-broken platforms;
								// on others the overlay-cmd falls through to the webview path.
								"pickfile" => {
									#[cfg(any(windows, target_os = "linux"))]
									{
										use tauri::Manager;
										let state = app.state::<crate::state::AppState>();
										let tx = state
											.plays
											.lock()
											.unwrap()
											.get(&cur_id)
											.map(|p| p.data_tx.clone());
										if let Some(tx) = tx {
											std::thread::spawn(move || {
												// rfd::FileDialog::pick_file() is synchronous (fine
												// on a dedicated thread) and opens a real native
												// dialog — no webview activation required.
												if let Some(path) = rfd::FileDialog::new().pick_file() {
													// The reader thread lives outside the Tauri async
													// runtime; build a minimal single-thread runtime to
													// drive the async send_file_abs call.
													let rt =
														tokio::runtime::Builder::new_current_thread()
															.enable_all()
															.build();
													if let Ok(rt) = rt {
														rt.block_on(async {
															if crate::fs_browse::send_file_abs(&tx, &path)
																.await
																.is_none()
															{
																tracing::warn!(
																	?path,
																	"pickfile: send_file_abs failed"
																);
															}
														});
													}
												}
											});
											// Consume the field — do NOT fall through to the
											// overlay-cmd emit (that would route pickfile to
											// the webview where .click() is silently blocked).
											continue 'lines;
										}
									}
								}
								_ => {}
							}
							let _ =
								app.emit("overlay-cmd", (cur_id, field.to_string(), val.to_string()));
						}
					}
					(Some("ov"), Some("end")) => {
						let _ = app.emit("overlay-end", cur_id);
					}
					(Some("ov"), Some("close")) => {
						let _ = app.emit("overlay-close", cur_id);
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
							let _ = app.emit("overlay-chat", (cur_id, text.to_string()));
						}
					}
					// The overlay's Files box: open the per-session file-manager window
					// (the frontend supplies the peer label and invokes the command).
					(Some("ov"), Some("files")) => {
						let _ = app.emit("overlay-files", cur_id);
					}
					// Native Files (remote pane): list / download / upload requests.
					(Some("ov"), Some(op @ ("fsls" | "fsget" | "fssend"))) => {
						let path = line.splitn(3, ' ').nth(2).unwrap_or("").trim();
						// fsls "" = the host's HOME — an empty path is valid there.
						if op == "fsls" || !path.is_empty() {
							let _ = app.emit("overlay-fs", (cur_id, op.to_string(), path.to_string()));
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
