//! Game controller support.
//!
//! Pulsar reads physical controllers on the **client** with `gilrs` (which wraps
//! XInput/DInput on Windows, IOKit on macOS, and evdev on Linux), normalizes
//! them into a transport-friendly [`GamepadState`], and replays them on the
//! **host** through a [`VirtualGamepad`] backend (ViGEm on Windows, uinput on
//! Linux, a driver on macOS).
//!
//! DualShock 3/4, DualSense, Xbox and generic pads are all supported; the
//! controller *kind* is detected from its USB vendor/product id so the host can
//! present a matching virtual device.

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

/// Create a host-side virtual pad for `kind`.
///
/// * Linux → a real **uinput** Xbox-style pad (falls back to recording if
///   `/dev/uinput` isn't writable).
/// * Windows → ViGEm, macOS → a driver — TODO; both fall back to recording so
///   the rest of the app still works.
pub fn create_virtual_pad(kind: GamepadKind) -> Box<dyn VirtualGamepad> {
	#[cfg(target_os = "linux")]
	{
		match uinput_backend::UinputGamepad::new(kind) {
			Ok(pad) => return Box::new(pad),
			Err(e) => {
				tracing::warn!("uinput virtual pad unavailable ({e}); using recording backend")
			}
		}
	}
	Box::new(RecordingPad::new(kind))
}

/// Maps our normalized button bits to evdev gamepad key codes (used by the
/// Linux backend; pure so it's testable).
#[cfg(target_os = "linux")]
fn evdev_buttons() -> [(u32, evdev::Key); 15] {
	use evdev::Key;
	[
		(button::A, Key::BTN_SOUTH),
		(button::B, Key::BTN_EAST),
		(button::X, Key::BTN_WEST),
		(button::Y, Key::BTN_NORTH),
		(button::LB, Key::BTN_TL),
		(button::RB, Key::BTN_TR),
		(button::BACK, Key::BTN_SELECT),
		(button::START, Key::BTN_START),
		(button::GUIDE, Key::BTN_MODE),
		(button::L3, Key::BTN_THUMBL),
		(button::R3, Key::BTN_THUMBR),
		(button::DPAD_UP, Key::BTN_DPAD_UP),
		(button::DPAD_DOWN, Key::BTN_DPAD_DOWN),
		(button::DPAD_LEFT, Key::BTN_DPAD_LEFT),
		(button::DPAD_RIGHT, Key::BTN_DPAD_RIGHT),
	]
}

#[cfg(target_os = "linux")]
mod uinput_backend {
	use super::{evdev_buttons, GamepadKind, GamepadState, VirtualGamepad};
	use evdev::{
		uinput::{VirtualDevice, VirtualDeviceBuilder},
		AbsInfo, AbsoluteAxisType, AttributeSet, EventType, InputEvent, Key, PropType,
		RelativeAxisType, UinputAbsSetup,
	};

	pub struct UinputGamepad {
		kind: GamepadKind,
		dev: VirtualDevice,
	}

