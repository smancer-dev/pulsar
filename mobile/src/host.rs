//! Mobile host (W3-host + W5-rust-host): this device registers with the relay
//! and accepts incoming sessions, so another device can connect to THIS phone
//! and see its screen.
//!
//! ## Lifecycle (W3-host additions)
//!
//! * `go_online` — re-runnable: aborts any previous accept loop, registers a
//!   fresh Node, generates a new OTP, stores [`HostState`].
//! * `go_offline` — cancels the accept loop, drops the Node, calls `stop_host`.
//! * `new_password` — rotates the OTP in-place (`Arc<Mutex<String>>`), emits
//!   `host-password`.
//! * `respond_request` — resolves the per-incoming approval oneshot (feeds the
//!   `session-request` / `respond_request` race). Auto-deny fires after 30 s.
//! * `disconnect_session` — kicks an active peer by `sid`.
//! * `open_a11y_settings` / `a11y_enabled` — plugin passthrough (unchanged).
//!
//! ## W5-rust-host additions
//!
//! * `host_codecs` — probe the Android `MediaCodecList` via the native plugin
//!   (W5-native adds `PulsarVideo::host_codecs()`) and cache the result in the
//!   managed [`HostCaps`] state.  `go_online`'s `StreamCaps` closure reads this
//!   cache so it advertises the real codec set (h265 > h264, AV1 when present)
//!   instead of the previous hard-coded `["h264"]`.
//!
//! ## Events emitted (Rust → JS)
//!
//! | Event | Payload |
//! |---|---|
//! | `host-password` | `{password:String}` |
//! | `session-request` | `{reqId:u32, peer:String, hasPassword:bool}` |
//! | `host-peer-connected` | `{sid:u32, peer:String}` |
//! | `host-peer-disconnected` | `{sid:u32}` |

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_pulsar_video::PulsarVideoExt;
use tokio::sync::{oneshot, Notify};

use pulsar_core::service::{
    accept, gen_password, media, need_password, recv_auth, reject, serve_with, DataHandlers,
    InputEvent, StreamCaps,
};
use crate::client::parse_mode;
use crate::config::load_config;

// ── Session ID counter ────────────────────────────────────────────────────────

/// Monotonically-increasing session ID. Each incoming connection gets a unique u32
/// so the JS side can track / kick individual peers.
static NEXT_SID: AtomicU32 = AtomicU32::new(1);

fn next_sid() -> u32 {
    NEXT_SID.fetch_add(1, Ordering::Relaxed)
}

// ── Active stream counter ─────────────────────────────────────────────────────

/// How many peer sessions are currently streaming. The native capture is only
/// stopped when the last session ends, so an overlapping reconnect never kills
/// a live MediaProjection+encoder that a freshly-connected session is using.
static ACTIVE_STREAMS: AtomicUsize = AtomicUsize::new(0);

// ── W5-rust-host: Host codec capabilities ────────────────────────────────────

/// Cached result of the Android `MediaCodecList` probe (W5-native provides the
/// plugin bridge; see `host_codecs`).
///
/// The cache is populated by an explicit JS call to `host_codecs` (typically on
/// the Cihazım screen before going online), and then consumed by every
/// `go_online` accept loop when building `StreamCaps`.  A `None` cache means
/// the probe has never been run: `go_online` falls back to `["h264"]` so
/// existing unprobed devices keep working.
#[derive(Default)]
pub struct HostCaps {
    /// Ordered codec list, best first (h265 before h264, av1 when present).
    /// `None` = probe not yet run; `Some([])` = probe ran but returned nothing
    /// (treat as h264-only fallback).
    pub codecs: Mutex<Option<Vec<String>>>,
}

/// `host_codecs` return type.
#[derive(Serialize, Deserialize, Clone)]
pub struct HostCodecsResult {
    pub codecs: Vec<String>,
}

