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
pub(crate) async fn send_clipboard(state: State<'_, AppState>, id: u64, text: String) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Clipboard(text))
		.await
		.map_err(|_| "pano gönderilemedi".to_string())
}

/// Client → host: send a chat line.
#[tauri::command]
pub(crate) async fn send_chat(state: State<'_, AppState>, id: u64, text: String) -> Result<(), String> {
	data_sender(&state, id)?
		.send(DataMsg::Chat(text))
		.await
		.map_err(|_| "mesaj gönderilemedi".to_string())
}

/// Host → client: reply to a connected peer's chat.
#[tauri::command]
pub(crate) async fn host_send_chat(
	state: State<'_, AppState>,
	peer: String,
	text: String,
) -> Result<(), String> {
	let tx = state.host_out.lock().unwrap().get(&peer).map(|(_, tx)| tx.clone());
	tx.ok_or_else(|| "cihaz bağlı değil".to_string())?
		.send(DataMsg::Chat(text))
		.await
		.map_err(|_| "mesaj gönderilemedi".to_string())
}

/// Client → host: send a file (chunked over the session, saved on the host).
#[tauri::command]
pub(crate) async fn send_file(
	state: State<'_, AppState>,
	id: u64,
	name: String,
	data: Vec<u8>,
) -> Result<(), String> {
	const CHUNK: usize = 16 * 1024;
	let tx = data_sender(&state, id)?;
	let chunks = data.len().div_ceil(CHUNK) as u32;
	tx.send(DataMsg::FileBegin {
		name,
		size: data.len() as u64,
		chunks,
	})
	.await
	.map_err(|_| "dosya gönderilemedi".to_string())?;
	for (i, ch) in data.chunks(CHUNK).enumerate() {
		tx.send(DataMsg::FileChunk {
			index: i as u32,
			data: ch.to_vec(),
		})
		.await
		.map_err(|_| "dosya gönderilemedi".to_string())?;
	}
	tx.send(DataMsg::FileEnd)
		.await
		.map_err(|_| "dosya gönderilemedi".to_string())
}

/// Client: start streaming the microphone to the host (raw PCM over the session).
#[tauri::command]
pub(crate) async fn mic_start(state: State<'_, AppState>, id: u64) -> Result<(), String> {
	let (tx, mic_slot) = {
		let plays = state.plays.lock().unwrap();
		let p = plays.get(&id).ok_or("oturum bulunamadı")?;
		(p.data_tx.clone(), p.mic.clone())
	};
	if mic_slot.lock().unwrap().is_some() {
		return Ok(()); // already on
	}
	let mut child =
		spawn_mic_recorder().ok_or("mikrofon kaydedici bulunamadı (parecord/pw-record/arecord)")?;
	let stdout = child.stdout.take().ok_or("mikrofon çıkışı alınamadı")?;
	*mic_slot.lock().unwrap() = Some(child);
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
pub(crate) async fn input_pointer(state: State<'_, AppState>, id: u64, x: f64, y: f64) -> Result<(), String> {
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
pub(crate) async fn input_scroll(state: State<'_, AppState>, id: u64, dx: f64, dy: f64) -> Result<(), String> {
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
		None => Err("oturum bulunamadı".into()),
	}
}

/// Client: disarm the keyboard hook (control released / canvas blurred / leave
/// combo). Local keys behave normally again. No-op on non-Windows.
#[tauri::command]
pub(crate) async fn kbd_capture_stop() -> Result<(), String> {
	kbdhook::disable();
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
			*state.fs_geom.lock().unwrap() = Some((pos, size));
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
