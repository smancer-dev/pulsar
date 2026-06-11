//! Windows capture implementation: the Interception driver path (works under
//! ASTER multiseat) with a `WH_KEYBOARD_LL` hook fallback, plus the shared
//! key/mouse forwarding + leave-combo logic.

use super::*;
use libloading::Library;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::Emitter;
use tokio::sync::mpsc::Sender;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
	CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
	TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSG,
	WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

mod keymap;
use keymap::{scancode_to_evdev, vk_to_evdev};

/// Mutable globals the callback reaches through a `OnceLock<Mutex<…>>`. The
/// callback is `extern "system"` and cannot capture, so everything it needs is
/// here. `tx` is a clone of the active play's input channel; swapping it (on
/// focus change between tabs) is just a re-store under the same lock.
struct Globals {
	tx: Option<Sender<InputEvent>>,
	app: Option<AppHandle>,
	hook_thread_id: u32,
	ctrl_down: bool,
	alt_down: bool,
	shift_down: bool,
}

static GLOBALS: OnceLock<Mutex<Globals>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Set once the hook thread has installed the hook + recorded its thread id.
static THREAD_STARTED: AtomicBool = AtomicBool::new(false);

fn globals() -> &'static Mutex<Globals> {
	GLOBALS.get_or_init(|| {
		Mutex::new(Globals {
			tx: None,
			app: None,
			hook_thread_id: 0,
			ctrl_down: false,
			alt_down: false,
			shift_down: false,
		})
	})
}

// ─────────────────────────── Interception path ───────────────────────────
// On machines where the Interception driver is installed (incl. ASTER multiseat,
// where WH_KEYBOARD_LL is bypassed) we capture *below* the OS hook layer via the
// driver's user-mode DLL, loaded at runtime so the app still runs without it.

type ItCreate = unsafe extern "C" fn() -> *mut c_void;
type ItPredicate = extern "C" fn(i32) -> i32;
type ItSetFilter = unsafe extern "C" fn(*mut c_void, ItPredicate, u16);
type ItWait = unsafe extern "C" fn(*mut c_void) -> i32;
type ItReceive = unsafe extern "C" fn(*mut c_void, i32, *mut u8, u32) -> i32;
type ItSend = unsafe extern "C" fn(*mut c_void, i32, *const u8, u32) -> i32;

/// Resolved Interception entry points + a live context. `_lib` keeps the DLL
/// mapped (the fn pointers point into it). All calls happen on the capture
/// thread; the struct is treated as Send/Sync so it can live in a static.
struct Interception {
	_lib: Library,
	set_filter: ItSetFilter,
	wait: ItWait,
	receive: ItReceive,
	send: ItSend,
	is_kbd: ItPredicate,
	is_mouse: ItPredicate,
	ctx: *mut c_void,
}
unsafe impl Send for Interception {}
unsafe impl Sync for Interception {}

static INTERCEPT: OnceLock<Interception> = OnceLock::new();
/// 0 = undecided, 1 = Interception driver, 2 = WH_KEYBOARD_LL fallback.
static MECHANISM: AtomicU8 = AtomicU8::new(0);
/// Also capture the MOUSE (relative) — only in native-renderer mode, where there's
/// no webview canvas to read pointer events from.
static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Turn the Interception keyboard (and, in native mode, mouse) filter on while
/// controlling, off otherwise. Only the controlling instance keeps it on, so
/// concurrent instances on the same box don't fight over input. No-op unless the
/// Interception mechanism is active.
fn set_capture_filter(on: bool) {
	if MECHANISM.load(Ordering::SeqCst) != 1 {
		return;
	}
	if let Some(it) = INTERCEPT.get() {
		// KEY_DOWN(0x01) | KEY_UP(0x02) while on; NONE(0x00) while off.
		unsafe { (it.set_filter)(it.ctx, it.is_kbd, if on { 0x0003 } else { 0x0000 }) };
		if MOUSE_CAPTURE.load(Ordering::SeqCst) {
			// FILTER_MOUSE_ALL (0xFFFF) captures move + buttons + wheel.
			unsafe { (it.set_filter)(it.ctx, it.is_mouse, if on { 0xFFFF } else { 0x0000 }) };
		}
	}
}