/// Parse a JSON string array without `serde_json`.
///
/// Handles the compact form returned by the Kotlin bridge:
/// `["video/hevc","video/avc"]` or a comma-separated plain list
/// `video/hevc,video/avc` as a convenience for tests.
fn parse_string_array(s: &str) -> Vec<String> {
    let trimmed = s.trim();
    // Strip outer `[` … `]` if present.
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    inner
        .split(',')
        .filter_map(|token| {
            let t = token.trim().trim_matches('"').trim_matches('\'').trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        })
        .collect()
}

/// Order a raw codec list so that higher-quality codecs appear first.
///
/// Priority: AV1 > H.265/HEVC > H.264/AVC.  Unknown codecs are appended in
/// their original order after the known ones.
fn prioritise_codecs(raw: &[String]) -> Vec<String> {
    let mut av1: Vec<&String> = Vec::new();
    let mut h265: Vec<&String> = Vec::new();
    let mut h264: Vec<&String> = Vec::new();
    let mut other: Vec<&String> = Vec::new();

    for c in raw {
        let lower = c.to_lowercase();
        if lower == "av1" || lower == "video/av01" {
            av1.push(c);
        } else if lower == "h265" || lower == "hevc" || lower == "video/hevc" {
            h265.push(c);
        } else if lower == "h264" || lower == "avc" || lower == "video/avc" {
            h264.push(c);
        } else {
            other.push(c);
        }
    }

    let mut result: Vec<String> = Vec::new();
    for c in av1.iter().chain(h265.iter()).chain(h264.iter()).chain(other.iter()) {
        // Normalise: always use the short names the wire protocol and the JS
        // codec segment use ("h265", "h264", "av1").
        let normalised = normalise_codec_name(c);
        if !result.contains(&normalised) {
            result.push(normalised);
        }
    }

    // Guarantee at least h264 as a universal fallback.
    if result.is_empty() {
        result.push("h264".to_string());
    } else if !result.iter().any(|c| c == "h264") {
        result.push("h264".to_string());
    }

    result
}

/// Convert a raw codec name (MIME or short) to the canonical short name used
/// by `StreamReq::codec` and the JS segmented control.
fn normalise_codec_name(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "video/av01" | "av01" => "av1".to_string(),
        "video/hevc" | "hevc" => "h265".to_string(),
        "video/avc" | "avc" => "h264".to_string(),
        other => other.to_string(),
    }
}

/// Probe the Android `MediaCodecList` for supported hardware video encoders and
/// return an ordered codec list (best first: AV1 > H.265 > H.264).
///
/// ## How the probe works (cross-lane dependency)
///
/// The actual MediaCodecList enumeration is performed by the W5-native lane, which
/// adds:
///   - `@Command fun hostCodecs(invoke: Invoke)` to `PulsarVideoPlugin.kt`
///     (iterates `MediaCodecList.getCodecInfos()`, filters to hardware encoders for
///     `video/hevc`, `video/av01`, `video/avc`, returns a JSON array in
///     `detail`, e.g. `["video/hevc","video/avc"]`).
///   - `PulsarVideo::host_codecs(&self)` to both `mobile.rs` and `desktop.rs`
///     (`desktop.rs` returns `noop("no MediaCodecList on desktop")`).
///
/// Once those additions are present, replace the stub body below with:
/// ```rust
/// let raw_codecs: Vec<String> = match app.pulsar_video().host_codecs() {
///     Ok(resp) if resp.ok && !resp.detail.is_empty() => {
///         parse_string_array(&resp.detail)
///     }
///     _ => {
///         log::info!("pulsar host: host_codecs probe unavailable, using h264 fallback");
///         vec!["h264".to_string()]
///     }
/// };
/// ```
///
/// ## Current behaviour (pre-W5-native stub)
///
/// Until the W5-native lane lands, `host_codecs` returns the codec list that is
/// already stored in the [`HostCaps`] cache (populated by an earlier call or by
/// external means), falling back to `["h264"]`.  This allows the `go_online` path
/// to consume a dynamically-built `StreamCaps` even before the native probe is
/// wired in.
///
/// The result is cached in the managed [`HostCaps`] state and also returned to
/// the caller so the JS can update the Cihazım screen immediately.
///
/// JS: `invoke('host_codecs', {})` → `{codecs: string[]}`
#[tauri::command]
pub async fn host_codecs<R: Runtime>(app: AppHandle<R>) -> Result<HostCodecsResult, String> {
    // ── W5-native hook ────────────────────────────────────────────────────────
    //
    // Once the W5-native lane adds `PulsarVideo::host_codecs()` to both
    // `crates/tauri-plugin-pulsar-video/src/mobile.rs` (real probe) and
    // `desktop.rs` (no-op), replace this entire block with the plugin call
    // shown in the doc-comment above.  The rest of the function (prioritise /
    // cache / return) is already correct and does not need to change.
    //
    // For now: read whatever is already in the cache (possibly seeded externally
    // in tests or by a previous call) and fall back to ["h264"].
    let caps_state = app.state::<HostCaps>();
    let existing = caps_state.codecs.lock().unwrap().clone();

    let raw_codecs: Vec<String> = existing.unwrap_or_else(|| {
        log::info!("pulsar host: host_codecs probe not yet wired (W5-native pending), using h264 fallback");
        vec!["h264".to_string()]
    });
    // ── end W5-native hook ────────────────────────────────────────────────────

    let ordered = prioritise_codecs(&raw_codecs);

    // Write back the ordered list (no-op if it was already ordered, but ensures
    // the canonical normalised form is stored).
    *caps_state.codecs.lock().unwrap() = Some(ordered.clone());

    log::info!("pulsar host: codec list = {:?}", ordered);
    Ok(HostCodecsResult { codecs: ordered })
}

