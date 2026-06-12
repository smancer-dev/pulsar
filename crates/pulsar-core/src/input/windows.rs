//! Windows host-side mouse + keyboard injection via the Win32 `SendInput` API
//! (the same user-mode approach Parsec uses for desktop control).

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
	SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
	KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
	MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
	MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};

const WHEEL_DELTA: i32 = 120;

fn send_mouse(dx: i32, dy: i32, data: i32, flags: u32) {
	let input = INPUT {
		r#type: INPUT_MOUSE,
		Anonymous: INPUT_0 {
			mi: MOUSEINPUT {
				dx,
				dy,
				mouseData: data as u32, // wheel delta is signed; SendInput reads the bits back as i32
				dwFlags: flags,
				time: 0,
				dwExtraInfo: 0,
			},
		},
	};
	unsafe { SendInput(1, &input, core::mem::size_of::<INPUT>() as i32) };
}

fn send_key(vk: u16, flags: u32) {
	let input = INPUT {
		r#type: INPUT_KEYBOARD,
		Anonymous: INPUT_0 {
			ki: KEYBDINPUT {
				wVk: vk,
				wScan: 0,
				dwFlags: flags,
				time: 0,
				dwExtraInfo: 0,
			},
		},
	};
	unsafe { SendInput(1, &input, core::mem::size_of::<INPUT>() as i32) };
}

/// Map a Linux evdev keycode (what the client sends — see `keymap.ts`) to a
/// Windows virtual-key code. Pure + testable.
fn evdev_to_vk(code: u32) -> Option<u16> {
	Some(match code {
		1 => 0x1B,                          // Escape
		2..=10 => 0x31 + (code - 2) as u16, // Digit1..9 → '1'..'9'
		11 => 0x30,                         // Digit0 → '0'
		12 => 0xBD,                         // Minus  (VK_OEM_MINUS)
		13 => 0xBB,                         // Equal  (VK_OEM_PLUS)
		14 => 0x08,                         // Backspace
		15 => 0x09,                         // Tab
		16 => 0x51,
		17 => 0x57,
		18 => 0x45,
		19 => 0x52,
		20 => 0x54, // Q W E R T
		21 => 0x59,
		22 => 0x55,
		23 => 0x49,
		24 => 0x4F,
		25 => 0x50, // Y U I O P
		26 => 0xDB, // [  (VK_OEM_4)
		27 => 0xDD, // ]  (VK_OEM_6)
		28 => 0x0D, // Enter
		29 => 0xA2, // Left Ctrl
		30 => 0x41,
		31 => 0x53,
		32 => 0x44,
		33 => 0x46,
		34 => 0x47,
		35 => 0x48, // A S D F G H
		36 => 0x4A,
		37 => 0x4B,
		38 => 0x4C, // J K L
		39 => 0xBA, // ;  (VK_OEM_1)
		40 => 0xDE, // '  (VK_OEM_7)
		41 => 0xC0, // `  (VK_OEM_3)
		42 => 0xA0, // Left Shift
		43 => 0xDC, // \  (VK_OEM_5)
		44 => 0x5A,
		45 => 0x58,
		46 => 0x43,
		47 => 0x56,
		48 => 0x42,
		49 => 0x4E,
		50 => 0x4D,                           // Z X C V B N M
		51 => 0xBC,                           // ,  (VK_OEM_COMMA)
		52 => 0xBE,                           // .  (VK_OEM_PERIOD)
		53 => 0xBF,                           // /  (VK_OEM_2)
		54 => 0xA1,                           // Right Shift
		55 => 0x6A,                           // Numpad *  (VK_MULTIPLY)
		56 => 0xA4,                           // Left Alt  (VK_LMENU)
		57 => 0x20,                           // Space
		58 => 0x14,                           // Caps Lock
		59..=68 => 0x70 + (code - 59) as u16, // F1..F10
		69 => 0x90,                           // Num Lock
		70 => 0x91,                           // Scroll Lock
		71 => 0x67,
		72 => 0x68,
		73 => 0x69, // Numpad 7 8 9
		74 => 0x6D, // Numpad -  (VK_SUBTRACT)
		75 => 0x64,
		76 => 0x65,
		77 => 0x66, // Numpad 4 5 6
		78 => 0x6B, // Numpad +  (VK_ADD)
		79 => 0x61,
		80 => 0x62,
		81 => 0x63,  // Numpad 1 2 3
		82 => 0x60,  // Numpad 0
		83 => 0x6E,  // Numpad .  (VK_DECIMAL)
		87 => 0x7A,  // F11
		88 => 0x7B,  // F12
		96 => 0x0D,  // Numpad Enter → Return
		97 => 0xA3,  // Right Ctrl
		98 => 0x6F,  // Numpad /  (VK_DIVIDE)
		100 => 0xA5, // Right Alt (VK_RMENU)
		102 => 0x24, // Home
		103 => 0x26, // Arrow Up
		104 => 0x21, // Page Up   (VK_PRIOR)
		105 => 0x25, // Arrow Left
		106 => 0x27, // Arrow Right
		107 => 0x23, // End
		108 => 0x28, // Arrow Down
		109 => 0x22, // Page Down (VK_NEXT)
		110 => 0x2D, // Insert
		111 => 0x2E, // Delete
		125 => 0x5B, // Left Win
		126 => 0x5C, // Right Win
		127 => 0x5D, // Context Menu (VK_APPS)
		_ => return None,
	})
}