/// Load interception.dll (shipped next to the exe) and open a context. Returns
/// None if the DLL is missing or the driver isn't installed/active — the caller
/// then falls back to the WH_KEYBOARD_LL hook.
fn init_interception() -> Option<Interception> {
	let lib = unsafe { Library::new("interception.dll") }.ok()?;
	unsafe {
		let create: ItCreate = *lib.get(b"interception_create_context\0").ok()?;
		let set_filter: ItSetFilter = *lib.get(b"interception_set_filter\0").ok()?;
		let wait: ItWait = *lib.get(b"interception_wait\0").ok()?;
		let receive: ItReceive = *lib.get(b"interception_receive\0").ok()?;
		let send: ItSend = *lib.get(b"interception_send\0").ok()?;
		let is_kbd: ItPredicate = *lib.get(b"interception_is_keyboard\0").ok()?;
		let is_mouse: ItPredicate = *lib.get(b"interception_is_mouse\0").ok()?;
		let ctx = create();
		if ctx.is_null() {
			return None;
		}
		Some(Interception {
			_lib: lib,
			set_filter,
			wait,
			receive,
			send,
			is_kbd,
			is_mouse,
			ctx,
		})
	}
}

/// Persistent capture thread for the Interception path. Mirrors the proven probe:
/// set the keyboard filter ONCE and **block** on `interception_wait` (the
/// `wait_with_timeout` variant proved unreliable on the installed driver — it
/// never delivered keys). Every key flows through here; we only *suppress + forward*
/// while ENABLED (controlling), otherwise we pass it straight through so local
/// typing is unaffected.
fn capture_thread() {
	let Some(it) = INTERCEPT.get() else { return };
	// NOTE: the filter is toggled by enable()/disable() (set only while THIS
	// instance is controlling), so non-controlling instances don't steal keys from
	// the active one (multi-instance on the same ASTER box). The thread just blocks
	// until a filtered key arrives.
	let mut stroke = [0u8; 24];
	loop {
		let device = unsafe { (it.wait)(it.ctx) }; // blocks until a keystroke
		if device <= 0 {
			continue;
		}
		let n = unsafe { (it.receive)(it.ctx, device, stroke.as_mut_ptr(), 1) };
		if n <= 0 {
			continue;
		}
		let armed = ENABLED.load(Ordering::SeqCst);
		let suppress = if !armed {
			false // not controlling → pass everything straight through
		} else if (it.is_kbd)(device) != 0 {
			// Keyboard stroke: code@0, state@2 (KEY_UP 0x01, E0 0x02, E1 0x04).
			let code = u16::from_le_bytes([stroke[0], stroke[1]]);
			let state = u16::from_le_bytes([stroke[2], stroke[3]]);
			if state & 0x04 != 0 {
				false // E1 (Pause/Break) 2-stroke seq we can't map cleanly — pass it
			} else {
				match scancode_to_evdev(code, state & 0x02 != 0) {
					Some(ev) => handle_key(ev, state & 0x01 == 0),
					None => false,
				}
			}
		} else {
			// Mouse stroke: state@0 (button/wheel flags), flags@2 (0x01=ABSOLUTE),
			// rolling@4 (wheel), x@8, y@12 (i32 deltas for relative mice).
			let mstate = u16::from_le_bytes([stroke[0], stroke[1]]);
			let mflags = u16::from_le_bytes([stroke[2], stroke[3]]);
			let rolling = i16::from_le_bytes([stroke[4], stroke[5]]);
			let x = i32::from_le_bytes([stroke[8], stroke[9], stroke[10], stroke[11]]);
			let y = i32::from_le_bytes([stroke[12], stroke[13], stroke[14], stroke[15]]);
			handle_mouse(mstate, mflags, rolling, x, y)
		};
		if !suppress {
			unsafe { (it.send)(it.ctx, device, stroke.as_ptr(), 1) };
		}
	}
}

