//! Client-side controller subsystem on **SDL3** (raw `sdl3-sys` FFI). ONE process-global
//! SDL gamepad context that BOTH reads input (buttons/axes → [`GamepadState`], forwarded
//! to the host) AND actuates **rumble** (`SDL_RumbleGamepad` over SDL's own HID drivers).
//! This is the Moonlight model — one owner of the physical pads — and replaces gilrs.
//!
//! **Event-driven**, exactly like Moonlight's `SdlInputHandler` (verified in
//! `_ref/moonlight-qt/app/streaming/session.cpp`): the SDL thread blocks in
//! `SDL_WaitEventTimeout` and drains with `SDL_PollEvent`, so it consumes **0 CPU while
//! idle** and wakes **once per controller event** — i.e. at the pad's native report rate,
//! up to 1000 Hz, with NO fixed poll cap. On any change it pings the active session's
//! "wake" channel; the play reader blocks on that (also event-driven) and forwards the
//! changed state to the host. Rumble is applied here too (SDL is thread-affine, so the
//! `*mut SDL_Gamepad` handles never leave this thread).
//!
//! Consumers never touch SDL: they read a shared snapshot ([`SdlPads::snapshot`]), block on
//! a wake subscription ([`SdlPads::subscribe`]), and post rumble ([`SdlPads::rumble`]).

use std::collections::HashMap;
use std::ffi::{c_int, c_void, CStr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pulsar_core::input::{button, vid_pid_from_sdl_guid, GamepadKind, GamepadState};
use sdl3_sys::events::{
    SDL_PollEvent, SDL_WaitEventTimeout, SDL_Event, SDL_EVENT_GAMEPAD_ADDED,
    SDL_EVENT_GAMEPAD_REMAPPED, SDL_EVENT_GAMEPAD_REMOVED,
};
use sdl3_sys::gamepad::*;
use sdl3_sys::guid::SDL_GUID;
use sdl3_sys::hints::{
    SDL_SetHint, SDL_HINT_JOYSTICK_HIDAPI, SDL_HINT_JOYSTICK_HIDAPI_PS4,
    SDL_HINT_JOYSTICK_HIDAPI_PS5,
};
use sdl3_sys::init::{SDL_InitSubSystem, SDL_INIT_GAMEPAD};
use sdl3_sys::stdinc::SDL_free;

/// Rumble window handed to SDL — matches Moonlight's `SDL_GameControllerRumble(..., 30000)`.
/// The host re-sends on every force-feedback change (and 0/0 to stop), so the motors track
/// the game; the long fallback only self-stops if a stream of updates is cut off mid-rumble.
const RUMBLE_MS: u32 = 30_000;

/// How long the SDL thread blocks waiting for an event before looping to service the rumble
/// channel. While blocked it uses no CPU; controller events wake it immediately (native
/// rate). 8 ms bounds idle rumble latency without a busy loop.
const EVENT_WAIT_MS: i32 = 8;

/// A connected pad + its current input state, as published by the SDL reader on each change.
/// `uuid` is the SDL GUID hex — the SAME stable key the rest of the app uses for
/// `controllerOrder` / emulation-target maps (gilrs derived its `uuid()` from this very SDL
/// GUID, so persisted slot/target assignments stay compatible).
#[derive(Clone)]
pub struct PadView {
    pub uuid: String,
    pub kind: GamepadKind,
    pub name: String,
    pub state: GamepadState,
}

/// A rumble request routed to the SDL thread: prefer the GUID-matched pad, else slot-Nth.
struct RumbleCmd {
    guid: Option<String>,
    slot: u8,
    large: u8,
    small: u8,
}

struct Shared {
    /// Latest connected-pad snapshot, sorted by uuid (stable). Replaced when state changes.
    pads: Mutex<Vec<PadView>>,
    /// The active session's wake channel — pinged on any pad change so a blocked reader
    /// wakes immediately (no polling). `(generation, sender)`: the generation lets an old
    /// subscription's drop avoid clearing a newer one's sender.
    wake: Mutex<Option<(u64, SyncSender<()>)>>,
    wake_gen: AtomicU64,
}

impl Shared {
    /// Wake the active reader (if any). Best-effort: a full 1-slot channel already has a
    /// pending wake, so dropping is correct (the reader will read the latest snapshot).
    fn ping(&self) {
        if let Some((_, tx)) = &*self.wake.lock().unwrap() {
            let _ = tx.try_send(());
        }
    }
}

/// Handle to the process-global SDL controller manager. `Sync` (Arc + SyncSender), so
/// `&'static SdlPads` can be read/subscribed/rumbled from any thread.
pub struct SdlPads {
    shared: Arc<Shared>,
    rumble_tx: SyncSender<RumbleCmd>,
}

/// A reader's wake subscription. Block on `rx` (`recv_timeout`) to sleep until the next pad
/// change. Dropping it detaches from the manager (so it stops pinging a dead channel).
pub struct WakeSub {
    pub rx: Receiver<()>,
    shared: Arc<Shared>,
    gen: u64,
}

impl Drop for WakeSub {
    fn drop(&mut self) {
        let mut w = self.shared.wake.lock().unwrap();
        if w.as_ref().map(|(g, _)| *g) == Some(self.gen) {
            *w = None;
        }
    }
}

impl SdlPads {
    /// Current connected pads with their live input state (clone of the shared snapshot).
    pub fn snapshot(&self) -> Vec<PadView> {
        self.shared.pads.lock().unwrap().clone()
    }

    /// Subscribe to pad-change wakeups. Replaces any prior subscription (only one session
    /// forwards controllers at a time). The returned `WakeSub` is seeded so a `recv` returns
    /// promptly if pads are already connected; block on `WakeSub::rx` to wait for changes.
    pub fn subscribe(&self) -> WakeSub {
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let gen = self.shared.wake_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = tx.try_send(()); // seed: reader does an initial snapshot pass immediately
        *self.shared.wake.lock().unwrap() = Some((gen, tx));
        WakeSub { rx, shared: self.shared.clone(), gen }
    }

    /// Replay a host rumble command on the physical pad. `guid` = the slot's device key from
    /// the live controller order (preferred match); 0/0 stops the motors. Best-effort.
    pub fn rumble(&self, guid: Option<String>, slot: u8, large: u8, small: u8) {
        let _ = self.rumble_tx.try_send(RumbleCmd { guid, slot, large, small });
    }
}

static PADS: OnceLock<Option<SdlPads>> = OnceLock::new();

/// The process-global SDL controller manager, started on first call. `None` if SDL's
/// gamepad subsystem can't initialize (then there's simply no controller input/rumble —
/// never fatal). The SDL thread then runs for the rest of the process.
pub fn manager() -> Option<&'static SdlPads> {
    PADS.get_or_init(spawn).as_ref()
}