/// Injects mouse + keyboard onto the host desktop via Win32 `SendInput`.
/// SendInput targets the calling process's interactive desktop, which for the
/// user-launched Pulsar app is the seat the host is sharing. Held buttons/keys
/// are tracked so they can be released on Drop — otherwise a client that drops
/// mid-press leaves a button/modifier stuck down (e.g. a runaway drag-select).
pub struct DesktopInput {
	held_buttons: std::collections::HashSet<u8>,
	held_keys: std::collections::HashSet<u16>,
	/// Carried-over fractional scroll (in wheel notches). Fine/precision wheel deltas
	/// (< one notch) used to `round()` to 0 and do nothing; we accumulate the remainder
	/// so successive small scrolls eventually move (and big deltas behave as before).
	scroll_acc_v: f64,
	scroll_acc_h: f64,
}

impl DesktopInput {
	pub fn new() -> std::io::Result<Self> {
		Ok(Self {
			held_buttons: std::collections::HashSet::new(),
			held_keys: std::collections::HashSet::new(),
			scroll_acc_v: 0.0,
			scroll_acc_h: 0.0,
		})
	}

	/// Move the pointer to a normalized (0..1) position on the primary display
	/// (what ddagrab output_idx=0 captures).
	pub fn pointer(&mut self, x: f64, y: f64) {
		let dx = (x.clamp(0.0, 1.0) * 65535.0).round() as i32;
		let dy = (y.clamp(0.0, 1.0) * 65535.0).round() as i32;
		send_mouse(dx, dy, 0, MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE);
	}