/// Populate the [`HostCaps`] cache directly from a raw MIME list string returned
/// by the native plugin bridge (called by `host_codecs_from_plugin` once the
/// W5-native lane wires in the `PulsarVideo::host_codecs()` method).
///
/// This helper is `pub(crate)` so it can also be called from a future wrapper
/// without going through the Tauri command layer.
pub(crate) fn seed_host_caps(caps: &HostCaps, raw_detail: &str) {
    let raw = parse_string_array(raw_detail);
    let ordered = prioritise_codecs(&raw);
    *caps.codecs.lock().unwrap() = Some(ordered);
}

/// Read the cached probed codec list, or return the h264-only fallback when the
/// probe has not been run yet.  Called from the `go_online` `StreamCaps` closure.
fn cached_codecs(caps: &HostCaps) -> Vec<String> {
    caps.codecs
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| vec!["h264".to_string()])
}

// ── Managed host state ────────────────────────────────────────────────────────

/// Per-peer kick handle. Notifying it causes the session task to exit cleanly.
pub(crate) struct PeerHandle {
    pub(crate) kick: Arc<Notify>,
}

/// Managed state for the host accept loop. A fresh `Arc<HostState>` is created
/// (and stored via the `Mutex<Arc<HostState>>` managed type) each time `go_online`
/// is called, cancelling the previous loop.
pub struct HostState {
    /// Cancel the current accept loop (notifying it exits the accept `select!`).
    pub cancel: Arc<Notify>,
    /// The rotating one-time password shown on the Cihazım screen.
    pub otp: Arc<Mutex<String>>,
    /// Active peer sessions: sid → kick handle.
    pub(crate) peers: Mutex<HashMap<u32, PeerHandle>>,
    /// Per-incoming-request approval oneshots: reqId → sender.
    pub pending: Mutex<HashMap<u32, oneshot::Sender<bool>>>,
    /// Monotonically-increasing request ID for the current accept loop.
    pub next_req: AtomicU32,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            cancel: Arc::new(Notify::new()),
            otp: Arc::new(Mutex::new(String::new())),
            peers: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU32::new(1),
        }
    }
}