fn spawn() -> Option<SdlPads> {
    let (rumble_tx, rumble_rx) = std::sync::mpsc::sync_channel::<RumbleCmd>(64);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
    let shared = Arc::new(Shared {
        pads: Mutex::new(Vec::new()),
        wake: Mutex::new(None),
        wake_gen: AtomicU64::new(0),
    });
    let shared_thread = shared.clone();
    std::thread::Builder::new()
        .name("sdl-pads".into())
        .spawn(move || run(shared_thread, rumble_rx, ready_tx))
        .ok()?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => Some(SdlPads { shared, rumble_tx }),
        _ => {
            tracing::warn!("controllers: SDL gamepad subsystem unavailable — no input/rumble");
            None
        }
    }
}

/// An opened SDL pad (lives only on the SDL thread — the raw pointer never crosses a
/// thread boundary).
struct OpenPad {
    ptr: *mut SDL_Gamepad,
    uuid: String,
    kind: GamepadKind,
    name: String,
}

/// The SDL thread body: init the gamepad subsystem, then run the Moonlight-style event loop
/// (block for an event, drain the queue, publish changed state, apply rumble) forever.
fn run(shared: Arc<Shared>, rumble_rx: Receiver<RumbleCmd>, ready: std::sync::mpsc::Sender<bool>) {
    unsafe {
        // Use SDL's own HID drivers for PlayStation pads (rumble where the evdev node has no
        // EV_FF). HIDAPI is on by default in SDL3; set it + the PS4/PS5 drivers explicitly.
        SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI, c"1".as_ptr());
        SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI_PS4, c"1".as_ptr());
        SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI_PS5, c"1".as_ptr());
        // GAMEPAD implies JOYSTICK + EVENTS; no SDL_INIT_VIDEO → safe off the main thread.
        if !SDL_InitSubSystem(SDL_INIT_GAMEPAD) {
            let _ = ready.send(false);
            return;
        }
    }
    let _ = ready.send(true);
    tracing::info!("controllers: SDL gamepad subsystem up (event-driven input + rumble)");

    let mut open: HashMap<u32, OpenPad> = HashMap::new();
    let mut last: HashMap<u32, GamepadState> = HashMap::new();
    let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
    let mut needs_refresh = true; // open already-connected pads on the first pass

    loop {
        // Block until a controller event arrives (0 CPU while waiting) or the timeout fires
        // (to service rumble). Then drain the whole queue — exactly Moonlight's loop shape.
        let got = unsafe { SDL_WaitEventTimeout(&mut ev, EVENT_WAIT_MS) };
        if got {
            loop {
                let t = unsafe { ev.r#type };
                if t == SDL_EVENT_GAMEPAD_ADDED.0
                    || t == SDL_EVENT_GAMEPAD_REMOVED.0
                    || t == SDL_EVENT_GAMEPAD_REMAPPED.0
                {
                    needs_refresh = true;
                }
                if !unsafe { SDL_PollEvent(&mut ev) } {
                    break;
                }
            }
        }

        let mut changed = false;
        if needs_refresh {
            refresh(&mut open);
            needs_refresh = false;
            last.retain(|inst, _| open.contains_key(inst));
            changed = true; // device set changed → reader rebuilds its list / disconnects
        }

        // Read each open pad, diff against the last published state, and rebuild the snapshot.
        let mut views: Vec<PadView> = Vec::with_capacity(open.len());
        for (inst, p) in &open {
            let st = unsafe { read_state(p.ptr) };
            if last.get(inst) != Some(&st) {
                last.insert(*inst, st);
                changed = true;
            }
            views.push(PadView {
                uuid: p.uuid.clone(),
                kind: p.kind,
                name: p.name.clone(),
                state: st,
            });
        }
        views.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        *shared.pads.lock().unwrap() = views;

        if changed {
            shared.ping();
        }

        // Apply any pending rumble (the host's force-feedback), latest-wins coalesced.
        while let Ok(cmd) = rumble_rx.try_recv() {
            apply_rumble(&open, &cmd);
        }
    }
}

