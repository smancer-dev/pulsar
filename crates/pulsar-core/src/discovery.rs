//! Zero-config LAN auto-discovery via a UDP multicast beacon.
//!
//! Every running node periodically multicasts an [`Announce`] (its relay id,
//! friendly name, main UDP port, public key, platform) to a well-known group, and
//! listens for the others'. This lets the app populate a "devices on your network"
//! list with **no relay round-trip and no manually typed IDs** — you just see the
//! other Pulsar machines on the same LAN.
//!
//! Multicast (not broadcast) is deliberate: with `SO_REUSEADDR`/`SO_REUSEPORT` plus
//! `IP_MULTICAST_LOOP` two instances on the *same* machine can both bind the port
//! and both receive every datagram — exactly the local two-instance test setup —
//! and broadcast is filtered out on many networks.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

use pulsar_proto::{decode, encode, DeviceId};

/// Well-known multicast group + port for Pulsar LAN discovery. The port sits next
/// to the relay's `:21116`. Self-hostable / overridable in tests via [`Discovery::start_on`].
pub const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 71, 21);
pub const DISCOVERY_PORT: u16 = 21117;

/// How often we re-announce ourselves.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
/// Forget a peer we haven't heard from in this long (≈ a few missed announces).
const PEER_TTL: Duration = Duration::from_secs(8);
/// Distinguishes our datagrams from anything else that lands on the port.
const MAGIC: u32 = 0x5055_4c53; // "PULS"
const ANNOUNCE_VERSION: u16 = 1;

/// A human label for this machine's current OS user (e.g. "Ahmet Enes Duruer"),
/// used as the device's identity — especially relay-less, where there's no id to
/// show. Falls back to the login name, then a generic label.
pub fn os_display_name() -> String {
	let real = whoami::realname();
	if !real.trim().is_empty() {
		return real;
	}
	let user = whoami::username();
	if user.trim().is_empty() {
		"Pulsar".to_string()
	} else {
		user
	}
}

/// The beacon payload, bincode-encoded into one datagram.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Announce {
	magic: u32,
	version: u16,
	/// Relay-assigned id, if registered; `None` when running relay-less.
	id: Option<u32>,
	name: String,
	/// The node's main UDP port (where a future relay-less direct LAN link goes).
	node_port: u16,
	pubkey: [u8; 32],
	/// `windows` / `linux` / `macos` — purely informational for the UI.
	platform: String,
	/// Per-process random nonce so a node ignores its own echoes and we can tell
	/// two instances behind one IP apart.
	nonce: u64,
}

/// A peer found on the local network.
///
/// SECURITY — everything here is an **UNVERIFIED hint** supplied by whoever sent the
/// multicast beacon. A beacon is unauthenticated: any host on the LAN (or anything
/// that can reach the multicast group) can spoof another machine's `name`, `id` and
/// `pubkey`. So this struct is only safe as input to the "devices on your network"
/// *list* — never as an established trusted identity:
///
/// * `pubkey` is the beacon's *claimed* X25519 key and is **UNVERIFIED**. It MUST NOT
///   be used for the direct-connect key path: deriving a session key from a beacon-
///   announced pubkey would let an attacker impersonate a peer and MITM the
///   "connect by name" flow. The real key must come from a verified channel (the
///   relay's rendezvous handshake, or a user-confirmed id), not from a beacon. It is
///   kept here only for display/debugging — callers deriving session keys go through
///   the relay handshake, never through this field.
/// * `id` is the beacon's *claimed* relay id. It is **not** corroborated against the
///   relay here, so it is only a hint to pre-fill the connect field; the actual trust
///   is still established by the normal relay-authenticated `connect()` (which verifies
///   the target via its token/handshake), not by the beacon alone.
///
/// Connecting by **IP** (the LAN direct path) stays fine: the IP is the observed
/// source of the datagram, and the session's own E2E handshake authenticates the peer.
///
/// (Field names are kept as `id`/`pubkey` for the existing UI callers; the contract is
/// "unverified hint" — see each field's note. A larger fix would type-wrap them as an
/// explicit `Unverified<…>`, but that is out of scope for this contained change.)
#[derive(Clone, Debug)]
pub struct DiscoveredPeer {
	/// The beacon's *claimed* relay id, if announced. UNVERIFIED — a UI hint, NOT proof
	/// the peer holds this relay id. Trust is established by the authenticated connect().
	pub id: Option<DeviceId>,
	pub name: String,
	/// Source IP + the announced node port (where a direct LAN link would aim).
	pub addr: SocketAddr,
	/// The beacon's *claimed* X25519 pubkey. UNVERIFIED and spoofable — do NOT use it to
	/// derive a session key (that path goes through the relay handshake). Display only.
	pub pubkey: [u8; 32],
	pub platform: String,
	pub last_seen: Instant,
}

