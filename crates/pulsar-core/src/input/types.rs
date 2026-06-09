//! Core controller types shared across the client and host: the controller
//! [`GamepadKind`], the normalized [`GamepadState`], the [`VirtualGamepad`]
//! host trait, and the pure mapping helpers.

use serde::{Deserialize, Serialize};

/// The recognized controller families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GamepadKind {
	/// Sony DualShock 3.
	Ds3,
	/// Sony DualShock 4.
	Ds4,
	/// Sony DualSense (PS5).
	Ds5,
	/// Any Xbox 360 / One / Series pad (or XInput-compatible).
	Xbox,
	/// A generic/standard PC gamepad.
	Standard,
	/// Couldn't be identified.
	Unknown,
}

impl GamepadKind {
	/// Detect from a USB vendor + product id.
	pub fn from_vid_pid(vid: u16, pid: u16) -> Self {
		const SONY: u16 = 0x054C;
		const MICROSOFT: u16 = 0x045E;
		match (vid, pid) {
			(SONY, 0x0268) => Self::Ds3,
			(SONY, 0x05C4) | (SONY, 0x09CC) | (SONY, 0x0BA0) => Self::Ds4,
			(SONY, 0x0CE6) | (SONY, 0x0DF2) => Self::Ds5,
			(MICROSOFT, _) => Self::Xbox,
			(SONY, _) => Self::Standard,
			(0, 0) => Self::Unknown,
			_ => Self::Standard,
		}
	}

	/// Human label.
	pub fn label(&self) -> &'static str {
		match self {
			Self::Ds3 => "DualShock 3",
			Self::Ds4 => "DualShock 4",
			Self::Ds5 => "DualSense",
			Self::Xbox => "Xbox",
			Self::Standard => "Standart Gamepad",
			Self::Unknown => "Bilinmeyen",
		}
	}
}

/// A detected controller, for listing in the UI (in-app device list + the live
/// in-session panel). `connected` distinguishes a pad that's plugged in and usable
/// right now from one the backend still remembers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerInfo {
	/// Positional index in the backend's list (stable within a session).
	pub index: u32,
	/// Human-readable name reported by the OS/driver, e.g. "Wireless Controller".
	pub name: String,
	/// Detected family (DS4/DS5/Xbox/…), from the USB vendor/product id.
	pub kind: GamepadKind,
	/// True if the pad is currently connected (forwardable to the host).
	pub connected: bool,
}

/// Extract `(vendor, product)` from an SDL2-style 16-byte controller GUID
/// (the format `gilrs` exposes via `Gamepad::uuid()`).
pub fn vid_pid_from_sdl_guid(guid: [u8; 16]) -> (u16, u16) {
	let vendor = u16::from_le_bytes([guid[4], guid[5]]);
	let product = u16::from_le_bytes([guid[8], guid[9]]);
	(vendor, product)
}

/// Button bitmask values for [`GamepadState::buttons`].
pub mod button {
	pub const A: u32 = 1 << 0;
	pub const B: u32 = 1 << 1;
	pub const X: u32 = 1 << 2;
	pub const Y: u32 = 1 << 3;
	pub const LB: u32 = 1 << 4;
	pub const RB: u32 = 1 << 5;
	pub const BACK: u32 = 1 << 6;
	pub const START: u32 = 1 << 7;
	pub const GUIDE: u32 = 1 << 8;
	pub const L3: u32 = 1 << 9;
	pub const R3: u32 = 1 << 10;
	pub const DPAD_UP: u32 = 1 << 11;
	pub const DPAD_DOWN: u32 = 1 << 12;
	pub const DPAD_LEFT: u32 = 1 << 13;
	pub const DPAD_RIGHT: u32 = 1 << 14;
}

/// A normalized snapshot of a controller, ready to serialize across the wire and
/// replay on the host. Sticks are full-range `i16`; triggers are `u8`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamepadState {
	pub buttons: u32,
	pub left_x: i16,
	pub left_y: i16,
	pub right_x: i16,
	pub right_y: i16,
	pub left_trigger: u8,
	pub right_trigger: u8,
}

impl GamepadState {
	pub fn is_pressed(&self, button: u32) -> bool {
		self.buttons & button != 0
	}

