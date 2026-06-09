//! Linux (evdev) capture implementation.
//!
//! On Linux the embedded native renderer (mpv inside the app window) covers the webview,
//! so the webview's JS input handlers never see the keyboard/mouse. Instead we grab the
//! local input devices via evdev (EVIOCGRAB — like the Windows Interception path) and
//! forward every event to the host as an `InputEvent`. Ctrl+Shift+F12 emits `kbd-leave`
//! (the UI then ends the session, which calls `disable()` → the grab is released).

use super::*;
use evdev::{InputEventKind, Key, RelativeAxisType};
use xkbcommon::xkb;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;
use tokio::sync::mpsc::Sender;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// True while the Pulsar window is focused. Capture (grab + forward + the overlay/leave
/// combos) is active ONLY when focused — the evdev grab is global, so an unfocused window
/// must not steal input or fire combos. Driven by `set_focused()` (Tauri focus event).
static FOCUSED: AtomicBool = AtomicBool::new(true);
/// While true the capture thread releases every EVIOCGRAB hold and STOPS
/// forwarding, but keeps its fds/poll/device list alive so the overlay can
/// re-grab instantly on close. Driven by `overlay_suspend()` (called from the
/// `set_overlay` command), NOT by the leave combo — the session stays alive.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// Build an xkb keyboard state from the X session's ACTIVE layout (e.g. Turkish-Q), so a grabbed
/// evdev keycode can be resolved to the Unicode char that layout produces — enabling
/// layout-independent (WYSIWYG) forwarding (the host then types the exact char via
/// KEYEVENTF_UNICODE, regardless of ITS layout). Reads the layout via `setxkbmap -query` (the
/// kbdhook thread runs with DISPLAY=:0). Returns None on any failure → the caller falls back to
/// the raw-keycode (VK) path (old behavior). v1 resolves layout group 0; live multi-layout group
/// tracking (a `tr,us` toggle) is a future xkbcommon-x11 upgrade.
fn build_xkb_state() -> Option<xkb::State> {
	let out = std::process::Command::new("setxkbmap")
		.arg("-query")
		.output()
		.ok()?;
	let text = String::from_utf8_lossy(&out.stdout);
	let (mut layout, mut variant) = (String::new(), String::new());
	for line in text.lines() {
		if let Some(v) = line.strip_prefix("layout:") {
			layout = v.trim().to_string();
		} else if let Some(v) = line.strip_prefix("variant:") {
			variant = v.trim().to_string();
		}
	}
	if layout.is_empty() {
		layout = "us".to_string();
	}
	let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
	let keymap = xkb::Keymap::new_from_names(
		&ctx,
		"",
		"",
		&layout,
		&variant,
		None,
		xkb::KEYMAP_COMPILE_NO_FLAGS,
	)?;
	Some(xkb::State::new(&keymap))
}

/// Forward one input event to the session.
fn fwd(tx: &Sender<InputEvent>, ev: InputEvent) {
	let _ = tx.try_send(ev);
}

// evdev keycodes for the leave combo + the modifiers it needs.
const KEY_LEFTCTRL: u16 = 29;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_F12: u16 = 88;
const KEY_Q: u16 = 16; // reliable leave key (media-mode keyboards don't emit KEY_F12)
const KEY_M: u16 = 50; // overlay-toggle combo Ctrl+Shift+M (distinct from leave Q/F12)

/// Keep a device if it's a keyboard (has letter/escape keys) or, when `mouse`, a mouse
/// (relative axes + a left button). Gamepads (read separately via gilrs) are skipped so
/// we don't fight that path.
fn wanted(dev: &evdev::Device, mouse: bool) -> bool {
	let keys = dev.supported_keys();
	// BTN_SOUTH (== BTN_GAMEPAD) marks a gamepad — those go through gilrs, skip them.
	if keys.map_or(false, |k| k.contains(Key::BTN_SOUTH)) {
		return false;
	}
	let is_kbd = keys.map_or(false, |k| k.contains(Key::KEY_A) || k.contains(Key::KEY_ESC));
	let is_mouse = mouse
		&& dev
			.supported_relative_axes()
			.map_or(false, |r| r.contains(RelativeAxisType::REL_X))
		&& keys.map_or(false, |k| k.contains(Key::BTN_LEFT));
	is_kbd || is_mouse
}