struct Inner {
	announce: Announce,
	/// Discovered peers keyed by their announce `nonce`.
	peers: HashMap<u64, DiscoveredPeer>,
}

/// A running LAN discovery beacon: announces this node and collects peers. Stop it
/// by dropping it (the background tasks unwind).
pub struct Discovery {
	sock: Arc<UdpSocket>,
	group: SocketAddr,
	/// Every real LAN NIC. We multicast the beacon out of EACH (not just the OS default
	/// multicast interface, which on a multi-homed machine is often a dead APIPA/virtual
	/// adapter), so peers on any reachable NIC hear us.
	ifaces: Vec<Ipv4Addr>,
	inner: Mutex<Inner>,
	cancel: Arc<Notify>,
	/// When true the beacon stops ANNOUNCING (broadcasting) itself — set while this device
	/// is in gaming mode, a pure client that is not a host and must not advertise on the LAN.
	/// Receiving still runs, so it can keep discovering others. Default false (announcing).
	paused: std::sync::atomic::AtomicBool,
}

impl Discovery {
	/// Start discovery on the default Pulsar group/port.
	pub async fn start(
		name: String,
		node_port: u16,
		pubkey: [u8; 32],
		id: Option<DeviceId>,
	) -> std::io::Result<Arc<Self>> {
		Self::start_on(DISCOVERY_GROUP, DISCOVERY_PORT, name, node_port, pubkey, id).await
	}

	/// Start discovery on a specific group/port (tests use a unique one so they
	/// don't see a live app's beacons).
	pub async fn start_on(
		group: Ipv4Addr,
		port: u16,
		name: String,
		node_port: u16,
		pubkey: [u8; 32],
		id: Option<DeviceId>,
	) -> std::io::Result<Arc<Self>> {
		let ifaces = lan_ifaces_v4();
		let sock = Arc::new(bind_multicast(group, port, &ifaces)?);
		let announce = Announce {
			magic: MAGIC,
			version: ANNOUNCE_VERSION,
			id: id.map(|d| d.0),
			name,
			node_port,
			pubkey,
			platform: std::env::consts::OS.to_string(),
			nonce: rand::random(),
		};
		let disc = Arc::new(Self {
			sock: sock.clone(),
			group: SocketAddr::V4(SocketAddrV4::new(group, port)),
			ifaces,
			inner: Mutex::new(Inner {
				announce,
				peers: HashMap::new(),
			}),
			cancel: Arc::new(Notify::new()),
			paused: std::sync::atomic::AtomicBool::new(false),
		});
		tokio::spawn(announce_loop(Arc::downgrade(&disc), disc.cancel.clone()));
		tokio::spawn(recv_loop(Arc::downgrade(&disc), disc.cancel.clone(), sock));
		Ok(disc)
	}

	/// Update the announced id once relay registration finishes (or it's lost).
	pub async fn set_id(&self, id: Option<DeviceId>) {
		self.inner.lock().await.announce.id = id.map(|d| d.0);
	}

	/// Pause/resume ANNOUNCING (broadcasting) this device on the LAN. Paused while in gaming
	/// mode (a pure client must not advertise itself as connectable). Receiving is unaffected.
	pub fn set_paused(&self, paused: bool) {
		self.paused
			.store(paused, std::sync::atomic::Ordering::Relaxed);
	}

	/// The non-stale peers seen so far (excluding ourselves), sorted by name.
	pub async fn peers(&self) -> Vec<DiscoveredPeer> {
		let now = Instant::now();
		let mut g = self.inner.lock().await;
		g.peers
			.retain(|_, p| now.duration_since(p.last_seen) < PEER_TTL);
		let mut v: Vec<DiscoveredPeer> = g.peers.values().cloned().collect();
		v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
		v
	}