	pub fn set(&mut self, button: u32, pressed: bool) {
		if pressed {
			self.buttons |= button;
		} else {
			self.buttons &= !button;
		}
	}
}

/// Map a normalized `[-1.0, 1.0]` axis to full-range `i16`.
pub fn axis_to_i16(v: f32) -> i16 {
	(v.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// Map a `[0.0, 1.0]` trigger to `u8`.
pub fn trigger_to_u8(v: f32) -> u8 {
	(v.clamp(0.0, 1.0) * u8::MAX as f32).round() as u8
}

/// A host-side virtual controller that replays [`GamepadState`]s.
pub trait VirtualGamepad: Send {
	fn kind(&self) -> GamepadKind;
	/// Apply the latest input state to the emulated device.
	fn apply(&mut self, state: &GamepadState);
}

/// A test/no-op backend that just records what it was told to apply. The real
/// backends (ViGEm / uinput / IOKit) implement this same trait.
#[derive(Debug, Default)]
pub struct RecordingPad {
	kind_override: Option<GamepadKind>,
	pub applied: usize,
	pub last: Option<GamepadState>,
}

impl RecordingPad {
	pub fn new(kind: GamepadKind) -> Self {
		Self {
			kind_override: Some(kind),
			applied: 0,
			last: None,
		}
	}
}

impl VirtualGamepad for RecordingPad {
	fn kind(&self) -> GamepadKind {
		self.kind_override.unwrap_or(GamepadKind::Standard)
	}
	fn apply(&mut self, state: &GamepadState) {
		self.applied += 1;
		self.last = Some(*state);
	}
}

/// Map our normalized button bits to the XInput button bitmask (the wire format
/// ViGEm's Xbox360 pad expects). Pure so it's testable without the driver.
pub fn xinput_buttons(state: &GamepadState) -> u16 {
	const MAP: [(u32, u16); 15] = [
		(button::DPAD_UP, 0x0001),
		(button::DPAD_DOWN, 0x0002),
		(button::DPAD_LEFT, 0x0004),
		(button::DPAD_RIGHT, 0x0008),
		(button::START, 0x0010),
		(button::BACK, 0x0020),
		(button::L3, 0x0040),
		(button::R3, 0x0080),
		(button::LB, 0x0100),
		(button::RB, 0x0200),
		(button::GUIDE, 0x0400),
		(button::A, 0x1000),
		(button::B, 0x2000),
		(button::X, 0x4000),
		(button::Y, 0x8000),
	];
	let mut bits = 0u16;
	for (our, xinput) in MAP {
		if state.is_pressed(our) {
			bits |= xinput;
		}
	}
	bits
}

/// Create a host-side virtual pad for `kind`.
///
/// * Linux → a real **uinput** Xbox-style pad (falls back to recording if
///   `/dev/uinput` isn't writable).
/// * Windows → a **ViGEm** Xbox 360 pad (needs the ViGEmBus driver; falls back to
///   recording if it isn't installed). macOS → a driver, TODO (recording for now).
pub fn create_virtual_pad(kind: GamepadKind) -> Box<dyn VirtualGamepad> {
	#[cfg(target_os = "linux")]
	{
		match super::uinput::UinputGamepad::new(kind) {
			Ok(pad) => return Box::new(pad),
			Err(e) => {
				tracing::warn!("uinput virtual pad unavailable ({e}); using recording backend")
			}
		}
	}
	#[cfg(windows)]
	{
		match super::vigem::ViGEmGamepad::new(kind) {
			Ok(pad) => return Box::new(pad),
			Err(e) => {
				tracing::warn!("ViGEm virtual pad unavailable ({e}); using recording backend")
			}
		}
	}
	Box::new(RecordingPad::new(kind))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn detects_sony_and_microsoft_controllers() {
		assert_eq!(GamepadKind::from_vid_pid(0x054C, 0x0268), GamepadKind::Ds3);
		assert_eq!(GamepadKind::from_vid_pid(0x054C, 0x05C4), GamepadKind::Ds4);
		assert_eq!(GamepadKind::from_vid_pid(0x054C, 0x09CC), GamepadKind::Ds4);
		assert_eq!(GamepadKind::from_vid_pid(0x054C, 0x0CE6), GamepadKind::Ds5);
		assert_eq!(GamepadKind::from_vid_pid(0x045E, 0x028E), GamepadKind::Xbox); // 360
		assert_eq!(GamepadKind::from_vid_pid(0x045E, 0x0B12), GamepadKind::Xbox);
		// Series
	}

	#[test]
	fn unknown_and_generic_fallbacks() {
		assert_eq!(GamepadKind::from_vid_pid(0, 0), GamepadKind::Unknown);
		assert_eq!(
			GamepadKind::from_vid_pid(0x1234, 0x5678),
			GamepadKind::Standard
		);
		assert_eq!(
			GamepadKind::from_vid_pid(0x054C, 0xFFFF),
			GamepadKind::Standard
		);
	}

	#[test]
	fn parses_vendor_product_from_sdl_guid() {
		// SDL GUID with vendor 0x054C (bytes 4..6 LE) and product 0x05C4 (bytes 8..10 LE).
		let mut guid = [0u8; 16];
		guid[4] = 0x4C;
		guid[5] = 0x05;
		guid[8] = 0xC4;
		guid[9] = 0x05;
		assert_eq!(vid_pid_from_sdl_guid(guid), (0x054C, 0x05C4));
		assert_eq!(GamepadKind::from_vid_pid(0x054C, 0x05C4), GamepadKind::Ds4);
	}

	#[test]
	fn button_bitmask_round_trips() {
		let mut st = GamepadState::default();
		assert!(!st.is_pressed(button::A));
		st.set(button::A, true);
		st.set(button::DPAD_LEFT, true);
		assert!(st.is_pressed(button::A));
		assert!(st.is_pressed(button::DPAD_LEFT));
		assert!(!st.is_pressed(button::B));
		st.set(button::A, false);
		assert!(!st.is_pressed(button::A));
	}

	#[test]
	fn axis_and_trigger_scaling() {
		assert_eq!(axis_to_i16(0.0), 0);
		assert_eq!(axis_to_i16(1.0), i16::MAX);
		assert_eq!(axis_to_i16(-1.0), -i16::MAX);
		assert_eq!(axis_to_i16(2.0), i16::MAX); // clamped
		assert_eq!(trigger_to_u8(0.0), 0);
		assert_eq!(trigger_to_u8(1.0), 255);
		assert_eq!(trigger_to_u8(-1.0), 0); // clamped
	}

	#[test]
	fn state_serializes_for_the_wire() {
		let mut st = GamepadState::default();
		st.set(button::A | button::START, true);
		st.left_x = -12345;
		st.right_trigger = 200;
		let json = serde_json::to_string(&st).unwrap();
		let back: GamepadState = serde_json::from_str(&json).unwrap();
		assert_eq!(st, back);
	}

	#[test]
	fn virtual_pad_records_applied_states() {
		let mut pad = RecordingPad::new(GamepadKind::Ds4);
		assert_eq!(VirtualGamepad::kind(&pad), GamepadKind::Ds4);
		let mut st = GamepadState::default();
		st.set(button::B, true);
		pad.apply(&st);
		pad.apply(&st);
		assert_eq!(pad.applied, 2);
		assert_eq!(pad.last, Some(st));
	}

	#[test]
	fn create_virtual_pad_reports_its_kind() {
		let pad = create_virtual_pad(GamepadKind::Ds5);
		assert_eq!(pad.kind(), GamepadKind::Ds5);
	}

	#[test]
	fn xinput_button_mapping_matches_xinput_bits() {
		let mut st = GamepadState::default();
		assert_eq!(xinput_buttons(&st), 0);
		st.set(button::A, true);
		assert_eq!(xinput_buttons(&st), 0x1000); // XINPUT_GAMEPAD_A
		st.set(button::DPAD_UP, true);
		assert_eq!(xinput_buttons(&st), 0x1000 | 0x0001);
		st.set(button::START, true);
		st.set(button::Y, true);
		assert_eq!(xinput_buttons(&st), 0x1000 | 0x0001 | 0x0010 | 0x8000);
	}

	#[test]
	fn controller_info_serializes_for_the_wire() {
		let info = ControllerInfo {
			index: 0,
			name: "Wireless Controller".into(),
			kind: GamepadKind::Ds4,
			connected: true,
		};
		let json = serde_json::to_string(&info).unwrap();
		let back: ControllerInfo = serde_json::from_str(&json).unwrap();
		assert_eq!(info, back);
	}
}
