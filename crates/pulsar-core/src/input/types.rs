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

/// What virtual controller a client pad should be presented to host games as.
/// `Auto` (the wire/serde default) lets the host pick from the detected GamepadKind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmulationTarget {
	#[default]
	Auto,
	Xbox360,
	Ds4,
}

/// The concrete backend identity an EmulationTarget resolves to (never Auto).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedTarget {
	Xbox360,
	Ds4,
}

impl EmulationTarget {
	/// Resolve to a concrete backend. Auto maps Sony families (Ds3/Ds4/Ds5) to DS4
	/// and everything else (Xbox/Standard/Unknown) to Xbox360; explicit variants pass
	/// through. Never returns Auto.
	pub fn resolve(self, kind: GamepadKind) -> ResolvedTarget {
		match self {
			EmulationTarget::Xbox360 => ResolvedTarget::Xbox360,
			EmulationTarget::Ds4 => ResolvedTarget::Ds4,
			EmulationTarget::Auto => match kind {
				GamepadKind::Ds3 | GamepadKind::Ds4 | GamepadKind::Ds5 => ResolvedTarget::Ds4,
				_ => ResolvedTarget::Xbox360,
			},
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
	/// Stable device key: the gilrs `Gamepad::uuid()` bytes encoded as a lowercase
	/// hex string. Used as the key in `controllerOrder` / `AppState::controller_order`
	/// so player-slot assignments survive gilrs enumeration-order reshuffles on hotplug.
	pub uuid: String,
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

/// Convert a touchpad-finger delta into a relative mouse `(dx, dy)` motion pair.
///
/// `prev` and `cur` are raw ABS_MT_POSITION (or ABS_X/Y) integer coordinates as
/// reported by the DS4/DS5 touchpad. `sens` is a multiplier (e.g. 0.5 maps one
/// touchpad unit to half a screen pixel; typical values 0.1 – 1.0).
///
/// Pure (no evdev dependency) so it can be unit-tested without a device.
pub fn touch_to_delta(prev: (i32, i32), cur: (i32, i32), sens: f64) -> (f64, f64) {
	let dx = (cur.0 - prev.0) as f64 * sens;
	let dy = (cur.1 - prev.1) as f64 * sens;
	(dx, dy)
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

/// Encode our normalized [`GamepadState`] into the DualShock 4 HID report fields the
/// ViGEm DS4 target expects. Returns `(buttons, special, lx, ly, rx, ry)`.
///
/// `buttons: u16` packs byte0 (low 8 bits) + byte1 (high 8 bits):
/// - bits 0..3 = DPAD HAT (0=N, 1=NE, 2=E, 3=SE, 4=S, 5=SW, 6=W, 7=NW, 8=neutral)
/// - bit 4 = Square (our X), bit 5 = Cross (our A), bit 6 = Circle (our B), bit 7 = Triangle (our Y)
/// - bit 8 = L1, bit 9 = R1, bit 10 = L2-as-bit, bit 11 = R2-as-bit
/// - bit 12 = Share (our BACK), bit 13 = Options (our START), bit 14 = L3, bit 15 = R3
///
/// `special: u8`: bit 0 = PS (our GUIDE), bit 1 = touchpad click (not in our state → 0).
///
/// Sticks: `i16 → u8` with 0x80 as center (i16 0 → 128). Y is left as-is (down-positive
/// convention matches the Xbox path — no inversion on either axis). Pure so it can be
/// unit-tested without the driver.
pub fn ds4_report_fields(state: &GamepadState) -> (u16, u8, u8, u8, u8, u8) {
	// DPAD HAT encoding.
	let up = state.is_pressed(button::DPAD_UP);
	let down = state.is_pressed(button::DPAD_DOWN);
	let left = state.is_pressed(button::DPAD_LEFT);
	let right = state.is_pressed(button::DPAD_RIGHT);
	let hat: u16 = match (up, down, left, right) {
		(true, false, false, false) => 0, // N
		(true, false, false, true) => 1,  // NE
		(false, false, false, true) => 2, // E
		(false, true, false, true) => 3,  // SE
		(false, true, false, false) => 4, // S
		(false, true, true, false) => 5,  // SW
		(false, false, true, false) => 6, // W
		(true, false, true, false) => 7,  // NW
		_ => 8,                           // neutral (no input or conflicting)
	};

	// Face buttons (byte0 high nibble).
	let square = if state.is_pressed(button::X) { 1u16 << 4 } else { 0 };
	let cross = if state.is_pressed(button::A) { 1u16 << 5 } else { 0 };
	let circle = if state.is_pressed(button::B) { 1u16 << 6 } else { 0 };
	let triangle = if state.is_pressed(button::Y) { 1u16 << 7 } else { 0 };

	// Shoulder / trigger-as-bit / meta (byte1).
	let l1 = if state.is_pressed(button::LB) { 1u16 << 8 } else { 0 };
	let r1 = if state.is_pressed(button::RB) { 1u16 << 9 } else { 0 };
	let l2_bit = if state.left_trigger > 0 { 1u16 << 10 } else { 0 };
	let r2_bit = if state.right_trigger > 0 { 1u16 << 11 } else { 0 };
	let share = if state.is_pressed(button::BACK) { 1u16 << 12 } else { 0 };
	let options = if state.is_pressed(button::START) { 1u16 << 13 } else { 0 };
	let l3 = if state.is_pressed(button::L3) { 1u16 << 14 } else { 0 };
	let r3 = if state.is_pressed(button::R3) { 1u16 << 15 } else { 0 };

	let buttons: u16 =
		hat | square | cross | circle | triangle | l1 | r1 | l2_bit | r2_bit | share | options | l3
			| r3;

	// Special: PS button in bit 0; touchpad click (bit 1) is not tracked → 0.
	let special: u8 = if state.is_pressed(button::GUIDE) { 1 } else { 0 };

	// Stick conversion: i16 → u8, 0x80 = center.
	let conv = |v: i16| ((v as i32 >> 8) + 128).clamp(0, 255) as u8;
	let lx = conv(state.left_x);
	let ly = conv(state.left_y);
	let rx = conv(state.right_x);
	let ry = conv(state.right_y);

	(buttons, special, lx, ly, rx, ry)
}

/// Create a host-side virtual pad for `kind` using `EmulationTarget::Auto` to select
/// the backend. See [`create_virtual_pad_target`] for full control over the target.
///
/// * Linux → a real **uinput** Xbox-style pad (falls back to recording if
///   `/dev/uinput` isn't writable).
/// * Windows → a **ViGEm** Xbox 360 pad (needs the ViGEmBus driver; falls back to
///   recording if it isn't installed). macOS → a driver, TODO (recording for now).
pub fn create_virtual_pad(kind: GamepadKind) -> Box<dyn VirtualGamepad> {
	create_virtual_pad_target(kind, EmulationTarget::Auto)
}

/// Create a host-side virtual pad for `kind` with an explicit `target` emulation
/// strategy. `target` is resolved against `kind` via [`EmulationTarget::resolve`]
/// so `Auto` maps Sony families (Ds3/Ds4/Ds5) to DS4 and everything else to Xbox
/// 360. Explicit `Xbox360` / `Ds4` override the detected kind regardless of `kind`.
///
/// Platforms:
/// * Linux → **uinput** (falls back to recording if `/dev/uinput` isn't writable).
/// * Windows → **ViGEm** (falls back to recording if ViGEmBus isn't installed).
/// * macOS / other → recording (no-op) backend; `target` is documented-ignored.
pub fn create_virtual_pad_target(
	kind: GamepadKind,
	target: EmulationTarget,
) -> Box<dyn VirtualGamepad> {
	let resolved = target.resolve(kind);
	#[cfg(target_os = "linux")]
	{
		match super::uinput::UinputGamepad::new_target(kind, resolved) {
			Ok(pad) => return Box::new(pad),
			Err(e) => {
				tracing::warn!("uinput virtual pad unavailable ({e}); using recording backend")
			}
		}
	}
	#[cfg(windows)]
	{
		match super::vigem::ViGEmGamepad::new_target(kind, resolved) {
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
		// create_virtual_pad_target with Auto must also propagate the kind.
		let pad2 = create_virtual_pad_target(GamepadKind::Ds5, EmulationTarget::Auto);
		assert_eq!(pad2.kind(), GamepadKind::Ds5);
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
			uuid: "030000004c050000cc09000000006800".into(),
			name: "Wireless Controller".into(),
			kind: GamepadKind::Ds4,
			connected: true,
		};
		let json = serde_json::to_string(&info).unwrap();
		let back: ControllerInfo = serde_json::from_str(&json).unwrap();
		assert_eq!(info, back);
	}

	#[test]
	fn touch_to_delta_scales_correctly() {
		// Zero delta when prev == cur.
		assert_eq!(touch_to_delta((100, 200), (100, 200), 1.0), (0.0, 0.0));
		// Positive movement with sens = 1.0.
		assert_eq!(touch_to_delta((0, 0), (10, 5), 1.0), (10.0, 5.0));
		// Negative (left/up) movement.
		assert_eq!(touch_to_delta((50, 50), (30, 40), 1.0), (-20.0, -10.0));
		// Sensitivity scaling.
		let (dx, dy) = touch_to_delta((0, 0), (100, 50), 0.5);
		assert!((dx - 50.0).abs() < f64::EPSILON);
		assert!((dy - 25.0).abs() < f64::EPSILON);
	}

	#[test]
	fn emulation_target_resolve_maps_kinds() {
		// Auto: Sony families → Ds4
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Ds3), ResolvedTarget::Ds4);
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Ds4), ResolvedTarget::Ds4);
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Ds5), ResolvedTarget::Ds4);
		// Auto: everything else → Xbox360
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Xbox), ResolvedTarget::Xbox360);
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Standard), ResolvedTarget::Xbox360);
		assert_eq!(EmulationTarget::Auto.resolve(GamepadKind::Unknown), ResolvedTarget::Xbox360);
		// Explicit overrides: Xbox360 forces Xbox360 regardless of detected kind
		assert_eq!(EmulationTarget::Xbox360.resolve(GamepadKind::Ds4), ResolvedTarget::Xbox360);
		// Explicit overrides: Ds4 forces Ds4 regardless of detected kind
		assert_eq!(EmulationTarget::Ds4.resolve(GamepadKind::Xbox), ResolvedTarget::Ds4);
	}

	#[test]
	fn emulation_target_serializes_lowercase() {
		assert_eq!(serde_json::to_string(&EmulationTarget::Auto).unwrap(), "\"auto\"");
		assert_eq!(serde_json::to_string(&EmulationTarget::Xbox360).unwrap(), "\"xbox360\"");
		assert_eq!(serde_json::to_string(&EmulationTarget::Ds4).unwrap(), "\"ds4\"");
		// Round-trip
		let back: EmulationTarget = serde_json::from_str("\"ds4\"").unwrap();
		assert_eq!(back, EmulationTarget::Ds4);
		let back: EmulationTarget = serde_json::from_str("\"auto\"").unwrap();
		assert_eq!(back, EmulationTarget::Auto);
	}

	#[test]
	fn ds4_report_neutral() {
		let st = GamepadState::default();
		let (buttons, special, lx, ly, rx, ry) = ds4_report_fields(&st);
		// DPAD HAT low nibble must be 8 (neutral).
		assert_eq!(buttons & 0x000F, 8, "hat should be 8 (neutral), got {}", buttons & 0x000F);
		assert_eq!(special, 0);
		assert_eq!(lx, 0x80);
		assert_eq!(ly, 0x80);
		assert_eq!(rx, 0x80);
		assert_eq!(ry, 0x80);
	}

	#[test]
	fn ds4_report_faces() {
		let mut st = GamepadState::default();
		// A (Cross) → bit 5.
		st.set(button::A, true);
		let (buttons, _, _, _, _, _) = ds4_report_fields(&st);
		assert_ne!(buttons & (1 << 5), 0, "Cross (bit5) should be set");
		assert_eq!(buttons & (1 << 7), 0, "Triangle (bit7) should be clear");

		// Y (Triangle) → bit 7.
		let mut st2 = GamepadState::default();
		st2.set(button::Y, true);
		let (buttons2, _, _, _, _, _) = ds4_report_fields(&st2);
		assert_ne!(buttons2 & (1 << 7), 0, "Triangle (bit7) should be set");
		assert_eq!(buttons2 & (1 << 5), 0, "Cross (bit5) should be clear");
	}

	#[test]
	fn ds4_report_dpad_diagonals() {
		// UP + RIGHT → NE = nibble 1.
		let mut st = GamepadState::default();
		st.set(button::DPAD_UP | button::DPAD_RIGHT, true);
		let (buttons, _, _, _, _, _) = ds4_report_fields(&st);
		assert_eq!(buttons & 0x000F, 1, "UP+RIGHT should give NE (1)");

		// DOWN + LEFT → SW = nibble 5.
		let mut st2 = GamepadState::default();
		st2.set(button::DPAD_DOWN | button::DPAD_LEFT, true);
		let (buttons2, _, _, _, _, _) = ds4_report_fields(&st2);
		assert_eq!(buttons2 & 0x000F, 5, "DOWN+LEFT should give SW (5)");

		// LEFT only → W = nibble 6.
		let mut st3 = GamepadState::default();
		st3.set(button::DPAD_LEFT, true);
		let (buttons3, _, _, _, _, _) = ds4_report_fields(&st3);
		assert_eq!(buttons3 & 0x000F, 6, "LEFT only should give W (6)");
	}

	#[test]
	fn ds4_report_sticks() {
		// left_x = i16::MAX → ~0xFF (near 255).
		let mut st = GamepadState::default();
		st.left_x = i16::MAX;
		let (_, _, lx, _, _, _) = ds4_report_fields(&st);
		assert!(lx >= 254, "i16::MAX left_x should map to ~0xFF, got {lx}");

		// left_x = i16::MIN → 0x00 (or very close).
		let mut st2 = GamepadState::default();
		st2.left_x = i16::MIN;
		let (_, _, lx2, _, _, _) = ds4_report_fields(&st2);
		assert!(lx2 <= 1, "i16::MIN left_x should map to ~0x00, got {lx2}");

		// left_x = 0 → 0x80.
		let st3 = GamepadState::default();
		let (_, _, lx3, _, _, _) = ds4_report_fields(&st3);
		assert_eq!(lx3, 0x80, "zero left_x should map to 0x80 (center)");
	}

	#[test]
	fn ds4_report_triggers_as_bits() {
		let mut st = GamepadState::default();
		st.left_trigger = 200;
		let (buttons, _, _, _, _, _) = ds4_report_fields(&st);
		// bit 10 = L2-as-bit.
		assert_ne!(
			buttons & (1 << 10),
			0,
			"left_trigger > 0 should set L2-as-bit (bit 10)"
		);
		// R2 bit should be clear.
		assert_eq!(buttons & (1 << 11), 0, "R2-as-bit (bit 11) should be clear");
	}
}
