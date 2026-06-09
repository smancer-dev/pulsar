//! DXGI Desktop Duplication capture — the Sunshine/Parsec frame-pacing source.
//!
//! Owns the `ID3D11Device` (Strategy A: capture + encode share ONE device — see the
//! crate plan). `CaptureDevice::create` enumerates the adapter/output that actually
//! *owns the desktop* (the iGPU on a hybrid laptop), spins up a D3D11 device on it,
//! and `DuplicateOutput`s the requested monitor. `run` is the integer-exact pacing
//! loop: it sleeps with a HIGH-RESOLUTION waitable timer to the client's frame
//! interval and `AcquireNextFrame(0)`s — reusing the last surface on a timeout so the
//! encoder always sees a steady cadence (DXGI only delivers on screen *change*).
//!
//! All of this is unsafe Win32 FFI; the load-bearing / foot-gun lines are commented.
//! The whole module is Windows-only; `lib.rs` gates the platform.
#![cfg(windows)]

mod cursor;
mod cursor_shape;
mod device;
mod pacing;
pub(crate) mod platform;

// Re-export the public API at the original `crate::dxgi::*` paths. `lib.rs` uses
// `dxgi::CaptureDevice`; the platform helpers (`raise_thread_priority`,
// `DisplayKeepAlive`) keep their original `pub` reachability here too.
pub use device::CaptureDevice;
pub use platform::{raise_thread_priority, DisplayKeepAlive};