/// Authoritative, live state of the chord modifiers — read straight from the kernel
/// (`EVIOCGKEY`) for every grabbed device, NOT from the `ctrl`/`shift` booleans the
/// capture loop tracks as events stream by. Those booleans desync (a `SYN_DROPPED`
/// burst under heavy gameplay input loses a modifier down/up, a device grabbed while a
/// modifier is already held never saw its press, the overlay-suspend reset zeroes them) —
/// and once `ctrl` drifts to false EVERY later Ctrl+Shift+M / Ctrl+Shift+Q silently fails
/// until a fresh Ctrl-down happens to resync it. That intermittency is exactly the bug.
/// Querying the kernel at the trigger moment is immune to all of it. OR across devices so
/// any Ctrl + any Shift on any keyboard counts (Parsec/Moonlight semantics); works on
/// ungrabbed-but-open fds too, so the close combo is detected while the overlay suspends.
fn chord_mods(devs: &[evdev::Device]) -> (bool, bool) {
	let (mut ctrl, mut shift) = (false, false);
	for d in devs {
		if let Ok(st) = d.get_key_state() {
			ctrl |= st.contains(Key::KEY_LEFTCTRL) || st.contains(Key::KEY_RIGHTCTRL);
			shift |= st.contains(Key::KEY_LEFTSHIFT) || st.contains(Key::KEY_RIGHTSHIFT);
		}
	}
	(ctrl, shift)
}

