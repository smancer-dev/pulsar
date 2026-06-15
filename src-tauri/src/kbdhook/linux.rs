//! Linux (evdev) capture implementation.
//!
//! On Linux the embedded native renderer (mpv inside the app window) covers the webview,
//! so the webview's JS input handlers never see the keyboard/mouse. Instead we grab the
//! local input devices via evdev (EVIOCGRAB — like the Windows Interception path) and
//! forward every event to the host as an `InputEvent`. Capture is CLICK-TO-ENGAGE: the
//! thread arms disengaged (no grab) and only takes the devices after the user clicks the
//! session video (BTN_LEFT while focused, or the standalone render window's `ov engage`).
//! Ctrl+Shift+Q emits `kbd-leave` (the UI ENDS the session); 3×RightCtrl emits
//! `kbd-released` and only DISENGAGES (grab released, thread stays alive, the next video
//! click re-engages). Full teardown is `disable()` (session end).

use super::*;
use evdev::{InputEventKind, Key, RelativeAxisType};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::Emitter;
use tokio::sync::mpsc::Sender;
use xkbcommon::xkb;

/// Capture-thread generation. `enable()` bumps this and hands the new value to the
/// thread it spawns; the thread loops only while it is still the NEWEST generation,
/// and `disable()` bumps again. A plain running-bool can't do this: a disable()→
/// enable() inside one ~200 ms poll interval (exactly what tab-switching between two
/// native sessions does) revived the flag before the old thread observed false,
/// leaving TWO threads sharing the ENGAGED/SUSPENDED statics and racing EVIOCGRAB —
/// with events draining into the dead (or the WRONG host's) tx. With a generation,
/// every superseded thread exits deterministically; only the newest tx survives.
static GEN: AtomicU64 = AtomicU64::new(0);
/// True while a capture thread is armed (session live) — gates `engage()`.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// One-shot kiosk auto-engage: armed by `lib.rs` only for a launch that will actually
/// auto-connect (CLI `--connect` present and no `.skip-autoconnect` relaunch marker),
/// consumed by the first `enable()`. Keying engage off `AUTO_CONNECT` directly was
/// wrong — `app.restart()` preserves argv, so it stays `Some` for every later MANUAL
/// session in the process, which then started with the keyboard/mouse grabbed.
static KIOSK_ENGAGE: AtomicBool = AtomicBool::new(false);
/// True for the LIFETIME of a kiosk-started session (cleared on `disable()`). A kiosk
/// appliance has no local desktop to protect, so capture RE-engages whenever focus
/// returns — without this, GNOME's focus-stealing prevention left the auto-launched
/// window unfocused at startup, the 200 ms debounce below cleared the one-shot
/// engage, and the "auto-connect controls immediately" promise silently broke.
static KIOSK_SESSION: AtomicBool = AtomicBool::new(false);
/// True while the Pulsar window is focused. Capture (grab + forward + the overlay/leave
/// combos) is active ONLY when focused — the evdev grab is global, so an unfocused window
/// must not steal input or fire combos. Driven by `set_focused()` (Tauri focus event).
static FOCUSED: AtomicBool = AtomicBool::new(true);
/// While true the capture thread releases every EVIOCGRAB hold and STOPS
/// forwarding, but keeps its fds/poll/device list alive so the overlay can
/// re-grab instantly on close. Driven by `overlay_suspend()` (called from the
/// `set_overlay` command), NOT by the leave combo — the session stays alive.
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// The play id the capture is currently armed for — lets a re-arm of the SAME live
/// session (effect re-run / hotplug restart) preserve ENGAGED instead of silently
/// disengaging the user mid-control. Crucially this is NOT reset by `disable()`: the
/// real tab deactivate→reactivate path is Session.svelte's $effect cleanup calling
/// kbdCaptureStop()→disable() and THEN the re-run calling enable(), so the engagement
/// memory has to survive a disable() for `same_session` to ever be true (see ENGAGED_MEMO).
static LAST_PLAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
/// Engagement intent for `LAST_PLAY`, remembered ACROSS `disable()`. disable() clears the
/// live ENGAGED (the grab must drop on a stop), but a stop that is immediately followed by
/// an enable() for the SAME id (the tab-switch churn above) has to restore the user's
/// engagement — otherwise the recently-added same-session preservation is a no-op for its
/// only real trigger. A genuinely new session (different id) ignores this and starts
/// disengaged (click-to-engage).
static ENGAGED_MEMO: AtomicBool = AtomicBool::new(false);
/// Click-to-engage gate: the grab is held ONLY while engaged. Sessions start
/// DISENGAGED (except CLI `--connect` kiosk starts) so the user keeps their own
/// keyboard/mouse until they explicitly click the session video. Set by a
/// BTN_LEFT press while focused (embedded video = the whole app window) or by
/// the standalone render window's `ov engage` stdout line (`engage()`); cleared
/// by the leave combos and whenever focus is lost.
static ENGAGED: AtomicBool = AtomicBool::new(false);
/// Set when the user EXPLICITLY detaches (Ctrl+Alt+Z / 3×RightCtrl) so a KIOSK session's
/// focus-driven auto-RE-engage (see KIOSK_SESSION) does NOT instantly revert it — otherwise the
/// appliance grabbed the keyboard straight back on the next loop and the detach combo "did
/// nothing". Cleared when the user re-engages by clicking the video (`engage`) or on a fresh arm.
static MANUAL_DISENGAGE: AtomicBool = AtomicBool::new(false);
/// True while the STANDALONE native render window (pulsar-render with no `--wid`,
/// e.g. a Wayland client where the X11 embed fails) has input focus. That window
/// is a separate X(Wayland) toplevel, so focusing it UNfocuses the Tauri window —
/// capture must treat "render window focused" as focused too. Driven by the
/// renderer's `ov focus 0|1` stdout lines via `set_render_focused()`.
static RENDER_FOCUSED: AtomicBool = AtomicBool::new(false);

