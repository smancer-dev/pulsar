//! Client-side I/O commands: the side channels (clipboard / chat / file / mic),
//! input forwarding, the Windows keyboard-capture toggle, and the fullscreen
//! window toggle (with the Win32 borderless-fullscreen helper).

use std::io::Read;

use pulsar_core::service::{DataMsg, InputEvent};
use tauri::{AppHandle, State};

use crate::audio_io::spawn_mic_recorder;
use crate::kbdhook;
use crate::process::ffmpeg_bin;
use crate::state::AppState;
use crate::util::{data_sender, forward};

/// Client → host: push clipboard text (read from the webview) to the remote.
#[tauri::command]
pub(crate) async fn send_clipboard(
	state: State<'_, AppState>,
	id: u64,
	text: String,
) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Clipboard(text))
		.await
		.map_err(|_| crate::i18n::t("err.clipboard").to_string())
}

/// Client → host: send a chat line.
#[tauri::command]
pub(crate) async fn send_chat(
	state: State<'_, AppState>,
	id: u64,
	text: String,
) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Chat(text))
		.await
		.map_err(|_| crate::i18n::t("err.message").to_string())
}

/// Host → client: reply to a connected SESSION's chat. Routed by session id (`sid`),
/// so two panes from one client device receive the host's replies on the right pane.
#[tauri::command]
pub(crate) async fn host_send_chat(
	state: State<'_, AppState>,
	sid: u64,
	text: String,
) -> Result<(), String> {
	// Look up the session's outbound sender + its peer (for the backlog key) under one lock.
	let entry = state
		.host_out
		.lock()
		.unwrap()
		.get(&sid)
		.map(|(peer, tx)| (peer.clone(), tx.clone()));
	let (peer, tx) = entry.ok_or_else(|| "cihaz bağlı değil".to_string())?;
	tx.send(DataMsg::Chat(text.clone()))
		.await
		.map_err(|_| crate::i18n::t("err.message").to_string())?;
	// Into the backlog too: sent lines have no broadcast event of their own, so a
	// re-opened connections window rebuilds the full conversation from here. Keyed by
	// peer (the chat modal groups history per device, matching inbound chat).
	{
		let mut log = state.chat_log.lock().unwrap();
		log.push((peer, text, true));
		let excess = log.len().saturating_sub(500);
		if excess > 0 {
			log.drain(..excess);
		}
	}
	Ok(())
}

/// The host-side chat backlog (peer, text, me) — seeds the connections window's
/// message modal with history from before that window existed.
#[tauri::command]
pub(crate) async fn chat_log(
	state: State<'_, AppState>,
) -> Result<Vec<(String, String, bool)>, String> {
	Ok(state.chat_log.lock().unwrap().clone())
}

/// Client → host: send a file (chunked over the session, saved on the host).
#[tauri::command]
pub(crate) async fn send_file(
	state: State<'_, AppState>,
	id: u64,
	name: String,
	data: Vec<u8>,
) -> Result<(), String> {
	// MUST mirror fs_browse.rs's CHUNK: the session transport is one datagram per
	// message, serde_json encodes Vec<u8> at ≈4 chars/byte worst case, and macOS
	// only sends ~9216-byte UDP datagrams by default (net.inet.udp.maxdgram) —
	// 2 KiB raw ≈ 8.3 KB JSON fits everywhere; bigger chunks fail EMSGSIZE and
	// are silently dropped (serve_with/hold swallow send errors).
	const CHUNK: usize = 2048;
	let tx = data_sender(&state, id)?;
	let chunks = data.len().div_ceil(CHUNK) as u32;
	// Tag every message of this transfer so it never collides with a concurrent
	// transfer's reassembly state on the receiver (see fs_browse::next_transfer_id).
	let xfer = crate::fs_browse::next_transfer_id();
	tx.send(DataMsg::FileBegin {
		id: xfer,
		name,
		size: data.len() as u64,
		chunks,
	})
	.await
	.map_err(|_| crate::i18n::t("err.file").to_string())?;
	for (i, ch) in data.chunks(CHUNK).enumerate() {
		tx.send(DataMsg::FileChunk {
			id: xfer,
			index: i as u32,
			data: ch.to_vec(),
		})
		.await
		.map_err(|_| crate::i18n::t("err.file").to_string())?;
	}
	tx.send(DataMsg::FileEnd { id: xfer })
		.await
		.map_err(|_| crate::i18n::t("err.file").to_string())
}