pub fn enable(app: AppHandle, tx: Sender<InputEvent>, mouse: bool) {
	if RUNNING.swap(true, Ordering::SeqCst) {
		return; // already armed
	}
	// A fresh session never starts suspended (a prior overlay close may have
	// raced teardown). The capture thread re-grabs from a clean state below.
	SUSPENDED.store(false, Ordering::SeqCst);
	std::thread::spawn(move || {
		let mut grabbed: Vec<evdev::Device> = Vec::new();
		let mut grabbed_paths: std::collections::HashSet<std::path::PathBuf> =
			std::collections::HashSet::new();
		let mut pfds: Vec<libc::pollfd> = Vec::new();
		let (mut ctrl, mut shift) = (false, false);
		// Robust escape: 3 quick Right-Ctrl taps (no chord) — Ctrl+Shift+Q can be unreliable
		// on multi-device grabs. Tracks recent RightCtrl press times; 3 within 1s → leave.
		let mut rctrl_taps: Vec<std::time::Instant> = Vec::new();
		// Last suspend state we actually applied to the grabs. When SUSPENDED
		// flips we ungrab (release EVIOCGRAB, keep fds) / re-grab every device —
		// one ioctl per device, no re-enumerate (contrast the leave path which
		// drops `grabbed` and closes fds).
		let mut applied_suspend = false;
		// Shortcut modifiers (Ctrl/LeftAlt/Win): when ANY is held a key is a shortcut → forward the
		// raw keycode (VK path) so Ctrl+C etc. work. RightAlt (AltGr=100) is deliberately NOT here —
		// it's a char-composition modifier (Turkish AltGr chars), consumed by the xkb resolution.
		let (mut lalt, mut win) = (false, false);
		// Keycodes currently down that were forwarded as a resolved `Char`; their key-UP is
		// suppressed (a Unicode insert is one-shot — there is no VK to release).
		let mut char_keys: std::collections::HashSet<u16> = std::collections::HashSet::new();
		// Active-layout keyboard state for WYSIWYG char resolution (None → raw-keycode VK fallback).
		let mut xkb_state = build_xkb_state();
		// Re-enumerate ~once a second to grab a newly plugged / KVM-switched device. This
		// MUST be WALL-CLOCK based, not per-iteration: poll() below returns immediately while
		// input is flowing, so an iteration counter fired the rescan continuously during
		// active use — and evdev::enumerate() (which opens every /dev/input node) then blocked
		// the loop, making forwarded mouse/keys arrive in stuttering bursts (the "input lag /
		// jerky" symptom). Time-based → at most one ~once-a-second blip.
		let mut last_rescan: Option<std::time::Instant> = None;
		// Every /dev/input/event* node we've already evaluated. The rescan below first does a
		// CHEAP read_dir against this set and only runs the EXPENSIVE evdev::enumerate() when a
		// genuinely new node appears (real hotplug) — see the comment at the gate.
		let mut seen_nodes: std::collections::HashSet<std::path::PathBuf> =
			std::collections::HashSet::new();
		while RUNNING.load(Ordering::SeqCst) {
			if last_rescan.map_or(true, |t| t.elapsed() >= std::time::Duration::from_secs(1)) {
				last_rescan = Some(std::time::Instant::now());
				let before = grabbed.len();
				// CHEAP gate: list event nodes (read_dir, NO device open). The EXPENSIVE
				// evdev::enumerate() OPENS all ~10 /dev/input nodes + queries caps; running it
				// every second STALLED this poll loop ~1×/s, so buffered mouse motion drained on
				// the next poll as one big accumulated PointerRelative = a periodic cursor "jump"
				// (the residual after the video pacing fix). Only enumerate when a NEW node
				// appears (real hotplug); steady state = read_dir only → no blip, no jump.
				let cur_nodes: std::collections::HashSet<std::path::PathBuf> =
					std::fs::read_dir("/dev/input")
						.into_iter()
						.flatten()
						.flatten()
						.map(|e| e.path())
						.filter(|p| {
							p.file_name()
								.and_then(|n| n.to_str())
								.map_or(false, |n| n.starts_with("event"))
						})
						.collect();
				let new_devs: Vec<(std::path::PathBuf, evdev::Device)> =
					if cur_nodes.is_subset(&seen_nodes) {
						Vec::new()
					} else {
						seen_nodes.extend(cur_nodes);
						evdev::enumerate().collect()
					};
				for (path, mut d) in new_devs {
					if grabbed_paths.contains(&path) || !wanted(&d, mouse) {
						continue;
					}
					// Non-blocking so fetch_events never wedges the poll loop.
					unsafe {
						let fd = d.as_raw_fd();
						let fl = libc::fcntl(fd, libc::F_GETFL);
						libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
					}
					if d.grab().is_ok() {
						// Hotplugged mid-overlay: keep the fd (for poll) but release
						// the grab so the local OS keeps driving the menu.
						if SUSPENDED.load(Ordering::SeqCst) {
							let _ = d.ungrab();
						}
						grabbed_paths.insert(path);
						grabbed.push(d);
					}
				}
				if grabbed.len() != before {
					pfds = grabbed
						.iter()
						.map(|d| libc::pollfd {
							fd: d.as_raw_fd(),
							events: libc::POLLIN,
							revents: 0,
						})
						.collect();
				}
			}

			// Apply overlay suspend/resume transitions. On suspend we release every
			// EVIOCGRAB (so the local OS + webview drive the overlay menu) but keep
			// the fds/poll/device list alive; on resume we re-grab. Flush synthetic
			// key-ups for the held modifiers + M BEFORE ungrabbing so the host never
			// sees a stuck key. Newly-rescanned devices grabbed above are ungrabbed
			// here too if we're currently suspended.
			// Release the grab (and stop forwarding) when the overlay is open OR the Pulsar
			// window is NOT focused — the evdev grab is global, so without the focus gate
			// the user couldn't use other apps and the combos fired regardless of focus.
			let want_suspend =
				SUSPENDED.load(Ordering::SeqCst) || !FOCUSED.load(Ordering::SeqCst);
			if want_suspend != applied_suspend {
				if want_suspend {
					// Release any keys the host is still holding for this combo.
					for code in [
						KEY_LEFTCTRL,
						KEY_RIGHTCTRL,
						KEY_LEFTSHIFT,
						KEY_RIGHTSHIFT,
						KEY_M,
					] {
						let _ = tx.try_send(InputEvent::Key {
							code: code as u32,
							down: false,
						});
					}
					ctrl = false;
					shift = false;
					for d in grabbed.iter_mut() {
						let _ = d.ungrab();
					}
				} else {
					for d in grabbed.iter_mut() {
						let _ = d.grab();
					}
				}
				applied_suspend = want_suspend;
			}

			if pfds.is_empty() {
				std::thread::sleep(std::time::Duration::from_millis(200));
				continue;
			}
			let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, 200) };
			if n <= 0 {
				continue;
			}
			for i in 0..grabbed.len() {
				if pfds[i].revents & libc::POLLIN == 0 {
					continue;
				}
				pfds[i].revents = 0;
				// Accumulate relative motion within this batch → one PointerRelative.
				let (mut adx, mut ady) = (0f64, 0f64);
				// Collect into an owned Vec so the &mut borrow on grabbed[i] ends HERE.
				// The chord check below reads every grabbed device's live key state
				// (`chord_mods(&grabbed)`), which it can't do while this device is still
				// mutably borrowed by the event iterator. InputEvent is Copy, so this is
				// a cheap shallow copy of the batch.
				let events: Vec<evdev::InputEvent> = match grabbed[i].fetch_events() {
					Ok(e) => e.collect(),
					Err(_) => continue,
				};
				for ev in events {
					match ev.kind() {
						InputEventKind::Key(key) => {
							let code = key.code();
							let down = ev.value() != 0; // 1 down / 2 repeat → down
							match code {
									KEY_LEFTCTRL | KEY_RIGHTCTRL => ctrl = down,
									KEY_LEFTSHIFT | KEY_RIGHTSHIFT => shift = down,
									56 => lalt = down, // KEY_LEFTALT (RightAlt=100 = AltGr, a char modifier, not a shortcut)
									125 | 126 => win = down, // KEY_LEFTMETA / KEY_RIGHTMETA
									_ => {}
								}
							// Robust escape: 3 quick Right-Ctrl taps within 1s (no chord needed).
							if code == KEY_RIGHTCTRL
								&& ev.value() == 1
								&& FOCUSED.load(Ordering::SeqCst)
							{
								let now = std::time::Instant::now();
								rctrl_taps.retain(|t| {
									now.duration_since(*t) < std::time::Duration::from_millis(1000)
								});
								rctrl_taps.push(now);
								if rctrl_taps.len() >= 3 {
									let _ = app.emit("kbd-leave", ());
									RUNNING.store(false, Ordering::SeqCst);
									continue;
								}
							}
							// Chord combos on the trigger keys only — overlay-toggle Ctrl+Shift+M
							// (50) and leave Ctrl+Shift+F12 (88) / Ctrl+Shift+Q (16). Gate on
							// the LIVE kernel key state (`chord_mods` over every grabbed device),
							// falling back to the tracked booleans only if that ioctl ever
							// fails — the tracked state alone was the source of the "doesn't
							// fire every time" bug (see `chord_mods`). Checked BEFORE the
							// `want_suspend` gate so the overlay can also be CLOSED (and the
							// session left) by combo while the grab is suspended.
							if ev.value() == 1
								&& matches!(code, KEY_M | KEY_F12 | KEY_Q)
								&& FOCUSED.load(Ordering::SeqCst)
							{
								let (lc, ls) = chord_mods(&grabbed);
								let (cmod, smod) = (ctrl || lc, shift || ls);
								if cmod && smod {
									if code == KEY_M {
										// Overlay toggle — does NOT end the session (RUNNING
										// stays true); the ungrab/regrab is driven by
										// set_overlay → overlay_suspend(), keeping grab state
										// owned by this one thread.
										let _ = app.emit("overlay-toggle", ());
									} else if code == KEY_F12 {
										// Fullscreen toggle (Ctrl+Shift+F12) -- does NOT end the
										// session; the frontend listens for `fullscreen-toggle`
										// and flips the Tauri window fullscreen state.
										let _ = app.emit("fullscreen-toggle", ());
									} else {
										// Leave (Ctrl+Shift+Q). Q is the reliable leave key --
										// media-mode keyboards (e.g. Logitech MX Keys) do not
										// emit KEY_F12, and F12 is fullscreen now.
										let _ = app.emit("kbd-leave", ());
										RUNNING.store(false, Ordering::SeqCst);
									}
									continue;
								}
							}
							// While the overlay is open we keep tracking modifiers + the two
							// combos above, but forward NOTHING to the host (the local OS +
							// webview drive the menu) — no phantom input during the overlay.
							if want_suspend {
								continue;
							}
							// Mouse buttons also arrive as EV_KEY (BTN_LEFT/RIGHT/MIDDLE
							// = 272/273/274 → 0/1/2).
							// Keep the xkb state synced with every forwarded key so Shift/AltGr affect the char a
								// printable key resolves to (no-op when xkb is absent).
								if let Some(st) = xkb_state.as_mut() {
									st.update_key(
										xkb::Keycode::new((code as u32) + 8),
										if down { xkb::KeyDirection::Down } else { xkb::KeyDirection::Up },
									);
								}
								match code {
									272 | 273 | 274 => {
										let _ = tx.try_send(InputEvent::PointerButton {
											button: (code - 272) as u8,
											down: ev.value() == 1,
										});
									}
									_ if !down => {
										// key UP: a Char key had a one-shot Unicode insert (no VK to release) -> suppress
										// its up; otherwise release the VK as before.
										if !char_keys.remove(&code) {
											fwd(&tx, InputEvent::Key { code: code as u32, down: false });
										}
									}
									_ => {
										// key DOWN/REPEAT - WYSIWYG: a printable key with NO shortcut modifier (Ctrl/
										// LeftAlt/Win) is resolved via the client's OWN layout to a Unicode char and sent
										// as Char (host types it regardless of ITS layout). A key already in char-mode
										// keeps re-sending Char on autorepeat. Shortcuts, non-text keys (xkb yields no
										// printable) and the no-xkb fallback take the raw-keycode VK path.
										let shortcut = ctrl || lalt || win;
										let ch = if char_keys.contains(&code) || !shortcut {
											xkb_state
												.as_ref()
												.map(|st| st.key_get_utf8(xkb::Keycode::new((code as u32) + 8)))
												.and_then(|s| s.chars().next())
												.filter(|c| !c.is_control())
										} else {
											None
										};
										if let Some(c) = ch {
											char_keys.insert(code);
											fwd(&tx, InputEvent::Char(c));
										} else if !char_keys.contains(&code) {
											fwd(&tx, InputEvent::Key { code: code as u32, down: true });
										}
									}
								}
						}
						// While suspended, drop pointer motion + scroll too (the local OS
						// drives the cursor over the overlay).
						InputEventKind::RelAxis(rel) if !want_suspend => match rel {
							RelativeAxisType::REL_X => adx += ev.value() as f64,
							RelativeAxisType::REL_Y => ady += ev.value() as f64,
							// evdev wheel notch (+1 = up/right); the host scales /100 like a
							// browser wheel delta and inverts dy, so up → dy negative.
							RelativeAxisType::REL_WHEEL => {
								let _ = tx.try_send(InputEvent::Scroll {
									dx: 0.0,
									dy: -(ev.value() as f64) * 100.0,
								});
							}
							RelativeAxisType::REL_HWHEEL => {
								let _ = tx.try_send(InputEvent::Scroll {
									dx: ev.value() as f64 * 100.0,
									dy: 0.0,
								});
							}
							_ => {}
						},
						_ => {}
					}
				}
				if adx != 0.0 || ady != 0.0 {
					fwd(&tx, InputEvent::PointerRelative { dx: adx, dy: ady });
				}
			}
		}
		// `grabbed` drops here → fds close → every EVIOCGRAB is released.
	});
}

pub fn disable() {
	RUNNING.store(false, Ordering::SeqCst);
	// Clear the overlay gate so a teardown mid-overlay leaves clean state and the
	// next session never starts suspended.
	SUSPENDED.store(false, Ordering::SeqCst);
}

/// Open/close the gaming overlay: release (suspend) or restore (resume) the
/// evdev grabs WITHOUT killing the capture thread. Called from the `set_overlay`
/// Tauri command. Sets the cross-thread `SUSPENDED` atomic; the capture thread
/// observes the transition and performs the actual `Device::ungrab()/grab()`
/// (releasing/restoring EVIOCGRAB) on its next loop pass, keeping all grab state
/// owned by that one thread.
pub fn overlay_suspend(suspend: bool) {
	SUSPENDED.store(suspend, Ordering::SeqCst);
}

/// Window focus changed (Tauri `WindowEvent::Focused`). When unfocused the capture thread
/// releases its evdev grab (so other apps work) and ignores all keys incl. the combos;
/// when focused it re-grabs and resumes. Same observe-the-atomic model as `overlay_suspend`.
pub fn set_focused(focused: bool) {
	FOCUSED.store(focused, Ordering::SeqCst);
}