/// Effective focus: the Tauri window OR the standalone render window has focus.
// A KIOSK session counts as always-focused: the appliance runs nothing else, and
// GNOME's focus-stealing prevention may never focus the auto-launched window at
// all (no user click) — the focus gate would otherwise keep capture disengaged
// forever and silently break "auto-connect controls immediately".
fn is_focused() -> bool {
	FOCUSED.load(Ordering::SeqCst)
		|| RENDER_FOCUSED.load(Ordering::SeqCst)
		|| KIOSK_SESSION.load(Ordering::SeqCst)
}

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
///
/// Always uses `try_send` — NEVER `blocking_send`. This function is called from
/// the evdev capture thread while the devices may be EVIOCGRAB-held. A
/// `blocking_send` on a full channel would stall the capture thread while it holds
/// the grab, making the local keyboard+mouse appear dead until the channel drains
/// (the C5 bug). The teardown path already uses the same best-effort approach with
/// the explicit note that the host's `DesktopInput` Drop flushes held state on its
/// side, making a dropped UP safe: the host never stays stuck. Motion was already
/// try_send (coalescible), and key/button edges are equally safe to drop here
/// because every disengage edge sends a kernel-sourced flush (flush_held via
/// EVIOCGKEY) that covers the real held state, and the host releases everything on
/// its own teardown. Blocking the grab for an UP that the host will release anyway
/// is strictly worse than a dropped try_send.
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
const KEY_LEFTALT: u16 = 56; // chord modifier for the release combo (AltGr=100 excluded)
const KEY_Z: u16 = 44; // release combo Ctrl+Alt+Z (Parsec's detach shortcut)