/// Client → host: send a local file by its HOME-relative path (the file panel's
/// "gönder" action). Unlike `send_file`, the webview never reads the bytes — Rust
/// streams them straight from disk with the same chunker the host's FsGet reply
/// uses, jailed to HOME exactly like the `local_ls` listing the path came from.
#[tauri::command]
pub(crate) async fn send_file_path(
	state: State<'_, AppState>,
	id: u64,
	path: String,
) -> Result<(), String> {
	let tx = data_sender(&state, id)?;
	crate::fs_browse::send_file_at(&tx, &path)
		.await
		.ok_or_else(|| crate::i18n::t("err.file").to_string())
}

/// Client: start streaming the microphone to the host (raw PCM over the session).
#[tauri::command]
pub(crate) async fn mic_start(
	app: tauri::AppHandle,
	state: State<'_, AppState>,
	id: u64,
) -> Result<(), String> {
	let (tx, mic_slot, render_seed) = {
		let plays = state.plays.lock().unwrap();
		let p = plays.get(&id).ok_or(crate::i18n::t("err.session"))?;
		(p.data_tx.clone(), p.mic.clone(), p.render_seed.clone())
	};
	// Hold the slot lock across check→spawn→insert (spawn_mic_recorder is
	// synchronous, no await): two concurrent invocations could otherwise both
	// pass the is_some() check, and the loser's parecord would be dropped
	// without kill() — recording and sending duplicate Audio frames for the
	// session's lifetime.
	let mut slot = mic_slot.lock().unwrap();
	if slot.is_some() {
		return Ok(()); // already on
	}
	let ffmpeg = ffmpeg_bin(&app);
	let mut child =
		spawn_mic_recorder(&ffmpeg).ok_or(crate::i18n::t("err.micRecorder"))?;
	let stdout = child.stdout.take().ok_or(crate::i18n::t("err.micOutput"))?;
	*slot = Some(child);
	drop(slot);
	// Record the mic-on bit in the audio truth so a codec/monitor-switch respawn
	// re-seeds the fresh renderer's overlay with mic=1 (set_play_audio preserves
	// this bit but never sets it — the mic state is owned here).
	if let Some(seed) = render_seed.lock().unwrap().audio.as_mut() {
		seed.2 = true;
	}
	// Blocking read loop on a dedicated thread; killing the child ends it.
	std::thread::spawn(move || {
		let mut rdr = stdout;
		let mut buf = [0u8; 3840]; // ~20 ms @ 48 kHz mono s16
		loop {
			match rdr.read(&mut buf) {
				Ok(0) | Err(_) => break,
				Ok(n) => {
					if tx.blocking_send(DataMsg::Audio(buf[..n].to_vec())).is_err() {
						break;
					}
				}
			}
		}
	});
	Ok(())
}

/// Client: stop streaming the microphone.
#[tauri::command]
pub(crate) async fn mic_stop(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	let (tx, mic_slot, render_seed) = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else {
			return Ok(());
		};
		(p.data_tx.clone(), p.mic.clone(), p.render_seed.clone())
	};
	if let Some(mut c) = mic_slot.lock().unwrap().take() {
		let _ = c.kill();
		// Reap immediately — kill() alone leaves the recorder as a zombie until
		// session teardown (visible as a defunct parecord for the session's life).
		let _ = c.wait();
	}
	// Clear the mic-on bit in the audio truth so a later respawn re-seeds mic=0
	// (mirrors mic_start; set_play_audio preserves but never sets this bit).
	if let Some(seed) = render_seed.lock().unwrap().audio.as_mut() {
		seed.2 = false;
	}
	let _ = tx.send(DataMsg::AudioEnd).await;
	Ok(())
}

