//! DS4/DS5 touchpad-as-mouse reader for Linux clients.
//!
//! Enumerates `/dev/input/event*` for a Sony (VID `0x054C`) touchpad device
//! — specifically one whose kernel device name contains `"Touchpad"` AND
//! whose USB VID matches Sony, or whose PID matches a known DS4/DS5 PID (see
//! [`crate::input::GamepadKind::from_vid_pid`]). Reads ABS_MT_POSITION_X/Y
//! (falls back to ABS_X/Y) for finger tracking, BTN_TOUCH for finger
//! down/up, and BTN_LEFT for the physical click pad, then synthesizes
//! `InputEvent::PointerRelative` and `InputEvent::PointerButton` events on
//! the provided channel — zero wire changes needed.
//!
//! **Permissions**: the calling process (or its user) must be in the `input`
//! group (or otherwise have read access to `/dev/input/event*`). If the
//! device is absent or not accessible the reader silently exits without
//! panicking or logging errors visible to the user.
//!
//! **Follow-ups (not implemented here)**:
//! - Windows: raw HID via `hidapi` (`hid_read` on the DS4/DS5 HID interface,
//!   touchpad report ID 0x01 for DS4 or 0x31 for DualSense).
//! - macOS: `IOHIDManager` callback on the matching Usage Page/Usage
//!   `GD/Multiaxis Controller`, reading touchpad usage pages.
//!
//! # cfg gate
//! This module is `#[cfg(target_os = "linux")]` — it will not compile (or be
//! linked) on Windows or macOS.

#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use evdev::{AbsoluteAxisType, Device, EventType, InputEventKind, Key};

use crate::input::touch_to_delta;
use crate::service::InputEvent;

/// Sensitivity multiplier applied to raw ABS_MT/ABS_X/Y deltas.
///
/// DS4 touchpad resolution is roughly 1920 × 942 raw units for a ~50 × 25 mm
/// surface. A sens of 0.35 maps ~2.9 raw units to one screen pixel on a 1080p
/// display — a reasonable default that the user can override in Settings later.
const DEFAULT_SENS: f64 = 0.35;

/// Well-known Sony DS4/DS5 USB PIDs for evdev ID matching.
/// Matches the set in [`crate::input::GamepadKind::from_vid_pid`].
const SONY_VID: u16 = 0x054C;
const DS4_PIDS: &[u16] = &[0x05C4, 0x09CC, 0x0BA0];
const DS5_PIDS: &[u16] = &[0x0CE6, 0x0DF2];

/// Find the first `/dev/input/event*` device that looks like a DS4/DS5 touchpad.
///
/// Selection criteria (any one is sufficient):
/// 1. Device name contains `"Touchpad"` (case-insensitive) **and** USB VID is
///    `0x054C` (Sony).
/// 2. USB VID/PID matches any known DS4 or DS5 PID regardless of the name.
///
/// Returns `None` if no matching device is found or if `/dev/input` is
/// unreadable (e.g. missing `input` group membership).
fn find_touchpad() -> Option<Device> {
	let dir = std::fs::read_dir("/dev/input").ok()?;
	for entry in dir.flatten() {
		let path = entry.path();
		// Only consider `event*` nodes.
		if !path
			.file_name()
			.and_then(|n| n.to_str())
			.map(|n| n.starts_with("event"))
			.unwrap_or(false)
		{
			continue;
		}
		let dev = match Device::open(&path) {
			Ok(d) => d,
			Err(_) => continue,
		};
		let name = dev.name().unwrap_or("").to_lowercase();
		let id = dev.input_id();
		let vid = id.vendor();
		let pid = id.product();

		let is_sony = vid == SONY_VID;
		let is_ds4_ds5_pid =
			DS4_PIDS.contains(&pid) || DS5_PIDS.contains(&pid);
		let name_is_touchpad = name.contains("touchpad");

		if (name_is_touchpad && is_sony) || (is_sony && is_ds4_ds5_pid) {
			// Extra guard: the device must support ABS_MT_POSITION_X or ABS_X
			// (not every Sony event node is the touchpad — DS4 exposes several).
			let abs = dev.supported_absolute_axes();
			let has_touch = abs.map_or(false, |a| {
				a.contains(AbsoluteAxisType::ABS_MT_POSITION_X)
					|| a.contains(AbsoluteAxisType::ABS_X)
			});
			if has_touch {
				return Some(dev);
			}
		}
	}
	None
}