// ── Event payloads ─────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct HostPasswordPayload {
    password: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SessionRequestPayload {
    req_id: u32,
    peer: String,
    has_password: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PeerConnectedPayload {
    sid: u32,
    peer: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PeerDisconnectedPayload {
    sid: u32,
}

// ── go_online result ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct OnlineResult {
    pub ok: bool,
    pub id: u32,
    pub password: String,
}

// ── Auth approval race (30-second auto-deny) ──────────────────────────────────

/// Host-side auth race for one incoming session:
///
/// 1. Read the client's first `Auth` message.
/// 2. If it matches the OTP or standing connect-password → accept immediately.
/// 3. Otherwise send `NeedPassword` (so the client prompts), emit `session-request`,
///    then race: UI `respond_request` **vs.** 30-second auto-deny.
///
/// Returns `true` if the session should proceed.
async fn approval_race<R: Runtime>(
    app: &AppHandle<R>,
    sess: &mut pulsar_core::Session,
    host_state: &Arc<HostState>,
    peer: &str,
    connect_password: &str,
) -> bool {
    // Always read the initial Auth first.
    let first_pw = recv_auth(sess).await.unwrap_or_default();

    let otp = host_state.otp.lock().unwrap().clone();

    // Fast path: correct OTP.
    if !otp.is_empty() && first_pw == otp {
        let _ = accept(sess).await;
        // Rotate the OTP after a successful one-time use.
        let new_pw = gen_password();
        *host_state.otp.lock().unwrap() = new_pw.clone();
        let _ = app.emit("host-password", HostPasswordPayload { password: new_pw });
        return true;
    }

    // Fast path: standing connect-password.
    if !connect_password.is_empty() && first_pw == connect_password {
        let _ = accept(sess).await;
        return true;
    }

    // Slow path: tell the client a password is required, show the approval sheet,
    // then race the UI vs. auto-deny.
    let _ = need_password(sess).await;

    // Emit session-request so the JS shows the approval bottom-sheet.
    let req_id = host_state.next_req.fetch_add(1, Ordering::Relaxed);
    let has_pw = !first_pw.is_empty();
    let _ = app.emit(
        "session-request",
        SessionRequestPayload {
            req_id,
            peer: peer.to_string(),
            has_password: has_pw,
        },
    );
    // Also raise a high-priority Android notification: the JS approval sheet is
    // invisible when the app is backgrounded or the screen is off, so a host phone
    // sitting idle would otherwise give the user no alert. Tapping it relaunches the
    // app to the still-pending sheet (30 s window). No-op on desktop.
    let _ = app.pulsar_video().notify_request(peer);

    // Create the approval oneshot and store it so `respond_request` can resolve it.
    let (tx, rx) = oneshot::channel::<bool>();
    host_state.pending.lock().unwrap().insert(req_id, tx);

    // Race: UI response OR 30-second auto-deny.
    let allowed = tokio::select! {
        result = rx => result.unwrap_or(false),
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            host_state.pending.lock().unwrap().remove(&req_id);
            false
        }
    };

    // Remove from pending (may already be gone if oneshot was consumed by select).
    host_state.pending.lock().unwrap().remove(&req_id);

    if allowed {
        let _ = accept(sess).await;
    } else {
        let _ = reject(sess).await;
    }

    allowed
}

// ── go_online ─────────────────────────────────────────────────────────────────