/// Long-lived OS clipboard handle. On X11 pasted data is served BY the owning
/// process — a per-call `Clipboard::new()` dropped at return would make every
/// `write_clipboard_text` evaporate immediately, so one instance lives for the
/// app's lifetime.
static CLIPBOARD: std::sync::Mutex<Option<arboard::Clipboard>> = std::sync::Mutex::new(None);

fn with_clipboard<R>(
	f: impl FnOnce(&mut arboard::Clipboard) -> Result<R, arboard::Error>,
) -> Result<R, String> {
	let mut g = CLIPBOARD.lock().unwrap();
	if g.is_none() {
		*g = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
	}
	f(g.as_mut().unwrap()).map_err(|e| e.to_string())
}

/// Read the OS clipboard app-side. The webview's `navigator.clipboard.readText()`
/// silently fails on the Linux native-video path (occluded/unfocused WebKitGTK), so
/// the overlay's "Panoyu karşıya gönder" reads here instead.
#[tauri::command]
pub(crate) fn read_clipboard_text() -> Result<String, String> {
	with_clipboard(|c| c.get_text())
}

/// Write the OS clipboard app-side (inbound host clipboard / copy actions) — same
/// WebKitGTK constraint as `read_clipboard_text`.
#[tauri::command]
pub(crate) fn write_clipboard_text(text: String) -> Result<(), String> {
	with_clipboard(|c| c.set_text(text))
}

/// Client: forward absolute pointer motion (normalized 0..1) to the host.
#[tauri::command]
pub(crate) async fn input_pointer(
	state: State<'_, AppState>,
	id: u64,
	x: f64,
	y: f64,
) -> Result<(), String> {
	forward(&state, id, InputEvent::PointerMotion { x, y }).await;
	Ok(())
}

/// Client: forward a mouse button (0=left, 1=right, 2=middle) press/release.
#[tauri::command]
pub(crate) async fn input_button(
	state: State<'_, AppState>,
	id: u64,
	button: u8,
	down: bool,
) -> Result<(), String> {
	forward(&state, id, InputEvent::PointerButton { button, down }).await;
	Ok(())
}

/// Client: forward a scroll delta.
#[tauri::command]
pub(crate) async fn input_scroll(
	state: State<'_, AppState>,
	id: u64,
	dx: f64,
	dy: f64,
) -> Result<(), String> {
	forward(&state, id, InputEvent::Scroll { dx, dy }).await;
	Ok(())
}

/// Client: forward a keyboard evdev keycode press/release.
#[tauri::command]
pub(crate) async fn input_key(
	state: State<'_, AppState>,
	id: u64,
	code: u32,
	down: bool,
) -> Result<(), String> {
	forward(&state, id, InputEvent::Key { code, down }).await;
	Ok(())
}

/// Client: forward a resolved Unicode character to type verbatim (layout-independent).
/// The webview resolves a printable keypress through ITS OWN keyboard layout to this
/// codepoint, so the host inserts it regardless of the host's active layout (matching the
/// Linux/Windows native hook's `Char` path). The UI sends a one-char string; an empty or
/// multi-`char` string (no scalar value) is ignored.
#[tauri::command]
pub(crate) async fn input_char(
	state: State<'_, AppState>,
	id: u64,
	ch: String,
) -> Result<(), String> {
	let Some(c) = ch.chars().next().filter(|_| ch.chars().count() == 1) else {
		return Ok(());
	};
	forward(&state, id, InputEvent::Char(c)).await;
	Ok(())
}

