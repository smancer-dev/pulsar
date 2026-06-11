//! Client-side I/O commands: the side channels (clipboard / chat / file / mic),
//! input forwarding, the Windows keyboard-capture toggle, and the fullscreen
//! window toggle (with the Win32 borderless-fullscreen helper).

use std::io::Read;

use pulsar_core::service::{DataMsg, InputEvent};
use tauri::{AppHandle, State};

use crate::audio_io::spawn_mic_recorder;
use crate::kbdhook;
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

/// Host → client: reply to a connected peer's chat.
#[tauri::command]
pub(crate) async fn host_send_chat(
	state: State<'_, AppState>,
	peer: String,
	text: String,
) -> Result<(), String> {
	let tx = state
		.host_out
		.lock()
		.unwrap()
		.get(&peer)
		.map(|(_, tx)| tx.clone());
	let tx = tx.ok_or_else(|| "cihaz bağlı değil".to_string())?;
	tx.send(DataMsg::Chat(text.clone()))
		.await
		.map_err(|_| crate::i18n::t("err.message").to_string())?;
	// Into the backlog too: sent lines have no broadcast event of their own, so a
	// re-opened connections window rebuilds the full conversation from here.
	state.chat_log.lock().unwrap().push((peer, text, true));
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
	tx.send(DataMsg::FileBegin {
		name,
		size: data.len() as u64,
		chunks,
	})
	.await
	.map_err(|_| crate::i18n::t("err.file").to_string())?;
	for (i, ch) in data.chunks(CHUNK).enumerate() {
		tx.send(DataMsg::FileChunk {
			index: i as u32,
			data: ch.to_vec(),
		})
		.await
		.map_err(|_| crate::i18n::t("err.file").to_string())?;
	}
	tx.send(DataMsg::FileEnd)
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
pub(crate) async fn mic_start(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	let (tx, mic_slot) = {
		let plays = state.plays.lock().unwrap();
		let p = plays.get(&id).ok_or(crate::i18n::t("err.session"))?;
		(p.data_tx.clone(), p.mic.clone())
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
	let mut child =
		spawn_mic_recorder().ok_or(crate::i18n::t("err.micRecorder"))?;
	let stdout = child.stdout.take().ok_or(crate::i18n::t("err.micOutput"))?;
	*slot = Some(child);
	drop(slot);
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
	let (tx, mic_slot) = {
		let plays = state.plays.lock().unwrap();
		let Some(p) = plays.get(&id) else {
			return Ok(());
		};
		(p.data_tx.clone(), p.mic.clone())
	};
	if let Some(mut c) = mic_slot.lock().unwrap().take() {
		let _ = c.kill();
	}
	let _ = tx.send(DataMsg::AudioEnd).await;
	Ok(())
}

/// Client: forward absolute pointer motion (normalized 0..1) to the host.
#[tauri::command]
pub(crate) async fn input_pointer(
	state: State<'_, AppState>,
	id: u64,
	x: f64,
	y: f64,
) -> Result<(), String> {
	forward(&state, id, InputEvent::PointerMotion { x, y });
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
	forward(&state, id, InputEvent::PointerButton { button, down });
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
	forward(&state, id, InputEvent::Scroll { dx, dy });
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
	forward(&state, id, InputEvent::Key { code, down });
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
			kbdhook::enable(app, tx, mouse);
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
		use std::io::Write as _;
		if let Some(p) = state.plays.lock().unwrap().get(&id) {
			if let Some(si) = p.render_stdin.lock().unwrap().as_mut() {
				let _ = writeln!(si, "viewrect {} {} {} {}", px(x), px(y), px(w), px(h));
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
	if on {
		if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
			// Save only when ENTERING from windowed state: an on=true while already
			// fullscreen (the F12 combo and the UI toggle racing out of sync) would
			// otherwise capture the fullscreen rect, and exiting would "restore" a
			// fullscreen-sized borderless window instead of the windowed geometry.
			let mut g = state.fs_geom.lock().unwrap();
			if g.is_none() {
				*g = Some((pos, size));
			}
		}
	}
	let saved = if on {
		None
	} else {
		state.fs_geom.lock().unwrap().take()
	};
	// Native decorations are on now, so hide the OS title bar / frame while fullscreen (e.g. a
	// game) and restore it on exit — otherwise the title bar would sit across the top.
	let _ = window.set_decorations(!on);
	#[cfg(windows)]
	{
		let w = window.clone();
		// Drive Win32 on the UI thread so SetWindowPos targets the window correctly.
		let _ = window.run_on_main_thread(move || win_fullscreen(&w, on, saved));
	}
	#[cfg(not(windows))]
	{
		let _ = saved; // geometry restore is a Windows-only concern here
				 // GTK window ops must run on the main (GTK) thread; a Tauri command runs off it, so
				 // dispatch there — calling set_fullscreen directly off-thread can silently no-op.
		let w = window.clone();
		let _ = window.run_on_main_thread(move || {
			// Leaving fullscreen must also drop topmost: the Focused handler in lib.rs only
			// manages always-on-top while fs_geom is Some, and fs_geom was just cleared above —
			// without this the window stays above every other app forever (the Windows branch
			// already does the equivalent via HWND_NOTOPMOST).
			if !on {
				let _ = w.set_always_on_top(false);
			}
			let _ = w.set_fullscreen(on); // X11/Wayland/macOS hide the panel/dock correctly
								 // Keep input focus on the GTK toplevel after the toggle — otherwise X moves focus
								 // to the embedded native-renderer child window, which flips kbdhook's FOCUSED to
								 // false and silently kills the evdev combos (F12/M/Q) until the user clicks back in.
			let _ = w.set_focus();
		});
	}
	Ok(())
}

/// Windows borderless-fullscreen via raw Win32: cover the window's monitor and go
/// top-most, which (unlike `set_fullscreen` on a transparent window) reliably hides
/// the taskbar. Restores the saved windowed rect when turning off.
#[cfg(windows)]
fn win_fullscreen(
	window: &tauri::WebviewWindow,
	on: bool,
	saved: Option<(tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>)>,
) {
	use windows_sys::Win32::Foundation::{HWND, RECT};
	use windows_sys::Win32::Graphics::Gdi::{
		GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
	};
	use windows_sys::Win32::UI::WindowsAndMessaging::{
		SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOMOVE,
		SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
	};
	let Ok(handle) = window.hwnd() else {
		return;
	};
	let hwnd: HWND = handle.0 as _;
	unsafe {
		if on {
			// Clear any maximize/minimize first, else SetWindowPos is clamped to the
			// work area (taskbar stays visible).
			ShowWindow(hwnd, SW_RESTORE);
			let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
			let mut mi: MONITORINFO = std::mem::zeroed();
			mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
			if GetMonitorInfoW(mon, &mut mi) != 0 {
				let r: RECT = mi.rcMonitor;
				SetWindowPos(
					hwnd,
					HWND_TOPMOST,
					r.left,
					r.top,
					r.right - r.left,
					r.bottom - r.top,
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
	}
}
