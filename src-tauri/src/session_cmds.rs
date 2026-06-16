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
		// Clone the plays Arc so the task can re-check session liveness after its
		// sleep without borrowing the Tauri State (C20: the session may be torn down
		// during the 1200 ms window, in which case the post-sleep show must be skipped
		// to avoid re-raising a dead-session's container over the webview).
		let plays2 = state.plays.clone();
		tokio::spawn(async move {
			crate::render::set_container_visible(&app2, id, false);
			tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
			// Only reveal if the session is still alive (guard against disconnect racing
			// the encoder-switch veil — C20).
			if plays2.lock().unwrap().contains_key(&id) {
				crate::render::set_container_visible(&app2, id, true);
			}
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
	// RK3588: the renderer process must survive (killing it corrupts WebKit's shared
	// Mali GL state), but the demuxer/decoder were still fixed at spawn — without this
	// the old hevc_rkmpp kept eating the new H.264 RTP ("Multi-layer HEVC" spam, frozen
	// video). Rewrite the SDP and tell the LIVE renderer to reopen in place.
	#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
	reopen_render_for_codec(&state, id, &codec);
	#[cfg(not(any(
		all(unix, not(target_os = "macos")),
		windows
	)))]
	let _ = &app;
	#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
	let _ = &app;
	Ok(())
}

