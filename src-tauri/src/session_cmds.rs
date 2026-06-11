//! Live, in-session client stream-tuning commands: the re-config setters
//! (resolution / encoder / codec / fps / bitrate / quality / audio), frame pacing,
//! the gaming-overlay toggle, and the reverse-direction request.

use pulsar_core::service::{DataMsg, QualityPref};
use tauri::{AppHandle, State};

use crate::kbdhook;
use crate::state::{AppState, Restream};
use crate::util::{client_auto_fps, data_sender};

/// Client: change the resolution of an active remote-play session on the fly. The
/// host kills its current ffmpeg and restarts capture at the new size. `0`/`0`
/// means "let the host use its configured size".
#[tauri::command]
pub(crate) async fn set_play_resolution(
	state: State<'_, AppState>,
	id: u64,
	width: u32,
	height: u32,
) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Resolution(width, height))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: switch the host's video encoder of an active session on the fly (the host
/// restarts ffmpeg with it; an unavailable encoder degrades gracefully). Value is
/// "auto"/"nvenc"/"amf"/"qsv"/"vaapi"/"videotoolbox"/"software".
#[tauri::command]
pub(crate) async fn set_play_encoder(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	encoder: String,
) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Encoder(encoder))
			.await
			.map_err(|e| e.to_string())?;
	}
	// Same codec, new host encoder: the renderer keeps running, but hide the container
	// briefly so the webview's "switching" veil is visible over the restart hiccup.
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		let app2 = app.clone();
		tokio::spawn(async move {
			crate::render::set_container_visible(&app2, id, false);
			tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
			crate::render::set_container_visible(&app2, id, true);
		});
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	let _ = &app;
	Ok(())
}

/// Client: switch the video codec of an active session on the fly ("h264"/"h265"/"av1").
/// The host restarts ffmpeg with it; the client's WebCodecs decoder re-derives its codec
/// string from the new stream's SPS automatically.
#[tauri::command]
pub(crate) async fn set_play_codec(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	codec: String,
) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Codec(codec.clone()))
			.await
			.map_err(|e| e.to_string())?;
	}
	// The native renderer's depacketizer/decoder were fixed at spawn from the OLD
	// SDP — a codec switch needs a renderer RESPAWN with a rewritten SDP (the host
	// re-encodes to the same port). On Linux the container is hidden for the gap so
	// the webview's "switching" veil is actually visible; on Windows the renderer is
	// a child HWND with no container (the webview shows through while it's down).
	// RK3588 keeps the old renderer (killing it corrupts WebKit's shared Mali GL —
	// known platform constraint).
	#[cfg(any(
		all(unix, not(target_os = "macos"), not(target_arch = "aarch64")),
		windows
	))]
	respawn_render_for_codec(&app, &state, id, &codec).await;
	#[cfg(not(any(
		all(unix, not(target_os = "macos"), not(target_arch = "aarch64")),
		windows
	)))]
	let _ = &app;
	Ok(())
}

