//! Windows WH_KEYBOARD_LL keyboard hook for the Pulsar *client*.
//!
//! While the user is controlling a remote session, this captures EVERY key
//! (including OS-reserved ones the webview never sees — Win, Alt+Tab, Ctrl+Esc,
//! media keys) *before* Windows handles them, forwards each as an
//! `InputEvent::Key` to the active play session, and returns 1 to suppress it
//! locally. The hook is only armed while controlling, so the local desktop is
//! completely unaffected otherwise.
//!
//! The LL callback must be a plain `extern "system" fn` with NO captured state,
//! so the active sender + app handle live in process-global statics. The hook
//! also must run on a thread with a Windows message pump, so we own a dedicated
//! thread that installs the hook, pumps `GetMessageW`, and uninstalls on quit.

use pulsar_core::service::InputEvent;
use tauri::AppHandle;

// macOS (and any non-Windows, non-Linux target): no client-side capture yet.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn enable(_app: AppHandle, _tx: tokio::sync::mpsc::Sender<InputEvent>, _mouse: bool) {}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn disable() {}
// No grab to release on these targets (the webview floats over the live video).
#[cfg(not(any(windows, target_os = "linux")))]
pub fn overlay_suspend(_suspend: bool) {}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn set_focused(_focused: bool) {}

#[cfg(windows)]
mod imp;
#[cfg(windows)]
pub use imp::{disable, enable, overlay_suspend, set_focused};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{disable, enable, overlay_suspend, set_focused};