/// Shared key handling for both capture paths: track modifiers, fire the leave
/// combo (Ctrl+Shift+F12 — Parsec-style Ctrl+Shift+key; F12 never occurs in
/// gameplay and the chord doesn't collide with any Win11 shell shortcut), forward
/// to the active session. Returns true if the key should be SUPPRESSED locally (it
/// then only acts on the remote).
fn handle_key(evdev: u32, down: bool) -> bool {
	let mut g = globals().lock().unwrap();
	match evdev {
		29 | 97 => g.ctrl_down = down,  // L/R Ctrl
		56 | 100 => g.alt_down = down,  // L/R Alt
		42 | 54 => g.shift_down = down, // L/R Shift
		_ => {}
	}
	// Overlay-toggle combo: Ctrl+Shift+M (evdev 50). Same frontend handler as
	// Linux — opens the game menu — but Windows keeps capture ON (the webview
	// HW-decodes and the overlay floats over the live canvas; no ungrab). Suppress
	// the M so it never leaks into the game. Does NOT end the session.
	if down && evdev == 50 && g.ctrl_down && g.shift_down {
		if let Some(app) = &g.app {
			let _ = app.emit("overlay-toggle", ());
		}
		return true;
	}
	// The webview never sees these keys (suppressed first), so the leave combo must
	// be detected here. F12 (evdev 88) while Ctrl+Shift are held → drop control;
	// tell the UI via an event.
	if down && evdev == 88 && g.ctrl_down && g.shift_down {
		if let Some(app) = &g.app {
			let _ = app.emit("kbd-leave", ());
		}
		return false;
	}
	if let Some(tx) = &g.tx {
		// Key repeat re-fires down=true; forward each (the host de-dupes).
		let _ = tx.try_send(InputEvent::Key { code: evdev, down });
		return true;
	}
	false
}

/// Forward a captured mouse stroke (native-renderer mode) to the host as relative
/// motion + buttons + wheel, and suppress it locally (cursor lock — the mouse only
/// drives the remote). Returns true (suppress) whenever a session is active.
fn handle_mouse(state: u16, flags: u16, rolling: i16, x: i32, y: i32) -> bool {
	let g = globals().lock().unwrap();
	let Some(tx) = &g.tx else { return false };
	// Relative movement (skip absolute-coordinate mice — 0x001 = ABSOLUTE flag).
	if flags & 0x001 == 0 && (x != 0 || y != 0) {
		let _ = tx.try_send(InputEvent::PointerRelative {
			dx: x as f64,
			dy: y as f64,
		});
	}
	// Buttons (Interception mouse-state bitflags → 0=left, 1=right, 2=middle).
	for (bit, button, down) in [
		(0x001u16, 0u8, true),
		(0x002, 0, false),
		(0x004, 1, true),
		(0x008, 1, false),
		(0x010, 2, true),
		(0x020, 2, false),
	] {
		if state & bit != 0 {
			let _ = tx.try_send(InputEvent::PointerButton { button, down });
		}
	}
	// Wheel: state carries the WHEEL bit (0x400), rolling is the signed delta.
	if state & 0x400 != 0 && rolling != 0 {
		let _ = tx.try_send(InputEvent::Scroll {
			dx: 0.0,
			dy: -(rolling as f64),
		});
	}
	true
}

/// Arm capture for `tx`'s session. The first call picks a mechanism: the
/// Interception driver if present (works under ASTER), else WH_KEYBOARD_LL. Later
/// calls just swap in the active sender + app handle and flip ENABLED.
pub fn enable(app: AppHandle, tx: Sender<InputEvent>, mouse: bool) {
	{
		let mut g = globals().lock().unwrap();
		g.tx = Some(tx);
		g.app = Some(app);
		g.ctrl_down = false;
		g.alt_down = false;
		g.shift_down = false;
	}
	// Native-renderer sessions also capture the mouse (relative); webview sessions
	// read the pointer from the canvas instead, so keep mouse capture off there.
	MOUSE_CAPTURE.store(mouse, Ordering::SeqCst);
	ENABLED.store(true, Ordering::SeqCst);
	match MECHANISM.load(Ordering::SeqCst) {
		0 => {
			// First arm — decide the capture mechanism once.
			if let Some(it) = init_interception() {
				let _ = INTERCEPT.set(it);
				MECHANISM.store(1, Ordering::SeqCst);
				std::thread::spawn(capture_thread);
			} else {
				MECHANISM.store(2, Ordering::SeqCst);
				THREAD_STARTED.store(true, Ordering::SeqCst);
				std::thread::spawn(hook_thread);
			}
		}
		2 => {
			// WH fallback tears its thread down on disable; re-spawn if needed.
			if !THREAD_STARTED.swap(true, Ordering::SeqCst) {
				std::thread::spawn(hook_thread);
			}
		}
		_ => {} // Interception: persistent thread; the filter is what arms it.
	}
	// Interception: turn the filter ON so this (controlling) instance captures.
	set_capture_filter(true);
}

