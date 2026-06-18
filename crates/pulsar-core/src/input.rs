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

mod types;

pub use types::{
	axis_to_i16, button, create_virtual_pad, create_virtual_pad_target, ds4_report_fields,
	touch_to_delta, trigger_to_u8, vid_pid_from_sdl_guid, xinput_buttons, ControllerInfo,
	EmulationTarget, GamepadKind, GamepadState, RecordingPad, ResolvedTarget, RumbleReader,
	VirtualGamepad,
};

/// Windows host-side virtual gamepad via **ViGEmBus** — see [`vigem::ViGEmGamepad`].
#[cfg(windows)]
mod vigem;

/// Linux host-side uinput backends (virtual gamepad + desktop input).
#[cfg(target_os = "linux")]
mod uinput;

/// DS4/DS5 touchpad-as-mouse reader via evdev (Linux client-side).
///
/// Enumerates `/dev/input/event*` for a Sony touchpad device and synthesizes
/// `PointerRelative` / `PointerButton` [`InputEvent`]s from raw ABS_MT / BTN
/// events. Follow-ups: Windows (raw HID via hidapi) and macOS (IOHIDManager).
#[cfg(target_os = "linux")]
pub mod touchpad_linux;

/// Windows host-side mouse + keyboard injection via `SendInput`.
#[cfg(windows)]
mod windows;

/// macOS host-side mouse + keyboard injection via CoreGraphics `CGEvent`.
#[cfg(target_os = "macos")]
mod macos;

/// No-op desktop-input stub for platforms without a real backend.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod desktop_stub;

/// Host-side mouse + keyboard injection for remote control. Linux uses uinput
/// (works on Wayland and X11), Windows uses the Win32 `SendInput` API, and macOS
/// uses CoreGraphics `CGEvent` (the same user-mode approach Parsec uses for desktop
/// control on each OS); any remaining platform is a no-op stub.
#[cfg(target_os = "linux")]
pub use uinput::DesktopInput;

#[cfg(windows)]
pub use windows::{DesktopInput, MonitorRect};

#[cfg(target_os = "macos")]
pub use macos::DesktopInput;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub use desktop_stub::DesktopInput;

// Controller reading + rumble now live in the app crate via SDL3 (sdl3-sys) —
// see desktop-app/src-tauri/src/controllers.rs. gilrs is gone (one library, the
// Moonlight model, and it rumbles pads with no evdev EV_FF). pulsar-core keeps only
// the shared data types (GamepadState/Kind, the VirtualGamepad host trait, helpers).