/// Reconcile the open-pad map with SDL's current device list: open newly-attached pads,
/// close detached ones.
fn refresh(open: &mut HashMap<u32, OpenPad>) {
    let mut present: std::collections::HashSet<u32> = std::collections::HashSet::new();
    unsafe {
        let mut count: c_int = 0;
        let ids = SDL_GetGamepads(&mut count);
        if !ids.is_null() {
            for i in 0..count as isize {
                let id = *ids.offset(i);
                present.insert(id.0);
                if !open.contains_key(&id.0) {
                    let ptr = SDL_OpenGamepad(id);
                    if ptr.is_null() {
                        continue;
                    }
                    let guid = SDL_GetGamepadGUIDForID(id);
                    let uuid = guid_hex(&guid);
                    let (vid, pid) = vid_pid_from_sdl_guid(guid.data);
                    let kind = GamepadKind::from_vid_pid(vid, pid);
                    let name_ptr = SDL_GetGamepadName(ptr);
                    let name = if name_ptr.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(name_ptr).to_string_lossy().into_owned()
                    };
                    tracing::info!(instance = id.0, %uuid, ?kind, %name, "controllers: SDL opened pad");
                    open.insert(id.0, OpenPad { ptr, uuid, kind, name });
                }
            }
            SDL_free(ids as *mut c_void);
        }
    }
    let gone: Vec<u32> = open
        .keys()
        .copied()
        .filter(|k| !present.contains(k))
        .collect();
    for k in gone {
        if let Some(p) = open.remove(&k) {
            tracing::info!(instance = k, name = %p.name, "controllers: SDL pad removed");
            unsafe { SDL_CloseGamepad(p.ptr) };
        }
    }
}

