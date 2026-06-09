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
pub(crate) async fn set_play_encoder(state: State<'_, AppState>, id: u64, encoder: String) -> Result<(), String> {
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
	Ok(())
}

/// Client: switch the video codec of an active session on the fly ("h264"/"h265"/"av1").
/// The host restarts ffmpeg with it; the client's WebCodecs decoder re-derives its codec
/// string from the new stream's SPS automatically.
#[tauri::command]
pub(crate) async fn set_play_codec(state: State<'_, AppState>, id: u64, codec: String) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Codec(codec))
			.await
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: change the frame rate of an active session on the fly (0 = host default).
#[tauri::command]
pub(crate) async fn set_play_fps(app: AppHandle, state: State<'_, AppState>, id: u64, fps: u32) -> Result<(), String> {
	// 0 = "auto" → target the client's display refresh (nearest of 30/60/120).
	let fps = if fps == 0 { client_auto_fps(&app).await } else { fps };
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
	if let Some(tx) = tx {
		tx.send(Restream::Fps(fps)).await.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Client: change the target bitrate (kbit/s) of an active session on the fly
/// (0 = host default). The host restarts ffmpeg with the new bitrate. The UI converts
/// Mbit → kbps (×1000) before invoking.
#[tauri::command]
pub(crate) async fn set_play_bitrate(state: State<'_, AppState>, id: u64, kbps: u32) -> Result<(), String> {
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
pub(crate) async fn set_play_quality(state: State<'_, AppState>, id: u64, quality: String) -> Result<(), String> {
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
pub(crate) async fn set_frame_pacing(state: State<'_, AppState>, id: u64, on: bool) -> Result<(), String> {
	let stdin = state.plays.lock().unwrap().get(&id).map(|p| p.render_stdin.clone());
	if let Some(stdin) = stdin {
		use std::io::Write;
		if let Some(si) = stdin.lock().unwrap().as_mut() {
			let _ = writeln!(si, "pace {}", if on { 1 } else { 0 });
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
			// Native overlay process: map/unmap over the live video.
			let sig = if open { libc::SIGUSR1 } else { libc::SIGUSR2 };
			unsafe {
				libc::kill(pid as i32, sig);
			}
		} else if vbin.is_some() {
			// vidsink but no overlay process (binary missing): nothing to reveal — leave video.
		} else if let Some(sdp) = sdp {
			// mpv fallback can't corner-shrink: kill on open / respawn on close (old behavior).
			if open {
				if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
					if let Some(mut c) = p.ffplay.take() {
						let _ = c.kill();
						let _ = c.wait(); // reap so the X window is destroyed → webview repaints
					}
				}
			} else if let Some(ipc) = &ipc {
				// The existing IPC stats poller keeps reading this same socket path.
				if let Some(c) = crate::native_view::spawn_mpv(&sdp, wid, ipc) {
					if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
						p.ffplay = Some(c);
					}
				}
			}
		}
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	let _ = (&app, &state, id); // kill/respawn reveal is Linux-only; Windows/macOS overlay floats
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
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.restream_tx.clone());
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
pub(crate) async fn reverse_play(state: State<'_, AppState>, id: u64, my_id: String) -> Result<(), String> {
	let tx = data_sender(&state, id)?;
	tx.send(DataMsg::ReverseRequest(my_id))
		.await
		.map_err(|e| e.to_string())
}
