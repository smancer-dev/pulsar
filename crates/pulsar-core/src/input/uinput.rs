//! Linux host-side backends built on **uinput**: the virtual [`UinputGamepad`]
//! and the mouse + keyboard injector [`DesktopInput`]. Both work on Wayland and
//! X11 (kernel input layer), unlike `x11grab`/`xdotool`.

use super::{button, GamepadKind, GamepadState, VirtualGamepad};
use evdev::{
	uinput::{VirtualDevice, VirtualDeviceBuilder},
	AbsInfo, AbsoluteAxisType, AttributeSet, EventType, InputEvent, Key, PropType,
	RelativeAxisType, UinputAbsSetup,
};

/// Maps our normalized button bits to evdev gamepad key codes (used by the
/// Linux backend; pure so it's testable).
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
		rels.insert(RelativeAxisType::REL_X); // relative pointer (native renderer)
		rels.insert(RelativeAxisType::REL_Y);
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

	/// Move the pointer by a relative delta (native renderer / games).
	pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
		let mut ev = Vec::new();
		let x = dx.round() as i32;
		let y = dy.round() as i32;
		if x != 0 {
			ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, x));
		}
		if y != 0 {
			ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, y));
		}
		if !ev.is_empty() {
			let _ = self.pointer.emit(&ev);
		}
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
		let _ = self
			.keyboard
			.emit(&[InputEvent::new(EventType::KEY, code as u16, down as i32)]);
	}

	/// Type a resolved Unicode character (layout-independent). uinput is keycode-based with no
	/// direct Unicode-insert path, so a Linux HOST can't trivially type an arbitrary codepoint;
	/// no-op for now (Windows hosts use KEYEVENTF_UNICODE). A Linux-host client typically shares
	/// the same layout, and the `Key` (keycode) path still works. TODO: synthesize via a temporary
	/// keymap remap if Linux-host non-matching-layout input becomes a requirement.
	pub fn type_char(&mut self, _c: char) {}
}

#[cfg(test)]
mod tests {
	use super::{button, evdev_buttons};

	#[test]
	fn evdev_button_map_is_complete_and_correct() {
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
