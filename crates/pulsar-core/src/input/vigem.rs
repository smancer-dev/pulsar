//! Windows host-side virtual gamepad via **ViGEmBus** (emulates an Xbox 360 pad,
//! which every game reads through XInput). Needs the ViGEmBus driver installed;
//! `new` errors if it isn't, and the caller falls back to a recording pad.

use super::{xinput_buttons, GamepadKind, GamepadState, VirtualGamepad};

pub struct ViGEmGamepad {
	kind: GamepadKind,
	target: vigem_client::Xbox360Wired<vigem_client::Client>,
}

impl ViGEmGamepad {
	pub fn new(kind: GamepadKind) -> Result<Self, Box<dyn std::error::Error>> {
		let client = vigem_client::Client::connect()?;
		let mut target =
			vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
		target.plugin()?;
		target.wait_ready()?;
		Ok(Self { kind, target })
	}
}

impl VirtualGamepad for ViGEmGamepad {
	fn kind(&self) -> GamepadKind {
		self.kind
	}
	fn apply(&mut self, state: &GamepadState) {
		let pad = vigem_client::XGamepad {
			buttons: vigem_client::XButtons {
				raw: xinput_buttons(state),
			},
			// XInput thumb Y is up-positive, same as our GamepadState — no invert.
			thumb_lx: state.left_x,
			thumb_ly: state.left_y,
			thumb_rx: state.right_x,
			thumb_ry: state.right_y,
			left_trigger: state.left_trigger,
			right_trigger: state.right_trigger,
		};
		let _ = self.target.update(&pad);
	}
}