/// SPLIT MODE: the frontend calls this whenever the focused pane changes, with that pane's
/// play/session id (0 = none). It records the focus into [`AppState::focused_session`], which the
/// controller forward gate (play.rs) reads to route UNLOCKED pads + which kb/mouse routing follows.
///
/// On a real focus CHANGE (and only while split mode is active) it also FLUSHES any held keyboard
/// keys / mouse buttons via `kbdhook::disable()` — that path runs the same internal `flush_held`
/// (sends an UP for every key/button still held + resets the modifier chord state) as a normal
/// disengage edge, so a key held down in the old pane can't stay stuck on the old host. The
/// frontend re-arms capture for the newly-focused pane immediately after (its existing
/// `kbd_capture_start(newId)` call), exactly like the single-session tab-switch flow
/// (disable()→enable()). With split mode OFF this is a no-op flush so existing single-session
/// behavior is unchanged.
#[tauri::command]
pub(crate) async fn set_active_session(
	state: State<'_, AppState>,
	play_id: u64,
) -> Result<(), String> {
	use std::sync::atomic::Ordering;
	let prev = state.focused_session.swap(play_id, Ordering::SeqCst);
	let split_on = state.split_pane_count.load(Ordering::SeqCst) > 1;
	if split_on && prev != play_id {
		// Release+flush held keys/buttons from the previously-focused pane so nothing sticks on
		// its host. The frontend re-arms capture for the new pane right after this returns.
		crate::kbdhook::disable();
	}
	Ok(())
}

/// SPLIT MODE: apply a controller-lock toggle from a session's egui overlay. The overlay emits the
/// `ctrllock` command with payload `"<uuid>,<0|1>"` (mirroring `ctrldisable` exactly); the renderer
/// stdout reader tags it with that renderer's own session id (`cur_id`) and the frontend forwards
/// it here with that `play_id`. This is the backend application of the contract's `ctrllock` wire:
///   * `locked = true`  → `set_controller_lock(uuid, play_id)`: only this session forwards the pad.
///   * `locked = false` → `clear_controller_lock(uuid)` IF this session currently owns the lock
///     (so a stale unlock from another pane can't release a lock it doesn't hold).
/// The play.rs forward gate reads `CONTROLLER_SESSION_LOCK` live, so the change takes effect on the
/// next pad tick with no reconnect.
#[tauri::command]
pub(crate) async fn set_controller_lock(
	uuid: String,
	play_id: u64,
	locked: bool,
) -> Result<(), String> {
	if locked {
		crate::controllers::set_controller_lock(uuid, play_id);
	} else if crate::controllers::controller_lock_owner(&uuid) == Some(play_id) {
		crate::controllers::clear_controller_lock(&uuid);
	}
	Ok(())
}

/// SPLIT MODE (couch co-op): lock a physical KEYBOARD/MOUSE (by its stable evdev device key) to one
/// pane so two players can use separate kb+mouse. Mirrors `set_controller_lock`: the kbdhook routes
/// a locked device's input to its owner; an unlocked device follows the focused pane (unchanged).
/// `locked=false` only clears the lock if THIS session owns it (a stale unlock can't steal a lock).
#[tauri::command]
pub(crate) async fn set_kbm_lock(
	dev_key: String,
	play_id: u64,
	locked: bool,
) -> Result<(), String> {
	if locked {
		crate::controllers::set_kbm_lock(dev_key, play_id);
	} else if crate::controllers::kbm_lock_owner(&dev_key) == Some(play_id) {
		crate::controllers::clear_kbm_lock(&dev_key);
	}
	Ok(())
}

/// SPLIT MODE: the keyboards/mice the local capture is holding, by stable key, for the per-pane
/// assignment UI (each is lockable via `set_kbm_lock`). Linux-only content; empty elsewhere.
#[tauri::command]
pub(crate) async fn list_input_devices() -> Result<Vec<String>, String> {
	Ok(crate::kbdhook::captured_input_devices())
}

/// SPLIT MODE: the frontend reports the number of panes currently shown (1..=4) here. Stored into
/// [`AppState::split_pane_count`], which `reap_excess_resident_pool` uses as its cap so up to 4
/// live per-pane renderers are not SIGTERM'd as "excess". Clamped to at least 1.
#[tauri::command]
pub(crate) async fn set_pane_count(
	state: State<'_, AppState>,
	count: usize,
) -> Result<(), String> {
	state
		.split_pane_count
		.store(count.max(1), std::sync::atomic::Ordering::SeqCst);
	Ok(())
}