	impl UinputGamepad {
		pub fn new(kind: GamepadKind) -> std::io::Result<Self> {
			let mut keys = AttributeSet::<Key>::new();
			for (_, key) in evdev_buttons() {
				keys.insert(key);
			}
			let stick = |axis| {
				UinputAbsSetup::new(
					axis,
					AbsInfo::new(0, i16::MIN as i32, i16::MAX as i32, 16, 128, 1),
				)
			};
			let trig = |axis| UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 255, 0, 0, 1));
			let dev = VirtualDeviceBuilder::new()?
				.name("Pulsar Virtual Gamepad")
				.with_keys(&keys)?
				.with_absolute_axis(&stick(AbsoluteAxisType::ABS_X))?
				.with_absolute_axis(&stick(AbsoluteAxisType::ABS_Y))?
				.with_absolute_axis(&stick(AbsoluteAxisType::ABS_RX))?
				.with_absolute_axis(&stick(AbsoluteAxisType::ABS_RY))?
				.with_absolute_axis(&trig(AbsoluteAxisType::ABS_Z))?
				.with_absolute_axis(&trig(AbsoluteAxisType::ABS_RZ))?
				.build()?;
			Ok(Self { kind, dev })
		}
	}

	impl VirtualGamepad for UinputGamepad {
		fn kind(&self) -> GamepadKind {
			self.kind
		}
		fn apply(&mut self, state: &GamepadState) {
			let mut events = Vec::with_capacity(21);
			for (bit, key) in evdev_buttons() {
				let val = if state.is_pressed(bit) { 1 } else { 0 };
				events.push(InputEvent::new(EventType::KEY, key.code(), val));
			}
			let abs =
				|axis: AbsoluteAxisType, v: i32| InputEvent::new(EventType::ABSOLUTE, axis.0, v);
			events.push(abs(AbsoluteAxisType::ABS_X, state.left_x as i32));
			// evdev Y points down; gamepad up is +, so invert.
			events.push(abs(AbsoluteAxisType::ABS_Y, -(state.left_y as i32)));
			events.push(abs(AbsoluteAxisType::ABS_RX, state.right_x as i32));
			events.push(abs(AbsoluteAxisType::ABS_RY, -(state.right_y as i32)));
			events.push(abs(AbsoluteAxisType::ABS_Z, state.left_trigger as i32));
			events.push(abs(AbsoluteAxisType::ABS_RZ, state.right_trigger as i32));
			let _ = self.dev.emit(&events);
		}
	}

	/// Absolute-coordinate range for the virtual pointer; the compositor maps this
	/// onto the screen, so the client's normalized 0..1 lands at the right pixel.
	const ABS_MAX: i32 = 65535;

	/// Injects mouse + keyboard onto the host desktop via two uinput devices — an
	/// absolute pointer (maps directly to the screen) and a keyboard. Works on
	/// Wayland and X11 (kernel input layer), unlike `x11grab`/`xdotool`.
	pub struct DesktopInput {
		pointer: VirtualDevice,
		keyboard: VirtualDevice,
	}

	impl DesktopInput {
		pub fn new() -> std::io::Result<Self> {
			let mut btns = AttributeSet::<Key>::new();
			btns.insert(Key::BTN_LEFT);
			btns.insert(Key::BTN_RIGHT);
			btns.insert(Key::BTN_MIDDLE);
			let mut rels = AttributeSet::<RelativeAxisType>::new();
			rels.insert(RelativeAxisType::REL_WHEEL);
			rels.insert(RelativeAxisType::REL_HWHEEL);
			// INPUT_PROP_DIRECT → coordinates map directly to the screen (touchscreen
			// style) so absolute positioning matches the streamed display.
			let mut props = AttributeSet::<PropType>::new();
			props.insert(PropType::DIRECT);
			let abs = |axis| UinputAbsSetup::new(axis, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 1));
			let pointer = VirtualDeviceBuilder::new()?
				.name("Pulsar Virtual Pointer")
				.with_properties(&props)?
				.with_keys(&btns)?
				.with_absolute_axis(&abs(AbsoluteAxisType::ABS_X))?
				.with_absolute_axis(&abs(AbsoluteAxisType::ABS_Y))?
				.with_relative_axes(&rels)?
				.build()?;

			let mut keys = AttributeSet::<Key>::new();
			for c in 1u16..=248 {
				keys.insert(Key::new(c));
			}
			let keyboard = VirtualDeviceBuilder::new()?
				.name("Pulsar Virtual Keyboard")
				.with_keys(&keys)?
				.build()?;
			Ok(Self { pointer, keyboard })
		}

		/// Move the pointer to a normalized (0..1) position on the screen.
		pub fn pointer(&mut self, x: f64, y: f64) {
			let cx = (x.clamp(0.0, 1.0) * ABS_MAX as f64) as i32;
			let cy = (y.clamp(0.0, 1.0) * ABS_MAX as f64) as i32;
			let _ = self.pointer.emit(&[
				InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_X.0, cx),
				InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, cy),
			]);
		}

		/// Press/release a mouse button (0=left, 1=right, 2=middle).
		pub fn button(&mut self, button: u8, down: bool) {
			let key = match button {
				1 => Key::BTN_RIGHT,
				2 => Key::BTN_MIDDLE,
				_ => Key::BTN_LEFT,
			};
			let _ = self
				.pointer
				.emit(&[InputEvent::new(EventType::KEY, key.code(), down as i32)]);
		}

		/// Scroll by a delta (browser wheel pixels → wheel clicks).
		pub fn scroll(&mut self, dx: f64, dy: f64) {
			let v = -(dy / 100.0).round() as i32; // evdev wheel up is +; browser down is +
			let h = (dx / 100.0).round() as i32;
			let mut ev = Vec::new();
			if v != 0 {
				ev.push(InputEvent::new(
					EventType::RELATIVE,
					RelativeAxisType::REL_WHEEL.0,
					v,
				));
			}
			if h != 0 {
				ev.push(InputEvent::new(
					EventType::RELATIVE,
					RelativeAxisType::REL_HWHEEL.0,
					h,
				));
			}
			if !ev.is_empty() {
				let _ = self.pointer.emit(&ev);
			}
		}

		/// Press/release a key by evdev keycode.
		pub fn key(&mut self, code: u32, down: bool) {
			if code == 0 || code > 248 {
				return;
			}
			let _ =
				self.keyboard
					.emit(&[InputEvent::new(EventType::KEY, code as u16, down as i32)]);
		}
	}
}