/// Spawn a blocking reader thread that forwards DS4/DS5 touchpad events as
/// synthesized [`InputEvent::PointerRelative`] / [`InputEvent::PointerButton`]
/// messages on `tx`.
///
/// # Parameters
/// - `tx`: an `mpsc::Sender<InputEvent>` (the same channel the gamepad reader
///   uses).  The channel is bounded in the caller; `blocking_send` is used so
///   back-pressure is respected.
/// - `running`: the session's shared `AtomicBool`; the loop exits when it
///   becomes `false`.
/// - `sens`: sensitivity multiplier (`None` → `DEFAULT_SENS`).
///
/// If the touchpad device is not found or cannot be opened the function
/// returns immediately (silent no-op) without spawning anything.
pub fn spawn_touchpad_reader(
	tx: tokio::sync::mpsc::Sender<InputEvent>,
	running: Arc<AtomicBool>,
	sens: Option<f64>,
) {
	let sens = sens.unwrap_or(DEFAULT_SENS);
	// Try to find the device upfront; if none is available, no-op silently.
	let mut dev = match find_touchpad() {
		Some(d) => d,
		None => {
			tracing::debug!("touchpad_linux: no Sony DS4/DS5 touchpad found — reader not started");
			return;
		}
	};
	tracing::info!(
		name = dev.name().unwrap_or("<unknown>"),
		"touchpad_linux: DS4/DS5 touchpad found, starting reader"
	);

	std::thread::spawn(move || {
		// Previous finger position (raw ABS_MT / ABS_X/Y coordinates).
		let mut prev: Option<(i32, i32)> = None;
		// In-progress finger position accumulated across the current EV_SYN batch.
		let mut cur_x: Option<i32> = None;
		let mut cur_y: Option<i32> = None;
		// Whether a finger is currently touching the pad.
		let mut finger_down = false;

		'outer: while running.load(Ordering::SeqCst) {
			// `fetch_events` blocks until at least one event is available.
			// On permission error or device removal it returns Err — exit cleanly.
			let events = match dev.fetch_events() {
				Ok(ev) => ev,
				Err(e) => {
					tracing::debug!("touchpad_linux: fetch_events error ({e}) — reader exiting");
					break 'outer;
				}
			};
			for ev in events {
				if !running.load(Ordering::SeqCst) {
					break 'outer;
				}
				match ev.kind() {
					// Finger position (multi-touch slot 0; we track one finger).
					InputEventKind::AbsAxis(AbsoluteAxisType::ABS_MT_POSITION_X) => {
						cur_x = Some(ev.value());
					}
					InputEventKind::AbsAxis(AbsoluteAxisType::ABS_MT_POSITION_Y) => {
						cur_y = Some(ev.value());
					}
					// Fall back to single-touch ABS_X / ABS_Y.
					InputEventKind::AbsAxis(AbsoluteAxisType::ABS_X) => {
						cur_x = cur_x.or(Some(ev.value()));
					}
					InputEventKind::AbsAxis(AbsoluteAxisType::ABS_Y) => {
						cur_y = cur_y.or(Some(ev.value()));
					}
					// BTN_TOUCH: finger placed / lifted.
					InputEventKind::Key(Key::BTN_TOUCH) => {
						if ev.value() == 0 {
							// Lift — reset tracking so there's no jump on the next touch.
							finger_down = false;
							prev = None;
						} else {
							finger_down = true;
						}
					}
					// BTN_LEFT (physical click pad) → left mouse button.
					InputEventKind::Key(Key::BTN_LEFT) => {
						let down = ev.value() != 0;
						let _ = tx.blocking_send(InputEvent::PointerButton {
							button: 0,
							down,
						});
					}
					// EV_SYN / SYN_REPORT: commit the accumulated position.
					InputEventKind::Synchronization(_) => {
						if finger_down {
							if let (Some(x), Some(y)) = (cur_x, cur_y) {
								if let Some(p) = prev {
									let (dx, dy) = touch_to_delta(p, (x, y), sens);
									if dx != 0.0 || dy != 0.0 {
										let _ = tx.blocking_send(
											InputEvent::PointerRelative { dx, dy },
										);
									}
								}
								prev = Some((x, y));
							}
						}
						// Reset per-SYN accumulators; keep prev for next delta.
						cur_x = None;
						cur_y = None;
					}
					_ => {}
				}
			}
		}
		tracing::debug!("touchpad_linux: reader thread exiting");
	});
}