/// Register this device with the relay and start accepting incoming sessions.
///
/// Re-runnable: calling `go_online` while already online cancels the previous
/// accept loop and starts a fresh one with a new OTP.
///
/// `relay`, `name`, and `netmode` fall back to the persisted [`Config`] when empty.
///
/// JS: `invoke('go_online', { relay, name, netmode })` → `OnlineResult`
#[tauri::command]
pub async fn go_online<R: Runtime>(
    app: AppHandle<R>,
    relay: String,
    name: String,
    netmode: String,
) -> Result<OnlineResult, String> {
    // ── Config fallback ───────────────────────────────────────────────────────
    let cfg = load_config(&app);

    let relay_str = if relay.is_empty() { cfg.relay.clone() } else { relay };
    let relay_addr: SocketAddr = relay_str.parse().map_err(|e| format!("bad relay: {e}"))?;

    let dev_name = if name.is_empty() {
        if cfg.device_name.is_empty() { "Pulsar Telefon".to_string() } else { cfg.device_name.clone() }
    } else {
        name
    };
    let mode = if netmode.is_empty() { cfg.network_mode } else { parse_mode(&netmode) };

    let unattended = cfg.unattended_access;
    let connect_password = cfg.connect_password.clone();

    // ── Cancel any previous accept loop ──────────────────────────────────────
    {
        let prev = app.state::<Mutex<Arc<HostState>>>().lock().unwrap().clone();
        prev.cancel.notify_one();
    }

    // ── Build fresh OTP / HostState ───────────────────────────────────────────
    let password = if unattended && !connect_password.is_empty() {
        // Unattended with standing password: surface that password on the screen.
        connect_password.clone()
    } else {
        gen_password()
    };

    let new_state = Arc::new(HostState {
        cancel: Arc::new(Notify::new()),
        otp: Arc::new(Mutex::new(password.clone())),
        peers: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        next_req: AtomicU32::new(1),
    });
    *app.state::<Mutex<Arc<HostState>>>().lock().unwrap() = new_state.clone();

    // ── Shared node (bound + registered once; reused by client connects too) ──
    // Accept incoming on the SAME node the client role dials out on. A separate
    // host node would register a second socket under our identity and let the relay
    // misroute inbound connections to a client node with no accept loop — the
    // incoming-request popup then never fired. See `net.rs`. Re-running go_online
    // reuses this node (only the OTP + accept loop are refreshed).
    let (node, id) = crate::net::get_or_create_node(&app, relay_addr, mode, dev_name).await?;

    // Emit the initial password so the UI can display it immediately.
    let _ = app.emit("host-password", HostPasswordPayload { password: password.clone() });

    // ── Spawn accept loop ─────────────────────────────────────────────────────
    let app2 = app.clone();
    let cancel = new_state.cancel.clone();
    let state2 = new_state.clone();
    let conn_pw = connect_password.clone();

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = cancel.notified() => {
                    log::info!("pulsar host: accept loop cancelled");
                    break;
                }
                incoming = node.next_incoming() => {
                    let Some(mut sess) = incoming else {
                        log::info!("pulsar host: node closed, accept loop ending");
                        break;
                    };
                    let app3 = app2.clone();
                    let state3 = state2.clone();
                    let cpw = conn_pw.clone();
                    let unatt = unattended;

                    tauri::async_runtime::spawn(async move {
                        // Identify the peer by their UDP address.
                        let peer = sess.peer_addr()
                            .await
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| "?.?.?.?".to_string());

                        // ── Auth ──────────────────────────────────────────────
                        let authed = if unatt && cpw.is_empty() {
                            // Unattended with no password: auto-accept, skip OTP.
                            let _ = recv_auth(&mut sess).await;
                            let _ = accept(&mut sess).await;
                            true
                        } else {
                            approval_race(&app3, &mut sess, &state3, &peer, &cpw).await
                        };

                        if !authed {
                            log::info!("pulsar host: peer {peer} denied");
                            return;
                        }

                        // ── Assign a session ID ───────────────────────────────
                        let sid = next_sid();
                        let kick = Arc::new(Notify::new());
                        state3.peers.lock().unwrap().insert(sid, PeerHandle { kick: kick.clone() });

                        // Emit connected event (name not available from serve_with).
                        let _ = app3.emit("host-peer-connected", PeerConnectedPayload { sid, peer: peer.clone() });

                        // ── Media: loopback UDP → session ─────────────────────
                        let sender = sess.sender();
                        let (vsock, asock) = match (
                            tokio::net::UdpSocket::bind("127.0.0.1:0").await,
                            tokio::net::UdpSocket::bind("127.0.0.1:0").await,
                        ) {
                            (Ok(v), Ok(a)) => (v, a),
                            _ => {
                                state3.peers.lock().unwrap().remove(&sid);
                                let _ = app3.emit("host-peer-disconnected", PeerDisconnectedPayload { sid });
                                return;
                            }
                        };
                        let video_port = vsock.local_addr().map(|a| a.port()).unwrap_or(0);
                        let audio_port = asock.local_addr().map(|a| a.port()).unwrap_or(0);

                        let vsender = sender.clone();
                        let vfwd = tauri::async_runtime::spawn(async move {
                            let mut buf = vec![0u8; 4096];
                            while let Ok((n, _)) = vsock.recv_from(&mut buf).await {
                                if n > 0 {
                                    let _ = vsender.send(&media::frame(media::TAG_VIDEO, &buf[..n])).await;
                                }
                            }
                        });
                        let afwd = tauri::async_runtime::spawn(async move {
                            let mut buf = vec![0u8; 4096];
                            while let Ok((n, _)) = asock.recv_from(&mut buf).await {
                                if n > 0 {
                                    let _ = sender.send(&media::frame(media::TAG_AUDIO, &buf[..n])).await;
                                }
                            }
                        });

                        // ── Stream handler ────────────────────────────────────
                        let app4 = app3.clone();
                        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let started_s = started.clone();
                        let on_stream = move |req: pulsar_core::service::StreamReq, _addr: SocketAddr| {
                            if !started_s.swap(true, Ordering::SeqCst) {
                                ACTIVE_STREAMS.fetch_add(1, Ordering::SeqCst);
                            }
                            let app5 = app4.clone();
                            let codec = req.codec.clone();
                            let (w, h, fps, kbps) = (req.width, req.height, req.fps, req.bitrate_kbps);
                            let aport = if req.transmit_audio { audio_port } else { 0 };
                            tauri::async_runtime::spawn(async move {
                                let _ = app5.pulsar_video().start_host(video_port, aport, &codec, w, h, fps, kbps);
                            });
                        };

                        // ── Input handler ─────────────────────────────────────
                        let app_in = app3.clone();
                        let mut last = (0f64, 0f64);
                        let mut down_at: Option<(f64, f64)> = None;
                        let on_input = move |ev: InputEvent| match ev {
                            InputEvent::PointerMotion { x, y } => last = (x, y),
                            InputEvent::PointerButton { down: true, .. } => down_at = Some(last),
                            InputEvent::PointerButton { down: false, .. } => {
                                if let Some((sx, sy)) = down_at.take() {
                                    let (ex, ey) = last;
                                    let app = app_in.clone();
                                    tauri::async_runtime::spawn(async move {
                                        let _ = app.pulsar_video().host_gesture(sx, sy, ex, ey);
                                    });
                                }
                            }
                            _ => {}
                        };

                        // ── Stream caps (W5-rust-host: dynamic codec list) ─────
                        //
                        // Use the codec list probed by `host_codecs` (cached in
                        // HostCaps).  If the JS hasn't called `host_codecs` yet,
                        // `cached_codecs` returns the safe ["h264"] fallback so
                        // un-probed devices continue to work unchanged.
                        //
                        // The ordering from `prioritise_codecs` guarantees the
                        // connecting client sees h265 (or AV1) before h264, so it
                        // can negotiate the best codec its decoder supports.
                        let probed_codecs = {
                            let caps_state = app3.state::<HostCaps>();
                            cached_codecs(&caps_state)
                        };

                        let mut data = DataHandlers::default();
                        data.stream_caps = Box::new(move || StreamCaps {
                            codecs: probed_codecs.clone(),
                            encoders: vec!["mediacodec".to_string()],
                            features: vec![media::FEAT_MOS.to_string()],
                            ..Default::default()
                        });

                        // ── Serve (race against kick) ──────────────────────────
                        let kick2 = kick.clone();
                        tokio::select! {
                            _ = serve_with(
                                sess,
                                || Vec::<pulsar_core::service::GameInfo>::new(),
                                |_game_id: String| {},
                                on_stream,
                                on_input,
                                data,
                            ) => {}
                            _ = kick2.notified() => {
                                log::info!("pulsar host: kicked peer sid={sid} peer={peer}");
                            }
                        }

                        // ── Teardown ──────────────────────────────────────────
                        state3.peers.lock().unwrap().remove(&sid);
                        let _ = app3.emit("host-peer-disconnected", PeerDisconnectedPayload { sid });

                        if started.load(Ordering::SeqCst)
                            && ACTIVE_STREAMS.fetch_sub(1, Ordering::SeqCst) == 1
                        {
                            let _ = app3.pulsar_video().stop_host();
                        }
                        vfwd.abort();
                        afwd.abort();
                    });
                }
            }
        }
    });

    Ok(OnlineResult { ok: true, id: id.0, password })
}

