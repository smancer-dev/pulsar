//! macOS host-side mouse + keyboard injection via **CoreGraphics CGEvent** — the
//! standard user-mode approach every macOS remote-desktop app uses (Parsec, VNC,
//! RustDesk). Events are posted to `CGEventTapLocation::HID`, i.e. as low in the
//! event stream as a user-mode process can, so they reach foreground apps the same
//! way physical input would.
//!
//! ## Accessibility permission (TCC) — REQUIRED
//!
//! macOS gates synthetic input behind the **Accessibility** privacy permission
//! (System Settings → Privacy & Security → Accessibility). The hosting app
//! (Pulsar / its bundle) must be granted it. **Without the grant, CGEvent posting
//! silently no-ops** — the calls succeed and return no error, but the OS drops the
//! events. So an ungranted host looks like dead input with no diagnostic; the fix
//! is always "enable Accessibility for Pulsar", not a code bug. (Likewise, the
//! process must run in a GUI session — a LaunchDaemon with no window server has no
//! event stream to post into.)

use core_graphics::display::CGDisplay;
use core_graphics::event::{
	CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::collections::HashSet;

/// Map a Linux evdev keycode (what the client sends — see `keymap.ts`) to a macOS
/// virtual key code (the `kVK_*` constants from `<HIToolbox/Events.h>`). Pure +
/// testable. Mirrors the coverage of the Windows `evdev_to_vk` table.
///
/// Note: macOS virtual key codes are positional (ANSI layout), not character
/// codes — `kVK_ANSI_A` (0x00) is the *physical* A key. Layout translation for
/// printable characters is handled by `type_char` (Unicode injection); this table
/// is for non-text keys (modifiers, arrows, F-keys, Enter, etc.) and the ASCII
/// fast path.
fn evdev_to_vk(code: u32) -> Option<u16> {
	Some(match code {
		1 => 0x35,  // Escape       (kVK_Escape)
		2 => 0x12,  // Digit1       (kVK_ANSI_1)
		3 => 0x13,  // Digit2
		4 => 0x14,  // Digit3
		5 => 0x15,  // Digit4
		6 => 0x17,  // Digit5
		7 => 0x16,  // Digit6
		8 => 0x1A,  // Digit7
		9 => 0x1C,  // Digit8
		10 => 0x19, // Digit9
		11 => 0x1D, // Digit0
		12 => 0x1B, // Minus        (kVK_ANSI_Minus)
		13 => 0x18, // Equal        (kVK_ANSI_Equal)
		14 => 0x33, // Backspace    (kVK_Delete)
		15 => 0x30, // Tab          (kVK_Tab)
		16 => 0x0C, // Q
		17 => 0x0D, // W
		18 => 0x0E, // E
		19 => 0x0F, // R
		20 => 0x11, // T
		21 => 0x10, // Y
		22 => 0x20, // U
		23 => 0x22, // I
		24 => 0x1F, // O
		25 => 0x23, // P
		26 => 0x21, // [            (kVK_ANSI_LeftBracket)
		27 => 0x1E, // ]            (kVK_ANSI_RightBracket)
		28 => 0x24, // Enter        (kVK_Return)
		29 => 0x3B, // Left Ctrl    (kVK_Control)
		30 => 0x00, // A
		31 => 0x01, // S
		32 => 0x02, // D
		33 => 0x03, // F
		34 => 0x05, // G
		35 => 0x04, // H
		36 => 0x26, // J
		37 => 0x28, // K
		38 => 0x25, // L
		39 => 0x29, // ;            (kVK_ANSI_Semicolon)
		40 => 0x27, // '            (kVK_ANSI_Quote)
		41 => 0x32, // `            (kVK_ANSI_Grave)
		42 => 0x38, // Left Shift   (kVK_Shift)
		43 => 0x2A, // \            (kVK_ANSI_Backslash)
		44 => 0x06, // Z
		45 => 0x07, // X
		46 => 0x08, // C
		47 => 0x09, // V
		48 => 0x0B, // B
		49 => 0x2D, // N
		50 => 0x2E, // M
		51 => 0x2B, // ,            (kVK_ANSI_Comma)
		52 => 0x2F, // .            (kVK_ANSI_Period)
		53 => 0x2C, // /            (kVK_ANSI_Slash)
		54 => 0x3C, // Right Shift  (kVK_RightShift)
		55 => 0x43, // Numpad *     (kVK_ANSI_KeypadMultiply)
		56 => 0x3A, // Left Alt     (kVK_Option)
		57 => 0x31, // Space        (kVK_Space)
		58 => 0x39, // Caps Lock    (kVK_CapsLock)
		59 => 0x7A, // F1
		60 => 0x78, // F2
		61 => 0x63, // F3
		62 => 0x76, // F4
		63 => 0x60, // F5
		64 => 0x61, // F6
		65 => 0x62, // F7
		66 => 0x64, // F8
		67 => 0x65, // F9
		68 => 0x6D, // F10
		69 => 0x47, // Num Lock → Keypad Clear (kVK_ANSI_KeypadClear; no NumLock on Mac)
		// 70 Scroll Lock — no macOS equivalent
		71 => 0x59, // Numpad 7
		72 => 0x5B, // Numpad 8
		73 => 0x5C, // Numpad 9
		74 => 0x4E, // Numpad -     (kVK_ANSI_KeypadMinus)
		75 => 0x56, // Numpad 4
		76 => 0x57, // Numpad 5
		77 => 0x58, // Numpad 6
		78 => 0x45, // Numpad +     (kVK_ANSI_KeypadPlus)
		79 => 0x53, // Numpad 1
		80 => 0x54, // Numpad 2
		81 => 0x55, // Numpad 3
		82 => 0x52, // Numpad 0
		83 => 0x41, // Numpad .     (kVK_ANSI_KeypadDecimal)
		87 => 0x67, // F11
		88 => 0x6F, // F12
		96 => 0x4C, // Numpad Enter (kVK_ANSI_KeypadEnter)
		97 => 0x3E, // Right Ctrl   (kVK_RightControl)
		98 => 0x4B, // Numpad /     (kVK_ANSI_KeypadDivide)
		100 => 0x3D, // Right Alt   (kVK_RightOption)
		102 => 0x73, // Home        (kVK_Home)
		103 => 0x7E, // Arrow Up    (kVK_UpArrow)
		104 => 0x74, // Page Up     (kVK_PageUp)
		105 => 0x7B, // Arrow Left  (kVK_LeftArrow)
		106 => 0x7C, // Arrow Right (kVK_RightArrow)
		107 => 0x77, // End         (kVK_End)
		108 => 0x7D, // Arrow Down  (kVK_DownArrow)
		109 => 0x79, // Page Down   (kVK_PageDown)
		110 => 0x72, // Insert → Help (kVK_Help; closest macOS slot)
		111 => 0x75, // Delete (forward) (kVK_ForwardDelete)
		125 => 0x37, // Left Win  → Command (kVK_Command)
		126 => 0x36, // Right Win → Right Command (kVK_RightCommand)
		// 127 Context Menu — no macOS equivalent
		_ => return None,
	})
}

/// Modifier evdev codes → the CGEventFlags bit they assert. Used to rebuild the
/// flag mask CGEvent attaches to each event from our tracked modifier state
/// (CoreGraphics does NOT infer held modifiers from prior key-down events the way
/// a hardware keyboard does, so synthetic events must carry the flags explicitly,
/// or e.g. Shift+A injects a lowercase 'a').
fn evdev_modifier_flag(code: u32) -> Option<CGEventFlags> {
	Some(match code {
		29 | 97 => CGEventFlags::CGEventFlagControl,   // L/R Ctrl
		42 | 54 => CGEventFlags::CGEventFlagShift,     // L/R Shift
		56 | 100 => CGEventFlags::CGEventFlagAlternate, // L/R Alt → Option
		125 | 126 => CGEventFlags::CGEventFlagCommand, // L/R Win → Command
		_ => return None,
	})
}

/// Injects mouse + keyboard onto the host's GUI session via CoreGraphics.
///
/// Held buttons are tracked so a move while a button is down becomes the matching
/// `*MouseDragged` event (CoreGraphics distinguishes Moved vs Dragged — without
/// this, drag-and-drop and drag-select never register). Held buttons/keys and the
/// modifier mask are released on `Drop` so a client that disconnects mid-press
/// can't leave the host with a stuck button/modifier.
pub struct DesktopInput {
	// NOTE: no stored CGEventSource — its NonNull pointer isn't Send, and the host
	// serve loop holds DesktopInput across awaits (tokio task ⇒ the type must be
	// Send). A fresh source per event is cheap and keeps this struct plain data.
	held_buttons: HashSet<u8>,
	held_keys: HashSet<u16>,
	/// Current modifier mask, kept in sync as modifier keys go down/up.
	flags: CGEventFlags,
}

/// Translate our button id (0=left, 1=right, 2=middle) to the CGMouseButton plus
/// the down/up CGEventType, and the Dragged type used when the button is held.
fn button_events(button: u8, down: bool) -> (CGMouseButton, CGEventType) {
	match (button, down) {
		(1, true) => (CGMouseButton::Right, CGEventType::RightMouseDown),
		(1, false) => (CGMouseButton::Right, CGEventType::RightMouseUp),
		(2, true) => (CGMouseButton::Center, CGEventType::OtherMouseDown),
		(2, false) => (CGMouseButton::Center, CGEventType::OtherMouseUp),
		(_, true) => (CGMouseButton::Left, CGEventType::LeftMouseDown),
		(_, false) => (CGMouseButton::Left, CGEventType::LeftMouseUp),
	}
}

impl DesktopInput {
	pub fn new() -> std::io::Result<Self> {
		// Validate once that a source CAN be created (catches "no window-server
		// session" early); the per-event sources below repeat this cheaply.
		Self::source().ok_or_else(|| {
			std::io::Error::new(
				std::io::ErrorKind::Other,
				"failed to create CGEventSource (no window-server session?)",
			)
		})?;
		Ok(Self {
			held_buttons: HashSet::new(),
			held_keys: HashSet::new(),
			flags: CGEventFlags::empty(),
		})
	}

	/// A fresh HIDSystemState event source. HIDSystemState: events appear to come
	/// from the HID layer (a real device), which is what foreground apps expect.
	/// Created per event because CGEventSource is not Send (see struct note).
	fn source() -> Option<CGEventSource> {
		CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
	}

	/// The current pointer location in global display (pixel) coordinates, read
	/// back from CoreGraphics. Used to anchor relative moves.
	fn current_location(&self) -> CGPoint {
		// A throwaway null event carries the live cursor location.
		match Self::source().and_then(|s| CGEvent::new(s).ok()) {
			Some(ev) => ev.location(),
			None => CGPoint::new(0.0, 0.0),
		}
	}

	/// If a button is held, the matching dragged event so the move counts as a drag.
	fn move_event_type(&self) -> CGEventType {
		if self.held_buttons.contains(&1) {
			CGEventType::RightMouseDragged
		} else if self.held_buttons.contains(&2) {
			CGEventType::OtherMouseDragged
		} else if self.held_buttons.contains(&0) {
			CGEventType::LeftMouseDragged
		} else {
			CGEventType::MouseMoved
		}
	}

	fn post_mouse(&self, ty: CGEventType, point: CGPoint) {
		// Center maps to "other"; left/right are ignored for Moved/Dragged anyway.
		let btn = if self.held_buttons.contains(&1) {
			CGMouseButton::Right
		} else if self.held_buttons.contains(&2) {
			CGMouseButton::Center
		} else {
			CGMouseButton::Left
		};
		if let Some(ev) = Self::source().and_then(|s| CGEvent::new_mouse_event(s, ty, point, btn).ok()) {
			ev.set_flags(self.flags);
			ev.post(CGEventTapLocation::HID);
		}
	}

	/// Move the pointer to a normalized (0..1) position on the main display.
	pub fn pointer(&mut self, x: f64, y: f64) {
		let display = CGDisplay::main();
		let w = display.pixels_wide() as f64;
		let h = display.pixels_high() as f64;
		let px = x.clamp(0.0, 1.0) * w;
		let py = y.clamp(0.0, 1.0) * h;
		let ty = self.move_event_type();
		self.post_mouse(ty, CGPoint::new(px, py));
	}

	/// Move the pointer by a raw relative delta (native renderer / games). Added
	/// to the live cursor location read back from CoreGraphics.
	pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
		let cur = self.current_location();
		let ty = self.move_event_type();
		self.post_mouse(ty, CGPoint::new(cur.x + dx, cur.y + dy));
	}

	/// Press/release a mouse button (0=left, 1=right, 2=middle).
	pub fn button(&mut self, button: u8, down: bool) {
		// Update held set FIRST so the event posts at the live cursor location.
		if down {
			self.held_buttons.insert(button);
		} else {
			self.held_buttons.remove(&button);
		}
		let (cg_btn, ty) = button_events(button, down);
		let point = self.current_location();
		if let Some(ev) =
			Self::source().and_then(|s| CGEvent::new_mouse_event(s, ty, point, cg_btn).ok())
		{
			ev.set_flags(self.flags);
			ev.post(CGEventTapLocation::HID);
		}
	}

	/// Scroll by a delta (browser wheel pixels → wheel lines).
	pub fn scroll(&mut self, dx: f64, dy: f64) {
		let v = (dy / 100.0).round() as i32; // browser down(+) → scroll down (CG: negative is down)
		let h = (dx / 100.0).round() as i32;
		if v == 0 && h == 0 {
			return;
		}
		// CGEvent scroll: positive Y scrolls content up (toward the top), so negate
		// the browser delta (browser positive = scroll down).
		if let Some(ev) = Self::source()
			.and_then(|s| CGEvent::new_scroll_event(s, ScrollEventUnit::LINE, 2, -v, -h, 0).ok())
		{
			ev.post(CGEventTapLocation::HID);
		}
	}

	/// Press/release a key by evdev keycode. Tracks held modifiers and stamps the
	/// resulting flag mask onto the event (CoreGraphics won't infer it).
	pub fn key(&mut self, code: u32, down: bool) {
		// Keep the modifier mask in sync even though the key event itself also
		// carries the flag — downstream chords (Cmd+C) need the flag asserted.
		if let Some(flag) = evdev_modifier_flag(code) {
			if down {
				self.flags.insert(flag);
			} else {
				self.flags.remove(flag);
			}
		}
		let Some(vk) = evdev_to_vk(code) else {
			return;
		};
		if down {
			self.held_keys.insert(vk);
		} else {
			self.held_keys.remove(&vk);
		}
		if let Some(ev) = Self::source().and_then(|s| CGEvent::new_keyboard_event(s, vk, down).ok())
		{
			ev.set_flags(self.flags);
			ev.post(CGEventTapLocation::HID);
		}
	}

	/// Type a resolved Unicode character verbatim (layout-independent). The client
	/// mapped a keypress through ITS keyboard layout to this codepoint;
	/// `CGEventKeyboardSetUnicodeString` injects the exact char so it lands
	/// regardless of the host's active layout (a Turkish-Q client typing `ç` lands
	/// `ç` on a US-layout host). One-shot down+up, no held state — the host's own
	/// key-repeat is bypassed (the client re-sends `Char` on evdev autorepeat).
	///
	/// We do NOT stamp `self.flags` here: the codepoint is already the resolved
	/// character, so asserting e.g. Shift would risk re-interpreting it.
	pub fn type_char(&mut self, c: char) {
		let mut buf = [0u16; 2];
		let units = c.encode_utf16(&mut buf);
		for &down in &[true, false] {
			// vk=0 is fine; the Unicode string overrides the keycode's character.
			if let Some(ev) =
				Self::source().and_then(|s| CGEvent::new_keyboard_event(s, 0, down).ok())
			{
				ev.set_string_from_utf16_unchecked(units);
				ev.post(CGEventTapLocation::HID);
			}
		}
	}
}

