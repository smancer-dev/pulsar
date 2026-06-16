//! Live controller reading via `gilrs`. Always compiled (the dep is small), but
//! only useful where physical controllers exist.

use super::{
	axis_to_i16, button, trigger_to_u8, vid_pid_from_sdl_guid, ControllerInfo, GamepadKind,
	GamepadState,
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

	/// Pump pending events and return `(uuid_hex, kind, state)` for every connected pad.
	/// `uuid_hex` is the gilrs `Gamepad::uuid()` encoded as a lowercase hex string — the
	/// same stable device key stored in `controllerOrder` / `AppState::controller_order`.
	pub fn snapshot_with_keys(&mut self) -> Vec<(String, GamepadKind, GamepadState)> {
		while self.gilrs.next_event().is_some() {}
		self.gilrs
			.gamepads()
			.filter(|(_, gp)| gp.is_connected())
			.map(|(_, gp)| {
				let uuid_hex = gp
					.uuid()
					.iter()
					.map(|b| format!("{b:02x}"))
					.collect::<String>();
				(uuid_hex, kind_of(&gp), state_from_gamepad(&gp))
			})
			.collect()
	}

	/// Enumerate every controller the backend knows about (connected or not),
	/// with its name + detected kind — for the UI's device list and the live
	/// in-session controller panel.
	pub fn list(&mut self) -> Vec<ControllerInfo> {
		while self.gilrs.next_event().is_some() {}
		self.gilrs
			.gamepads()
			.enumerate()
			.map(|(i, (_, gp))| ControllerInfo {
				index: i as u32,
				uuid: gp.uuid().iter().map(|b| format!("{b:02x}")).collect(),
				name: gp.name().to_string(),
				kind: kind_of(&gp),
				connected: gp.is_connected(),
			})
			.collect()
	}
}
