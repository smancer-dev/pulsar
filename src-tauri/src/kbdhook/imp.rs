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
	/// Recent RightCtrl press times for the 3×RightCtrl release combo (≤1 s window).
	rctrl_presses: Vec<std::time::Instant>,
	/// Every key currently held DOWN on the host (forwarded down, no up yet). Flushed
	/// as up-strokes whenever the gate closes (disengage/disable) — otherwise a combo
	/// like Ctrl+Shift+M leaves Ctrl+Shift stuck on the host (their up-strokes arrive
	/// after the gate closed and are never forwarded).
	held: std::collections::HashSet<u32>,
	/// Every mouse BUTTON currently held DOWN on the host (0=left/1=right/2=middle).
	/// Same guarantee as `held` for buttons: a press-and-hold (drag) followed by a
	/// disengage edge (click-outside implicit release, 3×RightCtrl, Ctrl+Alt+Z,
	/// focus-loss, disable) would otherwise leave the button stuck DOWN on the host —
	/// the up-stroke never gets forwarded because the gate has already closed.
	held_buttons: std::collections::HashSet<u8>,
}

static GLOBALS: OnceLock<Mutex<Globals>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Set once the hook thread has installed the hook + recorded its thread id.
static THREAD_STARTED: AtomicBool = AtomicBool::new(false);
/// Click-to-engage gate (parity with the Linux evdev capture): the hook is ARMED
/// (ENABLED) for the whole native session, but until the user clicks the video it
/// neither forwards nor suppresses anything — the local desktop stays usable, and a
/// broken session can't trap the keyboard. Set by `engage()` (video click → renderer
/// `ov engage` line), cleared by `release_engage()` / the 3×RightCtrl combo.
static ENGAGED: AtomicBool = AtomicBool::new(false);
/// The play id the capture is currently armed for (same-session re-arm detection).
/// NOT reset by `disable()`: the real tab deactivate→reactivate path is the frontend
/// calling kbd_capture_stop()→disable() and THEN kbd_capture_start()→enable() for the
/// same id, so the engagement memory must survive disable() (see ENGAGED_MEMO).
static LAST_PLAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
/// Engagement intent for `LAST_PLAY`, remembered ACROSS `disable()`. disable() drops the
/// live ENGAGED (capture must stop on a stop), but a stop immediately followed by an
/// enable() for the SAME id (the tab-switch churn) restores the user's engagement from
/// here — otherwise same-session preservation never fires (ENABLED is already false by
/// the time enable() runs). A different id ignores this and starts disengaged.
static ENGAGED_MEMO: AtomicBool = AtomicBool::new(false);
/// CLI kiosk (`--connect`) auto-engages on the next `enable()` (same as Linux).
static KIOSK_ENGAGE: AtomicBool = AtomicBool::new(false);
/// True for the lifetime of a kiosk-started session: focus loss must NOT disengage
/// (the appliance runs nothing else, and the auto-launched window may never get
/// focus at all under focus-stealing prevention). Cleared on `disable()`.
static KIOSK_SESSION: AtomicBool = AtomicBool::new(false);
/// Any app window focused (from the Tauri focus map via `set_focused`) — gates the
/// disengaged-state combos (Ctrl+Shift+M) so a GLOBAL hook never reacts while the
/// user is in another application.
static APP_FOCUSED: AtomicBool = AtomicBool::new(true);

fn globals() -> &'static Mutex<Globals> {
	GLOBALS.get_or_init(|| {
		Mutex::new(Globals {
			tx: None,
			app: None,
			hook_thread_id: 0,
			ctrl_down: false,
			alt_down: false,
			shift_down: false,
			rctrl_presses: Vec::new(),
			held: std::collections::HashSet::new(),
			held_buttons: std::collections::HashSet::new(),
		})
	})
}