// Compile-time guarantee: the host serve loop holds DesktopInput across awaits
// (tokio task), so it MUST be Send — this is exactly what broke the first CI mac
// build when a CGEventSource (non-Send NonNull) was stored in the struct.
const _: fn() = || {
	fn assert_send<T: Send>() {}
	assert_send::<DesktopInput>();
};

impl Drop for DesktopInput {
	/// Release anything still held when the session tears down, so a mid-press
	/// disconnect can't leave the host with a stuck mouse button (→ a runaway
	/// drag-select) or a stuck Cmd/Ctrl/Shift/Option.
	fn drop(&mut self) {
		let point = self.current_location();
		for b in self.held_buttons.clone() {
			let (cg_btn, ty) = button_events(b, false);
			if let Some(ev) =
				Self::source().and_then(|s| CGEvent::new_mouse_event(s, ty, point, cg_btn).ok())
			{
				ev.post(CGEventTapLocation::HID);
			}
		}
		self.held_buttons.clear();
		// Drop modifiers from the mask before releasing keys so the key-up events
		// don't re-assert a flag we're tearing down.
		self.flags = CGEventFlags::empty();
		for vk in self.held_keys.clone() {
			if let Some(ev) =
				Self::source().and_then(|s| CGEvent::new_keyboard_event(s, vk, false).ok())
			{
				ev.post(CGEventTapLocation::HID);
			}
		}
		self.held_keys.clear();
	}
}

