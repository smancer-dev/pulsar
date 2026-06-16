//! Windows host-side virtual gamepad via **ViGEmBus**. Supports Xbox 360 and
//! DualShock 4 emulation targets (ViGEm has no DS3 or DS5 target; those are
//! presented as DS4). Needs the ViGEmBus driver installed; `new_target` errors
//! if it isn't, and the caller falls back to a recording pad.
//!
//! note: unstable_ds4 is an unstable vigem-client feature, pinned via Cargo.lock 0.1.4

use super::{ds4_report_fields, xinput_buttons, GamepadKind, GamepadState, ResolvedTarget, VirtualGamepad};

enum Backend {
	Xbox360(vigem_client::Xbox360Wired<vigem_client::Client>),
	Ds4(vigem_client::DualShock4Wired<vigem_client::Client>),
}

pub struct ViGEmGamepad {
	kind: GamepadKind,
	backend: Backend,
}

impl ViGEmGamepad {
	/// Create a virtual gamepad for `kind` with an explicit emulation target.
	///
	/// - `ResolvedTarget::Xbox360` → ViGEm Xbox 360 Wired target.
	/// - `ResolvedTarget::Ds4` → ViGEm DualShock 4 Wired target (DS3/DS5 are
	///   presented as DS4, the only Sony target ViGEm supports).
	pub fn new_target(
		kind: GamepadKind,
		resolved: ResolvedTarget,
	) -> Result<Self, Box<dyn std::error::Error>> {
		let client = vigem_client::Client::connect()?;
		let backend = match resolved {
			ResolvedTarget::Xbox360 => {
				let mut t = vigem_client::Xbox360Wired::new(
					client,
					vigem_client::TargetId::XBOX360_WIRED,
				);
				t.plugin()?;
				t.wait_ready()?;
				Backend::Xbox360(t)
			}
			ResolvedTarget::Ds4 => {
				let mut t = vigem_client::DualShock4Wired::new(
					client,
					vigem_client::TargetId::DUALSHOCK4_WIRED,
				);
				t.plugin()?;
				t.wait_ready()?;
				Backend::Ds4(t)
			}
		};
		Ok(Self { kind, backend })
	}

	/// Convenience wrapper: create an Xbox 360 target (keeps existing call sites
	/// that predate the multi-target API compiling without changes).
	#[allow(dead_code)]
	pub fn new(kind: GamepadKind) -> Result<Self, Box<dyn std::error::Error>> {
		Self::new_target(kind, ResolvedTarget::Xbox360)
	}
}

impl VirtualGamepad for ViGEmGamepad {
	fn kind(&self) -> GamepadKind {
		self.kind
	}

	fn apply(&mut self, state: &GamepadState) {
		match &mut self.backend {
			Backend::Xbox360(t) => {
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
				let _ = t.update(&pad);
			}
			Backend::Ds4(t) => {
				let (buttons, special, lx, ly, rx, ry) = ds4_report_fields(state);
				let report = vigem_client::DS4Report {
					thumb_lx: lx,
					thumb_ly: ly,
					thumb_rx: rx,
					thumb_ry: ry,
					buttons,
					special,
					trigger_l: state.left_trigger,
					trigger_r: state.right_trigger,
				};
				let _ = t.update(&report);
			}
		}
	}
}