/// Forward one input event to the session.
///
/// The 256-slot input channel is drained one-await-per-event by the hold loop, so a
/// congested link can fill it. A bare `try_send` would then DROP the just-arrived
/// event — fatal if it's a button/key UP (a stuck key/button on the host that never
/// recovers mid-session). So coalescible motion may drop, but press/release EDGES
/// `blocking_send` to wait for a slot. The hook + Interception capture run on plain
/// `std::thread::spawn` threads (never a tokio worker), so blocking here is safe.
fn fwd(tx: &Sender<InputEvent>, ev: InputEvent) {
	if crate::util::is_coalescible_input(&ev) {
		let _ = tx.try_send(ev);
	} else {
		let _ = tx.blocking_send(ev);
	}
}

/// Send up-strokes for every key still held on the host and clear the set — called
/// whenever forwarding stops (disengage, disable) so no modifier stays stuck remotely.
fn flush_held(g: &mut Globals) {
	if let Some(tx) = &g.tx {
		for code in g.held.drain() {
			fwd(tx, InputEvent::Key { code, down: false });
		}
		// Same for held mouse buttons (drag-and-hold left/right/middle): release each
		// before the gate closes so a click-outside / leave-combo can't leave the host
		// with a stuck button (runaway drag-select). Mirrors the Linux flush_held.
		for button in g.held_buttons.drain() {
			fwd(tx, InputEvent::PointerButton { button, down: false });
		}
	} else {
		g.held.clear();
		g.held_buttons.clear();
	}
	// Drop the LOCAL chord-modifier tracking too. The combos (Ctrl+Shift+M / Q / F12)
	// and the Ctrl+Alt+Z / 3×RightCtrl release gate on these booleans, and unlike Linux
	// (which re-queries the kernel via `chord_mods` at the trigger moment) Windows trusts
	// them — so a modifier whose UP-stroke is never observed after a disengage edge (the
	// physical key was released while the hook was bypassed, suppressed, or the event was
	// injected) would stay latched true and let a BARE later key mis-fire a combo while
	// merely app-focused. flush_held runs on EVERY disengage/teardown edge, so resetting
	// here keeps the combo gate from ever acting on a stale modifier after control drops.
	g.ctrl_down = false;
	g.alt_down = false;
	g.shift_down = false;
	g.rctrl_presses.clear();
}

/// Arm kiosk auto-engage: the next `enable()` engages immediately (CLI `--connect`).
pub(super) fn arm_kiosk() {
	KIOSK_ENGAGE.store(true, Ordering::SeqCst);
}

/// Cursor-confine rectangle (screen px, LTRB) re-asserted on every mouse event while
/// engaged — the OS clears ClipCursor on focus changes / display events, so a one-shot
/// clip wouldn't survive. `None` = not confined (released). Set by `confine_to_video`.
static CONFINE: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/// Re-apply the stored confine rect (cheap ClipCursor syscall; no window lookup). Called
/// from the mouse handler so the trap survives focus flaps. No-op when not confined.
fn reassert_confine() {
	use windows_sys::Win32::Foundation::RECT;
	use windows_sys::Win32::UI::WindowsAndMessaging::ClipCursor;
	if let Some((l, t, r, b)) = *CONFINE.lock().unwrap() {
		let rc = RECT {
			left: l,
			top: t,
			right: r,
			bottom: b,
		};
		unsafe {
			ClipCursor(&rc);
		}
	}
}

/// Free the cursor (ClipCursor(NULL)) and forget the confine rect — for disengage paths
/// that don't go through `release_engage` (disable / implicit click-outside release).
fn clear_confine() {
	use windows_sys::Win32::UI::WindowsAndMessaging::ClipCursor;
	*CONFINE.lock().unwrap() = None;
	unsafe {
		ClipCursor(std::ptr::null());
	}
}