#[cfg(test)]
mod tests {
	use super::{evdev_modifier_flag, evdev_to_vk};
	use core_graphics::event::CGEventFlags;

	#[test]
	fn maps_common_evdev_keys_to_mac_vk() {
		assert_eq!(evdev_to_vk(30), Some(0x00)); // A → kVK_ANSI_A
		assert_eq!(evdev_to_vk(28), Some(0x24)); // Enter → kVK_Return
		assert_eq!(evdev_to_vk(57), Some(0x31)); // Space → kVK_Space
		assert_eq!(evdev_to_vk(2), Some(0x12)); // Digit1 → kVK_ANSI_1
		assert_eq!(evdev_to_vk(11), Some(0x1D)); // Digit0 → kVK_ANSI_0
		assert_eq!(evdev_to_vk(59), Some(0x7A)); // F1
		assert_eq!(evdev_to_vk(68), Some(0x6D)); // F10
		assert_eq!(evdev_to_vk(103), Some(0x7E)); // Arrow Up
		assert_eq!(evdev_to_vk(29), Some(0x3B)); // Left Ctrl
		assert_eq!(evdev_to_vk(125), Some(0x37)); // Left Win → Command
		assert_eq!(evdev_to_vk(0), None);
		assert_eq!(evdev_to_vk(250), None);
	}

	#[test]
	fn modifier_flags_map() {
		assert_eq!(
			evdev_modifier_flag(42),
			Some(CGEventFlags::CGEventFlagShift)
		);
		assert_eq!(
			evdev_modifier_flag(29),
			Some(CGEventFlags::CGEventFlagControl)
		);
		assert_eq!(
			evdev_modifier_flag(56),
			Some(CGEventFlags::CGEventFlagAlternate)
		);
		assert_eq!(
			evdev_modifier_flag(125),
			Some(CGEventFlags::CGEventFlagCommand)
		);
		assert_eq!(evdev_modifier_flag(30), None); // 'A' is not a modifier
	}
}
