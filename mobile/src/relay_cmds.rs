//! Local relay server — run the rendezvous/relay on THIS device.
//!
//! The relay crate (`pulsar_relay`) is portable (tokio UDP, no host-only deps),
//! so it embeds fine on Android. Starting it here lets the phone act as a relay
//! for the whole LAN: other devices set their relay to this phone's `ip:port` and
//! get IDs + fall-back transport from it (self-hosted rendezvous, per the product
//! design — "identity distribution + fallback transport are fully yours").
//!
//! Commands (app-level, invoked by bare name from JS — NOT plugin commands, so no
//! ACL/build.rs entry needed):
//!   - `start_local_relay { port? }` → RelayStatus   (idempotent; default port 21116)
//!   - `stop_local_relay`           → RelayStatus
//!   - `local_relay_status`         → RelayStatus
//!
//! The running task is held in [`LocalRelay`] managed state and stopped by
//! aborting it (which drops the UDP socket).

use std::net::SocketAddr;

use serde::Serialize;
use tauri::async_runtime::{self, JoinHandle};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Mutex;

/// Default relay UDP port — mirrors the design's `:21116`
/// (`pulsar_core::proto::DEFAULT_RELAY_PORT`).
const DEFAULT_RELAY_PORT: u16 = 21116;

/// The running local-relay task (if any) + its bound port. `None` = stopped.
#[derive(Default)]
pub struct LocalRelay(pub Mutex<Option<RelayTask>>);

pub struct RelayTask {
    handle: JoinHandle<()>,
    port: u16,
}

/// Reported to the UI: whether the relay is up + the address other devices use.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub running: bool,
    /// LAN IP peers use to reach this relay (`""` if unknown / not running).
    pub ip: String,
    /// Bound UDP port (`0` when not running).
    pub port: u16,
}

impl RelayStatus {
    fn stopped() -> Self {
        Self { running: false, ip: String::new(), port: 0 }
    }
}

/// Best-effort LAN IP of this device (same trick as `client::local_ip`): open a
/// UDP socket "toward" a public address — no packets are sent — and read the
/// kernel-chosen source IP.
async fn lan_ip() -> String {
    let sock = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if sock.connect("8.8.8.8:80").await.is_err() {
        return String::new();
    }
    sock.local_addr().map(|a| a.ip().to_string()).unwrap_or_default()
}

/// Start the local relay. Idempotent: if already running, returns the live status.
/// `port` of `0`/`None` → [`DEFAULT_RELAY_PORT`].
/// JS: `invoke('start_local_relay', { port })` → [`RelayStatus`].
#[tauri::command]
pub async fn start_local_relay<R: Runtime>(
    app: AppHandle<R>,
    port: Option<u16>,
) -> Result<RelayStatus, String> {
    let state = app.state::<LocalRelay>();
    let mut guard = state.0.lock().await;

    // Already running → just report it (don't double-bind the port).
    if let Some(t) = guard.as_ref() {
        return Ok(RelayStatus { running: true, ip: lan_ip().await, port: t.port });
    }

    let p = port.filter(|p| *p != 0).unwrap_or(DEFAULT_RELAY_PORT);
    let addr = SocketAddr::from(([0, 0, 0, 0], p));
    let relay = pulsar_relay::Relay::bind(addr)
        .await
        .map_err(|e| format!("relay bind {addr} failed: {e}"))?;
    // Use the actually-bound port (matters if the caller passed 0 explicitly via
    // a future "ephemeral" path; with our default it equals `p`).
    let bound = relay.local_addr().map(|a| a.port()).unwrap_or(p);

    let handle = async_runtime::spawn(async move {
        if let Err(e) = relay.run().await {
            log::warn!("[relay] local relay loop exited: {e}");
        }
    });

    *guard = Some(RelayTask { handle, port: bound });
    log::info!("[relay] local relay listening on 0.0.0.0:{bound}");
    Ok(RelayStatus { running: true, ip: lan_ip().await, port: bound })
}

/// Stop the local relay (aborts the task → drops the socket). No-op if not running.
/// JS: `invoke('stop_local_relay')` → [`RelayStatus`].
#[tauri::command]
pub async fn stop_local_relay<R: Runtime>(app: AppHandle<R>) -> Result<RelayStatus, String> {
    let state = app.state::<LocalRelay>();
    let mut guard = state.0.lock().await;
    if let Some(t) = guard.take() {
        t.handle.abort();
        log::info!("[relay] local relay stopped");
    }
    Ok(RelayStatus::stopped())
}

/// Current local-relay status (for the Settings toggle to reflect on mount).
/// JS: `invoke('local_relay_status')` → [`RelayStatus`].
#[tauri::command]
pub async fn local_relay_status<R: Runtime>(app: AppHandle<R>) -> Result<RelayStatus, String> {
    let state = app.state::<LocalRelay>();
    let guard = state.0.lock().await;
    match guard.as_ref() {
        Some(t) => Ok(RelayStatus { running: true, ip: lan_ip().await, port: t.port }),
        None => Ok(RelayStatus::stopped()),
    }
}
