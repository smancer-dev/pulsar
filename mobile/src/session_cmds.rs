//! Session lifecycle commands: `end_session` + per-slot cancel/restream registry +
//! password-prompt race helpers (`pw_pending` map, `submit_password` command).
//!
//! Each active client session gets two channels stored in [`SessionRegistry`]:
//!
//! * A [`tokio::sync::Notify`] cancel token — `end_session` notifies it and the
//!   read loop (in `client.rs`) sees it via a `select!` and tears the session down
//!   immediately, then emits `play-ended`.
//!
//! * An `mpsc::Sender<StreamReq>` restream channel — the W3 quality commands push
//!   a new `StreamReq` onto it and the read loop re-calls `request_stream` + re-arms
//!   `start_stream` for the new SPS.
//!
//! ## W3-rust additions
//! - `PwPending` map: per-slot oneshot sender used by the auth race loop in
//!   `client.rs`. When the host replies `NeedPassword`, the read-setup code
//!   stores a `oneshot::Sender<String>` in this map and emits `auth-prompt`.
//!   The JS calls `submit_password`; this command pulls the sender, sends the
//!   password string, and the auth loop in `client.rs` receives it.
//! - `submit_password` Tauri command.
//! - `set_play_codec`, `set_play_bitrate`, `set_play_fps`, `set_play_resolution`,
//!   `set_play_quality`, `set_play_encoder` — each builds a `StreamReq` patch and
//!   pushes it onto the per-slot restream channel.
//!
//! ## W4-rust-client additions
//! - `my_id` (relay-assigned device ID) stored per-slot so `reverse_play` can
//!   read it from Tauri commands without touching the read loop.
//! - `mic_cancel` per-slot Notify so `mic_stop` can abort the running mic loop.
//! - `set_play_monitor` — sets `display_idx` on the restream `StreamReq` with a
//!   leading-edge ~400 ms debounce (collapses bursts to one restream).
//!
//! ## W5-rust-session additions
//! - [`ActivePane`] managed state: which slot is currently in focus for audio
//!   routing. The read loop in `client.rs` only calls `feed_audio` when the
//!   incoming slot equals `active_pane`. Input is still routed per-slot (each
//!   slot has its own `SessionSender` in `InputSenders`).
//! - `set_active_pane` command: JS calls this on tap-to-focus and session switch.
//!   Emits `active-pane-changed { slot }` so the JS switcher can update its UI.
//! - [`ClaimedDisplays`] managed state: maps `(host_target, display_idx)` to the
//!   slot that claimed it. `connect_host` uses this to pick a distinct `display_idx`
//!   per pane when two split panes target the same host (freeDisplayFor logic).
//!   Cleared when the slot is removed in `SessionRegistry::remove_with_display`.
//! - `request_keyframe` command: sends `DataMsg::MediaNack([0])` (sequence 0 is
//!   the sentinel that asks the host for a fresh IDR / keyframe) via the per-slot
//!   `SessionSender`.  Called from JS when the native plugin emits `decoder-error`
//!   (W5-native lane).  If the host does not support `FEAT_NACK` the host silently
//!   ignores it; the fallback is a full restream triggered by `set_play_codec`
//!   with the current codec.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pulsar_core::service::{DataMsg, QualityPref, StreamReq};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::Notify;

/// Debounce window for monitor switches: if the user taps the monitor picker
/// rapidly, we collapse bursts to a leading restream + a final settling one,
/// matching the desktop `MON_COOLDOWN_MS = 400`.
const MONITOR_DEBOUNCE: Duration = Duration::from_millis(400);

// ── Per-slot session control handles ─────────────────────────────────────────