/// Live in-place codec switch for the surviving RK3588 renderer: rewrite the SDP for
/// `codec` (same video port — the host re-encodes to it) and send `reopen <path>` over
/// stdin; video.rs tears the demuxer+decoder down and reselects (h264_rkmpp ↔ hevc_rkmpp).
/// The overlay keeps all its state — no caps/seed replay needed (the process lives on).
#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
fn reopen_render_for_codec(state: &State<'_, AppState>, id: u64, codec: &str) {
	use std::io::Write as _;
	// Pull the render_stdin + caps_line Arcs out under the lock, then DROP the plays guard
	// before the blocking child-stdin write — a slow/backed-up pipe must not stall every
	// other state.plays user (forward() on each input event, the setters). Mirrors the
	// drop-guard-first pattern stop_stream uses. video_port is read here; mpv_sdp (a plain
	// field) is re-stored under a brief re-lock after the write.
	let (video_port, stdin, caps_line) = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else { return };
		if p.render_child.is_none() {
			return; // mpv/ffplay fallback paths keep their old behavior
		}
		(p.video_port, p.render_stdin.clone(), p.caps_line.clone())
	};
	let Ok(sdp) = crate::native_view::write_sdp(video_port, codec) else {
		return;
	};
	let sent = if let Some(si) = stdin.lock().unwrap().as_mut() {
		writeln!(si, "reopen {}", sdp.display()).and_then(|()| si.flush()).is_ok()
	} else {
		false
	};
	// Keep the stored caps line's codec field in sync for any later respawn/replay.
	{
		let mut line = caps_line.lock().unwrap();
		if !line.is_empty() {
			*line = line
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
		}
	}
	// Re-store mpv_sdp (plain field) under a brief lock — the play may have been torn
	// down while the pipe write was in flight, in which case there's nothing to update.
	if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
		// Track the freshly-written SDP for teardown (the old one stays tracked too).
		p.sdp_files.lock().unwrap().push(sdp.clone());
		p.mpv_sdp = Some(sdp);
	}
	tracing::info!(id, codec, sent, "renderer demuxer reopened for codec switch");
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
	// Serialize concurrent respawns (codec switch + monitor switch can overlap): clone
	// the per-session lock Arc under a brief std-Mutex hold, then await it OUTSIDE the
	// std lock so the SIGTERM-grace poll (~600 ms) never stalls other callers of
	// state.plays (input forward(), the setters). Without this, a second respawn for
	// the same id that arrives while render_child is transiently None (taken by the
	// first respawn) fires the old_child.is_none() early-return below — even though the
	// host has already restreamed to the new target — leaving the live renderer decoding
	// the wrong SPS/resolution until the user triggers yet another switch.
	let lock = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else { return };
		p.respawn_lock.clone()
	};
	// Wait for any in-flight respawn to complete before proceeding. Holding this guard
	// for the entirety of the respawn means the second respawn finds render_child = Some
	// (the freshly-spawned child from the first) instead of the transient None, and
	// applies its own codec/monitor params correctly in sequence.
	let _guard = lock.lock().await;
	let (vport, wid, game_mode, old_child, respawn_live_id) = {
		use std::sync::atomic::AtomicU64;
		let mut plays = state.plays.lock().unwrap();
		let Some(p) = plays.get_mut(&id) else { return };
		// Preserve the live_id Arc so the new reader stays updateable on future
		// reconnects (resident model). Reuse the existing Arc (so the reader from a
		// previous respawn is not orphaned on a double-switch) or create a fresh one
		// seeded with the current id if none exists yet.
		let live_id = p
			.render_live_id
			.clone()
			.unwrap_or_else(|| std::sync::Arc::new(AtomicU64::new(id)));
		live_id.store(id, std::sync::atomic::Ordering::Relaxed);
		(p.video_port, p.mpv_wid, p.game_mode, p.render_child.take(), live_id)
	};
	if old_child.is_none() {
		// render_child is None and we hold the exclusive respawn lock, so no other
		// respawn transiently cleared it — this session genuinely uses the mpv/ffplay
		// fallback path (render_child is always None there). Keep old behavior.
		return;
	}
	// Write the new SDP BEFORE killing the old renderer: a transient write failure
	// used to leave the session with render_child = None and no video for the rest
	// of its life. On failure, put the old child back and keep the old stream.
	let sdp = match crate::native_view::write_sdp(vport, codec) {
		Ok(s) => s,
		Err(_) => {
			if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
				p.render_child = old_child;
			}
			return;
		}
	};
	#[cfg(not(windows))]
	crate::render::set_container_visible(app, id, false);
	// Kill the old renderer FIRST so it releases its UDP video port before the new
	// renderer tries to bind the same port. Both processes use the same video_port
	// (the host re-encodes to the unchanged port on a codec/monitor switch), and the
	// RTP socket is bound with no SO_REUSEADDR/SO_REUSEPORT — starting the new
	// renderer while the old one still holds the port causes EADDRINUSE → recv_loop
	// (Windows rtp.rs) or avformat_open_input (Linux video.rs) fails immediately,
	// leaving the new renderer permanently deaf and the video permanently black.
	// stop_render_child_blocking offloads the SIGTERM-grace poll (~600 ms) to a
	// dedicated blocking thread so this async fn does not occupy a tokio worker
	// during the wait. We await the handle so the port IS released before spawn.
	if let Some(c) = old_child {
		let _ = crate::play::stop_render_child_blocking(c).await;
	}
	let pace_default = std::env::var("PULSAR_PACE")
		.map(|v| v == "1" || v == "on" || v == "true")
		.unwrap_or(true);
	let rbin = crate::process::render_bin(app);
	#[cfg(not(windows))]
	let mut rc = crate::native_view::spawn_render(
		&rbin,
		&sdp,
		wid,
		game_mode,
		pace_default,
		crate::i18n::lang(),
	);
	#[cfg(windows)]
	let mut rc = {
		let _ = wid; // X11 container XID — Linux-only; the HWND is re-resolved fresh
		crate::process::window_hwnd(app).and_then(|h| {
			crate::native_view::spawn_render_win(
				&rbin,
				&sdp,
				h,
				game_mode,
				pace_default,
				crate::i18n::lang(),
			)
		})
	};
	if rc.is_none() {
		// Spawn failed: the old renderer was already reaped, so the session is now
		// without video. Surface the failure via a warning; the session stays alive
		// (control channel intact) and the user can retry the codec/monitor switch.
		#[cfg(not(windows))]
		crate::render::set_container_visible(app, id, true);
		tracing::warn!(id, codec, "renderer respawn failed after killing old renderer");
		return;
	}
	if let Some(c) = rc.as_mut() {
		// Fresh renderer's PID — needed to re-assert an open overlay on Linux (SIGUSR1)
		// below, since the new process starts with OPEN=false (see the re-assert block).
		#[cfg(all(unix, not(target_os = "macos")))]
		let new_pid = c.id();
		if let Some(out) = c.stdout.take() {
			// Pass the live_id Arc (same one the fresh-spawn path uses) so this reader
			// is updateable on future reconnects — without this the reader's id was fixed
			// to the spawn-time id and ignored res.live_id.store() on the next reconnect,
			// causing play-ready to never fire for the new session (hung Connecting screen).
			crate::render_stats::start_render_reader(
				app,
				id,
				out,
				Some(respawn_live_id.clone()),
			);
		}
		let si = c.stdin.take();
		// Pull the per-id Arcs out under the lock, install the fresh stdin + mpv_sdp, then
		// DROP the plays guard BEFORE the blocking re-seed writes below — a backed-up child
		// pipe must not stall every other state.plays user (forward() on each input event,
		// the live setters). Mirrors stop_stream's drop-guard-first pattern. The Arcs
		// (render_stdin / caps_line / render_seed) outlive the guard, so the writes target
		// the same channels without holding the map locked.
		let chans = {
			let mut plays = state.plays.lock().unwrap();
			plays.get_mut(&id).map(|p| {
				*p.render_stdin.lock().unwrap() = si;
				// Track the freshly-written SDP for teardown (the old one stays tracked too).
				p.sdp_files.lock().unwrap().push(sdp.clone());
				p.mpv_sdp = Some(sdp);
				// Keep render_live_id in sync: the new reader holds respawn_live_id, so
				// stop_stream must park THIS Arc (not the old pre-switch one) so a subsequent
				// reconnect's res.live_id.store() updates the correct Arc the reader holds.
				p.render_live_id = Some(respawn_live_id.clone());
				(
					p.render_stdin.clone(),
					p.caps_line.clone(),
					p.render_seed.clone(),
				)
			})
		};
		if let Some((stdin, caps_line, render_seed)) = chans {
			// Re-seed the fresh renderer's overlay: take the last caps line, update its
			// codec=… field to the new codec, store + send it.
			let mut line = caps_line.lock().unwrap().clone();
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
				if let Some(si) = stdin.lock().unwrap().as_mut() {
					let _ = writeln!(si, "{line}");
				}
				*caps_line.lock().unwrap() = line;
			}
			// Re-push stdin-only overlay state the fresh process would otherwise
			// reset to defaults: open-button toggle + position, stats HUD, frame
			// pacing, view fit, audio truth, and the stream selections
			// (res/fps/bitrate/quality/display_idx — C14).
			let seed = render_seed.lock().unwrap().clone();
			{
				use std::io::Write as _;
				if let Some(si) = stdin.lock().unwrap().as_mut() {
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
					// Stream selections (C14): replay the user's last overlay picks so the
					// fresh renderer doesn't revert to its built-in defaults (auto/latency/0).
					if let Some(res) = seed.res.as_deref() {
						let _ = writeln!(si, "res {res}");
					}
					if let Some(fps) = seed.fps_sel.as_deref() {
						let _ = writeln!(si, "fps {fps}");
					}
					if let Some(bitrate) = seed.bitrate.as_deref() {
						let _ = writeln!(si, "bitrate {bitrate}");
					}
					if let Some(quality) = seed.quality.as_deref() {
						let _ = writeln!(si, "quality {quality}");
					}
					if let Some(idx) = seed.display_idx {
						let _ = writeln!(si, "display {idx}");
					}
					let _ = si.flush();
				}
			}
			// Re-assert the overlay open-state on the fresh renderer. The respawn was
			// triggered from INSIDE the open overlay (codec switch), so the frontend's
			// dock.overlayOpen is still true and the keyboard/evdev capture stays SUSPENDED
			// (state.overlay_open still holds this id). But the new process starts with
			// OPEN=false, so without this it would never draw the overlay — the user is
			// "stuck" (no video control, no menu) until they toggle Ctrl+Shift+M twice.
			// Mirror set_overlay's open path: SIGUSR1 + pass-through off on Linux, an `open`
			// stdin line elsewhere (Windows / future native macOS).
			if state.overlay_open.lock().unwrap().contains(&id) {
				#[cfg(all(unix, not(target_os = "macos")))]
				{
					crate::render::set_container_pass_through(app, id, false);
					unsafe {
						libc::kill(new_pid as i32, libc::SIGUSR1);
					}
				}
				#[cfg(not(all(unix, not(target_os = "macos"))))]
				{
					use std::io::Write as _;
					if let Some(si) = stdin.lock().unwrap().as_mut() {
						let _ = writeln!(si, "open");
						let _ = si.flush();
					}
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
		// Fire-and-forget: we're about to return, so no need to await.
		if let Some(c) = rc {
			crate::play::stop_render_child_blocking(c);
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
		// Re-check session liveness after the sleep: a disconnect during the
		// respawn gap would have called stop_stream (which removed the play entry)
		// while we were waiting. Revealing a dead session's container would briefly
		// raise it over the webview as a stray black/last-frame box (C20).
		if state.plays.lock().unwrap().contains_key(&id) {
			crate::render::set_container_visible(app, id, true);
		}
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

/// Leading-edge debounce state for the monitor picker, keyed PER play id (a client can stream
/// from several hosts at once — one tab each — so a single global would let one tab's pending
/// switch overwrite another's, dropping a switch or applying it to the wrong session). `last_ms`
/// = when this session's last switch fired; `pending` = its most-recent requested monitor while
/// coalescing (`u32::MAX` = none); `gen` distinguishes coalesced calls so only the newest one
/// fires the trailing switch.
struct MonDebounce {
	last_ms: u64,
	pending: u32,
	gen: u64,
}
impl Default for MonDebounce {
	fn default() -> Self {
		// `pending == u32::MAX` is the "no pending switch" sentinel (a real display_idx is < MAX).
		Self {
			last_ms: 0,
			pending: u32::MAX,
			gen: 0,
		}
	}
}
static MON_STATE: std::sync::Mutex<Option<std::collections::HashMap<u64, MonDebounce>>> =
	std::sync::Mutex::new(None);
/// How long after a switch fires that further picks coalesce instead of each spawning their own
/// reopen. ~A single switch's wall time, so deliberate switches (seconds apart) never coalesce,
/// but spamming the picker collapses to (first + final) instead of N decoder teardowns.
const MON_COOLDOWN_MS: u64 = 400;

/// Monotonic milliseconds since first use (for the debounce window).
fn mon_now_ms() -> u64 {
	use std::sync::OnceLock;
	use std::time::Instant;
	static E: OnceLock<Instant> = OnceLock::new();
	E.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Drop a session's monitor-picker debounce entry when its play session is torn down
/// (called from `stop_stream`) so the per-id map doesn't accumulate stale entries.
pub(crate) fn forget_monitor_debounce(id: u64) {
	if let Some(map) = MON_STATE.lock().unwrap().as_mut() {
		map.remove(&id);
	}
}

/// Client: switch which HOST monitor is streamed (session menu), as an index into the
/// host's advertised `StreamCaps::displays` (0 = primary). Re-requests the stream — the
/// host restarts capture on the selected output. Debounced per play id (see `MON_STATE`) so
/// spamming the picker doesn't stack reopens (each tears the decoder down → the "stuck on
/// switching").
#[tauri::command]
pub(crate) async fn set_play_monitor(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	display_idx: u32,
) -> Result<(), String> {
	tracing::info!(id, display_idx, "set_play_monitor command");
	let now = mon_now_ms();
	// Leading-edge gate: check cooldown AND update last_ms atomically under one lock
	// acquisition so that two concurrent calls for the same id both reading `last == 0`
	// cannot both pass the gate (TOCTOU fix — C26).
	// Returns `Some(last_ms_before_update)` when inside the cooldown (for wait calculation),
	// or `None` when the leading edge fires (last_ms already updated to `now`).
	let inside_cooldown_last = {
		let mut map = MON_STATE.lock().unwrap();
		let st = map.get_or_insert_with(Default::default).entry(id).or_default();
		let last = st.last_ms;
		// `last == 0` means this session has never fired a switch — always treat that as
		// eligible so the very first monitor pick of a session isn't penalised the cooldown.
		if last == 0 || now.saturating_sub(last) >= MON_COOLDOWN_MS {
			st.last_ms = now;
			None
		} else {
			Some(last)
		}
	};
	// Leading edge: a switch outside the cooldown fires IMMEDIATELY (no added latency).
	if inside_cooldown_last.is_none() {
		do_monitor_switch(&app, &state, id, display_idx).await;
		return Ok(());
	}
	let last = inside_cooldown_last.unwrap();
	// Inside the cooldown: record the latest target FOR THIS SESSION, wait out the remainder, and
	// only the newest coalesced call (highest generation) for this id fires the trailing switch.
	let g = {
		let mut map = MON_STATE.lock().unwrap();
		let st = map.get_or_insert_with(Default::default).entry(id).or_default();
		st.pending = display_idx;
		st.gen += 1;
		st.gen
	};
	let wait = MON_COOLDOWN_MS - now.saturating_sub(last);
	tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
	let p = {
		let mut guard = MON_STATE.lock().unwrap();
		// Use get_mut — do NOT use or_default() here.  If forget_monitor_debounce already
		// removed this id (session torn down while we slept), we must not re-insert a stale
		// entry; just bail silently so the coalescing task leaves no orphan in the map.
		let Some(map) = guard.as_mut() else {
			return Ok(());
		};
		let Some(st) = map.get_mut(&id) else {
			return Ok(());
		};
		if st.gen != g {
			// A newer coalesced call is the designated winner — clean up this entry if no
			// further switch is in flight (gen will have advanced beyond g, meaning another
			// task owns the trailing edge; leave the entry for that task to remove).
			return Ok(());
		}
		let p = std::mem::replace(&mut st.pending, u32::MAX);
		if p != u32::MAX {
			st.last_ms = mon_now_ms();
		}
		// Do NOT remove the entry here.  Removing it would discard the last_ms we just
		// wrote, so the very next pick within MON_COOLDOWN_MS would find a fresh
		// MonDebounce (last_ms == 0) and fire immediately — defeating the cooldown that
		// should follow the trailing switch.  The entry is bounded per-session-id and is
		// reclaimed by forget_monitor_debounce when the session stops, so leaving it is
		// not a leak.  pending is already reset to u32::MAX above, so the entry is inert
		// until the next set_play_monitor call updates it.
		p
	};
	if p != u32::MAX {
		do_monitor_switch(&app, &state, id, p).await;
	}
	Ok(())
}

/// The actual monitor switch: re-request the stream on the new host output and resync the local
/// renderer (its demuxer/decoder were fixed at spawn). Split out of `set_play_monitor` so the
/// debounce can fire it on both the leading edge and the trailing (coalesced) edge.
async fn do_monitor_switch(
	app: &AppHandle,
	state: &State<'_, AppState>,
	id: u64,
	display_idx: u32,
) {
	// Pull the restream channel + the active codec (from the stored caps line) under one lock.
	let (tx, codec) = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else {
			tracing::warn!(id, "set_play_monitor: no play session for id");
			return;
		};
		let codec = p
			.caps_line
			.lock()
			.unwrap()
			.split_whitespace()
			.find_map(|kv| kv.strip_prefix("codec=").map(str::to_string))
			.unwrap_or_default();
		(p.restream_tx.clone(), codec)
	};
	if tx.send(Restream::Display(display_idx)).await.is_err() {
		return;
	}
	// Monitors can differ in resolution (e.g. 2560×1440 vs 1920×1200), so the host's
	// restarted capture emits a stream with NEW dimensions/SPS. The renderer's demuxer +
	// decoder are fixed at spawn, so — exactly like a codec switch — they must resync or
	// the video freezes on the old params. Reuse the codec-switch reopen/respawn path with
	// the UNCHANGED codec.
	let _ = &app;
	if !codec.is_empty() {
		#[cfg(any(
			all(unix, not(target_os = "macos"), not(target_arch = "aarch64")),
			windows
		))]
		respawn_render_for_codec(app, state, id, &codec).await;
		#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
		reopen_render_for_codec(state, id, &codec);
	}
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
	// (no-op on Windows — the overlay floats over the live canvas there). The latch is
	// derived from the SET of open-overlay tabs, not this one call: with several tabs,
	// "my overlay closed" must not resume capture while another tab's is still open —
	// and a session that dies with its overlay open is cleaned up in stop_stream.
	{
		let mut set = state.overlay_open.lock().unwrap();
		if open {
			set.insert(id);
		} else {
			set.remove(&id);
		}
		kbdhook::overlay_suspend(!set.is_empty());
	}
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
				// Take the old mpv child out under the lock, DROP the guard, THEN
				// kill/wait — c.wait() blocks until the process exits, and holding the
				// plays guard across it stalls every other state.plays user (forward() on
				// each input event, the setters). Mirrors stop_stream's drop-first pattern.
				let old = state
					.plays
					.lock()
					.unwrap()
					.get_mut(&id)
					.and_then(|p| p.ffplay.take());
				if let Some(mut c) = old {
					let _ = c.kill();
					let _ = c.wait(); // reap so the X window is destroyed → webview repaints
				}
				crate::render::set_container_visible(&app, id, false);
			} else if let Some(ipc) = &ipc {
				// The existing IPC stats poller keeps reading this same socket path.
				if let Some(c) = crate::native_view::spawn_mpv(&sdp, wid, ipc) {
					if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
						p.ffplay = Some(c);
					}
					crate::render::set_container_visible(&app, id, true);
				} else {
					// Respawn failed (mpv vanished mid-session / transient resource
					// exhaustion). The open branch already killed the previous mpv, so a
					// silent None would leave the container hidden = video gone for good with
					// no error. Surface it on the `host-stats` channel the session UI renders
					// as a status string (mirrors play.rs's mpv-missing notice), so the failure
					// is visible and the user can retry by re-toggling the overlay.
					use tauri::Emitter;
					tracing::error!("mpv fallback respawn failed on overlay close — video unavailable");
					let _ = app.emit(
						"host-stats",
						crate::events::PlayStats {
							id,
							label: "Video yok — mpv yeniden başlatılamadı".to_string(),
						},
					);
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