/// Host-side mouse + keyboard injection for remote control. Linux uses uinput
/// (works on Wayland and X11); other platforms are no-op stubs for now.
#[cfg(target_os = "linux")]
pub use uinput_backend::DesktopInput;

#[cfg(not(target_os = "linux"))]
pub struct DesktopInput;
#[cfg(not(target_os = "linux"))]
impl DesktopInput {
	pub fn new() -> std::io::Result<Self> {
		Ok(Self)
	}
	pub fn pointer(&mut self, _x: f64, _y: f64) {}
	pub fn button(&mut self, _button: u8, _down: bool) {}
	pub fn scroll(&mut self, _dx: f64, _dy: f64) {}
	pub fn key(&mut self, _code: u32, _down: bool) {}
}

pub use hub::ControllerHub;

/// Live controller reading via `gilrs`. Always compiled (the dep is small), but
/// only useful where physical controllers exist.
pub mod hub {
	use super::{
		axis_to_i16, button, trigger_to_u8, vid_pid_from_sdl_guid, GamepadKind, GamepadState,
	};
	use gilrs::{Axis, Button, Gamepad, Gilrs};

	/// Map a `gilrs` button to our bitmask value (analog triggers excluded — they
	/// travel as axes).
	pub fn button_bit(b: Button) -> Option<u32> {
		Some(match b {
			Button::South => button::A,
			Button::East => button::B,
			Button::West => button::X,
			Button::North => button::Y,
			Button::LeftTrigger => button::LB,
			Button::RightTrigger => button::RB,
			Button::Select => button::BACK,
			Button::Start => button::START,
			Button::Mode => button::GUIDE,
			Button::LeftThumb => button::L3,
			Button::RightThumb => button::R3,
			Button::DPadUp => button::DPAD_UP,
			Button::DPadDown => button::DPAD_DOWN,
			Button::DPadLeft => button::DPAD_LEFT,
			Button::DPadRight => button::DPAD_RIGHT,
			_ => return None,
		})
	}

	/// Read the current state of one connected gamepad.
	pub fn state_from_gamepad(gp: &Gamepad<'_>) -> GamepadState {
		let mut st = GamepadState::default();
		for (b, bit) in [
			Button::South,
			Button::East,
			Button::West,
			Button::North,
			Button::LeftTrigger,
			Button::RightTrigger,
			Button::Select,
			Button::Start,
			Button::Mode,
			Button::LeftThumb,
			Button::RightThumb,
			Button::DPadUp,
			Button::DPadDown,
			Button::DPadLeft,
			Button::DPadRight,
		]
		.into_iter()
		.filter_map(|b| button_bit(b).map(|bit| (b, bit)))
		{
			st.set(bit, gp.is_pressed(b));
		}
		st.left_x = axis_to_i16(gp.value(Axis::LeftStickX));
		st.left_y = axis_to_i16(gp.value(Axis::LeftStickY));
		st.right_x = axis_to_i16(gp.value(Axis::RightStickX));
		st.right_y = axis_to_i16(gp.value(Axis::RightStickY));
		st.left_trigger = trigger_to_u8(gp.value(Axis::LeftZ));
		st.right_trigger = trigger_to_u8(gp.value(Axis::RightZ));
		st
	}

	/// Detect a connected pad's [`GamepadKind`] from its SDL GUID.
	pub fn kind_of(gp: &Gamepad<'_>) -> GamepadKind {
		let (vid, pid) = vid_pid_from_sdl_guid(gp.uuid());
		GamepadKind::from_vid_pid(vid, pid)
	}

	/// Wraps a `gilrs` context to read all connected controllers.
	pub struct ControllerHub {
		gilrs: Gilrs,
	}

	impl ControllerHub {
		pub fn new() -> Result<Self, gilrs::Error> {
			Ok(Self {
				gilrs: Gilrs::new()?,
			})
		}

		/// Pump pending events and return `(kind, state)` for every connected pad.
		pub fn snapshot(&mut self) -> Vec<(GamepadKind, GamepadState)> {
			while self.gilrs.next_event().is_some() {}
			self.gilrs
				.gamepads()
				.filter(|(_, gp)| gp.is_connected())
				.map(|(_, gp)| (kind_of(&gp), state_from_gamepad(&gp)))
				.collect()
		}
	}
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

	#[cfg(target_os = "linux")]
	#[test]
	fn evdev_button_map_is_complete_and_correct() {
		use super::evdev_buttons;
		let map = evdev_buttons();
		assert_eq!(map.len(), 15);
		assert!(map
			.iter()
			.any(|(b, k)| *b == button::A && *k == evdev::Key::BTN_SOUTH));
		assert!(map
			.iter()
			.any(|(b, k)| *b == button::START && *k == evdev::Key::BTN_START));
		assert!(map
			.iter()
			.any(|(b, k)| *b == button::DPAD_UP && *k == evdev::Key::BTN_DPAD_UP));
	}
}