	/// Move the pointer by a raw relative delta (native renderer / games). No
	/// ABSOLUTE flag → SendInput treats dx/dy as a relative move.
	pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
		send_mouse(dx.round() as i32, dy.round() as i32, 0, MOUSEEVENTF_MOVE);
	}

	/// Press/release a mouse button (0=left, 1=right, 2=middle).
	pub fn button(&mut self, button: u8, down: bool) {
		if down {
			self.held_buttons.insert(button);
		} else {
			self.held_buttons.remove(&button);
		}
		let flag = match (button, down) {
			(1, true) => MOUSEEVENTF_RIGHTDOWN,
			(1, false) => MOUSEEVENTF_RIGHTUP,
			(2, true) => MOUSEEVENTF_MIDDLEDOWN,
			(2, false) => MOUSEEVENTF_MIDDLEUP,
			(_, true) => MOUSEEVENTF_LEFTDOWN,
			(_, false) => MOUSEEVENTF_LEFTUP,
		};
		send_mouse(0, 0, 0, flag);
	}

	/// Scroll by a delta (browser wheel pixels → wheel notches).
	pub fn scroll(&mut self, dx: f64, dy: f64) {
		// Accumulate fractional notches so fine/precision scroll (< 100px) isn't lost to
		// rounding; carry the sub-notch remainder to the next call (trunc toward zero
		// preserves direction). browser down(+) → wheel down (negative), so negate dy.
		self.scroll_acc_v += -dy / 100.0;
		self.scroll_acc_h += dx / 100.0;
		let v = self.scroll_acc_v.trunc() as i32;
		let h = self.scroll_acc_h.trunc() as i32;
		self.scroll_acc_v -= v as f64;
		self.scroll_acc_h -= h as f64;
		if v != 0 {
			send_mouse(0, 0, v * WHEEL_DELTA, MOUSEEVENTF_WHEEL);
		}
		if h != 0 {
			send_mouse(0, 0, h * WHEEL_DELTA, MOUSEEVENTF_HWHEEL);
		}
	}

	/// Press/release a key by evdev keycode.
	pub fn key(&mut self, code: u32, down: bool) {
		if let Some(vk) = evdev_to_vk(code) {
			if down {
				self.held_keys.insert(vk);
			} else {
				self.held_keys.remove(&vk);
			}
			send_key(vk, if down { 0 } else { KEYEVENTF_KEYUP });
		}
	}

	/// Type a resolved Unicode character verbatim (layout-independent). The client mapped a
	/// keypress through ITS keyboard layout to this codepoint; `KEYEVENTF_UNICODE` injects the
	/// exact char so Windows inserts it regardless of the host's active layout (a Turkish-Q
	/// client typing `ç` lands `ç` on a US-layout host). One-shot down+up, no held state —
	/// the host's own key-repeat is bypassed (the client re-sends `Char` on evdev autorepeat).
	pub fn type_char(&mut self, c: char) {
		let mut buf = [0u16; 2];
		for unit in c.encode_utf16(&mut buf) {
			for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
				let input = INPUT {
					r#type: INPUT_KEYBOARD,
					Anonymous: INPUT_0 {
						ki: KEYBDINPUT {
							wVk: 0, // MUST be 0 for KEYEVENTF_UNICODE
							wScan: *unit,
							dwFlags: flags,
							time: 0,
							dwExtraInfo: 0,
						},
					},
				};
				unsafe { SendInput(1, &input, core::mem::size_of::<INPUT>() as i32) };
			}
		}
	}
}

impl Drop for DesktopInput {
	/// Release anything still held when the session tears down, so a mid-press
	/// disconnect can't leave the host with a stuck mouse button (→ a runaway
	/// black drag-select rectangle) or a stuck Ctrl/Alt/Shift.
	fn drop(&mut self) {
		for b in self.held_buttons.drain() {
			let flag = match b {
				1 => MOUSEEVENTF_RIGHTUP,
				2 => MOUSEEVENTF_MIDDLEUP,
				_ => MOUSEEVENTF_LEFTUP,
			};
			send_mouse(0, 0, 0, flag);
		}
		for vk in self.held_keys.drain() {
			send_key(vk, KEYEVENTF_KEYUP);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::evdev_to_vk;

	#[test]
	fn maps_common_evdev_keys_to_vk() {
		assert_eq!(evdev_to_vk(30), Some(0x41)); // A
		assert_eq!(evdev_to_vk(28), Some(0x0D)); // Enter
		assert_eq!(evdev_to_vk(57), Some(0x20)); // Space
		assert_eq!(evdev_to_vk(2), Some(0x31)); // Digit1
		assert_eq!(evdev_to_vk(11), Some(0x30)); // Digit0
		assert_eq!(evdev_to_vk(59), Some(0x70)); // F1
		assert_eq!(evdev_to_vk(68), Some(0x79)); // F10
		assert_eq!(evdev_to_vk(103), Some(0x26)); // Arrow Up
		assert_eq!(evdev_to_vk(29), Some(0xA2)); // Left Ctrl
		assert_eq!(evdev_to_vk(0), None);
		assert_eq!(evdev_to_vk(250), None);
	}
}