/// Holds the cancel and restream handles for one active slot.
pub struct SessionControl {
    /// Notify this to tear the read loop down immediately (see `client.rs`).
    pub cancel: Arc<Notify>,
    /// Push a new `StreamReq` here to trigger an in-session restream.
    /// The sender is stored here; the receiver is held by the read loop.
    pub restream_tx: tokio::sync::mpsc::Sender<StreamReq>,
    /// The last `StreamReq` that was sent for this slot — used by quality commands
    /// to produce a patch (copy the previous, override the changed field).
    pub last_req: Option<StreamReq>,
    /// Relay-assigned device ID for this session (stored so `reverse_play` can
    /// build `DataMsg::ReverseRequest(myId)` from any Tauri command without
    /// access to the read loop's local variables).
    pub my_id: u32,
    /// Cancel the running mic loop for this slot (`mic_stop` notifies this).
    pub mic_cancel: Arc<Notify>,
    /// Timestamp of the last successfully dispatched monitor-switch restream.
    /// Used by `set_play_monitor` to enforce the ~400 ms debounce.
    pub last_monitor_switch: Option<Instant>,
    /// W5: the normalised target string (`DeviceId` or `IP:port`) this slot is
    /// connected to. Used by [`ClaimedDisplays`] to find panes targeting the same
    /// host.
    pub host_target: String,
    /// W5: whether FEAT_NACK was advertised by the host for this slot. If true,
    /// `request_keyframe` can send `DataMsg::MediaNack` to nudge an IDR.
    pub feat_nack: bool,
}

// ── Managed state: session registry ──────────────────────────────────────────

/// Map from slot index → session control handles. Inserted by `client.rs`
/// immediately before spawning the read loop; removed on session end.
#[derive(Default)]
pub struct SessionRegistry(pub Mutex<HashMap<u8, SessionControl>>);

impl SessionRegistry {
    /// Register a new slot and return the receiver end of the restream channel
    /// so the read loop can `select!` on it.
    ///
    /// `my_id` is the relay-assigned device ID for this session (needed by
    /// `reverse_play` which builds `DataMsg::ReverseRequest(myId_string)`).
    /// `host_target` is the normalised connect target string (stored for the
    /// W5 `ClaimedDisplays` same-host detection).
    /// `feat_nack` indicates whether the host advertised `FEAT_NACK` in its
    /// `StreamCaps`, enabling `DataMsg::MediaNack`-based IDR requests.
    pub fn register(
        &self,
        slot: u8,
        my_id: u32,
        host_target: String,
        feat_nack: bool,
    ) -> (Arc<Notify>, tokio::sync::mpsc::Receiver<StreamReq>, Arc<Notify>) {
        let cancel = Arc::new(Notify::new());
        let mic_cancel = Arc::new(Notify::new());
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctrl = SessionControl {
            cancel: cancel.clone(),
            restream_tx: tx,
            last_req: None,
            my_id,
            mic_cancel: mic_cancel.clone(),
            last_monitor_switch: None,
            host_target,
            feat_nack,
        };
        {
            let mut g = self.0.lock().unwrap();
            // Reconnect into a slot whose previous read loop is still alive: cancel
            // it so its `Node` drops and stops re-registering with the relay. Each
            // connect binds a NEW `Node` but registers under the SAME identity-derived
            // device id, so two live nodes make the relay's device address flap
            // between them on every heartbeat — which breaks session routing and makes
            // the NEW session die at the host's ~6 s PEER_TIMEOUT. (The superseded
            // loop's teardown is ownership-guarded via `is_owner`, so it will NOT
            // remove the entry we insert right below.)
            if let Some(prev) = g.get(&slot) {
                prev.cancel.notify_one();
            }
            g.insert(slot, ctrl);
        }
        (cancel, rx, mic_cancel)
    }

    /// Remove a slot's entry (called from the read loop when it exits).
    pub fn remove(&self, slot: u8) {
        self.0.lock().unwrap().remove(&slot);
    }

    /// The cancel [`Notify`] for `slot`'s current read loop, if any. `connect_host`
    /// fires this at the TOP of a (re)connect — before binding the new Node — so the
    /// previous loop's Node drops and stops flapping the relay device address during
    /// the new connect's setup window. Clones the `Arc` so the registry lock is not
    /// held across the caller's `await`.
    pub fn cancel_for(&self, slot: u8) -> Option<Arc<Notify>> {
        self.0.lock().unwrap().get(&slot).map(|c| c.cancel.clone())
    }

