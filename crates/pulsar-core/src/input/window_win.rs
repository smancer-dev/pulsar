//! Windows **per-window** mouse + keyboard injection via `PostMessage` (Phase 3B
//! same-host co-op). When a session captures a *specific window* (a launched game
//! or a picked app window) rather than the whole display, its input is delivered to
//! THAT window's message queue instead of the OS-global `SendInput` cursor/focus —
//! so two co-op panes sharing one host can each drive their own app window without
//! fighting over the single system mouse/keyboard focus.
//!
//! ## HONEST CEILING (read before relying on this for games)
//!
//! `PostMessage`/`SendMessage` deliver synthetic `WM_*` window messages to the
//! target's message pump. This reaches apps that read input from the **Win32
//! message queue** — productivity/management apps, classic Win32 controls, and
//! some windowed 2D games. It does **NOT** reach:
//!   * **DirectInput / Raw Input (`WM_INPUT`)** games — they read the HID device
//!     directly, below the windowed message queue;
//!   * **`GetAsyncKeyState`/`GetKeyboardState`** polling games — those read the
//!     *global* async key state, which only `SendInput` (or a hardware/driver
//!     injector) updates, never a posted `WM_KEYDOWN`;
//!   * anything that requires the window to hold real keyboard **focus** for its
//!     input (posted messages don't set focus, by design — that's the point).
//!
//! True per-app input isolation for arbitrary fullscreen games needs OS-level
//! multiseat (ASTER-class) or a driver that scopes a HID device to one process
//! (HidHide-style) — explicitly **out of scope** here (per the project direction:
//! do app-targeted routing ourselves where we can, no ASTER dependency). For the
//! cases this DOES cover (the management/co-op message-pump case) it works with no
//! extra drivers; for everything else the caller should fall back to the global
//! [`super::DesktopInput`] (`SendInput`) path.

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::WindowsAndMessaging::{
	GetClientRect, IsWindow, PostMessageW, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
	WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
	WM_RBUTTONUP,
};

use super::windows::evdev_to_vk;

const WHEEL_DELTA: i32 = 120;

// `WM_MOUSEMOVE`/button-message wParam "which buttons are down" mask bits. These live
// in `Win32::System::SystemServices` in windows-sys; defined locally (they're stable
// Win32 ABI values) to avoid pulling in that whole feature module for three constants.
const MK_LBUTTON: u32 = 0x0001;
const MK_RBUTTON: u32 = 0x0002;
const MK_MBUTTON: u32 = 0x0010;

/// Pack a low/high 16-bit pair into an `LPARAM` (Win32 `MAKELPARAM`).
#[inline]
fn make_lparam(low: i32, high: i32) -> LPARAM {
	(((high as u32) << 16) | ((low as u32) & 0xFFFF)) as LPARAM
}

/// Pack a high-word value into a `WPARAM` keeping a low-word state mask
/// (Win32 `MAKEWPARAM(stateMask, highWord)` — used for `WM_MOUSEWHEEL`).
#[inline]
fn make_wparam(low: u16, high: i16) -> WPARAM {
	(((high as u16 as u32) << 16) | (low as u32)) as WPARAM
}

/// Injects mouse + keyboard into a SPECIFIC top-level window via posted `WM_*`
/// messages (see the module-level ceiling note). Mirrors the [`super::DesktopInput`]
/// surface (`pointer`/`pointer_relative`/`button`/`scroll`/`key`/`type_char`/
/// `flush_held`) so the host's `on_input` closure can swap one for the other based
/// on whether the session has a window target — no caller branching per event.
///
/// Coordinates: the client sends a normalized (0..1) absolute pointer; we map it
/// onto the target's **client rect** (`GetClientRect`) so a click at the top-left of
/// the streamed window lands at the window's client top-left, regardless of where
/// the window sits on the host desktop. A cached last position lets relative deltas
/// and button events reuse the most recent pointer location (posted button messages
/// carry the click point in their lParam).
pub struct WindowInput {
	hwnd: HWND,
	/// Last client-area pointer position (pixels), reused by button/scroll messages
	/// and advanced by relative-delta moves. Clamped to the client rect each update.
	last_x: i32,
	last_y: i32,
	/// Held mouse-button mask (MK_* bits) so move/scroll messages report the
	/// in-progress drag state, and so a mid-press teardown can release them.
	btn_mask: u32,
	/// Held VK set — released on Drop / flush so a client that drops mid-press
	/// doesn't leave a key latched down in the target window.
	held_keys: std::collections::HashSet<u16>,
	/// Carried-over fractional wheel notches (matches DesktopInput's behavior so
	/// fine/precision scroll isn't lost to rounding).
	scroll_acc_v: f64,
	scroll_acc_h: f64,
}