/// Disarm: stop suppressing + forwarding, drop the sender, and tear the hook
/// thread down cleanly (posts WM_QUIT, which exits the pump and unhooks).
pub fn disable() {
	ENABLED.store(false, Ordering::SeqCst);
	// Interception: drop the filter so we stop capturing (frees keys for any other
	// instance + the local OS).
	set_capture_filter(false);
	let tid = {
		let mut g = globals().lock().unwrap();
		g.tx = None;
		g.ctrl_down = false;
		g.alt_down = false;
		g.shift_down = false;
		g.hook_thread_id
	};
	if tid != 0 {
		unsafe { PostThreadMessageW(tid, WM_QUIT, 0, 0) };
		THREAD_STARTED.store(false, Ordering::SeqCst);
	}
}

/// Overlay open/close. On Windows the webview HW-decodes and the overlay floats
/// over the still-live canvas, so there's no grab to release — capture stays ON
/// (Ctrl+Shift+M only opens the menu; see `handle_key`). No-op for grab state.
pub fn overlay_suspend(_suspend: bool) {}

/// Window focus changed. Windows uses a global LL/Interception hook with its own
/// focus handling; no-op here (the Linux evdev-grab focus gate is the one that matters).
pub fn set_focused(_focused: bool) {}

/// Owns the hook for its lifetime: install → pump messages → uninstall.
fn hook_thread() {
	unsafe {
		let hmod = GetModuleHandleW(std::ptr::null());
		let hook: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0);
		if hook.is_null() {
			THREAD_STARTED.store(false, Ordering::SeqCst);
			return;
		}
		globals().lock().unwrap().hook_thread_id = GetCurrentThreadId();
		// Required: LL hooks deliver via this thread's message queue.
		let mut msg: MSG = std::mem::zeroed();
		// GetMessageW returns 0 on WM_QUIT, -1 on error.
		while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
			TranslateMessage(&msg);
			DispatchMessageW(&msg);
		}
		UnhookWindowsHookEx(hook);
		globals().lock().unwrap().hook_thread_id = 0;
	}
}

/// The LL keyboard callback. Runs on the hook thread for every key system-wide.
unsafe extern "system" fn ll_keyboard_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
	// Per docs: if ncode < 0 we must pass through untouched.
	if ncode < 0 || !ENABLED.load(Ordering::SeqCst) {
		return CallNextHookEx(std::ptr::null_mut(), ncode, wparam, lparam);
	}
	let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
	// Never consume injected events (we don't inject on the client, but this
	// keeps us robust against feedback loops / accessibility tools).
	if kb.flags & LLKHF_INJECTED != 0 {
		return CallNextHookEx(std::ptr::null_mut(), ncode, wparam, lparam);
	}
	let msg = wparam as u32;
	let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN) && (kb.flags & LLKHF_UP == 0);
	// Shared with the Interception path: forward + combo detection + suppress.
	if let Some(evdev) = vk_to_evdev(kb.vkCode as u16) {
		if handle_key(evdev, down) {
			return 1; // SUPPRESS locally — the key only acts on the remote.
		}
	}
	// Unknown VK (no evdev mapping) or no active sender: don't break the local
	// machine — let it through normally.
	CallNextHookEx(std::ptr::null_mut(), ncode, wparam, lparam)
}