    /// True iff `slot`'s current control is the one created with `cancel` (compared
    /// by `Arc` identity). A read loop calls this at teardown so a loop that was
    /// superseded by a reconnect — its slot re-registered under a NEW cancel token —
    /// skips the slot-keyed cleanup and does NOT clobber the live session that
    /// replaced it (its registry entry, decoder, active pane, or play-ended event).
    pub fn is_owner(&self, slot: u8, cancel: &Arc<Notify>) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(&slot)
            .map(|c| Arc::ptr_eq(&c.cancel, cancel))
            .unwrap_or(false)
    }

    /// Update the last known `StreamReq` for `slot`. The read loop calls this
    /// after successfully applying a restream so quality commands can build patches.
    pub fn update_last_req(&self, slot: u8, req: StreamReq) {
        if let Some(ctrl) = self.0.lock().unwrap().get_mut(&slot) {
            ctrl.last_req = Some(req);
        }
    }

    /// Return the relay-assigned device ID for `slot`, or `None` if `slot` has
    /// no active session.  Used by `reverse_play`.
    pub fn my_id(&self, slot: u8) -> Option<u32> {
        self.0.lock().unwrap().get(&slot).map(|c| c.my_id)
    }

    /// Return the mic-cancel [`Notify`] for `slot`, or `None` if `slot` has no
    /// active session.  Used by `mic_stop`.
    pub fn mic_cancel(&self, slot: u8) -> Option<Arc<Notify>> {
        self.0.lock().unwrap().get(&slot).map(|c| c.mic_cancel.clone())
    }

    /// W5: return whether the host for `slot` advertised `FEAT_NACK`.
    /// Used by `request_keyframe` to decide whether a `MediaNack` is worth sending.
    pub fn feat_nack(&self, slot: u8) -> bool {
        self.0.lock().unwrap().get(&slot).map(|c| c.feat_nack).unwrap_or(false)
    }

    /// W5: return the host target string for `slot` (for [`ClaimedDisplays`] lookup).
    pub fn host_target(&self, slot: u8) -> Option<String> {
        self.0.lock().unwrap().get(&slot).map(|c| c.host_target.clone())
    }
}

// ── Managed state: active pane ───────────────────────────────────────────────

/// Which slot is currently in the foreground (receives audio from the read loop
/// and is the one the user's touches implicitly target in game mode). Defaults
/// to slot 0. JS updates this via `set_active_pane`; the read loop checks it
/// in `client.rs` before forwarding audio AU to the plugin.
///
/// This is `AtomicU8` rather than a `Mutex` so the read loop can read it
/// without locking in a hot path.
pub struct ActivePane(pub std::sync::atomic::AtomicU8);

impl Default for ActivePane {
    fn default() -> Self {
        // Slot 0 is always the initial active pane (the first and only
        // session until the user triggers a split).
        Self(std::sync::atomic::AtomicU8::new(0))
    }
}

impl ActivePane {
    pub fn get(&self) -> u8 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set(&self, slot: u8) {
        self.0.store(slot, std::sync::atomic::Ordering::Relaxed);
    }
}

// ── Managed state: claimed display map ───────────────────────────────────────

/// Maps `(host_target, display_idx)` → `slot`. Prevents two split panes
/// targeting the same host from requesting the same monitor, mirroring the
/// desktop `freeDisplayFor` logic in `split.svelte.ts`.
///
/// `connect_host` (W5 path in `client.rs`) calls `claim` before building the
/// `StreamReq` to get a free `display_idx` for the new slot. When the slot
/// ends its read loop calls `release` to free the claim.
#[derive(Default)]
pub struct ClaimedDisplays(pub Mutex<HashMap<(String, u32), u8>>);