/// Kill + respawn the native renderer with an SDP rewritten for `codec` (live codec
/// switch). Linux hides the in-app container during the gap (webview veil shows
/// through); Windows re-resolves the app HWND and respawns the child renderer there.
#[cfg(any(
	all(unix, not(target_os = "macos"), not(target_arch = "aarch64")),
	windows
))]
async fn respawn_render_for_codec(
	app: &AppHandle,
	state: &State<'_, AppState>,
	id: u64,
	codec: &str,
) {
	let (vport, wid, game_mode, old_child) = {
		let mut plays = state.plays.lock().unwrap();
		let Some(p) = plays.get_mut(&id) else { return };
		(p.video_port, p.mpv_wid, p.game_mode, p.render_child.take())
	};
	if old_child.is_none() {
		return; // mpv/ffplay fallback paths keep their old behavior
	}
	#[cfg(not(windows))]
	crate::render::set_container_visible(app, id, false);
	if let Some(mut c) = old_child {
		crate::play::stop_render_child(&mut c);
	}
	let Ok(sdp) = crate::native_view::write_sdp(vport, codec) else {
		#[cfg(not(windows))]
		crate::render::set_container_visible(app, id, true);
		return;
	};
	let pace_default = std::env::var("PULSAR_PACE")
		.map(|v| v == "1" || v == "on" || v == "true")
		.unwrap_or(true);
	let rbin = crate::process::render_bin(app);
	#[cfg(not(windows))]
	let mut rc = crate::native_view::spawn_render(&rbin, &sdp, wid, game_mode, pace_default);
	#[cfg(windows)]
	let mut rc = {
		let _ = wid; // X11 container XID — Linux-only; the HWND is re-resolved fresh
		crate::process::window_hwnd(app).and_then(|h| {
			crate::native_view::spawn_render_win(&rbin, &sdp, h, game_mode, pace_default)
		})
	};
	if let Some(c) = rc.as_mut() {
		if let Some(out) = c.stdout.take() {
			crate::render_stats::start_render_reader(app, id, out);
		}
		let si = c.stdin.take();
		let mut plays = state.plays.lock().unwrap();
		if let Some(p) = plays.get_mut(&id) {
			*p.render_stdin.lock().unwrap() = si;
			p.mpv_sdp = Some(sdp);
			// Re-seed the fresh renderer's overlay: take the last caps line, update its
			// codec=… field to the new codec, store + send it.
			let mut line = p.caps_line.lock().unwrap().clone();
			if !line.is_empty() {
				line = line
					.split_whitespace()
					.map(|kv| {
						if kv.starts_with("codec=") {
							format!("codec={codec}")
						} else {
							kv.to_string()
						}
					})
					.collect::<Vec<_>>()
					.join(" ");
				use std::io::Write as _;
				if let Some(si) = p.render_stdin.lock().unwrap().as_mut() {
					let _ = writeln!(si, "{line}");
				}
				*p.caps_line.lock().unwrap() = line;
			}
			// Re-push stdin-only overlay state the fresh process would otherwise
			// reset to defaults: open-button toggle + position, stats HUD, frame
			// pacing, view fit and the audio truth line.
			let seed = p.render_seed.lock().unwrap().clone();
			{
				use std::io::Write as _;
				if let Some(si) = p.render_stdin.lock().unwrap().as_mut() {
					if let Some(on) = seed.ovbtn {
						let _ = writeln!(si, "ovbtn {}", if on { 1 } else { 0 });
					}
					if let Some((x, y)) = seed.ovbtn_pos {
						let _ = writeln!(si, "ovbtnpos {x} {y}");
					}
					if let Some(on) = seed.statshud {
						let _ = writeln!(si, "statshud {}", if on { 1 } else { 0 });
					}
					if let Some(on) = seed.pace {
						let _ = writeln!(si, "pace {}", if on { 1 } else { 0 });
					}
					if let Some(fit) = seed.fit.as_deref() {
						let _ = writeln!(si, "fit {fit}");
					}
					if let Some((tx, mute, mic)) = seed.audio {
						let _ = writeln!(
							si,
							"audio tx={} mute={} mic={}",
							if tx { 1 } else { 0 },
							if mute { 1 } else { 0 },
							if mic { 1 } else { 0 }
						);
					}
					let _ = si.flush();
				}
			}
		}
	}
	let respawned = rc.is_some();
	let orphaned = {
		let mut plays = state.plays.lock().unwrap();
		match plays.get_mut(&id) {
			Some(p) => {
				p.render_child = rc.take();
				false
			}
			None => true,
		}
	};
	if orphaned {
		// stop_stream removed the play between the spawn and this re-lock: the fresh
		// renderer has no owner, and dropping a Child does NOT kill the process — reap
		// it here or it holds its UDP port + GL context until app exit (the documented
		// orphan-pile-up class, see native_view/spawn.rs).
		if let Some(mut c) = rc {
			crate::play::stop_render_child(&mut c);
		}
		return;
	}
	tracing::info!(id, codec, respawned, "renderer respawned for codec switch");
	// Keep the container hidden until the new renderer has its first frames (host
	// re-sends an IDR immediately on restart) — revealing earlier shows its black
	// GL clear instead of the webview's switching veil. (Linux only: Windows has
	// no container to reveal.)
	#[cfg(not(windows))]
	{
		tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
		crate::render::set_container_visible(app, id, true);
	}
}