/// Client (Windows): arm the low-level keyboard hook for play `id`, so OS-reserved
/// keys (Win, Alt+Tab, Ctrl+Esc, media) are captured before Windows handles them,
/// forwarded to the host, and suppressed locally. No-op on non-Windows. Called from
/// the UI when the user takes control of a session.
#[tauri::command]
pub(crate) async fn kbd_capture_start(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	mouse: bool,
) -> Result<(), String> {
	let tx = state
		.plays
		.lock()
		.unwrap()
		.get(&id)
		.map(|p| p.input_tx.clone());
	match tx {
		Some(tx) => {
			// `mouse` = also capture the mouse (native-renderer mode, no canvas).
			// Compute the desired suspend state from the authoritative overlay set (the
			// source of truth, owned here in the Tauri layer — the kbdhook static can't
			// see it). A re-arm while THIS session's overlay is still on screen must start
			// SUSPENDED so the evdev grab doesn't eat local input; enable() blindly
			// clearing SUSPENDED was the desync bug. (Per-id, not "any tab" — another
			// tab's open overlay must not suspend this one.)
			let start_suspended = state.overlay_open.lock().unwrap().contains(&id);
			kbdhook::enable(app, tx, mouse, id, start_suspended);
			Ok(())
		}
		None => Err(crate::i18n::t("err.session").into()),
	}
}

/// Client: disarm the keyboard hook (control released / canvas blurred / leave
/// combo). Local keys behave normally again. No-op on non-Windows.
#[tauri::command]
pub(crate) async fn kbd_capture_stop() -> Result<(), String> {
	kbdhook::disable();
	Ok(())
}

/// Client (native renderer): explicit click-to-engage. The in-app video container is
/// input pass-through, so a click on the video lands on the webview underneath — the
/// session screen forwards it here and the armed evdev capture takes the devices.
#[tauri::command]
pub(crate) fn kbd_engage(app: AppHandle) -> Result<(), String> {
	kbdhook::engage(&app);
	Ok(())
}

/// Client (native): position the in-app native video over the session tab's content
/// area. Linux: moves the pass-through GDK container (viewport CSS px == GDK logical
/// px). Windows: streams `viewrect` (scaled to PHYSICAL px) to the renderer child over
/// stdin — same effect, chrome/tabs stay visible. Zero area hides it (the tab went
/// inactive / the session screen unmounted). No-op on macOS.
#[tauri::command]
pub(crate) fn native_view_rect(
	app: AppHandle,
	state: State<'_, AppState>,
	id: u64,
	x: i32,
	y: i32,
	w: i32,
	h: i32,
) -> Result<(), String> {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		let _ = &state;
		crate::render::native_container_rect(&app, id, x, y, w, h);
	}
	#[cfg(windows)]
	{
		use tauri::Manager as _;
		// CSS px → physical px (the Win32 child lives in physical client coords).
		let scale = app
			.get_webview_window("main")
			.and_then(|w| w.scale_factor().ok())
			.unwrap_or(1.0);
		let px = |v: i32| (v as f64 * scale).round() as i32;
		let (rx, ry, rw, rh) = (px(x), px(y), px(w), px(h));
		use std::io::Write as _;
		// Pull the render_stdin Arc out under the lock, then DROP the plays guard before the
		// blocking child-stdin write — a full/backed-up pipe must not stall every other
		// state.plays user (forward() on each input event, the setters). Mirrors render_hint.
		// Record the rect into RenderSeed under the same lock so a codec/monitor-switch
		// respawn can replay it — otherwise the fresh child defaults to the whole client
		// area and buries the app chrome until the user resizes the window.
		let stdin = state.plays.lock().unwrap().get(&id).map(|p| {
			p.render_seed.lock().unwrap().viewrect = Some((rx, ry, rw, rh));
			p.render_stdin.clone()
		});
		if let Some(stdin) = stdin {
			if let Some(si) = stdin.lock().unwrap().as_mut() {
				let _ = writeln!(si, "viewrect {rx} {ry} {rw} {rh}");
			}
		}
	}
	#[cfg(target_os = "macos")]
	let _ = (app, state, id, x, y, w, h);
	Ok(())
}

