//! Extended input commands (W2-inputcmds).
//!
//! Mirrors the existing `send_pointer` / `send_button` (absolute pointer) in
//! `client.rs` with the additional input event variants needed by the mobile UI:
//!
//! | Command | `InputEvent` variant |
//! | --- | --- |
//! | `send_scroll` | `Scroll { dx, dy }` |
//! | `send_key` | `Key { code, down }` |
//! | `send_char` | `Char(char)` |
//! | `send_pointer_rel` | `PointerRelative { dx, dy }` |
//!
//! The gamepad commands (`send_gamepad`, `send_gamepad_disconnect`) were added in
//! W4-rust-input and are implemented in this file.
//!
//! All commands look up the per-slot `SessionSender` from `InputSenders` (managed
//! by `client.rs`) and forward the event via `send_input_via`, exactly as
//! `send_pointer` and `send_button` do.

use tauri::{AppHandle, Manager, Runtime};

use pulsar_core::input::{EmulationTarget, GamepadKind, GamepadState};
use pulsar_core::service::{send_input_via, InputEvent};

use crate::client::InputSenders;

/// Send a smooth scroll delta to session `slot`'s host.
///
/// JS: `invoke('send_scroll', { slot: 0, dx: 0.0, dy: 1.5 })`
#[tauri::command]
pub async fn send_scroll<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &InputEvent::Scroll { dx, dy }).await;
    }
    Ok(())
}

/// Send a keyboard evdev key-press or key-release to session `slot`'s host.
///
/// `code` is the Linux evdev keycode (as used in `keymap.ts` / `KEY_*` constants).
///
/// JS: `invoke('send_key', { slot: 0, code: 28, down: true })`
#[tauri::command]
pub async fn send_key<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    code: u32,
    down: bool,
) -> Result<(), String> {
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &InputEvent::Key { code, down }).await;
    }
    Ok(())
}

/// Send a resolved Unicode character to session `slot`'s host.
///
/// `ch` is a single-character string (first char is used); the host inserts it
/// verbatim regardless of its own active keyboard layout.
///
/// JS: `invoke('send_char', { slot: 0, ch: 'a' })`
#[tauri::command]
pub async fn send_char<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    ch: String,
) -> Result<(), String> {
    let c = ch.chars().next().ok_or("empty char")?;
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &InputEvent::Char(c)).await;
    }
    Ok(())
}

/// Send a relative pointer movement delta (raw mouse / trackpad delta) to session
/// `slot`'s host.  Used by the trackpad mode in the touch input engine.
///
/// JS: `invoke('send_pointer_rel', { slot: 0, dx: 3.5, dy: -1.2 })`
#[tauri::command]
pub async fn send_pointer_rel<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &InputEvent::PointerRelative { dx, dy }).await;
    }
    Ok(())
}

/// Send an on-screen gamepad state snapshot to session `slot`'s host.
///
/// `slot` is the SESSION slot (which host to route to — the key into
/// `InputSenders`); `pad_idx` is the per-session PAD index the host addresses
/// (0-based player slot — the key into the host's virtual-pad map, see
/// `InputEvent::GamepadSlot`). These are distinct: one phone session can drive
/// several pads (on-screen = pad 0, physical pads = 1, 2, …), and conflating them
/// would look the sender up under the pad index and silently drop input whenever
/// `pad_idx != slot`.
///
/// The virtual controller is presented to the host as an Xbox-family pad
/// (`kind: Xbox`) with `target: Auto` so the host resolves the concrete
/// backend (ViGEm Xbox360 on Windows, uinput on Linux) through the normal
/// `EmulationTarget::resolve` path.
///
/// Axis conventions (matching `GamepadState`):
/// - `lx` / `rx`: left/right stick X, full-range `i16` (negative = left, positive = right).
/// - `ly` / `ry`: left/right stick Y, **UP-positive** full-range `i16`
///   (positive = up, negative = down — the host's DS4/XInput converters invert Y
///   internally where required, e.g. `ds4_report_fields`).
/// - `lt` / `rt`: left/right trigger, `u8` 0..=255.
/// - `buttons`: XInput-style bitmask (`u16` from JS; widened to `u32` for `GamepadState`).
///
/// JS: `invoke('send_gamepad', { slot: 0, padIdx: 0, buttons: 0x1000, lx: 0, ly: 0, rx: 0, ry: 0, lt: 0, rt: 128 })`
#[tauri::command]
pub async fn send_gamepad<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    pad_idx: u8,
    buttons: u16,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
    lt: u8,
    rt: u8,
    // Desired host emulation: "xbox" | "ds4" | else Auto (resolves Xbox360 for our
    // XInput-format input). Lets the user pick what the controller is emulated as.
    target: Option<String>,
) -> Result<(), String> {
    let state = GamepadState {
        buttons: buttons as u32,
        left_x: lx,
        left_y: ly,
        right_x: rx,
        right_y: ry,
        left_trigger: lt,
        right_trigger: rt,
    };
    let target = match target.as_deref() {
        Some("xbox") => EmulationTarget::Xbox360,
        Some("ds4") => EmulationTarget::Ds4,
        _ => EmulationTarget::Auto,
    };
    let event = InputEvent::GamepadSlot {
        slot: pad_idx,
        kind: GamepadKind::Xbox,
        target,
        state,
    };
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &event).await;
    }
    Ok(())
}

/// Notify the host that the pad `pad_idx` on session `slot` has been disconnected
/// (user closed the gamepad overlay, unplugged a physical pad, or ended the
/// session). `slot` = session (sender lookup); `pad_idx` = host-side pad index —
/// see [`send_gamepad`].
///
/// The host releases the corresponding virtual pad so games see it as unplugged.
///
/// JS: `invoke('send_gamepad_disconnect', { slot: 0, padIdx: 0 })`
#[tauri::command]
pub async fn send_gamepad_disconnect<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    pad_idx: u8,
) -> Result<(), String> {
    let event = InputEvent::GamepadDisconnect { slot: pad_idx };
    let s = app.state::<InputSenders>().0.lock().unwrap().get(&slot).cloned();
    if let Some(s) = s {
        let _ = send_input_via(&s, &event).await;
    }
    Ok(())
}