/// Keep a device if it's a keyboard (has letter/escape keys) or, when `mouse`, a mouse
/// (relative axes + a left button). Gamepads (read separately via gilrs) are skipped so
/// we don't fight that path.
fn wanted(dev: &evdev::Device, mouse: bool) -> bool {
	let keys = dev.supported_keys();
	// BTN_SOUTH (== BTN_GAMEPAD) marks a gamepad — those go through gilrs, skip them.
	if keys.map_or(false, |k| k.contains(Key::BTN_SOUTH)) {
		return false;
	}
	let is_kbd = keys.map_or(false, |k| {
		k.contains(Key::KEY_A) || k.contains(Key::KEY_ESC)
	});
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
fn chord_mods(devs: &[evdev::Device]) -> (bool, bool, bool) {
	let (mut ctrl, mut shift, mut alt) = (false, false, false);
	for d in devs {
		if let Ok(st) = d.get_key_state() {
			ctrl |= st.contains(Key::KEY_LEFTCTRL) || st.contains(Key::KEY_RIGHTCTRL);
			shift |= st.contains(Key::KEY_LEFTSHIFT) || st.contains(Key::KEY_RIGHTSHIFT);
			// LeftAlt only — RightAlt is AltGr (a char-composition modifier, not a chord key).
			alt |= st.contains(Key::KEY_LEFTALT);
		}
	}
	(ctrl, shift, alt)
}

/// Live kernel state of the SHORTCUT modifiers (Ctrl / LeftAlt / Win) for ONE printable key's
/// char-vs-raw-VK routing decision. Kernel-authoritative (`EVIOCGKEY`), exactly like `chord_mods`:
/// the event-tracked booleans desync (a `SYN_DROPPED` burst drops a modifier up, a combo-disengage
/// ungrabs the device before its up is seen, a device grabbed mid-hold never saw the down), and a
/// single stale Ctrl/Alt/Win then latched EVERY later printable onto the raw-VK path — so a
/// Turkish-Q `ş` (evdev 39) typed as `;` (the host's VK_OEM_1) and every layout key mis-mapped on
/// the host. Querying the kernel at the routing moment is immune to all of it. RightAlt (AltGr=100)
/// is deliberately EXCLUDED — it's a char-composition modifier consumed by xkb, not a shortcut.
/// OR across devices so any Ctrl/Alt/Win on any keyboard counts; works on the open fds regardless
/// of grab state.
fn shortcut_held(devs: &[evdev::Device]) -> bool {
	devs.iter().any(|d| {
		d.get_key_state().map_or(false, |st| {
			st.contains(Key::KEY_LEFTCTRL)
				|| st.contains(Key::KEY_RIGHTCTRL)
				|| st.contains(Key::KEY_LEFTALT)
				|| st.contains(Key::KEY_LEFTMETA)
				|| st.contains(Key::KEY_RIGHTMETA)
		})
	})
}

/// Release EVERY key/button the host may still be holding for these grabbed devices. Queries the
/// kernel (`EVIOCGKEY`, authoritative — covers Char-forwarded keys and untracked letters that the
/// `ctrl`/`shift` booleans miss) and emits a key-UP for each held non-button code, plus an UP for
/// each tracked-held mouse button. Must run on EVERY in-session disengage edge (a local combo /
/// 3×RightCtrl release / overlay open / leave), because after that edge the modifier-up events are
/// either suppressed (`want_suspend`) or no longer grabbed — so without this the host is left with
/// Ctrl/Shift/Alt (or any key) latched (the "modifiers stick after Ctrl+Shift+M / Ctrl+Alt+Z"
/// bug). The Linux analog of the Windows `imp.rs::flush_held` — same guarantee, kernel-sourced.
fn flush_held(
	tx: &Sender<InputEvent>,
	grabbed: &[evdev::Device],
	held_buttons: &mut std::collections::HashSet<u8>,
) {
	let mut released: std::collections::HashSet<u16> = std::collections::HashSet::new();
	for d in grabbed.iter() {
		if let Ok(st) = d.get_key_state() {
			for key in st.iter() {
				let code = key.code();
				// 272..=274 = BTN_LEFT/RIGHT/MIDDLE — released via held_buttons below, never here
				// (querying them would re-introduce the phantom right/middle-click).
				if !(272..=274).contains(&code) && released.insert(code) {
					fwd(tx, InputEvent::Key { code: code as u32, down: false });
				}
			}
		}
	}
	for button in held_buttons.drain() {
		fwd(tx, InputEvent::PointerButton { button, down: false });
	}
}

pub fn enable(app: AppHandle, tx: Sender<InputEvent>, mouse: bool, id: u64, start_suspended: bool) {
	// Supersede any prior capture thread (a tab switch is stop+start back-to-back;
	// the bumped generation makes the old thread exit on its next loop pass).
	let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
	// A re-arm of the SAME live play session must PRESERVE the user's engagement:
	// resetting it mid-control silently dropped the grab semantics ("keys stopped
	// working after Ctrl+Shift+F12", seen live Pi→PC) with no UI cue. The real
	// trigger (Session.svelte tab switch) is a disable()→enable() pair, so RUNNING is
	// already FALSE here — same-session must therefore key off LAST_PLAY (which
	// survives disable()), NOT off RUNNING. A genuinely new session (different id)
	// still starts disengaged.
	RUNNING.store(true, Ordering::SeqCst);
	let same_session = LAST_PLAY.load(Ordering::SeqCst) == id;
	LAST_PLAY.store(id, Ordering::SeqCst);
	// Honor an actually-open overlay: the caller computes this from AppState.overlay_open
	// (the source of truth, which lives in the Tauri layer and can't be reached from this
	// static). Blindly clearing SUSPENDED here desynced from that set — a re-arm while
	// tab A's overlay was still on screen revived the grab and ate local input. We still
	// keep the "fresh session never starts stale" intent: a clean session passes
	// start_suspended=false.
	SUSPENDED.store(start_suspended, Ordering::SeqCst);
	RENDER_FOCUSED.store(false, Ordering::SeqCst);
	// Click-to-engage: a manual session starts DISENGAGED — the user keeps local
	// input until they click the session video. A CLI `--connect` (kiosk/appliance,
	// e.g. the Orange Pi player) starts ENGAGED: there is no local desktop to
	// protect and the remote should be controllable immediately. One-shot — only
	// the session the auto-connect actually starts may take it (see KIOSK_ENGAGE).
	// KIOSK_ENGAGE is LATCHED for the process lifetime, not consumed once: the
	// frontend re-arms capture (kbdCaptureStop→Start on an `active` toggle) shortly
	// after the auto-connect, and a one-shot left that SECOND session disengaged
	// (events flowed, forwarded=0). arm_kiosk_engage() is only called on a genuine
	// auto-connect launch (lib.rs gates on AUTO_CONNECT + no `.skip-autoconnect`
	// marker), so every enable() in this process belongs to the kiosk session —
	// re-engaging them all is correct (an appliance has no manual sessions).
	let kiosk = KIOSK_ENGAGE.load(Ordering::SeqCst);
	KIOSK_SESSION.store(kiosk, Ordering::SeqCst);
	// Same-session re-arm restores whatever engagement the user had when this id was last
	// active. The live ENGAGED was cleared by the intervening disable(), so read the memory
	// that survives it (ENGAGED_MEMO) instead — that's what makes preservation work for the
	// disable()→enable() tab-switch path. Everything else (new id) starts at the kiosk
	// default (engaged for appliances, off for manual sessions).
	let engaged = if same_session {
		ENGAGED_MEMO.load(Ordering::SeqCst) || kiosk
	} else {
		kiosk
	};
	ENGAGED.store(engaged, Ordering::SeqCst);
	// A fresh arm clears any prior manual-detach latch (a new/resumed session is controllable).
	MANUAL_DISENGAGE.store(false, Ordering::SeqCst);
	// Seed the cross-disable() memory with the engagement this enable() chose; the live
	// transitions below (engage/release/combos/focus) keep ENGAGED current, and disable()
	// snapshots it back into ENGAGED_MEMO so the NEXT same-session enable() can restore it.
	ENGAGED_MEMO.store(engaged, Ordering::SeqCst);
	tracing::info!(gen, kiosk, same_session, engaged, "evdev capture armed");
	if kiosk {
		// Keep the frontend's engage hint in sync with the auto-engage.
		let _ = app.emit("kbd-engaged", ());
	}
	std::thread::spawn(move || {
		// 1 Hz loop telemetry (see the "evdev capture state" log below).
		let mut diag_at = std::time::Instant::now();
		let (mut diag_events, mut diag_fwd) = (0u32, 0u32);
		let mut grabbed: Vec<evdev::Device> = Vec::new();
		// Event node of grabbed[i] (parallel array) — needed to purge a VANISHED device's
		// entry so a later device reusing the same event number can be grabbed again.
		let mut grabbed_nodes: Vec<std::path::PathBuf> = Vec::new();
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
		// Thread-local memory of the last engaged state, updated each loop. The exit-flush
		// below can't read the ENGAGED static: disable() clears ENGAGED *before* this thread
		// observes the GEN bump, so by exit it's always false. This local survives that.
		let mut was_engaged = false;
		// Shortcut modifiers (Ctrl/LeftAlt/Win): when ANY is held a key is a shortcut → forward the
		// raw keycode (VK path) so Ctrl+C etc. work. RightAlt (AltGr=100) is deliberately NOT here —
		// it's a char-composition modifier (Turkish AltGr chars), consumed by the xkb resolution.
		// Win is NOT tracked as a boolean: the char-vs-VK routing test is kernel-authoritative
		// (shortcut_held), so a stale Win/Ctrl boolean can't latch printables onto the raw-VK
		// path. LeftAlt stays tracked only as the combo-gate fallback (OR'd with chord_mods).
		let mut lalt = false;
		// Keycodes currently down that were forwarded as a resolved `Char`; their key-UP is
		// suppressed (a Unicode insert is one-shot — there is no VK to release).
		let mut char_keys: std::collections::HashSet<u16> = std::collections::HashSet::new();
		// Mouse buttons currently held (forwarded down, not yet up). Tracked so a disengage flush
		// releases ONLY genuinely-held buttons — the old `for button in 0..3` blindly sent a
		// RIGHT+MIDDLE button-UP every flush → a phantom right-click/context-menu on the host
		// whenever the overlay opened or control was released (the "Ctrl+Shift sends a right-tık").
		let mut held_buttons: std::collections::HashSet<u8> = std::collections::HashSet::new();
		// Active-layout keyboard state for WYSIWYG char resolution (None → raw-keycode VK fallback).
		let mut xkb_state = build_xkb_state();
		// Re-enumerate ~once a second to grab a newly plugged / KVM-switched device. This
		// MUST be WALL-CLOCK based, not per-iteration: poll() below returns immediately while
		// input is flowing, so an iteration counter fired the rescan continuously during
		// active use — and evdev::enumerate() (which opens every /dev/input node) then blocked
		// the loop, making forwarded mouse/keys arrive in stuttering bursts (the "input lag /
		// jerky" symptom). Time-based → at most one ~once-a-second blip.
		let mut last_rescan: Option<std::time::Instant> = None;
		// When the app FIRST went unfocused (None = focused) — the disengage debounce.
		let mut unfocused_since: Option<std::time::Instant> = None;
		// Every /dev/input/event* node we've already evaluated. The rescan below first does a
		// CHEAP read_dir against this set and only runs the EXPENSIVE evdev::enumerate() when a
		// genuinely new node appears (real hotplug) — see the comment at the gate.
		let mut seen_nodes: std::collections::HashSet<std::path::PathBuf> =
			std::collections::HashSet::new();
		while GEN.load(Ordering::SeqCst) == gen {
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
				// Nodes also VANISH (unplug, a closed virtual device) and the kernel REUSES
				// their event numbers: purge the stale bookkeeping first, or a later device
				// on a reused node looks "already seen/grabbed" and is never grabbed again
				// (a replugged/KVM-switched keyboard stayed dead until the next re-arm).
				let mut purged = false;
				if seen_nodes.iter().any(|p| !cur_nodes.contains(p)) {
					let mut i = 0;
					while i < grabbed_nodes.len() {
						if cur_nodes.contains(&grabbed_nodes[i]) {
							i += 1;
						} else {
							grabbed_paths.remove(&grabbed_nodes[i]);
							grabbed_nodes.remove(i);
							drop(grabbed.remove(i)); // closes the dead fd
							purged = true;
						}
					}
					seen_nodes.retain(|p| cur_nodes.contains(p));
				}
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
						grabbed_paths.insert(path.clone());
						grabbed_nodes.push(path);
						grabbed.push(d);
					} else {
						// Grab refused (EBUSY — e.g. a just-superseded capture thread
						// hasn't dropped its EVIOCGRAB yet): forget the node so the next
						// 1 s rescan re-enumerates and retries instead of writing the
						// device off for the session's lifetime.
						seen_nodes.remove(&path);
					}
				}
				if purged || grabbed.len() != before {
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
			// Losing all focus also DISENGAGES: refocusing alone must not resume the grab,
			// the user has to click back into the video (click-to-engage). DEBOUNCED:
			// app-internal focus handoffs (main → approval popup) deliver blur→focus as
			// two events with a gap; this loop samples per event batch, so an instant
			// latch turned that gap into a permanent mid-drag disengage. Only a SUSTAINED
			// unfocused state (200 ms) clears the latch.
			if is_focused() {
				unfocused_since = None;
				// Kiosk sessions RE-engage on focus (appliance: no local desktop to
				// protect) — see KIOSK_SESSION for why the one-shot wasn't enough. Emit
				// kbd-engaged only on the actual edge (false→true) so the renderer hides
				// the local cursor / updates its hint instead of staying stale.
				if KIOSK_SESSION.load(Ordering::SeqCst)
					&& !SUSPENDED.load(Ordering::SeqCst)
					&& !MANUAL_DISENGAGE.load(Ordering::SeqCst)
				{
					if !ENGAGED.swap(true, Ordering::SeqCst) {
						let _ = app.emit("kbd-engaged", ());
					}
				}
			} else {
				let t = *unfocused_since.get_or_insert_with(std::time::Instant::now);
				if t.elapsed() >= std::time::Duration::from_millis(200) {
					// Focus-loss disengage: emit kbd-released only on the edge (true→false),
					// otherwise the renderer keeps the cursor hidden / the UI hint stale after
					// an alt-tab away from the session.
					if ENGAGED.swap(false, Ordering::SeqCst) {
						let _ = app.emit("kbd-released", ());
					}
				}
			}
			was_engaged = ENGAGED.load(Ordering::SeqCst);
			let want_suspend = SUSPENDED.load(Ordering::SeqCst)
				|| !is_focused()
				|| !was_engaged;
			if want_suspend != applied_suspend {
				if want_suspend {
					// Mirror the teardown fix (C25): ungrab the devices FIRST, THEN flush.
					// Flushing before ungrab meant flush_held→fwd→try_send could theoretically
					// stall (if it were blocking_send) while the grab was still held — the C5
					// bug. With fwd() now always try_send the order matters less for correctness,
					// but ungrabbing first is the right safety model: the local OS regains input
					// the instant we release, before any channel send. EVIOCGKEY (get_key_state)
					// works on open fds regardless of grab state, so flush_held can still read the
					// kernel's authoritative held-key set after ungrab.
					for d in grabbed.iter_mut() {
						let _ = d.ungrab();
					}
					// Release EVERYTHING the host still holds for this disengage edge — kernel-sourced
					// (flush_held: EVIOCGKEY) so it covers modifiers (incl. Win), Char-forwarded keys
					// AND any held letter, not just a fixed modifier list. The old hand-written list
					// omitted Win(125/126) + letters, so an overlay/focus-loss/auto-suspend while one
					// was held latched it on the host for the rest of the session. Also drains
					// held_buttons. get_key_state is valid on ungrabbed-but-open fds (same fds,
					// just without EVIOCGRAB) — verified by the chord_mods comment above.
					flush_held(&tx, &grabbed, &mut held_buttons);
					// And any GENUINELY-held mouse button (a disengage mid-drag otherwise left the
					// host's uinput holding BTN_LEFT — a stuck drag). Only the buttons actually down,
					// NOT a blind 0..3 (which released RIGHT+MIDDLE → a phantom context-menu click).
					for button in held_buttons.drain() {
						fwd(
							&tx,
							InputEvent::PointerButton {
								button,
								down: false,
							},
						);
					}
					// Drop the tracked modifier state too: after the ungrab the physical key-UPs go to
					// the LOCAL OS, never to fetch_events, so a held modifier would stay latched here.
					// lalt was NOT reset before — a stale lalt (after Ctrl+Alt+Z) forced every later
					// printable onto the raw-VK path -> Turkish-Q keys mis-typed on the host (s->;).
					// char_keys likewise can't trust an up that the suspend gate swallowed.
					ctrl = false;
					shift = false;
					lalt = false;
					char_keys.clear();
				} else {
					for d in grabbed.iter_mut() {
						let _ = d.grab();
					}
				}
				applied_suspend = want_suspend;
			}

			// 1 Hz state telemetry: the capture loop was a black box during the
			// "grabbed but nothing forwards" kiosk debugging — keep it observable.
			if diag_at.elapsed().as_secs() >= 1 {
				tracing::info!(
					engaged = ENGAGED.load(Ordering::SeqCst),
					focused = is_focused(),
					suspended = applied_suspend,
					grabbed = grabbed.len(),
					events = diag_events,
					forwarded = diag_fwd,
					"evdev capture state"
				);
				(diag_events, diag_fwd) = (0, 0);
				diag_at = std::time::Instant::now();
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
					diag_events += 1;
					match ev.kind() {
						InputEventKind::Key(key) => {
							let code = key.code();
							let down = ev.value() != 0; // 1 down / 2 repeat → down
							match code {
								KEY_LEFTCTRL | KEY_RIGHTCTRL => ctrl = down,
								KEY_LEFTSHIFT | KEY_RIGHTSHIFT => shift = down,
								KEY_LEFTALT => lalt = down, // RightAlt=100 = AltGr (char modifier), not a shortcut
								// Win/Meta intentionally NOT chord-tracked: the char-vs-VK router queries
								// the kernel directly (shortcut_held), so there is no boolean to go stale.
								_ => {}
							}
							// NOTE: there is deliberately NO raw-BTN_LEFT engage here. evdev can't
							// tell a click on the video from a click on the app's own chrome (tab
							// bar, close button) or on another window — engaging on any click made
							// the local UI unclickable (the grab ate the very next click). Engage
							// comes from explicit channels instead: the webview's click on the
							// pass-through video area (`kbd_engage` command), the standalone render
							// window's `ov engage`, or the standalone mpv focus edge.
							// Robust escape: 3 quick Right-Ctrl taps within 1s (no chord needed).
							if code == KEY_RIGHTCTRL && ev.value() == 1 && is_focused() {
								let now = std::time::Instant::now();
								rctrl_taps.retain(|t| {
									now.duration_since(*t) < std::time::Duration::from_millis(1000)
								});
								rctrl_taps.push(now);
								if rctrl_taps.len() >= 3 {
									// RELEASE control (grab off, session + thread stay alive) —
									// the next video click re-engages. Ending is Ctrl+Shift+Q.
									// Release everything held FIRST so no key latches on the host.
									flush_held(&tx, &grabbed, &mut held_buttons);
									let _ = app.emit("kbd-released", ());
									ENGAGED.store(false, Ordering::SeqCst);
									// Explicit detach: don't let a kiosk session auto-re-engage it away.
									MANUAL_DISENGAGE.store(true, Ordering::SeqCst);
									rctrl_taps.clear();
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
								&& matches!(code, KEY_M | KEY_F12 | KEY_Q | KEY_Z)
								&& is_focused()
							{
								let (lc, ls, la) = chord_mods(&grabbed);
								let (cmod, smod, amod) = (ctrl || lc, shift || ls, lalt || la);
								// Ctrl+Alt+Z (Parsec's detach shortcut): RELEASE the mouse+keyboard
								// without ending the session — same effect as 3×RightCtrl. The user
								// can then click the overlay button / app chrome; clicking the video
								// re-engages.
								if code == KEY_Z {
									if cmod && amod {
										// Ctrl+Alt+Z detach: release every held key/button to the
										// host BEFORE disengaging, or Ctrl/Alt stay latched there.
										flush_held(&tx, &grabbed, &mut held_buttons);
										let _ = app.emit("kbd-released", ());
										ENGAGED.store(false, Ordering::SeqCst);
										// Explicit detach: don't let a kiosk session auto-re-engage it away.
										MANUAL_DISENGAGE.store(true, Ordering::SeqCst);
										continue;
									}
								} else if cmod && smod {
									if code == KEY_M {
										// Overlay toggle — does NOT end the session (RUNNING
										// stays true); the ungrab/regrab is driven by
										// set_overlay → overlay_suspend(), keeping grab state
										// owned by this one thread. Flush held keys NOW (not after
										// the Tauri→JS→overlay_suspend round-trip) — otherwise the
										// Ctrl+Shift held for this combo stay latched on the host
										// while the grab suspends + their key-up is swallowed.
										flush_held(&tx, &grabbed, &mut held_buttons);
										let _ = app.emit("overlay-toggle", ());
									} else if code == KEY_F12 {
										// Fullscreen toggle (Ctrl+Shift+F12) -- does NOT end the
										// session; the frontend listens for `fullscreen-toggle`
										// and flips the Tauri window fullscreen state.
										let _ = app.emit("fullscreen-toggle", ());
									} else {
										// Leave (Ctrl+Shift+Q). Q is the reliable leave key --
										// media-mode keyboards (e.g. Logitech MX Keys) do not
										// emit KEY_F12, and F12 is fullscreen now. The UI ENDS
										// the session on kbd-leave; drop the grab right away so
										// the user's input is free during the teardown. Flush the
										// held chord FIRST (Windows imp.rs does the same before
										// kbd-leave) so Ctrl/Shift don't latch on the still-alive
										// host while the session tears down.
										flush_held(&tx, &grabbed, &mut held_buttons);
										let _ = app.emit("kbd-leave", ());
										ENGAGED.store(false, Ordering::SeqCst);
									}
									continue;
								}
							}
							// A combo trigger key (M/F12/Q/Z) AUTOREPEATING while its chord is
							// still held is the user holding the LOCAL combo — its value==1 already
							// fired the action above. Never leak the repeat to the host: the overlay
							// combo (Ctrl+Shift+M) does NOT disengage synchronously (it waits for the
							// async overlay_suspend round-trip), so in that window M's value==2 would
							// fall through and the ctrl+shift shortcut path would forward M-down → a
							// phantom 'm' typed on the host. Only suppress when the chord is ACTUALLY
							// held (kernel-sourced, like the combo gate) — a BARE m/q/z still repeats.
							if ev.value() == 2 && matches!(code, KEY_M | KEY_F12 | KEY_Q | KEY_Z) {
								let (lc, ls, la) = chord_mods(&grabbed);
								let (cmod, smod, amod) = (ctrl || lc, shift || ls, lalt || la);
								if (matches!(code, KEY_M | KEY_F12 | KEY_Q) && cmod && smod)
									|| (code == KEY_Z && cmod && amod)
								{
									continue;
								}
							}
							// While the overlay is open we keep tracking modifiers + the two
							// combos above, but forward NOTHING to the host (the local OS +
							// webview drive the menu) — no phantom input during the overlay.
							// Keep xkb_state synced with EVERY physical key event — even while
							// suspended/disengaged. Skipping the releases that arrive during a
							// suspend (overlay open / Ctrl+Alt+Z disengage) leaves a held
							// Shift/AltGr/layout-group key's UP unseen, so xkb latches it and
							// later keys mis-resolve: a Turkish-Q ş (code 39) came out as ':'
							// after Ctrl+Shift+M. MUST run BEFORE the want_suspend forward-gate.
							if let Some(st) = xkb_state.as_mut() {
								st.update_key(
									xkb::Keycode::new((code as u32) + 8),
									if down {
										xkb::KeyDirection::Down
									} else {
										xkb::KeyDirection::Up
									},
								);
							}
							if want_suspend {
								continue;
							}
							// Mouse buttons also arrive as EV_KEY (BTN_LEFT/RIGHT/MIDDLE
							// = 272/273/274 → 0/1/2).
							match code {
								272 | 273 | 274 => {
									// EV_KEY values: 0=up, 1=down, 2=KERNEL AUTOREPEAT. EV_REP
									// devices soft-repeat BTN_* too (Logitech Unifying combo
									// nodes do — the MX Master 3 here), so a held button emits
									// value=2 at ~30 Hz after ~250 ms. Mapping that to `down:
									// ev.value() == 1` sent a RELEASE mid-hold → every drag
									// self-released after ~0.28 s. A repeat is neither a press
									// nor a release — skip it.
									if ev.value() != 2 {
										let button = (code - 272) as u8;
										let down = ev.value() == 1;
										// Track real held state so a disengage flush releases only
										// down buttons (no phantom right/middle-up).
										if down {
											held_buttons.insert(button);
										} else {
											held_buttons.remove(&button);
										}
										fwd(&tx, InputEvent::PointerButton { button, down });
									}
								}
								_ if !down => {
									// key UP: a Char key had a one-shot Unicode insert (no VK to release) -> suppress
									// its up; otherwise release the VK as before.
									if !char_keys.remove(&code) {
										fwd(
											&tx,
											InputEvent::Key {
												code: code as u32,
												down: false,
											},
										);
									}
								}
								_ => {
									// key DOWN/REPEAT - WYSIWYG: a printable key with NO shortcut modifier (Ctrl/
									// LeftAlt/Win) is resolved via the client's OWN layout to a Unicode char and sent
									// as Char (host types it regardless of ITS layout). A key already in char-mode
									// keeps re-sending Char on autorepeat. Shortcuts, non-text keys (xkb yields no
									// printable) and the no-xkb fallback take the raw-keycode VK path.
									// KERNEL-AUTHORITATIVE shortcut test (shortcut_held -> EVIOCGKEY), NOT the
									// tracked ctrl/lalt/win booleans: those desync (SYN_DROPPED, a combo-
									// disengage that ungrabs before the modifier-UP is seen) and a single
									// stale modifier latched every later printable onto the raw-VK path ->
									// Turkish-Q keys mis-typed on the host (s->;). A key already in char-mode
									// keeps re-sending Char on autorepeat and short-circuits the kernel query.
									let ch = if char_keys.contains(&code) || !shortcut_held(&grabbed) {
										xkb_state
											.as_ref()
											.map(|st| {
												st.key_get_utf8(xkb::Keycode::new(
													(code as u32) + 8,
												))
											})
											.and_then(|s| s.chars().next())
											.filter(|c| !c.is_control())
									} else {
										None
									};
									if let Some(c) = ch {
										char_keys.insert(code);
										fwd(&tx, InputEvent::Char(c));
									} else if !char_keys.contains(&code) {
										fwd(
											&tx,
											InputEvent::Key {
												code: code as u32,
												down: true,
											},
										);
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
					diag_fwd += 1;
					fwd(&tx, InputEvent::PointerRelative { dx: adx, dy: ady });
				}
			}
		}
		// Thread exit (GEN bump / disable — session end, host disconnect, tab close).
		// Priority order:
		//   1. Snapshot any still-held keys from the kernel (needs fds open for EVIOCGKEY).
		//   2. Drop `grabbed` IMMEDIATELY so every EVIOCGRAB is released — the local
		//      keyboard/mouse is returned to the OS before any channel send.  If we flushed
		//      first with blocking_send (via fwd) and the hold_session consumer was stalled,
		//      we'd hold the grab for as long as the send blocked — the user's local input
		//      would appear dead after a disconnect (the C25 bug).
		//   3. Best-effort try_send the collected UPs to the host.  The host also releases
		//      everything on its own DesktopInput Drop, so it's safe to drop these events
		//      rather than block the grab release.
		// We were only forwarding while engaged + not suspended, so only flush then —
		// otherwise nothing was sent down.
		if was_engaged && !applied_suspend {
			// Step 1 — collect held keys while fds are still valid.
			let mut pending_keys: Vec<InputEvent> = Vec::new();
			let mut released: std::collections::HashSet<u16> = std::collections::HashSet::new();
			for d in grabbed.iter() {
				if let Ok(st) = d.get_key_state() {
					for key in st.iter() {
						let code = key.code();
						// Mouse buttons (272..=274) are flushed explicitly below as
						// PointerButton; everything else is a keyboard key.
						if !(272..=274).contains(&code) && released.insert(code) {
							pending_keys.push(InputEvent::Key { code: code as u32, down: false });
						}
					}
				}
			}
			// Drain held_buttons now too — the Vec owns the data independently of `grabbed`.
			let pending_buttons: Vec<u8> = held_buttons.drain().collect();

			// Step 2 — release the grab so the local OS regains input NOW.
			drop(grabbed);

			// Step 3 — best-effort flush to the host (non-blocking; a stalled consumer
			// may drop some events, but the host's own teardown handles it).
			for ev in pending_keys {
				let _ = tx.try_send(ev);
			}
			for button in pending_buttons {
				let _ = tx.try_send(InputEvent::PointerButton { button, down: false });
			}
		}
		// `grabbed` already dropped above (or drops here on the non-flush path) →
		// fds close → every EVIOCGRAB is released.
	});
}

pub fn disable() {
	// Bump past every live thread's generation → each exits on its next loop pass
	// (and a quick re-enable can't revive them — it bumps again to a NEWER value).
	GEN.fetch_add(1, Ordering::SeqCst);
	RUNNING.store(false, Ordering::SeqCst);
	// Clear the overlay gate so a teardown mid-overlay leaves clean state and the
	// next session never starts suspended.
	SUSPENDED.store(false, Ordering::SeqCst);
	// Remember whether the user was engaged BEFORE clearing the live flag: the tab-switch
	// path is disable()→enable() for the SAME id, so a same-session enable() restores from
	// here. LAST_PLAY is deliberately NOT reset (a different id still starts disengaged;
	// only the matching id revives this engagement).
	ENGAGED_MEMO.store(ENGAGED.load(Ordering::SeqCst), Ordering::SeqCst);
	ENGAGED.store(false, Ordering::SeqCst);
	RENDER_FOCUSED.store(false, Ordering::SeqCst);
	// KIOSK_SESSION is NOT cleared here — it is recomputed from the latched
	// KIOSK_ENGAGE on every enable(), and clearing it on a stop→start re-arm was
	// exactly what disengaged the kiosk's second session.
}

/// Arm the one-shot kiosk auto-engage (see [`KIOSK_ENGAGE`]). Called from `lib.rs`
/// setup only when this launch will actually auto-connect.
pub fn arm_kiosk_engage() {
	KIOSK_ENGAGE.store(true, Ordering::SeqCst);
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

/// The STANDALONE native render window's focus changed (`ov focus 0|1` on the renderer's
/// stdout). That window is a separate toplevel — focusing it unfocuses the Tauri window,
/// so capture treats "either focused" as focused (see `is_focused`).
pub fn set_render_focused(focused: bool) {
	RENDER_FOCUSED.store(focused, Ordering::SeqCst);
}

/// Engage capture: the user explicitly clicked the session video (the webview saw the
/// click through the pass-through container and invoked `kbd_engage`). The Tauri window
/// is focused (it just received the click), so no focus bookkeeping is needed.
pub fn engage(app: &AppHandle) {
	// The user explicitly took control back (video click) — clear any manual-detach latch so a
	// kiosk session resumes its normal focus-driven re-engage afterwards.
	MANUAL_DISENGAGE.store(false, Ordering::SeqCst);
	if RUNNING.load(Ordering::SeqCst) && !ENGAGED.swap(true, Ordering::SeqCst) {
		let _ = app.emit("kbd-engaged", ());
	}
}

/// Release (disengage) capture programmatically — the overlay opening must drop the user
/// out of control mode (closing then leaves them disengaged: click-to-engage again).
/// Emits `kbd-released` so the frontend hint state stays in sync.
pub fn release(app: &AppHandle) {
	if ENGAGED.swap(false, Ordering::SeqCst) {
		let _ = app.emit("kbd-released", ());
	}
}

/// Engage capture from the STANDALONE render window (`ov engage` stdout line / the mpv
/// focus edge — the user clicked/focused the video toplevel). The click also focused
/// that window, so mark it focused; its FocusIn report may still be in flight and the
/// grab must not instantly re-suspend.
pub fn engage_render(app: &AppHandle) {
	RENDER_FOCUSED.store(true, Ordering::SeqCst);
	engage(app);
}