/// True, taskbar-covering fullscreen. A transparent Tauri window isn't treated as
/// a real fullscreen app on Windows (the shell keeps the taskbar on top), so we
/// cover the current monitor manually and stay above the taskbar — the same
/// borderless-fullscreen trick games use. Restores the windowed geometry on exit.
#[tauri::command]
pub(crate) fn set_window_fullscreen(
	window: tauri::WebviewWindow,
	state: State<'_, AppState>,
	on: bool,
) -> Result<(), String> {
	tracing::info!(on, "set_window_fullscreen");
	let _ = &state; // no longer needed (native fullscreen tracks the pre-fullscreen geometry)
	// Use the platform's NATIVE borderless fullscreen (tao/winit). The window is non-transparent
	// + frameless now, so on Windows the shell treats it as a real fullscreen app: the taskbar
	// auto-hides but stays summonable, Alt+Tab and the Win key behave normally, and the content
	// fills the monitor edge-to-edge — none of which the old manual SetWindowPos/topmost hack
	// (`win_fullscreen`, kept below for reference) could get right all at once.
	let w = window.clone();
	// Window ops must run on the UI/event thread (a Tauri command runs off it; calling these
	// off-thread can silently no-op, esp. on the GTK backend).
	let _ = window.run_on_main_thread(move || {
		// Self-heal any stray always-on-top the old fullscreen path could have left set.
		if !on {
			let _ = w.set_always_on_top(false);
		}
		let _ = w.set_fullscreen(on);
		// Keep input focus on the toplevel after the toggle — otherwise (Linux) X moves focus to
		// the embedded native-renderer child window, which flips kbdhook's FOCUSED to false and
		// silently kills the evdev combos (F12/M/Q) until the user clicks back in.
		let _ = w.set_focus();
	});
	Ok(())
}