// ── go_offline ────────────────────────────────────────────────────────────────

/// Stop accepting new sessions and kick all active peers.
///
/// JS: `invoke('go_offline', {})`
#[tauri::command]
pub async fn go_offline<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let hs = app.state::<Mutex<Arc<HostState>>>().lock().unwrap().clone();
    hs.cancel.notify_one();

    // Kick all active peers.
    let kick_handles: Vec<Arc<Notify>> = hs
        .peers
        .lock()
        .unwrap()
        .values()
        .map(|p| p.kick.clone())
        .collect();
    for kick in kick_handles {
        kick.notify_one();
    }

    // Stop native host capture.
    let _ = app.pulsar_video().stop_host();
    log::info!("pulsar host: went offline");
    Ok(())
}

// ── new_password ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct NewPasswordResult {
    pub password: String,
}

/// Rotate the OTP and emit `host-password` with the new value.
///
/// JS: `invoke('new_password', {})` → `{password:String}`
#[tauri::command]
pub async fn new_password<R: Runtime>(app: AppHandle<R>) -> Result<NewPasswordResult, String> {
    let pw = gen_password();
    {
        let hs = app.state::<Mutex<Arc<HostState>>>().lock().unwrap().clone();
        *hs.otp.lock().unwrap() = pw.clone();
    }
    let _ = app.emit("host-password", HostPasswordPayload { password: pw.clone() });
    Ok(NewPasswordResult { password: pw })
}

