//! Pure scancode/virtual-key → evdev keycode mapping tables for the Windows
//! capture paths (Interception set-1 scancodes + WH_KEYBOARD_LL virtual keys).

/// Interception delivers AT set-1 scancodes; map them to evdev keycodes (the
/// inverse of the host's table). The base block is identity (evdev keycodes were
/// derived from these scancodes); E0-extended keys need the explicit table.
pub(super) fn scancode_to_evdev(code: u16, e0: bool) -> Option<u32> {
	// Win/Menu are always E0-extended and never collide with base scancodes, so
	// map them regardless of whether the E0 flag was reported (be robust to paths
	// that drop it — otherwise the Win key leaks to the local Start menu).
	match code {
		0x5B => return Some(125), // Left Win
		0x5C => return Some(126), // Right Win
		0x5D => return Some(127), // Context Menu
		_ => {}
	}
	if !e0 {
		return (1..=88).contains(&code).then_some(code as u32);
	}
	Some(match code {
		0x1C => 96,  // Numpad Enter
		0x1D => 97,  // Right Ctrl
		0x35 => 98,  // Numpad /
		0x38 => 100, // Right Alt
		0x47 => 102, // Home
		0x48 => 103, // Arrow Up
		0x49 => 104, // Page Up
		0x4B => 105, // Arrow Left
		0x4D => 106, // Arrow Right
		0x4F => 107, // End
		0x50 => 108, // Arrow Down
		0x51 => 109, // Page Down
		0x52 => 110, // Insert
		0x53 => 111, // Delete
		0x5B => 125, // Left Win
		0x5C => 126, // Right Win
		0x5D => 127, // Context Menu
		_ => return None,
	})
}

/// Windows virtual-key → Linux evdev keycode — the inverse of
/// `pulsar_core::input::evdev_to_vk`. Kept in lock-step with that table.
pub(super) fn vk_to_evdev(vk: u16) -> Option<u32> {
	Some(match vk {
		0x1B => 1,                             // Escape
		0x31..=0x39 => 2 + (vk - 0x31) as u32, // '1'..'9' → Digit1..9
		0x30 => 11,                            // '0'
		0xBD => 12,                            // VK_OEM_MINUS
		0xBB => 13,                            // VK_OEM_PLUS
		0x08 => 14,                            // Backspace
		0x09 => 15,                            // Tab
		0x51 => 16,
		0x57 => 17,
		0x45 => 18,
		0x52 => 19,
		0x54 => 20, // Q W E R T
		0x59 => 21,
		0x55 => 22,
		0x49 => 23,
		0x4F => 24,
		0x50 => 25, // Y U I O P
		0xDB => 26,
		0xDD => 27, // [ ]
		0x0D => 28, // Enter
		0xA2 => 29, // Left Ctrl
		0x41 => 30,
		0x53 => 31,
		0x44 => 32,
		0x46 => 33,
		0x47 => 34,
		0x48 => 35, // A S D F G H
		0x4A => 36,
		0x4B => 37,
		0x4C => 38, // J K L
		0xBA => 39,
		0xDE => 40,
		0xC0 => 41, // ; ' `
		0xA0 => 42, // Left Shift
		0xDC => 43, // backslash
		0x5A => 44,
		0x58 => 45,
		0x43 => 46,
		0x56 => 47,
		0x42 => 48,
		0x4E => 49,
		0x4D => 50, // Z X C V B N M
		0xBC => 51,
		0xBE => 52,
		0xBF => 53,                             // , . /
		0xA1 => 54,                             // Right Shift
		0x6A => 55,                             // Numpad *
		0xA4 => 56,                             // Left Alt (VK_LMENU)
		0x20 => 57,                             // Space
		0x14 => 58,                             // Caps Lock
		0x70..=0x79 => 59 + (vk - 0x70) as u32, // F1..F10
		0x90 => 69,                             // Num Lock
		0x91 => 70,                             // Scroll Lock
		0x67 => 71,
		0x68 => 72,
		0x69 => 73, // Numpad 7 8 9
		0x6D => 74, // Numpad -
		0x64 => 75,
		0x65 => 76,
		0x66 => 77, // Numpad 4 5 6
		0x6B => 78, // Numpad +
		0x61 => 79,
		0x62 => 80,
		0x63 => 81,  // Numpad 1 2 3
		0x60 => 82,  // Numpad 0
		0x6E => 83,  // Numpad .
		0x7A => 87,  // F11
		0x7B => 88,  // F12
		0x6F => 98,  // Numpad / (VK_DIVIDE)
		0xA3 => 97,  // Right Ctrl
		0xA5 => 100, // Right Alt (VK_RMENU)
		0x24 => 102, // Home
		0x26 => 103, // Arrow Up
		0x21 => 104, // Page Up
		0x25 => 105, // Arrow Left
		0x27 => 106, // Arrow Right
		0x23 => 107, // End
		0x28 => 108, // Arrow Down
		0x22 => 109, // Page Down
		0x2D => 110, // Insert
		0x2E => 111, // Delete
		0x5B => 125, // Left Win
		0x5C => 126, // Right Win
		0x5D => 127, // Context Menu (VK_APPS)
		// Bare VK_SHIFT/CONTROL/MENU: fold to the left variant so they still
		// reach the host (the host treats L/R identically for these).
		0x10 => 42,
		0x11 => 29,
		0x12 => 56,
		_ => return None,
	})
}

#[cfg(test)]
mod tests {
	use super::{scancode_to_evdev, vk_to_evdev};
	#[test]
	fn win_keys_map_to_meta() {
		assert_eq!(vk_to_evdev(0x5B), Some(125)); // LWin → MetaLeft
		assert_eq!(vk_to_evdev(0x5C), Some(126)); // RWin → MetaRight
		assert_eq!(vk_to_evdev(0x41), Some(30)); // A
		assert_eq!(vk_to_evdev(0x1B), Some(1)); // Esc
	}

	#[test]
	fn interception_scancodes_map_to_evdev() {
		// Base block is identity (evdev derived from set-1 scancodes).
		assert_eq!(scancode_to_evdev(0x1E, false), Some(30)); // A
		assert_eq!(scancode_to_evdev(0x01, false), Some(1)); // Esc
		assert_eq!(scancode_to_evdev(0x1C, false), Some(28)); // Enter
		assert_eq!(scancode_to_evdev(0x1D, false), Some(29)); // Left Ctrl
		assert_eq!(scancode_to_evdev(0x2A, false), Some(42)); // Left Shift
														// E0-extended keys need the explicit table.
		assert_eq!(scancode_to_evdev(0x5B, true), Some(125)); // Left Win
		assert_eq!(scancode_to_evdev(0x1D, true), Some(97)); // Right Ctrl
		assert_eq!(scancode_to_evdev(0x38, true), Some(100)); // Right Alt
		assert_eq!(scancode_to_evdev(0x48, true), Some(103)); // Arrow Up
														// Out-of-range / unmapped → None (passed through locally).
		assert_eq!(scancode_to_evdev(0xF0, false), None);
		assert_eq!(scancode_to_evdev(0x2A, true), None); // PrtScn fake-shift
	}
}
