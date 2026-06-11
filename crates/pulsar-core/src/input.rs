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
	axis_to_i16, button, create_virtual_pad, trigger_to_u8, vid_pid_from_sdl_guid, xinput_buttons,
	ControllerInfo, GamepadKind, GamepadState, RecordingPad, VirtualGamepad,
};

/// Windows host-side virtual gamepad via **ViGEmBus** — see [`vigem::ViGEmGamepad`].
#[cfg(windows)]
mod vigem;

/// Linux host-side uinput backends (virtual gamepad + desktop input).
#[cfg(target_os = "linux")]
mod uinput;

/// Windows host-side mouse + keyboard injection via `SendInput`.
#[cfg(windows)]
mod windows;

/// No-op desktop-input stub for platforms without a real backend.
#[cfg(not(any(target_os = "linux", windows)))]
mod desktop_stub;

/// Host-side mouse + keyboard injection for remote control. Linux uses uinput
/// (works on Wayland and X11), Windows uses the Win32 `SendInput` API (the same
/// user-mode approach Parsec uses for desktop control); other platforms are
/// no-op stubs for now.
#[cfg(target_os = "linux")]
pub use uinput::DesktopInput;

#[cfg(windows)]
pub use windows::DesktopInput;

#[cfg(not(any(target_os = "linux", windows)))]
pub use desktop_stub::DesktopInput;

pub use hub::ControllerHub;

/// Live controller reading via `gilrs`. Always compiled (the dep is small), but
/// only useful where physical controllers exist.
pub mod hub;