impl ClaimedDisplays {
    /// Find a `display_idx` not yet claimed by any other slot targeting the
    /// same `host_target`, claim it for `slot`, and return it.
    ///
    /// The algorithm tries indices 0, 1, 2, … until it finds one that is either
    /// unclaimed or claimed by `slot` itself (idempotent re-claim on reconnect).
    /// This mirrors the desktop `nextFreeDisplay` / `freeDisplayFor` pattern.
    pub fn claim(&self, slot: u8, host_target: &str) -> u32 {
        let mut map = self.0.lock().unwrap();
        for idx in 0u32..8 {
            let key = (host_target.to_string(), idx);
            match map.get(&key) {
                None => {
                    map.insert(key, slot);
                    return idx;
                }
                Some(&owner) if owner == slot => return idx,
                _ => {} // claimed by another slot — try next
            }
        }
        // Fallback (>8 panes impossible in practice): use the slot number.
        let fallback = slot as u32;
        map.insert((host_target.to_string(), fallback), slot);
        fallback
    }

    /// Release all display claims held by `slot`.
    pub fn release(&self, slot: u8) {
        let mut map = self.0.lock().unwrap();
        map.retain(|_, &mut owner| owner != slot);
    }
}

// ── Managed state: password pending map ──────────────────────────────────────

/// Per-slot oneshot channel for the auth-password race.
///
/// When the auth race loop in `client.rs` receives `HostAuth::NeedPassword` it
/// creates a `oneshot::channel<String>()`, stores the sender here (keyed by
/// slot), and emits `auth-prompt { slot, peer }`. When the user submits their
/// password via `submit_password`, that command extracts the sender from this
/// map and forwards the string so the auth loop can call `send_auth` and
/// continue polling `recv_host_auth`.
#[derive(Default)]
pub struct PwPending(pub Mutex<HashMap<u8, tokio::sync::oneshot::Sender<String>>>);

// ── Commands ──────────────────────────────────────────────────────────────────

/// Cancel the read loop for `slot` and tear the session down cleanly.
///
/// The read loop will:
///   1. See the cancel `Notify` fire (in a `select!`).
///   2. Send `Bye` to the host (so the host tears down immediately, not after 6s).
///   3. Emit `play-ended { slot, reason: "cancelled" }`.
///   4. Call `pulsar_video().stop_stream(slot)` and remove the sender.
///
/// If `slot` has no active session this is a no-op.
///
/// JS: `invoke('end_session', { slot: 0 })`
#[tauri::command]
pub async fn end_session<R: Runtime>(app: AppHandle<R>, slot: u8) -> Result<(), String> {
    let cancel = {
        let reg = app.state::<SessionRegistry>();
        let g = reg.0.lock().unwrap();
        g.get(&slot).map(|c| c.cancel.clone())
    };
    if let Some(c) = cancel {
        c.notify_one();
    }
    Ok(())
}

/// Feed the user-supplied password into the auth race loop for `slot`.
///
/// The client must call this only after receiving an `auth-prompt` event for
/// the slot. If no auth race is pending for that slot (e.g. after timeout or
/// successful auth), this is a no-op.
///
/// JS: `invoke('submit_password', { slot: 0, password: 'xxxx-xxxx' })`
#[tauri::command]
pub async fn submit_password<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    password: String,
) -> Result<(), String> {
    let tx = {
        let pw_map = app.state::<PwPending>();
        let mut guard = pw_map.0.lock().unwrap();
        guard.remove(&slot)
    };
    if let Some(tx) = tx {
        // Ignore send error — the receiver may have timed out already.
        let _ = tx.send(password);
    }
    Ok(())
}

// ── Restream quality commands ─────────────────────────────────────────────────

/// Push a new `StreamReq` onto the per-slot restream channel.
///
/// The read loop `select!`s on this channel; on receipt it re-calls
/// `request_stream` and re-arms `start_stream` for the new SPS.
///
/// Internal helper shared by the `set_play_*` commands below.
pub(crate) async fn push_restream<R: Runtime>(
    app: &AppHandle<R>,
    slot: u8,
    req: StreamReq,
) -> Result<(), String> {
    let tx = {
        let reg = app.state::<SessionRegistry>();
        let g = reg.0.lock().unwrap();
        g.get(&slot)
            .map(|c| c.restream_tx.clone())
            .ok_or_else(|| format!("no active session on slot {slot}"))?
    };
    tx.send(req).await.map_err(|e| format!("restream send failed: {e}"))
}