/// Client: change the frame rate of an active session on the fly (0 = host default).
#[tauri::command]
pub(crate) async fn set_play_fps(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	fps: u32,
) -> Result<(), String> {
	// 0 = "auto" → target the client's display refresh (nearest of 30/60/120).
	let fps = if fps == 0 {
		client_auto_fps(&app).await
	} else {
		fps
	};
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Fps(fps))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: change the target bitrate (kbit/s) of an active session on the fly
/// (0 = host default). The host restarts ffmpeg with the new bitrate. The UI converts
/// Mbit → kbps (×1000) before invoking.
#[tauri::command]
pub(crate) async fn set_play_bitrate(
	state: State<'_, AppState>,
	id: u64,
	kbps: u32,
) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Bitrate(kbps))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: change the quality/perf bias of an active session on the fly. Parses
/// "latency" / "balanced" / "quality" → [`QualityPref`] (unknown → `Balanced`) and
/// re-requests the stream. The UI exposes only latency | quality.
#[tauri::command]
pub(crate) async fn set_play_quality(
	state: State<'_, AppState>,
	id: u64,
	quality: String,
) -> Result<(), String> {
	let pref = match quality.as_str() {
		"latency" => QualityPref::Latency,
		"quality" => QualityPref::Quality,
		_ => QualityPref::Balanced,
	};
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Quality(pref))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client (Linux native renderer): toggle Moonlight-style frame pacing live. Writes a
/// `pace 0|1` line to the `pulsar-render` child's stdin (the same channel the HUD `stat`
/// lines use); the renderer flips its present path between FIFO-drain (smooth) and
/// newest-wins (low latency) with no respawn. No-op when the session has no render process
/// (Windows ffplay / mpv fallback / macOS → render_stdin is None), so it's safe on any
/// platform; both the frontend Settings/overlay toggle and the egui overlay's own toggle
/// (round-tripped via overlay-cmd) call this.
#[tauri::command]
pub(crate) async fn set_frame_pacing(
	state: State<'_, AppState>,
	id: u64,
	on: bool,
) -> Result<(), String> {
	let play = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| (p.render_stdin.clone(), p.render_seed.clone()));
	if let Some((stdin, seed)) = play {
		seed.lock().unwrap().pace = Some(on);
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "pace {}", if on { 1 } else { 0 });
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: toggle the always-on mini stats HUD on the native renderer ("statshud
/// 0|1" stdin line) — visible while the overlay is closed. Persisted by the UI;
/// remembered in `render_seed` so a codec-switch respawn re-applies it.
#[tauri::command]
pub(crate) async fn set_stats_hud(
	state: State<'_, AppState>,
	id: u64,
	on: bool,
) -> Result<(), String> {
	let play = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| (p.render_stdin.clone(), p.render_seed.clone()));
	if let Some((stdin, seed)) = play {
		seed.lock().unwrap().statshud = Some(on);
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "statshud {}", if on { 1 } else { 0 });
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: toggle the Parsec-style always-visible overlay-open button on the native
/// renderer ("ovbtn 0|1" stdin line). Persisted by the UI; remembered in `render_seed`
/// so a codec-switch respawn re-applies it.
#[tauri::command]
pub(crate) async fn set_overlay_button(
	state: State<'_, AppState>,
	id: u64,
	on: bool,
) -> Result<(), String> {
	let play = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| (p.render_stdin.clone(), p.render_seed.clone()));
	if let Some((stdin, seed)) = play {
		seed.lock().unwrap().ovbtn = Some(on);
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "ovbtn {}", if on { 1 } else { 0 });
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: move the overlay-open button ("ovbtnpos <x> <y>" stdin line, egui points
/// from the renderer's top-left). Streamed live while the webview hotspot is dragged;
/// the UI persists the final spot. Remembered in `render_seed` for respawns.
#[tauri::command]
pub(crate) async fn set_overlay_button_pos(
	state: State<'_, AppState>,
	id: u64,
	x: f32,
	y: f32,
) -> Result<(), String> {
	let play = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| (p.render_stdin.clone(), p.render_seed.clone()));
	if let Some((stdin, seed)) = play {
		seed.lock().unwrap().ovbtn_pos = Some((x, y));
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "ovbtnpos {x} {y}");
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: push a transient helper tooltip to the native renderer ("hint engage|click"
/// stdin line, drawn bottom-center ~3 s) and sync the live engage state ("engaged 0|1"
/// — the renderer hides the local cursor ONLY while engaged). The engage/release edges
/// live app-side (evdev), so the frontend forwards them here. No-op without a renderer.
#[tauri::command]
pub(crate) async fn render_hint(
	state: State<'_, AppState>,
	id: u64,
	kind: String,
) -> Result<(), String> {
	let stdin = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let engage = kind == "engage";
			let _ = writeln!(si, "engaged {}", if engage { 1 } else { 0 });
			let _ = writeln!(si, "hint {}", if engage { "engage" } else { "click" });
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: feed one chat line into the native overlay's Chat view (`chat in|out`).
/// Both directions are echoed here so the renderer's log is the single truth.
#[tauri::command]
pub(crate) async fn render_chat(
	state: State<'_, AppState>,
	id: u64,
	dir: String,
	text: String,
) -> Result<(), String> {
	let stdin = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let dir = if dir == "out" { "out" } else { "in" };
			let _ = writeln!(si, "chat {dir} {}", text.replace('\n', " "));
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: push the host's directory listing to the native Files view as one-line
/// JSON (`fsjson {"path":…,"entries":[…]}`).
#[tauri::command]
pub(crate) async fn render_fs(
	state: State<'_, AppState>,
	id: u64,
	json: String,
) -> Result<(), String> {
	let stdin = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "fsjson {}", json.replace('\n', " "));
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: relay one keyboard input to the overlay's Chat composer (`kin t <text>` /
/// `kin k <name>`) — the renderer child can't take X focus, so the webview captures
/// keydowns while the overlay is open and pipes them here.
#[tauri::command]
pub(crate) async fn render_kin(
	state: State<'_, AppState>,
	id: u64,
	kind: String,
	data: String,
) -> Result<(), String> {
	let stdin = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			if kind == "t" {
				let _ = writeln!(si, "kin t {}", data.replace('\n', " "));
			} else {
				let _ = writeln!(si, "kin k {data}");
			}
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client: show a free-text toast on the native renderer (bottom-center, ~6 s with
/// fade). The webview is occluded by the video, so inbound chat etc. surfaces here.
#[tauri::command]
pub(crate) async fn render_toast(
	state: State<'_, AppState>,
	id: u64,
	text: String,
) -> Result<(), String> {
	let stdin = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			// Single line — the renderer's stdin protocol is line-based.
			let _ = writeln!(si, "toast {}", text.replace('\n', " "));
			let _ = si.flush();
		}
	}
	Ok(())
}

/// Client (game mode): open/close the in-session gaming overlay menu. Opening ungrabs
/// the local keyboard/mouse so they drive the webview overlay (without ending the
/// session) and pauses the embedded `--wid` mpv on Linux (keeping the last frame visible
/// + the decoder/socket warm); closing re-grabs and resumes. Idempotent and no-op-safe
/// when there's no IPC socket (Windows ffplay / single-surface — the overlay floats over
/// live video there).
#[tauri::command]
pub(crate) async fn set_overlay(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	open: bool,
) -> Result<(), String> {
	// Release/restore the evdev grab so the local OS + native overlay drive the menu
	// (no-op on Windows — the overlay floats over the live canvas there).
	kbdhook::overlay_suspend(open);
	// Opening the overlay DISENGAGES control (Parsec model): closing leaves the user
	// out of focus mode — the renderer prompts the click-to-control step — instead of
	// silently re-grabbing their keyboard/mouse.
	if open {
		kbdhook::release(&app);
	}
	// Linux: the native overlay (`pulsar-render`, an override-redirect ARGB window over the
	// video) is signalled SIGUSR1 (open → map + draw egui over the live video) / SIGUSR2
	// (close → unmap). The video keeps running underneath — no kill/pause. (mpv fallback, which
	// has no overlay process, still uses the old kill-on-open reveal.)
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		let (sdp, wid, ipc, vbin, rpid) = {
			let plays = state.plays.lock().unwrap();
			match plays.get(&id) {
				Some(p) => (
					p.mpv_sdp.clone(),
					p.mpv_wid,
					p.mpv_ipc.clone(),
					p.vidsink_bin.clone(),
					p.render_child.as_ref().map(|c| c.id()),
				),
				None => (None, None, None, None, None),
			}
		};
		if let Some(pid) = rpid {
			// Native overlay process: map/unmap over the live video. The in-app container is
			// input pass-through (clicks go to the webview); the egui overlay needs REAL
			// clicks while open, so pass-through flips off for the duration.
			crate::render::set_container_pass_through(&app, id, !open);
			let sig = if open { libc::SIGUSR1 } else { libc::SIGUSR2 };
			unsafe {
				libc::kill(pid as i32, sig);
			}
		} else if vbin.is_some() {
			// vidsink but no overlay process (binary missing): nothing to reveal — leave video.
		} else if let Some(sdp) = sdp {
			// mpv fallback can't corner-shrink: kill on open / respawn on close (old behavior).
			// Hide the (now empty) in-app container while open so it can't sit over the
			// webview menu; show it again once mpv is respawned into it.
			if open {
				if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
					if let Some(mut c) = p.ffplay.take() {
						let _ = c.kill();
						let _ = c.wait(); // reap so the X window is destroyed → webview repaints
					}
				}
				crate::render::set_container_visible(&app, id, false);
			} else if let Some(ipc) = &ipc {
				// The existing IPC stats poller keeps reading this same socket path.
				if let Some(c) = crate::native_view::spawn_mpv(&sdp, wid, ipc) {
					if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
						p.ffplay = Some(c);
					}
					crate::render::set_container_visible(&app, id, true);
				}
			}
		}
	}
	// Windows (and the future macOS native renderer): `pulsar-render` opens/closes its
	// egui overlay on stdin `open`/`close` lines (win/mod.rs stdin_control) — there are
	// no SIGUSR signals here, and without these lines the overlay is unreachable while
	// the child HWND occludes the webview menu (the grab is already released above).
	// No-op-safe when there's no render process (ffplay/mpv fallback → stdin is None).
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	{
		let stdin = state
			.plays
			.lock()
			.unwrap()
			.get(&id)
			.map(|p| p.render_stdin.clone());
		if let Some(stdin) = stdin {
			use std::io::Write;
			if let Some(si) = stdin.lock().unwrap().as_mut() {
				let _ = writeln!(si, "{}", if open { "open" } else { "close" });
				let _ = si.flush();
			}
		}
		let _ = &app; // only the Linux branch needs the AppHandle
	}
	Ok(())
}

/// Client: toggle host audio transmit + host-mute for an active session (session menu).
#[tauri::command]
pub(crate) async fn set_play_audio(
	state: State<'_, AppState>,
	id: u64,
	transmit: bool,
	mute: bool,
) -> Result<(), String> {
	let tx = state.plays.lock().unwrap().get(&id).map(|p| {
		// Track the audio truth for the codec-switch respawn re-seed (the fresh
		// renderer's Ses section would otherwise show defaults). Mic state is
		// owned by mic_start/stop — preserve whatever was last recorded.
		let mut seed = p.render_seed.lock().unwrap();
		let mic = seed.audio.map_or(false, |(_, _, m)| m);
		seed.audio = Some((transmit, mute, mic));
		p.restream_tx.clone()
	});
	if let Some(tx) = tx {
		tx.send(Restream::Audio(transmit, mute))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: ask the controlled host to **reverse direction** — it connects back to us
/// (`my_id`) so the roles swap. The host surfaces a `reverse-request`; this device
/// must be online (serving) for that reverse connect to land.
#[tauri::command]
pub(crate) async fn reverse_play(
	state: State<'_, AppState>,
	id: u64,
	my_id: String,
) -> Result<(), String> {
	let tx = data_sender(&state, id)?;
	tx.send(DataMsg::ReverseRequest(my_id))
		.await
		.map_err(|e| e.to_string())
}

/// Client: ask the host to list a directory (file panel's remote pane). `path` is
/// relative to the host user's HOME ("" = HOME); the reply arrives asynchronously
/// as the `fs-entries` event (the host answers even for rejected paths — with an
/// empty listing).
#[tauri::command]
pub(crate) async fn fs_list(
	state: State<'_, AppState>,
	id: u64,
	path: String,
) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::FsList { path })
		.await
		.map_err(|_| "klasör listelenemedi".to_string())
}

/// Client: ask the host to send the file at this HOME-relative path (file panel's
/// "indir"). The host streams it back as FileBegin/Chunk/End → saved under
/// "Pulsar Alınanlar" and surfaced via the `file-recv` event.
#[tauri::command]
pub(crate) async fn fs_get(
	state: State<'_, AppState>,
	id: u64,
	path: String,
) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::FsGet { path })
		.await
		.map_err(|_| "dosya istenemedi".to_string())
}