// HWND is a raw pointer (`*mut c_void`) → not auto-Send. The on_input closure runs
// on a single task and only ever touches the handle from that task; we never share
// it across threads. Asserting Send lets it live in the closure's captured state
// alongside the (already !Send-free) DesktopInput.
unsafe impl Send for WindowInput {}

impl WindowInput {
	/// Wrap a target window handle (an `i64` as stashed by the launch resolver /
	/// window picker). Returns `None` if the handle isn't a live window right now —
	/// the caller then keeps the global `SendInput` path for this session.
	pub fn new(hwnd: i64) -> Option<Self> {
		let h = hwnd as HWND;
		if h.is_null() {
			return None;
		}
		// Validate at construction; if the window died between resolve and connect
		// the caller falls back to global injection.
		if unsafe { IsWindow(h) } == 0 {
			return None;
		}
		Some(Self {
			hwnd: h,
			last_x: 0,
			last_y: 0,
			btn_mask: 0,
			held_keys: std::collections::HashSet::new(),
			scroll_acc_v: 0.0,
			scroll_acc_h: 0.0,
		})
	}

	/// Is the target window still alive? The caller can poll this to decide whether
	/// to keep routing here or fall back to the global path (e.g. the game closed).
	pub fn is_alive(&self) -> bool {
		unsafe { IsWindow(self.hwnd) != 0 }
	}

	/// Client-area size of the target window in pixels (0,0 if it can't be read).
	fn client_size(&self) -> (i32, i32) {
		let mut r = RECT {
			left: 0,
			top: 0,
			right: 0,
			bottom: 0,
		};
		if unsafe { GetClientRect(self.hwnd, &mut r) } != 0 {
			((r.right - r.left).max(0), (r.bottom - r.top).max(0))
		} else {
			(0, 0)
		}
	}

	#[inline]
	fn clamp_to_client(&self, x: i32, y: i32) -> (i32, i32) {
		let (w, h) = self.client_size();
		let cx = if w > 0 { x.clamp(0, w - 1) } else { 0 };
		let cy = if h > 0 { y.clamp(0, h - 1) } else { 0 };
		(cx, cy)
	}

	#[inline]
	fn post(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) {
		unsafe {
			PostMessageW(self.hwnd, msg, wparam, lparam);
		}
	}

	/// Move the pointer to a normalized (0..1) position on the target's client area
	/// and post a `WM_MOUSEMOVE` carrying the click point + held-button mask.
	pub fn pointer(&mut self, x: f64, y: f64) {
		let (w, h) = self.client_size();
		let px = (x.clamp(0.0, 1.0) * (w.max(1) - 1).max(0) as f64).round() as i32;
		let py = (y.clamp(0.0, 1.0) * (h.max(1) - 1).max(0) as f64).round() as i32;
		let (cx, cy) = self.clamp_to_client(px, py);
		self.last_x = cx;
		self.last_y = cy;
		self.post(
			WM_MOUSEMOVE,
			self.btn_mask as WPARAM,
			make_lparam(cx, cy),
		);
	}