/// Windows borderless-fullscreen via raw Win32: cover the window's monitor and go
/// top-most, which (unlike `set_fullscreen` on a transparent window) reliably hides
/// the taskbar. Restores the saved windowed rect when turning off.
#[cfg(windows)]
#[allow(dead_code)] // superseded by native set_fullscreen; kept for reference / quick revert
fn win_fullscreen(
	window: &tauri::WebviewWindow,
	on: bool,
	saved: Option<(tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>)>,
) {
	use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
	use windows_sys::Win32::Graphics::Gdi::{
		ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
	};
	use windows_sys::Win32::UI::WindowsAndMessaging::{
		GetClientRect, GetWindowRect, SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST,
		SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
	};
	use windows_sys::Win32::Graphics::Dwm::{
		DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
	};
	let Ok(handle) = window.hwnd() else {
		return;
	};
	let hwnd: HWND = handle.0 as _;
	unsafe {
		// Frameless fullscreen polish: kill the 1px DWM edge border + the rounded corners while
		// fullscreen (at the exact monitor rect they'd peek the desktop at the edges/corners);
		// restore the defaults on exit. DWMWA_COLOR_NONE = 0xFFFF_FFFE, _DEFAULT = 0xFFFF_FFFF;
		// DWMWCP_DONOTROUND = 1, DWMWCP_DEFAULT = 0.
		let border: u32 = if on { 0xFFFF_FFFE } else { 0xFFFF_FFFF };
		let corner: i32 = if on { 1 } else { 0 };
		DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR as u32, (&border as *const u32).cast(), 4);
		DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, (&corner as *const i32).cast(), 4);
		// The window is already FRAMELESS (decorations:false — the app draws its own title bar):
		// tao's undecorated WM_NCCALCSIZE keeps the client rect == the FULL window, so we must NOT
		// touch the window style here. Stripping WS_THICKFRAME (what the old DECORATED path did)
		// left the client rect inset by the resize border, so `fill_children_to_client` snapped the
		// webview in by that margin → the desktop showed through as gaps around the fullscreen
		// content. Just cover the monitor / restore the windowed rect; tao gives a gap-free client.
		if on {
			// Clear any maximize/minimize first, else SetWindowPos is clamped to the
			// work area (taskbar stays visible).
			ShowWindow(hwnd, SW_RESTORE);
			let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
			let mut mi: MONITORINFO = std::mem::zeroed();
			mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
			if GetMonitorInfoW(mon, &mut mi) != 0 {
				let r: RECT = mi.rcMonitor;
				let (mw, mh) = (r.right - r.left, r.bottom - r.top);
				// HWND_TOPMOST is what actually hides the taskbar on this setup (the shell's
				// non-topmost fullscreen auto-hide didn't trigger here — likely ASTER). Alt+Tab is
				// handled by the `Focused` handler in lib.rs (on blur it drops topmost AND sinks the
				// window to the bottom so the activated app comes forward).
				//
				// tao's frameless window keeps an ~8px resize border (left/right/bottom) that
				// GetClientRect excludes, so the webview — snapped to the client by
				// fill_children_to_client — would leave that border as a desktop gap down the edges.
				// Position at the monitor once, MEASURE the per-side client inset, then re-position
				// EXPANDED by it so the CLIENT (= the webview) covers the monitor exactly with the
				// NC border off-screen (fine — we're topmost, so the cover needn't be the exact rect).
				SetWindowPos(hwnd, HWND_TOPMOST, r.left, r.top, mw, mh, SWP_SHOWWINDOW | SWP_FRAMECHANGED);
				let (mut wr, mut cr) = (std::mem::zeroed::<RECT>(), std::mem::zeroed::<RECT>());
				let mut tl = POINT { x: 0, y: 0 };
				GetWindowRect(hwnd, &mut wr);
				GetClientRect(hwnd, &mut cr);
				ClientToScreen(hwnd, &mut tl);
				let il = (tl.x - wr.left).max(0);
				let it = (tl.y - wr.top).max(0);
				let ir = (wr.right - (tl.x + cr.right)).max(0);
				let ib = (wr.bottom - (tl.y + cr.bottom)).max(0);
				SetWindowPos(
					hwnd,
					HWND_TOPMOST,
					r.left - il,
					r.top - it,
					mw + il + ir,
					mh + it + ib,
					SWP_SHOWWINDOW | SWP_FRAMECHANGED,
				);
			}
		} else if let Some((p, s)) = saved {
			SetWindowPos(
				hwnd,
				HWND_NOTOPMOST,
				p.x,
				p.y,
				s.width as i32,
				s.height as i32,
				SWP_SHOWWINDOW | SWP_FRAMECHANGED,
			);
		} else {
			SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
		}
		// The webview child (WRY_WEBVIEW) keeps its PRE-toggle bounds: wry only re-lays
		// it out on size events of the OLD frame, so after the style change it sits
		// inset by the former 8px resize borders — a transparent gutter showing the
		// desktop around the fullscreen video. Snap every direct child to the fresh
		// client rect (webview AND the render child; the frontend's viewrect then
		// repositions the video inside it).
		fill_children_to_client(hwnd);
	}
}