	async fn send_announce(&self) {
		// Gaming mode: don't advertise ourselves (not a host).
		if self.paused.load(std::sync::atomic::Ordering::Relaxed) {
			return;
		}
		let bytes = encode(&self.inner.lock().await.announce);
		// No enumerable NIC → fall back to the OS default multicast interface.
		if self.ifaces.is_empty() {
			let _ = self.sock.send_to(&bytes, self.group).await;
			return;
		}
		// Send one copy out of EACH real NIC by pinning IP_MULTICAST_IF per send. A short-lived
		// socket2 socket keeps this off the shared recv socket (whose multicast_if we'd otherwise
		// race). Peers dedup by nonce, so a host on two NICs of the same LAN getting two copies is
		// harmless. The cost is trivial at the 2 s announce cadence.
		for ip in &self.ifaces {
			if let Ok(s) = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
				let _ = s.set_multicast_if_v4(ip);
				let _ = s.send_to(&bytes, &self.group.into());
			}
		}
	}

	async fn on_datagram(&self, buf: &[u8], from: SocketAddr) {
		let Ok(a) = decode::<Announce>(buf) else {
			return;
		};
		if a.magic != MAGIC || a.version != ANNOUNCE_VERSION {
			return;
		}
		// SECURITY: reject oversized names before acquiring any lock — an on-LAN
		// attacker can craft datagrams with arbitrary name lengths; cap them here.
		const MAX_NAME_LEN: usize = 64;
		if a.name.len() > MAX_NAME_LEN {
			return;
		}
		// Ignore our own multicast echo.
		if a.nonce == self.inner.lock().await.announce.nonce {
			return;
		}
		// SECURITY: a beacon is unauthenticated, so EVERYTHING below is an attacker-
		// controllable hint, not a trusted fact. We keep it ONLY for the on-screen
		// "devices on your network" list. We do NOT let the beacon establish identity:
		// `relay_id`/`beacon_pubkey` are stored as unverified claims and are not used to
		// derive any session key — real trust comes from the relay rendezvous handshake
		// (or a user-confirmed id) when the user actually connects. The one value we do
		// trust is `from.ip()` (the observed datagram source); the LAN direct-connect's
		// own E2E handshake then authenticates the peer. Residual risk: the list can show
		// a spoofed name/id, so the UI must not present a beacon entry as a verified
		// identity — connecting still runs the authenticated path before any trust.
		let peer = DiscoveredPeer {
			id: a.id.and_then(DeviceId::new),
			name: a.name,
			addr: SocketAddr::new(from.ip(), a.node_port),
			pubkey: a.pubkey,
			platform: a.platform,
			last_seen: Instant::now(),
		};
		// SECURITY: hard cap on the peers map to prevent unbounded memory growth from
		// an attacker flooding fresh nonces. Prune stale entries first (same predicate
		// as peers()); if the map is still at capacity, evict the oldest entry before
		// inserting the new one.
		const MAX_PEERS: usize = 256;
		let now = Instant::now();
		let mut g = self.inner.lock().await;
		g.peers.retain(|_, p| now.duration_since(p.last_seen) < PEER_TTL);
		if !g.peers.contains_key(&a.nonce) && g.peers.len() >= MAX_PEERS {
			if let Some(oldest) = g.peers
				.iter()
				.min_by_key(|(_, p)| p.last_seen)
				.map(|(k, _)| *k)
			{
				g.peers.remove(&oldest);
			}
		}
		g.peers.insert(a.nonce, peer);
	}
}

impl Drop for Discovery {
	fn drop(&mut self) {
		// Wake the loops so they observe the dropped `Weak` and exit promptly.
		self.cancel.notify_waiters();
	}
}

/// Every usable local IPv4 NIC address (excludes loopback + link-local APIPA `169.254/16`).
///
/// LAN discovery must join/send the multicast group on ALL of these, not just the OS default
/// multicast interface: that default is, on a multi-homed machine (Hyper-V `vEthernet`,
/// ZeroTier, WiFi + Ethernet, libvirt `virbr*`), frequently a dead APIPA/virtual adapter, so
/// the beacon never reaches the shared LAN. Worse, even with a good default, a peer's beacon
/// can ARRIVE on a *different* NIC than the default route (confirmed in the field: a Pi sent
/// from its Ethernet, the multi-homed host received it on its WiFi NIC, but the host had joined
/// the group only on its Ethernet → it silently never saw the Pi). Joining every NIC fixes both.
fn lan_ifaces_v4() -> Vec<Ipv4Addr> {
	if_addrs::get_if_addrs()
		.map(|ifs| {
			ifs.into_iter()
				.filter_map(|i| match i.addr.ip() {
					IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(v4),
					_ => None,
				})
				.collect()
		})
		.unwrap_or_default()
}

/// Build a UDP socket that can co-exist with other instances on this host and
/// receive its own host's multicast (both needed for the same-PC two-instance case).
fn bind_multicast(group: Ipv4Addr, port: u16, ifaces: &[Ipv4Addr]) -> std::io::Result<UdpSocket> {
	let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
	sock.set_reuse_address(true)?;
	#[cfg(unix)]
	sock.set_reuse_port(true)?;
	sock.bind(&SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port).into())?;
	sock.set_multicast_loop_v4(true)?;
	// Join on the unspecified iface (covers the same-host two-instance loopback case)…
	sock.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
	// …and explicitly on EVERY real NIC, so a peer's beacon is received no matter which NIC the
	// network delivers it on (see `lan_ifaces_v4`). A NIC already covered by the unspecified join
	// just errors → ignore.
	for ip in ifaces {
		let _ = sock.join_multicast_v4(&group, ip);
	}
	sock.set_nonblocking(true)?;
	UdpSocket::from_std(sock.into())
}