	/// Advance the cached pointer by a raw client-pixel delta and post the move.
	/// (Posted messages have no notion of a relative cursor, so we integrate the
	/// delta ourselves and report the resulting absolute client point.)
	pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
		let (cx, cy) =
			self.clamp_to_client(self.last_x + dx.round() as i32, self.last_y + dy.round() as i32);
		self.last_x = cx;
		self.last_y = cy;
		self.post(
			WM_MOUSEMOVE,
			self.btn_mask as WPARAM,
			make_lparam(cx, cy),
		);
	}

	/// Press/release a mouse button (0=left, 1=right, 2=middle) at the cached point.
	pub fn button(&mut self, button: u8, down: bool) {
		let bit = match button {
			1 => MK_RBUTTON,
			2 => MK_MBUTTON,
			_ => MK_LBUTTON,
		};
		if down {
			self.btn_mask |= bit;
		} else {
			self.btn_mask &= !bit;
		}
		let msg = match (button, down) {
			(1, true) => WM_RBUTTONDOWN,
			(1, false) => WM_RBUTTONUP,
			(2, true) => WM_MBUTTONDOWN,
			(2, false) => WM_MBUTTONUP,
			(_, true) => WM_LBUTTONDOWN,
			(_, false) => WM_LBUTTONUP,
		};
		self.post(
			msg,
			self.btn_mask as WPARAM,
			make_lparam(self.last_x, self.last_y),
		);
	}

	/// Scroll by a delta. `WM_MOUSEWHEEL` carries the wheel delta in the wParam high
	/// word and — unusually — the cursor position in **screen** coordinates in lParam,
	/// so we map the cached client point through `ClientToScreen`.
	pub fn scroll(&mut self, dx: f64, dy: f64) {
		// Same fractional-notch accumulation as DesktopInput. browser down(+) → wheel
		// down (negative), so negate dy.
		self.scroll_acc_v += -dy / 100.0;
		self.scroll_acc_h += dx / 100.0;
		let v = self.scroll_acc_v.trunc() as i32;
		let h = self.scroll_acc_h.trunc() as i32;
		self.scroll_acc_v -= v as f64;
		self.scroll_acc_h -= h as f64;
		let mut pt = POINT {
			x: self.last_x,
			y: self.last_y,
		};
		unsafe {
			ClientToScreen(self.hwnd, &mut pt);
		}
		let state_low = self.btn_mask as u16;
		if v != 0 {
			self.post(
				WM_MOUSEWHEEL,
				make_wparam(state_low, (v * WHEEL_DELTA) as i16),
				make_lparam(pt.x, pt.y),
			);
		}
		if h != 0 {
			// WM_MOUSEHWHEEL constant lives in WindowsAndMessaging too; use its literal
			// value (0x020E) to avoid importing yet another symbol for the rare h-scroll.
			const WM_MOUSEHWHEEL: u32 = 0x020E;
			self.post(
				WM_MOUSEHWHEEL,
				make_wparam(state_low, (h * WHEEL_DELTA) as i16),
				make_lparam(pt.x, pt.y),
			);
		}
	}

	/// Press/release a key by evdev keycode (reuses the global path's evdev→VK table).
	/// lParam packs a minimal key-message bit field: repeat-count 1, the hardware
	/// scancode is left 0 (synthetic), bit30 = previous key-down state, bit31 =
	/// transition (1 on key-up). Most message-pump apps only read wParam (the VK).
	pub fn key(&mut self, code: u32, down: bool) {
		if let Some(vk) = evdev_to_vk(code) {
			if down {
				self.held_keys.insert(vk);
			} else {
				self.held_keys.remove(&vk);
			}
			let (msg, lparam) = if down {
				(WM_KEYDOWN, 0x0000_0001_i64 as LPARAM)
			} else {
				// bit30 (prev down) + bit31 (key-up transition) + repeat 1.
				(WM_KEYUP, 0xC000_0001_u32 as i32 as LPARAM)
			};
			self.post(msg, vk as WPARAM, lparam);
		}
	}

	/// Type a resolved Unicode character verbatim via `WM_CHAR` (layout-independent —
	/// the client already mapped the keypress through its own layout to this codepoint).
	/// One message per UTF-16 code unit, mirroring `DesktopInput::type_char`.
	pub fn type_char(&mut self, c: char) {
		let mut buf = [0u16; 2];
		for unit in c.encode_utf16(&mut buf) {
			self.post(WM_CHAR, *unit as WPARAM, make_lparam(1, 0));
		}
	}

	/// Release every held key + mouse button on the target window. Called when control
	/// is revoked mid-press ("Sadece izleme") or on teardown, so a held modifier / drag
	/// doesn't latch in the target window after later up-events are dropped. Idempotent.
	pub fn flush_held(&mut self) {
		// Release buttons (left/right/middle) that are still in the held mask.
		for (bit, up_msg) in [
			(MK_LBUTTON, WM_LBUTTONUP),
			(MK_RBUTTON, WM_RBUTTONUP),
			(MK_MBUTTON, WM_MBUTTONUP),
		] {
			if self.btn_mask & bit != 0 {
				self.btn_mask &= !bit;
				self.post(
					up_msg,
					self.btn_mask as WPARAM,
					make_lparam(self.last_x, self.last_y),
				);
			}
		}
		let keys: Vec<u16> = self.held_keys.drain().collect();
		for vk in keys {
			self.post(WM_KEYUP, vk as WPARAM, 0xC000_0001_u32 as i32 as LPARAM);
		}
	}
}

impl Drop for WindowInput {
	fn drop(&mut self) {
		// Best-effort: only post releases if the window is still alive (avoids
		// posting to a freed handle if the target window already closed).
		if self.is_alive() {
			self.flush_held();
		}
	}
}