/// Read one pad's normalized [`GamepadState`]. SDL's stick Y is **down-positive**; our
/// `GamepadState` (and the host replay paths) are **up-positive**, so Y is negated here.
/// Triggers are SDL's `0..32767` → `0..255`.
unsafe fn read_state(gp: *mut SDL_Gamepad) -> GamepadState {
    let mut st = GamepadState::default();
    for (b, bit) in [
        (SDL_GAMEPAD_BUTTON_SOUTH, button::A),
        (SDL_GAMEPAD_BUTTON_EAST, button::B),
        (SDL_GAMEPAD_BUTTON_WEST, button::X),
        (SDL_GAMEPAD_BUTTON_NORTH, button::Y),
        (SDL_GAMEPAD_BUTTON_LEFT_SHOULDER, button::LB),
        (SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER, button::RB),
        (SDL_GAMEPAD_BUTTON_BACK, button::BACK),
        (SDL_GAMEPAD_BUTTON_START, button::START),
        (SDL_GAMEPAD_BUTTON_GUIDE, button::GUIDE),
        (SDL_GAMEPAD_BUTTON_LEFT_STICK, button::L3),
        (SDL_GAMEPAD_BUTTON_RIGHT_STICK, button::R3),
        (SDL_GAMEPAD_BUTTON_DPAD_UP, button::DPAD_UP),
        (SDL_GAMEPAD_BUTTON_DPAD_DOWN, button::DPAD_DOWN),
        (SDL_GAMEPAD_BUTTON_DPAD_LEFT, button::DPAD_LEFT),
        (SDL_GAMEPAD_BUTTON_DPAD_RIGHT, button::DPAD_RIGHT),
    ] {
        st.set(bit, SDL_GetGamepadButton(gp, b));
    }
    let neg = |v: i16| v.saturating_neg();
    let trig = |v: i16| ((v.max(0) as i32) >> 7).min(255) as u8;
    st.left_x = SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_LEFTX);
    st.left_y = neg(SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_LEFTY));
    st.right_x = SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_RIGHTX);
    st.right_y = neg(SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_RIGHTY));
    st.left_trigger = trig(SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_LEFT_TRIGGER));
    st.right_trigger = trig(SDL_GetGamepadAxis(gp, SDL_GAMEPAD_AXIS_RIGHT_TRIGGER));
    st
}

/// Drive one rumble command onto the right open pad: the GUID-matched pad (exact pad the
/// host addressed) first, else the `slot`-th open pad (stable by uuid).
fn apply_rumble(open: &HashMap<u32, OpenPad>, cmd: &RumbleCmd) {
    let target = cmd
        .guid
        .as_deref()
        .and_then(|g| open.values().find(|p| p.uuid == g))
        .or_else(|| {
            let mut pads: Vec<&OpenPad> = open.values().collect();
            pads.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            pads.into_iter().nth(cmd.slot as usize)
        });
    let Some(pad) = target else {
        tracing::debug!(slot = cmd.slot, guid = ?cmd.guid, pads = open.len(), "rumble: no SDL pad for slot");
        return;
    };
    // SDL takes 16-bit magnitudes; our wire carries 8-bit per motor → scale up.
    let low = (cmd.large as u16) << 8;
    let high = (cmd.small as u16) << 8;
    let ok = unsafe { SDL_RumbleGamepad(pad.ptr, low, high, RUMBLE_MS) };
    tracing::debug!(slot = cmd.slot, name = %pad.name, large = cmd.large, small = cmd.small, ok, "rumble: SDL_RumbleGamepad");
}

/// Lowercase-hex of an SDL GUID's 16 bytes — the stable device key (matches gilrs `uuid()`).
fn guid_hex(g: &SDL_GUID) -> String {
    g.data.iter().map(|b| format!("{b:02x}")).collect()
}