/// Confine the cursor to the MONITOR showing the streamed video while engaged, so the
/// pointer can't wander onto another screen — but it can still move freely on that whole
/// monitor (NOT trapped to the Pulsar window: the user can reach the rest of the screen
/// and a click outside implicitly releases). `on=false` frees it (ClipCursor(NULL)).
/// Picks the monitor under the Pulsar main window; no-confine if it can't resolve.
fn confine_to_video(app: &AppHandle, on: bool) {
	use tauri::Manager as _;
	use windows_sys::Win32::Foundation::HWND;
	use windows_sys::Win32::Graphics::Gdi::{
		GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
	};
	use windows_sys::Win32::UI::WindowsAndMessaging::ClipCursor;

	if !on {
		*CONFINE.lock().unwrap() = None;
		unsafe {
			ClipCursor(std::ptr::null());
		}
		return;
	}
	let Some(main_hwnd) = app
		.get_webview_window("main")
		.and_then(|w| w.hwnd().ok())
		.map(|h| h.0 as HWND)
	else {
		return;
	};
	unsafe {
		let mon = MonitorFromWindow(main_hwnd, MONITOR_DEFAULTTONEAREST);
		let mut mi: MONITORINFO = std::mem::zeroed();
		mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
		if GetMonitorInfoW(mon, &mut mi) == 0 {
			return;
		}
		let rc = mi.rcMonitor;
		if rc.right <= rc.left || rc.bottom <= rc.top {
			return;
		}
		*CONFINE.lock().unwrap() = Some((rc.left, rc.top, rc.right, rc.bottom));
		ClipCursor(&rc);
	}
}

/// Take control: start forwarding + suppressing input. Idempotent; emits
/// `kbd-engaged` on the rising edge (drives the UI hint + renderer cursor state).
pub(super) fn engage(app: &AppHandle) {
	if !ENGAGED.swap(true, Ordering::SeqCst) {
		tracing::info!("kbd capture ENGAGED");
		// Trap the cursor to the streamed screen so it can't roam to another monitor.
		confine_to_video(app, true);
		let _ = app.emit("kbd-engaged", ());
	}
}

