//! Shared relay [`Node`] used by BOTH roles: client (outgoing connects, see
//! [`crate::client::connect_host`]) and host (incoming accepts, see
//! [`crate::host::go_online`]).
//!
//! The phone is simultaneously a client and a host, and both roles derive the
//! SAME relay identity. Binding a SEPARATE `Node` per role (the previous design)
//! registered TWO UDP sockets under one device id, so the relay's `dev.addr`
//! flapped between them on every heartbeat and an INBOUND connection (another
//! device → this phone) was routed to whichever node registered last — usually a
//! client node, which has no accept loop. The host therefore never saw the
//! request: no approval popup, no session. (Verified live: the relay logged two
//! ports for one id and forwarded `Incoming` to the client node's port.)
//!
//! One shared `Arc<Node>` fixes this: a SINGLE registration → a single heartbeat
//! → a single relay address, so `next_incoming()` (host) and `connect()` (client)
//! share it and inbound connections always reach the accept loop. `Node::register`
//! spawns the heartbeat keyed on a weak ref, so the node stays alive exactly as
//! long as this `Arc` is held in managed state.

use std::net::SocketAddr;
use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Mutex;

use pulsar_core::proto::DeviceId;
use pulsar_core::{Discovery, NetworkMode, Node};

use crate::identity::load_identity;

/// The app's single relay `Node`, created on first use (a client connect or
/// `go_online`) and reused by both roles thereafter. `None` until first use.
#[derive(Default)]
pub struct SharedNode(pub Mutex<Option<SharedNodeInner>>);

pub struct SharedNodeInner {
    pub node: Arc<Node>,
    pub my_id: DeviceId,
    /// The relay + network mode this node was bound to. A later caller requesting a
    /// DIFFERENT relay/mode (user edited Settings → Ağ) rebinds the node so the new
    /// setting takes effect this connect instead of only after an app restart. The
    /// core `Node`'s relay address is immutable once bound, so a change means a rebuild.
    pub relay_addr: SocketAddr,
    pub mode: NetworkMode,
}

/// Return the shared node, binding + registering it on the FIRST call. Later callers
/// reuse the existing node when they request the SAME relay + network mode (the hot
/// path — both roles read the same persisted `Config`, so they agree and the node is
/// shared); a caller requesting a DIFFERENT relay/mode rebinds a fresh node so a
/// Settings change applies without an app restart. The device name is still taken
/// from whoever binds. The async `Mutex` is held across `bind`/`register` so two
/// concurrent first-callers can't race two nodes into existence.
///
/// Note: rebinding replaces the strong `Arc` held here; the previous node then drops
/// (stopping its weak-ref-keyed heartbeat) once no live read/accept loop still holds
/// it. A device actively HOSTING should re-run `go_online` after a relay change so its
/// accept loop moves to the new node — a client connect alone rebinds only the shared
/// node, not the host's already-spawned accept loop.
pub(crate) async fn get_or_create_node<R: Runtime>(
    app: &AppHandle<R>,
    relay_addr: SocketAddr,
    mode: NetworkMode,
    dev_name: String,
) -> Result<(Arc<Node>, DeviceId), String> {
    let shared = app.state::<SharedNode>();
    let mut g = shared.0.lock().await;
    if let Some(inner) = g.as_ref() {
        if inner.relay_addr == relay_addr && inner.mode == mode {
            return Ok((inner.node.clone(), inner.my_id));
        }
        log::info!(
            "pulsar: relay/mode changed ({} → {relay_addr}) — rebinding shared node",
            inner.relay_addr
        );
    }
    // Bind the user-configured node port (0 = ephemeral/random, the default).
    let port = crate::config::load_config(app).node_port;
    let local: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
    let identity = load_identity(app);
    let node = Node::bind_with_identity(local, relay_addr, mode, dev_name, identity)
        .await
        .map_err(|e| format!("bind failed: {e}"))?;
    let my_id = node
        .register()
        .await
        .map_err(|e| format!("register failed: {e:?}"))?;
    *g = Some(SharedNodeInner {
        node: node.clone(),
        my_id,
        relay_addr,
        mode,
    });
    Ok((node, my_id))
}

/// The app's single LAN-discovery beacon (multicast 239.255.71.21). Announces this
/// device and collects peers seen on the local network. Created on the first
/// `lan_devices` call and reused thereafter. `None` until first use.
///
/// Note: Android drops inbound multicast unless a `WifiManager.MulticastLock` is
/// held — `MainActivity` acquires one at startup so `recv_loop` actually sees
/// other devices' announces.
#[derive(Default)]
pub struct SharedDiscovery(pub Mutex<Option<Arc<Discovery>>>);

/// Start the discovery beacon on the first call, reuse it after. Announced id-less
/// (`id: None`) with port 0 — the phone is primarily a *discoverer* here (the
/// connect screen's "on your network" list + recents online dots); peers identify
/// it by name. The async `Mutex` serialises concurrent first-callers.
pub(crate) async fn get_or_create_discovery<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Arc<Discovery>, String> {
    let shared = app.state::<SharedDiscovery>();
    let mut g = shared.0.lock().await;
    if let Some(d) = g.as_ref() {
        return Ok(d.clone());
    }
    let cfg = crate::config::load_config(app);
    let pubkey = load_identity(app).public_bytes();
    let disc = Discovery::start(cfg.device_name, 0, pubkey, None)
        .await
        .map_err(|e| format!("discovery start failed: {e}"))?;
    *g = Some(disc.clone());
    Ok(disc)
}
