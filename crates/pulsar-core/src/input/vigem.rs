//! Windows host-side virtual gamepad via **ViGEmBus**. Supports Xbox 360 and
//! DualShock 4 emulation targets (ViGEm has no DS3 or DS5 target; those are
//! presented as DS4). Needs the ViGEmBus driver installed; `new_target` errors
//! if it isn't, and the caller falls back to a recording pad.
//!
//! note: unstable_ds4 is an unstable vigem-client feature, pinned via Cargo.lock 0.1.4

use super::{
	ds4_report_fields, xinput_buttons, GamepadKind, GamepadState, ResolvedTarget, RumbleReader,
	VirtualGamepad,
};

enum Backend {
	Xbox360(vigem_client::Xbox360Wired<vigem_client::Client>),
	Ds4(vigem_client::DualShock4Wired<vigem_client::Client>),
}

pub struct ViGEmGamepad {
	kind: GamepadKind,
	backend: Backend,
	/// DS4 report timestamp (5.333µs units) — MUST advance between sends or some apps
	/// (incl. Steam) ignore the DS4 input, notably the PS/guide button (matches Sunshine's
	/// `ds4_update_ts_and_send`). Incremented from real elapsed time on each apply.
	ds4_ts: u16,
	ds4_last: Option<std::time::Instant>,
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
		Ok(Self { kind, backend, ds4_ts: 0, ds4_last: None })
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
		// Advance the DS4 report timestamp from real elapsed time (5.333µs units) so it
		// changes between sends — Steam/others require this to register DS4 input, the PS
		// button especially. play.rs streams gamepad frames ~60Hz, so this ticks steadily.
		let now = std::time::Instant::now();
		let delta_ns = self
			.ds4_last
			.map(|t| now.duration_since(t).as_nanos())
			.unwrap_or(0);
		self.ds4_ts = self.ds4_ts.wrapping_add((delta_ns / 5333) as u16);
		self.ds4_last = Some(now);
		let ds4_ts = self.ds4_ts;
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
				// Submit the COMPLETE 63-byte DS4_REPORT_EX: ViGEmBus only surfaces the PS
				// button (special bit 0) + touchpad to Windows/Steam through the extended
				// report — the legacy basic report drops them (matches Sunshine's path).
				let ex = vigem_client::DS4ReportEx {
					thumb_lx: lx,
					thumb_ly: ly,
					thumb_rx: rx,
					thumb_ry: ry,
					buttons,
					special,
					trigger_l: state.left_trigger,
					trigger_r: state.right_trigger,
					timestamp: ds4_ts,
					..Default::default()
				};
				// DEBUG (PS/guide diagnosis): log the FIRST update_ex result so we can tell
				// whether the EX IOCTL path is active or falling back to the legacy report.
				let ex_res = t.update_ex(&ex);
				{
					use std::sync::Once;
					static EX_LOG: Once = Once::new();
					EX_LOG.call_once(|| match &ex_res {
						Ok(()) => tracing::info!("DS4 update_ex OK — extended report path active"),
						Err(e) => tracing::warn!(err = ?e, "DS4 update_ex FAILED — falling back to legacy report (no PS button)"),
					});
				}
				// Fall back to the legacy report if the EX IOCTL is unavailable (ViGEmBus
				// < 1.17), so the pad still works (just without the PS button) — never worse.
				if ex_res.is_err() {
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

	fn rumble_reader(&self) -> Option<Box<dyn RumbleReader>> {
		match &self.backend {
			// Only the DS4 backend forwards rumble (it has a notification path); the Xbox360
			// target's rumble notification is behind a disabled crate feature for now.
			Backend::Ds4(t) => Some(Box::new(Ds4Rumble { notifier: t.notifier() })),
			Backend::Xbox360(_) => None,
		}
	}
}

/// Blocking rumble reader over a ViGEm DS4 notification channel.
struct Ds4Rumble {
	notifier: vigem_client::Ds4Notifier,
}
impl RumbleReader for Ds4Rumble {
	fn next(&mut self) -> Option<(u8, u8)> {
		// Blocks until the game sends rumble; None when the virtual pad is unplugged.
		match self.notifier.await_notification() {
			Ok(n) => Some((n.large_motor, n.small_motor)),
			Err(e) => {
				tracing::warn!(err = ?e, "rumble: DS4 await_notification failed (IOCTL/struct issue?)");
				None
			}
		}
	}
}