/// Build a patch `StreamReq` based on the last known request for `slot`.
///
/// Returns `Err` if no session is active for `slot` or no prior `StreamReq`
/// has been recorded yet (the initial `StreamReq` is stored by the read loop
/// after the first successful `request_stream`).
fn build_patch<R: Runtime>(app: &AppHandle<R>, slot: u8) -> Result<StreamReq, String> {
    app.state::<SessionRegistry>()
        .0
        .lock()
        .unwrap()
        .get(&slot)
        .and_then(|c| c.last_req.clone())
        .ok_or_else(|| format!("no active session / no prior stream req for slot {slot}"))
}

/// Change the video codec (`"auto"` / `"h264"` / `"h265"`) for the active stream
/// on `slot`. The host will re-start its encoder; the client decoder is re-armed.
///
/// JS: `invoke('set_play_codec', { slot: 0, codec: 'h265' })`
#[tauri::command]
pub async fn set_play_codec<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    codec: String,
) -> Result<(), String> {
    let mut req = build_patch(&app, slot)?;
    req.codec = codec.clone();
    req.decode_codecs = vec![codec];
    push_restream(&app, slot, req).await
}

/// Change the encode bitrate (kbps) for the active stream on `slot`.
///
/// JS: `invoke('set_play_bitrate', { slot: 0, kbps: 8000 })`
#[tauri::command]
pub async fn set_play_bitrate<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    kbps: u32,
) -> Result<(), String> {
    let mut req = build_patch(&app, slot)?;
    req.bitrate_kbps = kbps;
    push_restream(&app, slot, req).await
}

/// Change the target frame-rate for the active stream on `slot`.
///
/// JS: `invoke('set_play_fps', { slot: 0, fps: 60 })`
#[tauri::command]
pub async fn set_play_fps<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    fps: u32,
) -> Result<(), String> {
    let mut req = build_patch(&app, slot)?;
    req.fps = fps;
    push_restream(&app, slot, req).await
}

/// Change the target resolution for the active stream on `slot`.
///
/// JS: `invoke('set_play_resolution', { slot: 0, width: 1280, height: 720 })`
#[tauri::command]
pub async fn set_play_resolution<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let mut req = build_patch(&app, slot)?;
    req.width = width;
    req.height = height;
    push_restream(&app, slot, req).await
}

/// Change the quality preference (`"latency"` / `"balanced"` / `"quality"`) for
/// the active stream on `slot`.
///
/// JS: `invoke('set_play_quality', { slot: 0, pref: 'latency' })`
#[tauri::command]
pub async fn set_play_quality<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    pref: String,
) -> Result<(), String> {
    let q = match pref.as_str() {
        "latency" => QualityPref::Latency,
        "quality" => QualityPref::Quality,
        _ => QualityPref::Balanced,
    };
    let mut req = build_patch(&app, slot)?;
    req.quality = q;
    push_restream(&app, slot, req).await
}

/// Change the encoder hint (`"auto"` / `"nvenc"` / `"vaapi"` / etc.) for the
/// active stream on `slot`.
///
/// JS: `invoke('set_play_encoder', { slot: 0, encoder: 'nvenc' })`
#[tauri::command]
pub async fn set_play_encoder<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    encoder: String,
) -> Result<(), String> {
    let mut req = build_patch(&app, slot)?;
    req.encoder = encoder;
    push_restream(&app, slot, req).await
}