// ── respond_request ───────────────────────────────────────────────────────────

/// Resolve a pending incoming-connection approval request (from the JS sheet).
///
/// JS: `invoke('respond_request', { reqId, allow })`
#[tauri::command]
pub async fn respond_request<R: Runtime>(
    app: AppHandle<R>,
    req_id: u32,
    allow: bool,
) -> Result<(), String> {
    let hs = app.state::<Mutex<Arc<HostState>>>().lock().unwrap().clone();
    if let Some(tx) = hs.pending.lock().unwrap().remove(&req_id) {
        let _ = tx.send(allow);
    }
    Ok(())
}

// ── disconnect_session ────────────────────────────────────────────────────────

/// Kick an active peer session by its `sid`.
///
/// JS: `invoke('disconnect_session', { sid })`
#[tauri::command]
pub async fn disconnect_session<R: Runtime>(app: AppHandle<R>, sid: u32) -> Result<(), String> {
    let hs = app.state::<Mutex<Arc<HostState>>>().lock().unwrap().clone();
    if let Some(peer) = hs.peers.lock().unwrap().get(&sid) {
        peer.kick.notify_one();
    }
    Ok(())
}

// ── open_a11y_settings / a11y_enabled ────────────────────────────────────────

/// Open Android's Accessibility settings (Cihazım → "Kontrolü etkinleştir").
#[tauri::command]
pub async fn open_a11y_settings<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    Ok(app.pulsar_video().open_a11y_settings().map(|r| r.ok).unwrap_or(false))
}

/// Whether the control AccessibilityService is currently enabled.
#[tauri::command]
pub async fn a11y_enabled<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    Ok(app.pulsar_video().a11y_enabled().map(|r| r.ok).unwrap_or(false))
}