/// Drop control WITHOUT ending the session (the user keeps the video and can click
/// back in). Emits `kbd-released` on the falling edge and releases any keys still
/// held on the host.
pub(super) fn release_engage(app: &AppHandle) {
	if ENGAGED.swap(false, Ordering::SeqCst) {
		tracing::info!("kbd capture RELEASED");
		confine_to_video(app, false); // free the cursor — local desktop usable again
		flush_held(&mut globals().lock().unwrap());
		let _ = app.emit("kbd-released", ());
	}
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

/// Load interception.dll and open a context. Returns None if the DLL is missing
/// or the driver isn't installed/active — the caller then falls back to the
/// WH_KEYBOARD_LL hook.
fn init_interception() -> Option<Interception> {
	// Search order: next to the exe / PATH (the dev layout), then the bundled copy
	// under resources/interception/ — the INSTALLED app ships it there (NSIS
	// `resources` list), and without this fallback an installed Pulsar silently
	// dropped to the WH_KEYBOARD_LL hook, which ASTER bypasses ("engaged but
	// nothing forwards" on multiseat machines).
	let lib = unsafe { Library::new("interception.dll") }.ok().or_else(|| {
		let p = std::env::current_exe()
			.ok()?
			.parent()?
			.join("resources")
			.join("interception")
			.join("interception.dll");
		unsafe { Library::new(p) }.ok()
	})?;
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
	// Overlay-toggle combo: Ctrl+Shift+M — must work BOTH engaged and merely
	// app-focused (disengaged): the user released control but still wants the menu.
	// Never fires while another app has focus (the hook is global).
	if down
		&& evdev == 50
		&& g.ctrl_down
		&& g.shift_down
		&& (ENGAGED.load(Ordering::SeqCst) || APP_FOCUSED.load(Ordering::SeqCst))
	{
		// Release control RIGHT HERE, synchronously. The overlay's own release
		// (set_overlay → kbdhook::release) arrives over a webview IPC roundtrip that
		// can lag SECONDS while the video child occludes the webview (WebView2
		// occlusion throttling) — until it landed, the user's next click was still
		// captured and went to the host ("first click swallowed").
		if ENGAGED.swap(false, Ordering::SeqCst) {
			tracing::info!("kbd capture RELEASED (overlay combo, immediate)");
			flush_held(&mut g);
			if let Some(app) = &g.app {
				let _ = app.emit("kbd-released", ());
			}
		}
		if let Some(app) = &g.app {
			let _ = app.emit("overlay-toggle", ());
		}
		return true;
	}
	// End combo: Ctrl+Shift+Q — ends the session. Like the overlay combo it must work
	// BOTH engaged and merely app-focused (the user released control, video still up).
	if down
		&& evdev == 16
		&& g.ctrl_down
		&& g.shift_down
		&& (ENGAGED.load(Ordering::SeqCst) || APP_FOCUSED.load(Ordering::SeqCst))
	{
		flush_held(&mut g); // un-stick the chord on the (still-alive) host
		if let Some(app) = &g.app {
			let _ = app.emit("kbd-leave", ());
		}
		return true; // never leak the Q locally
	}
	// Fullscreen combo: Ctrl+Shift+F12 — toggles the window's fullscreen state; the
	// session AND control state stay as they are (this is NOT the leave combo).
	if down
		&& evdev == 88
		&& g.ctrl_down
		&& g.shift_down
		&& (ENGAGED.load(Ordering::SeqCst) || APP_FOCUSED.load(Ordering::SeqCst))
	{
		if let Some(app) = &g.app {
			let _ = app.emit("fullscreen-toggle", ());
		}
		return true;
	}
	// Click-to-engage gate: armed but not engaged → behave as if no hook were
	// installed (no forward, no suppress) so local typing is unaffected.
	if !ENGAGED.load(Ordering::SeqCst) {
		return false;
	}
	// Release combo (matches the on-screen hint): 3×RightCtrl within 1 s DISENGAGES
	// capture (session stays alive; click the video to take control again).
	if down && evdev == 97 {
		let now = std::time::Instant::now();
		g.rctrl_presses
			.retain(|t| now.duration_since(*t).as_millis() < 1000);
		g.rctrl_presses.push(now);
		if g.rctrl_presses.len() >= 3 {
			g.rctrl_presses.clear();
			// The combo's own down-stroke is on the host too — count it, then flush
			// everything held so nothing stays stuck remotely.
			g.held.insert(97);
			flush_held(&mut g);
			ENGAGED.store(false, Ordering::SeqCst);
			if let Some(app) = &g.app {
				let _ = app.emit("kbd-released", ());
			}
			return true;
		}
	}
	// Release combo #2 (the one the engage hint advertises): Ctrl+Alt+Z (evdev 44)
	// — Parsec's detach shortcut, same behavior as 3×RightCtrl above.
	if down && evdev == 44 && g.ctrl_down && g.alt_down {
		ENGAGED.store(false, Ordering::SeqCst);
		// Un-stick everything held on the host (the chord's up-strokes won't be forwarded).
		flush_held(&mut g);
		if let Some(app) = &g.app {
			let _ = app.emit("kbd-released", ());
		}
		return true; // never leak the Z locally
	}
	// Clone the sender and build the event BEFORE releasing the lock so the
	// held-set update is atomic with the send decision.  Then drop the lock and
	// call fwd() outside it.
	//
	// This matters on the WH_KEYBOARD_LL path: ll_keyboard_proc runs on the
	// hook callback thread, where ANY blocking is forbidden — it stalls the OS
	// input queue and trips LowLevelHooksTimeout (~300 ms).  fwd() calls
	// blocking_send for non-coalescible edges, so holding the globals() Mutex
	// while blocking also prevents disable()/flush_held()/set_focused() from
	// acquiring the lock (they need it for teardown/recovery).  Drop the guard
	// first so the lock is always free before any potentially-blocking send.
	if let Some(tx) = g.tx.clone() {
		// Track held-on-host keys for the disengage flush (under the lock).
		if down {
			g.held.insert(evdev);
		} else {
			g.held.remove(&evdev);
		}
		// Release the globals lock BEFORE any send — on the WH path this must
		// not block while the OS input queue is held (LL hook callback thread).
		drop(g);
		// Key repeat re-fires down=true; forward each (the host de-dupes).
		fwd(&tx, InputEvent::Key { code: evdev, down });
		return true;
	}
	false
}

/// Forward a captured mouse stroke (native-renderer mode) to the host as relative
/// motion + buttons + wheel, and suppress it locally (cursor lock — the mouse only
/// drives the remote). Returns true (suppress) whenever a session is active.
fn handle_mouse(state: u16, flags: u16, rolling: i16, x: i32, y: i32) -> bool {
	// Same click-to-engage gate as handle_key: not engaged → local mouse untouched.
	if !ENGAGED.load(Ordering::SeqCst) {
		return false;
	}
	let mut g = globals().lock().unwrap();
	if g.tx.is_none() {
		return false;
	}
	// A physical click while the LOCAL cursor sits OUTSIDE every Pulsar window must not
	// go to the host — the user is clicking another application. (This happens when the
	// device's MOVES bypass the driver capture — e.g. precision-touchpad pointer
	// injection — so the cursor roams freely while buttons are still intercepted.)
	// Implicit release, Parsec-style: disengage and pass the click through, so the
	// FIRST click switches apps instead of silently clicking the remote.
	if state & (0x001 | 0x004 | 0x010) != 0 {
		use windows_sys::Win32::System::Threading::GetCurrentProcessId;
		use windows_sys::Win32::UI::WindowsAndMessaging::{
			GetAncestor, GetCursorPos, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
		};
		let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
		if unsafe { GetCursorPos(&mut pt) } != 0 {
			let hw = unsafe { WindowFromPoint(pt) };
			// Compare the TOP-LEVEL ancestor's process: the window under the cursor over
			// the video is the pulsar-render CHILD (a different process), but its root is
			// our Tauri window — only a root owned by another app counts as "outside".
			let root = if hw.is_null() {
				hw
			} else {
				unsafe { GetAncestor(hw, GA_ROOT) }
			};
			let mut pid = 0u32;
			unsafe { GetWindowThreadProcessId(root, &mut pid) };
			if !root.is_null() && pid != unsafe { GetCurrentProcessId() } {
				tracing::info!("click outside Pulsar while engaged → implicit release, click passes through");
				ENGAGED.store(false, Ordering::SeqCst);
				clear_confine();
				flush_held(&mut g);
				if let Some(app) = &g.app {
					let _ = app.emit("kbd-released", ());
				}
				return false; // the click acts locally (activates the other app)
			}
		}
	}
	let Some(tx) = &g.tx else { return false };
	// Keep the cursor trapped to the streamed screen — the OS drops ClipCursor on focus
	// flaps (e.g. the render-child activation), so re-assert it on each mouse event.
	reassert_confine();
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
			if down {
				// Diagnostic: every locally-SUPPRESSED physical click while engaged.
				tracing::info!(button, "physical click suppressed (engaged) → forwarded to host");
			}
			fwd(tx, InputEvent::PointerButton { button, down });
		}
	}
	// Wheel: state carries the WHEEL bit (0x400), rolling is the signed delta.
	if state & 0x400 != 0 && rolling != 0 {
		let _ = tx.try_send(InputEvent::Scroll {
			dx: 0.0,
			dy: -(rolling as f64),
		});
	}
	// Track held buttons for the disengage flush (mirror `g.held` for keys). Done after
	// the `tx` borrow above so the disjoint `g.held_buttons` field can be mutated. A
	// press-and-hold (drag) leaves the button in the set until its up-stroke arrives;
	// if a disengage edge fires first, flush_held() releases it on the host.
	for (bit, button, down) in [
		(0x001u16, 0u8, true),
		(0x002, 0, false),
		(0x004, 1, true),
		(0x008, 1, false),
		(0x010, 2, true),
		(0x020, 2, false),
	] {
		if state & bit != 0 {
			if down {
				g.held_buttons.insert(button);
			} else {
				g.held_buttons.remove(&button);
			}
		}
	}
	true
}