/// Resize all direct children of `hwnd` to its current client rect (Win32 child
/// coords are client-relative, so that's (0,0,w,h)).
/// Snap every DIRECT child window (the WRY_WEBVIEW host + the native video render child) to the
/// parent's client rect. Used on a fullscreen resize: native `set_fullscreen` resizes the main
/// window, but the occluded webview's `ResizeObserver` is starved so the frontend's render-child
/// report can fire with a stale (windowed) rect — leaving the video small with black bars. Snapping
/// here (driven by the actual `Resized` event) restores the immersive fill synchronously, like the
/// old `win_fullscreen` path. Only correct WHILE fullscreen (caller gates on it) — windowed leaves
/// the render child to the frontend, which tracks the video area, not the whole window.
#[cfg(windows)]
pub(crate) unsafe fn fill_children_to_client(hwnd: windows_sys::Win32::Foundation::HWND) {
	use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
	use windows_sys::Win32::UI::WindowsAndMessaging::{
		EnumChildWindows, GetAncestor, GetClientRect, MoveWindow, GA_PARENT,
	};
	let mut rc: RECT = std::mem::zeroed();
	if GetClientRect(hwnd, &mut rc) == 0 || rc.right <= 0 || rc.bottom <= 0 {
		return;
	}
	struct Ctx {
		parent: HWND,
		w: i32,
		h: i32,
	}
	unsafe extern "system" fn cb(child: HWND, lp: LPARAM) -> i32 {
		let ctx = &*(lp as *const Ctx);
		// Direct children only — grandchildren (the webview's internal chain) follow
		// their own parent's resize.
		if GetAncestor(child, GA_PARENT) == ctx.parent {
			MoveWindow(child, 0, 0, ctx.w, ctx.h, 1);
		}
		1
	}
	let ctx = Ctx {
		parent: hwnd,
		w: rc.right,
		h: rc.bottom,
	};
	EnumChildWindows(hwnd, Some(cb), &ctx as *const Ctx as LPARAM);
}

/// Confine the OS pointer to the main window while the VNC-style control path is
/// engaged (phone hosts / native remote), so the cursor can't walk off the app
/// mid-drag. tao's Linux `set_cursor_grab` is a silent no-op, so Linux issues a raw
/// server-side `XGrabPointer` with `confine_to` on the app's own X connection.
/// NOT `gdk_pointer_grab`: GDK's grab adds a CLIENT-side redirect that reroutes every
/// button event to the grab window's widget (the toplevel), so the webview stopped
/// receiving clicks entirely. With a raw X grab and `owner_events=True` the server
/// delivers events to this client's windows exactly as if no grab were active — the
/// webview keeps clicking — while the pointer stays confined to the toplevel.
/// Windows/macOS use tao's grab (ClipCursor / CGAssociateMouseAndMouseCursorPosition).
#[tauri::command]
pub(crate) async fn confine_pointer(app: AppHandle, on: bool) -> Result<(), String> {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		use tauri::Manager;
		let _ = app.clone().run_on_main_thread(move || unsafe {
			use gtk::glib::Cast;
			use gtk::glib::translate::ToGlibPtr;
			use gtk::prelude::WidgetExt;
			use x11::xlib;
			let dpy = gtk::gdk::Display::default()
				.and_then(|d| d.downcast::<gdkx11::X11Display>().ok())
				.map(|d| {
					let stash = ToGlibPtr::<*mut gdkx11::ffi::GdkX11Display>::to_glib_none(&d);
					gdkx11::ffi::gdk_x11_display_get_xdisplay(stash.0)
				});
			let Some(dpy) = dpy else { return }; // not on X11 (native Wayland override)
			if !on {
				xlib::XUngrabPointer(dpy, xlib::CurrentTime);
				xlib::XFlush(dpy);
				return;
			}
			let xid = app
				.get_webview_window("main")
				.and_then(|w| w.gtk_window().ok())
				.and_then(|gw| gw.window())
				.and_then(|gdkw| gdkw.downcast::<gdkx11::X11Window>().ok())
				.map(|x| x.xid());
			let Some(xid) = xid else { return };
			let mask = (xlib::PointerMotionMask
				| xlib::ButtonPressMask
				| xlib::ButtonReleaseMask
				| xlib::ButtonMotionMask) as u32;
			let status = xlib::XGrabPointer(
				dpy,
				xid,
				xlib::True,
				mask,
				xlib::GrabModeAsync,
				xlib::GrabModeAsync,
				xid, // confine_to: the app toplevel
				0,   // keep the current cursor
				xlib::CurrentTime,
			);
			xlib::XFlush(dpy);
			if status != xlib::GrabSuccess {
				tracing::warn!("confine_pointer: XGrabPointer failed status={status}");
			}
		});
	}
	#[cfg(any(not(unix), target_os = "macos"))]
	{
		use tauri::Manager;
		if let Some(w) = app.get_webview_window("main") {
			let _ = w.set_cursor_grab(on);
		}
	}
	Ok(())
}