async fn announce_loop(disc: Weak<Discovery>, cancel: Arc<Notify>) {
	// `interval`'s first tick fires immediately, so we announce right away.
	let mut tick = tokio::time::interval(ANNOUNCE_INTERVAL);
	loop {
		tokio::select! {
			_ = cancel.notified() => return,
			_ = tick.tick() => match disc.upgrade() {
				Some(d) => d.send_announce().await,
				None => return,
			},
		}
	}
}

async fn recv_loop(disc: Weak<Discovery>, cancel: Arc<Notify>, sock: Arc<UdpSocket>) {
	let mut buf = vec![0u8; 2048];
	loop {
		tokio::select! {
			_ = cancel.notified() => return,
			// Periodic wake so we still exit if `cancel` was missed and no packets arrive.
			_ = tokio::time::sleep(PEER_TTL) => {
				if disc.upgrade().is_none() { return; }
			}
			r = sock.recv_from(&mut buf) => {
				let (n, from) = match r {
					Ok(x) => x,
					// A recv error must NOT permanently kill LAN discovery (announce_loop
					// would keep beaconing while we never hear a peer again). Mirror the
					// node/relay recv loops: on Windows an unconnected UDP socket reports a
					// prior send's ICMP port-unreachable as WSAECONNRESET on the next recv,
					// and other stacks map ICMP transients to these kinds — all spurious for
					// a connectionless socket, so keep looping.
					Err(e) if matches!(
						e.kind(),
						std::io::ErrorKind::ConnectionReset
							| std::io::ErrorKind::ConnectionRefused
							| std::io::ErrorKind::HostUnreachable
							| std::io::ErrorKind::NetworkUnreachable
					) => continue,
					// Anything else (e.g. a Windows oversized-datagram WSAEMSGSIZE, which a
					// single on-LAN packet could trigger): yield briefly so a hypothetical
					// persistent error can't spin at 100% CPU, then keep the receiver alive.
					Err(e) => {
						tracing::warn!(?e, "discovery recv_loop: unexpected socket error");
						tokio::time::sleep(std::time::Duration::from_millis(50)).await;
						continue;
					}
				};
				match disc.upgrade() {
					Some(d) => d.on_datagram(&buf[..n], from).await,
					None => return,
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn os_display_name_is_non_empty() {
		// Always resolves to *something* (real name, else login name, else "Pulsar").
		assert!(!os_display_name().trim().is_empty());
	}

	#[test]
	fn announce_round_trips_through_proto_codec() {
		let a = Announce {
			magic: MAGIC,
			version: ANNOUNCE_VERSION,
			id: Some(482_913_056),
			name: "Salon PC’si".into(),
			node_port: 50_311,
			pubkey: [7u8; 32],
			platform: "windows".into(),
			nonce: 0xDEAD_BEEF,
		};
		assert_eq!(decode::<Announce>(&encode(&a)).unwrap(), a);
	}

	// Two beacons on the same host (distinct group/port from the live app) should
	// see each other via multicast loopback within a few announce intervals.
	// Ignored by default: GitHub-hosted CI runners don't reliably deliver IPv4
	// multicast on the loopback/default interface. Run locally with
	// `cargo test -p pulsar-core -- --ignored` to exercise the real beacon.
	#[ignore = "needs multicast loopback (run locally with --ignored)"]
	#[tokio::test]
	async fn two_instances_discover_each_other() {
		let group = Ipv4Addr::new(239, 255, 71, 99);
		let port = 21_199;
		let a = Discovery::start_on(
			group,
			port,
			"Alice".into(),
			40_001,
			[1u8; 32],
			DeviceId::new(111_111_111),
		)
		.await
		.expect("bind A");
		let b = Discovery::start_on(
			group,
			port,
			"Bob".into(),
			40_002,
			[2u8; 32],
			DeviceId::new(222_222_222),
		)
		.await
		.expect("bind B");

		// Poll until both have seen the other (announces fire immediately on start).
		let mut a_sees_b = false;
		let mut b_sees_a = false;
		for _ in 0..30 {
			a_sees_b = a.peers().await.iter().any(|p| p.name == "Bob");
			b_sees_a = b.peers().await.iter().any(|p| p.name == "Alice");
			if a_sees_b && b_sees_a {
				break;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
		assert!(a_sees_b, "A should discover B on the LAN");
		assert!(b_sees_a, "B should discover A on the LAN");

		// A never lists itself.
		assert!(a.peers().await.iter().all(|p| p.name != "Alice"));
	}
}