/// Arm capture for `tx`'s session. The first call picks a mechanism: the
/// Interception driver if present (works under ASTER), else WH_KEYBOARD_LL. Later
/// calls just swap in the active sender + app handle and flip ENABLED.
pub fn enable(app: AppHandle, tx: Sender<InputEvent>, mouse: bool, id: u64, _start_suspended: bool) {
	// A re-arm of the SAME live play session must preserve the user's engagement —
	// resetting it mid-control silently stopped forwarding with no UI cue (Linux
	// parity; see kbdhook/linux.rs enable). The real trigger is a disable()→enable()
	// pair (tab switch), so ENABLED is already false here — key same_session off
	// LAST_PLAY (which survives disable()), NOT off ENABLED.
	// (`start_suspended` is the Linux overlay-grab gate; Windows has no evdev grab —
	// the overlay floats over the live canvas — so it's unused here, kept for a shared
	// signature.)
	let same_session = LAST_PLAY.load(Ordering::SeqCst) == id;
	LAST_PLAY.store(id, Ordering::SeqCst);
	{
		let mut g = globals().lock().unwrap();
		g.tx = Some(tx);
		g.app = Some(app.clone());
		g.ctrl_down = false;
		g.alt_down = false;
		g.shift_down = false;
		g.rctrl_presses.clear();
	}
	// Sessions start DISENGAGED (click-to-engage) — except a kiosk `--connect` start,
	// which engages immediately like the Linux evdev path. KIOSK_ENGAGE is LATCHED
	// (not consumed): the frontend re-arms capture on tab-activation churn, and a
	// one-shot left that second enable() disengaged (see kbdhook/linux.rs).
	let kiosk = KIOSK_ENGAGE.load(Ordering::SeqCst);
	KIOSK_SESSION.store(kiosk, Ordering::SeqCst);
	if kiosk {
		engage(&app);
	} else if same_session {
		// Restore the engagement this id had when last active (disable() cleared the live
		// ENGAGED but stashed it in ENGAGED_MEMO). engage() emits kbd-engaged on the edge so
		// the renderer cursor/hint resyncs; a remembered-disengaged session stays off.
		if ENGAGED_MEMO.load(Ordering::SeqCst) {
			engage(&app);
		} else {
			ENGAGED.store(false, Ordering::SeqCst);
		}
	} else {
		ENGAGED.store(false, Ordering::SeqCst);
	}
	// Native-renderer sessions also capture the mouse (relative); webview sessions
	// read the pointer from the canvas instead, so keep mouse capture off there.
	MOUSE_CAPTURE.store(mouse, Ordering::SeqCst);
	ENABLED.store(true, Ordering::SeqCst);
	match MECHANISM.load(Ordering::SeqCst) {
		0 => {
			// First arm — decide the capture mechanism once.
			if let Some(it) = init_interception() {
				tracing::info!("kbd capture mechanism: Interception driver");
				let _ = INTERCEPT.set(it);
				MECHANISM.store(1, Ordering::SeqCst);
				std::thread::spawn(capture_thread);
			} else {
				// Visible in the log on machines where this matters (ASTER multiseat
				// physical input is INVISIBLE to the LL hook — capture will look
				// engaged but forward nothing).
				tracing::warn!("kbd capture mechanism: WH_KEYBOARD_LL fallback (interception.dll not loaded)");
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
	// Remember the engagement before dropping the live flag so a same-id re-arm (tab
	// switch = disable()→enable()) can restore it. LAST_PLAY is deliberately kept too;
	// a different id still starts disengaged.
	ENGAGED_MEMO.store(ENGAGED.load(Ordering::SeqCst), Ordering::SeqCst);
	ENGAGED.store(false, Ordering::SeqCst);
	clear_confine(); // free the cursor when the session is torn down / tab switched away
	KIOSK_SESSION.store(false, Ordering::SeqCst);
	// Interception: drop the filter so we stop capturing (frees keys for any other
	// instance + the local OS).
	set_capture_filter(false);
	let tid = {
		let mut g = globals().lock().unwrap();
		flush_held(&mut g); // un-stick anything still held on the host
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

/// Window focus changed. Losing ALL app focus while engaged must DISENGAGE the
/// capture (Linux parity): a global LL/Interception hook otherwise keeps eating the
/// keyboard while the user is in another app — the mouse looked free (they clicked
/// away) but every keystroke still went to the remote.
pub fn set_focused(focused: bool) {
	tracing::info!(focused, "app focus changed");
	// A click over the live video lands on the pulsar-render CHILD (a separate
	// process), which momentarily deactivates the Tauri webview window — Windows
	// reports focused=false then true within milliseconds. Disengaging on that flap
	// released the capture mid-click (host input devices re-armed → the click fell
	// into the recreate gap: "cursor moves but clicks do nothing"). Focus moving to
	// a window whose TOP-LEVEL root is still ours is NOT the user leaving the app.
	if !focused {
		use windows_sys::Win32::System::Threading::GetCurrentProcessId;
		use windows_sys::Win32::UI::WindowsAndMessaging::{
			GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, GA_ROOT,
		};
		let fg = unsafe { GetForegroundWindow() };
		if !fg.is_null() {
			let root = unsafe { GetAncestor(fg, GA_ROOT) };
			let mut pid = 0u32;
			unsafe { GetWindowThreadProcessId(root, &mut pid) };
			if !root.is_null() && pid == unsafe { GetCurrentProcessId() } {
				tracing::debug!("focus moved within our own window tree — keeping engagement");
				return;
			}
		}
	}
	APP_FOCUSED.store(focused, Ordering::SeqCst);
	// Kiosk sessions ignore focus loss entirely (see KIOSK_SESSION).
	if KIOSK_SESSION.load(Ordering::SeqCst) {
		return;
	}
	if !focused && ENGAGED.load(Ordering::SeqCst) {
		let app = globals().lock().unwrap().app.clone();
		match app {
			Some(app) => release_engage(&app),
			None => ENGAGED.store(false, Ordering::SeqCst),
		}
	}
}

/// Owns the hook for its lifetime: install → pump messages → uninstall.
fn hook_thread() {
	unsafe {
		let hmod = GetModuleHandleW(std::ptr::null());
		let hook: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0);
		if hook.is_null() {
			THREAD_STARTED.store(false, Ordering::SeqCst);
			return;
		}
		let my_tid = GetCurrentThreadId();
		globals().lock().unwrap().hook_thread_id = my_tid;
		// Required: LL hooks deliver via this thread's message queue.
		let mut msg: MSG = std::mem::zeroed();
		// GetMessageW returns 0 on WM_QUIT, -1 on error.
		while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
			TranslateMessage(&msg);
			DispatchMessageW(&msg);
		}
		UnhookWindowsHookEx(hook);
		// Compare-and-clear: a fast disable()→enable() churn can spawn a NEW hook
		// thread before this (old) one drains WM_QUIT. The new thread has already
		// stored its own id; blindly zeroing here would clobber the LIVE thread's id,
		// so a later disable() would PostThreadMessageW(0,…) (a no-op) and leak the
		// live thread with the keyboard still hooked. Only clear if it's still ours.
		let mut g = globals().lock().unwrap();
		if g.hook_thread_id == my_tid {
			g.hook_thread_id = 0;
		}
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