/// Switch the streamed monitor to `display_idx` for `slot`.
///
/// This is a remote-desktop-only command (gated in JS; never callable in game
/// mode). A leading-edge debounce of ~400 ms collapses rapid taps in the monitor
/// picker to at most one in-flight restream per 400 ms window (first tap fires
/// immediately; subsequent taps within the cooldown are silently ignored). This
/// matches the desktop's `MON_COOLDOWN_MS = 400` pattern.
///
/// After the restream the native decoder is re-armed by the read loop (via the
/// shared restream channel) so the new SPS/resolution is picked up automatically.
///
/// JS: `invoke('set_play_monitor', { slot: 0, displayIdx: 1 })`
#[tauri::command]
pub async fn set_play_monitor<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    display_idx: u32,
) -> Result<(), String> {
    // ── Debounce: check + update the last switch timestamp ───────────────────
    {
        let reg = app.state::<SessionRegistry>();
        let mut guard = reg.0.lock().unwrap();
        let ctrl = guard
            .get_mut(&slot)
            .ok_or_else(|| format!("no active session on slot {slot}"))?;

        let now = Instant::now();
        if let Some(last) = ctrl.last_monitor_switch {
            if now.duration_since(last) < MONITOR_DEBOUNCE {
                // Within the cooldown window — silently discard this tap.
                return Ok(());
            }
        }
        // Leading edge: record the timestamp before dispatching.
        ctrl.last_monitor_switch = Some(now);
    }

    // ── Build + push the patched StreamReq ───────────────────────────────────
    let mut req = build_patch(&app, slot)?;
    req.display_idx = display_idx;
    push_restream(&app, slot, req).await
}

// ── W5-rust-session commands ──────────────────────────────────────────────────

/// Set the active foreground pane to `slot`.
///
/// The read loop in `client.rs` only routes audio (`feed_audio`) to the slot
/// returned by `ActivePane::get()`. Input is always per-slot (each slot has
/// its own `SessionSender` in `InputSenders`), so only audio routing changes.
///
/// Emits `active-pane-changed { slot }` so the JS session switcher can update
/// its pill/indicator without polling.
///
/// JS: `invoke('set_active_pane', { slot: 1 })`
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ActivePaneChangedPayload {
    pub slot: u8,
}

#[tauri::command]
pub async fn set_active_pane<R: Runtime>(app: AppHandle<R>, slot: u8) -> Result<(), String> {
    app.state::<ActivePane>().set(slot);
    let _ = app.emit("active-pane-changed", ActivePaneChangedPayload { slot });
    Ok(())
}

/// Request a fresh IDR (keyframe) from the host for `slot`.
///
/// This is the Rust half of W5 decoder recovery. When the native plugin emits
/// a `decoder-error { slot }` event (W5-native lane), the JS layer should call
/// this command. It sends `DataMsg::MediaNack([0])` — sequence 0 is the
/// conventional sentinel value that signals "please emit an IDR" rather than
/// a true packet-loss retransmit — over the per-slot `SessionSender`.
///
/// If the host does NOT advertise `FEAT_NACK` in its `StreamCaps`, the
/// `MediaNack` message is silently ignored by the host and the method falls
/// back to a full restream (`set_play_codec` with the current codec). This
/// forces a host encoder reset + fresh SPS/PPS/IDR without changing any other
/// parameter.
///
/// JS: `invoke('request_keyframe', { slot: 0 })`
#[tauri::command]
pub async fn request_keyframe<R: Runtime>(app: AppHandle<R>, slot: u8) -> Result<(), String> {
    use pulsar_core::service::send_data_via;

    let (sender, feat_nack) = {
        let senders = app.state::<crate::client::InputSenders>();
        let guard = senders.0.lock().unwrap();
        let s = guard.get(&slot).cloned();
        let nack = app.state::<SessionRegistry>().feat_nack(slot);
        (s, nack)
    };

    if let Some(s) = sender {
        if feat_nack {
            // Primary path: NACK sequence 0 → host emits IDR immediately.
            send_data_via(&s, &DataMsg::MediaNack(vec![0]))
                .await
                .map_err(|e| format!("request_keyframe nack failed: {e:?}"))?;
        } else {
            // Fallback: trigger a full restream to get a fresh SPS/IDR.
            // Build a patch with the current codec (no other field changes).
            let req = build_patch(&app, slot)?;
            push_restream(&app, slot, req).await?;
        }
    }
    Ok(())
}
