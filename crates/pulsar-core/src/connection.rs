//! Connection orchestration: the register → P2P → relay-fallback flow.
//!
//! A [`Node`] owns one UDP socket and an X25519 [`Identity`]. It:
//! 1. **Registers** with the relay to obtain its [`DeviceId`] (this is where the
//!    ID comes from — no relay, no ID).
//! 2. To reach a peer, asks the relay to rendezvous, exchanges X25519 public keys
//!    through the relay (which only sees opaque blobs), then **tries to hole-punch
//!    a direct UDP path**.
//! 3. Depending on [`NetworkMode`]: `Auto` falls back to relaying traffic if the
//!    punch fails; `P2pOnly` errors instead; `RelayOnly` skips punching entirely.
//!
//! All application data is sealed with the per-session ChaCha20-Poly1305 key, so
//! the relay (and the network) only ever see ciphertext.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use pulsar_proto::{
	decode, encode, ClientMsg, DeviceId, PeerMsg, RelayMsg, SessionId, Token, PROTOCOL_VERSION,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::timeout;

use crate::config::NetworkMode;
use crate::crypto::{Identity, Role, Session as Crypto};

const REGISTER_TIMEOUT: Duration = Duration::from_secs(3);
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(3);
const PUNCH_TIMEOUT: Duration = Duration::from_millis(800);
/// How often to ping the relay so it doesn't evict us. Must stay well under the
/// relay's `DEVICE_TTL` (30s) or `connect()` later fails with `BadToken`.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const PUNCH_ATTEMPTS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum ConnError {
	#[error("not registered with a relay yet")]
	NotRegistered,
	#[error("relay did not respond (is it reachable?)")]
	RelayTimeout,
	#[error("target {0} could not be reached via the relay")]
	TargetUnreachable(DeviceId),
	#[error("direct P2P connection failed and relay fallback is disabled")]
	P2pFailed,
	#[error(transparent)]
	Io(#[from] std::io::Error),
}

/// How a session's media path is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
	/// Direct peer-to-peer UDP (hole-punched).
	Direct,
	/// Tunnelled through the relay.
	Relay,
}

struct SessionState {
	peer_addr: SocketAddr,
	crypto: Crypto,
	transport: Transport,
	send_seq: u64,
	direct_ok: bool,
	data_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

#[derive(Default)]
struct Inner {
	self_id: Option<DeviceId>,
	token: Option<Token>,
	sessions: HashMap<SessionId, SessionState>,
	peer_found: HashMap<SessionId, Arc<Notify>>,
	punched: HashMap<SessionId, Arc<Notify>>,
}

/// A Pulsar endpoint: one UDP socket, one identity, talking to one relay.
pub struct Node {
	sock: Arc<UdpSocket>,
	identity: Identity,
	relay: SocketAddr,
	mode: NetworkMode,
	name: String,
	registered: Notify,
	inner: Mutex<Inner>,
	incoming_tx: mpsc::UnboundedSender<Session>,
	incoming_rx: Mutex<mpsc::UnboundedReceiver<Session>>,
}

impl Node {
	/// Bind locally and start the receive loop. `relay` is the configurable
	/// rendezvous server; `local` may be `0.0.0.0:0` for an ephemeral port.
	pub async fn bind(
		local: SocketAddr,
		relay: SocketAddr,
		mode: NetworkMode,
	) -> std::io::Result<Arc<Self>> {
		Self::bind_named(local, relay, mode, "Pulsar".into()).await
	}

	pub async fn bind_named(
		local: SocketAddr,
		relay: SocketAddr,
		mode: NetworkMode,
		name: String,
	) -> std::io::Result<Arc<Self>> {
		let sock = Arc::new(UdpSocket::bind(local).await?);
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		let node = Arc::new(Self {
			sock,
			identity: Identity::generate(),
			relay,
			mode,
			name,
			registered: Notify::new(),
			inner: Mutex::new(Inner::default()),
			incoming_tx,
			incoming_rx: Mutex::new(incoming_rx),
		});
		let weak = Arc::downgrade(&node);
		tokio::spawn(recv_loop(weak));
		Ok(node)
	}

	pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
		self.sock.local_addr()
	}

	pub fn public_key(&self) -> [u8; 32] {
		self.identity.public_bytes()
	}

	/// Our relay-assigned id, once registered.
	pub async fn self_id(&self) -> Option<DeviceId> {
		self.inner.lock().await.self_id
	}

	/// Register with the relay and obtain a [`DeviceId`]. Errors if the relay is
	/// unreachable (e.g. taken down) — without it there is no ID.
	pub async fn register(self: &Arc<Self>) -> Result<DeviceId, ConnError> {
		let msg = ClientMsg::Register {
			version: PROTOCOL_VERSION,
			pubkey: self.identity.public_bytes(),
			name: Some(self.name.clone()),
		};
		self.sock.send_to(&encode(&msg), self.relay).await?;
		timeout(REGISTER_TIMEOUT, self.registered.notified())
			.await
			.map_err(|_| ConnError::RelayTimeout)?;
		let id = self.inner.lock().await.self_id.ok_or(ConnError::RelayTimeout)?;
		// Keep the registration alive: the relay drops devices that go silent for
		// `DEVICE_TTL`, after which `connect()` would fail with `BadToken`.
		tokio::spawn(heartbeat_loop(Arc::downgrade(self)));
		Ok(id)
	}

	/// Connect to a peer by id, following the configured [`NetworkMode`].
	pub async fn connect(self: &Arc<Self>, target: DeviceId) -> Result<Session, ConnError> {
		let (id, token) = {
			let g = self.inner.lock().await;
			(
				g.self_id.ok_or(ConnError::NotRegistered)?,
				g.token.ok_or(ConnError::NotRegistered)?,
			)
		};
		let session: SessionId = rand::random();

		// Register a waiter, then ask the relay to rendezvous.
		let pf = Arc::new(Notify::new());
		self.inner
			.lock()
			.await
			.peer_found
			.insert(session, pf.clone());
		let hello = self.identity.public_bytes().to_vec();
		self.sock
			.send_to(
				&encode(&ClientMsg::Connect {
					id,
					token,
					target,
					session,
					hello,
				}),
				self.relay,
			)
			.await?;
		timeout(RENDEZVOUS_TIMEOUT, pf.notified())
			.await
			.map_err(|_| ConnError::TargetUnreachable(target))?;

		// Decide the transport.
		let transport = self.establish_transport(session).await?;

		// Wire up an inbound data channel for the caller.
		let (data_tx, data_rx) = mpsc::unbounded_channel();
		{
			let mut g = self.inner.lock().await;
			if let Some(s) = g.sessions.get_mut(&session) {
				s.transport = transport;
				s.data_tx = Some(data_tx);
			}
		}
		Ok(Session {
			id: session,
			peer: target,
			transport,
			node: self.clone(),
			data_rx,
		})
	}

	/// Await the next inbound (accepted) connection.
	pub async fn next_incoming(&self) -> Option<Session> {
		self.incoming_rx.lock().await.recv().await
	}

	async fn establish_transport(&self, session: SessionId) -> Result<Transport, ConnError> {
		if self.mode == NetworkMode::RelayOnly {
			return Ok(Transport::Relay);
		}
		let peer_addr = match self.inner.lock().await.sessions.get(&session) {
			Some(s) => s.peer_addr,
			None => return Err(ConnError::P2pFailed),
		};

		let pk = Arc::new(Notify::new());
		self.inner.lock().await.punched.insert(session, pk.clone());
		for seq in 0..PUNCH_ATTEMPTS as u32 {
			let _ = self
				.sock
				.send_to(&encode(&PeerMsg::Punch { session, seq }), peer_addr)
				.await;
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
		match timeout(PUNCH_TIMEOUT, pk.notified()).await {
			Ok(()) => Ok(Transport::Direct),
			Err(_) if self.mode == NetworkMode::Auto => Ok(Transport::Relay),
			Err(_) => Err(ConnError::P2pFailed),
		}
	}

	async fn handle_datagram(self: &Arc<Self>, buf: &[u8], from: SocketAddr) {
		if from == self.relay {
			if let Ok(msg) = decode::<RelayMsg>(buf) {
				self.handle_relay(msg).await;
			}
		} else if let Ok(msg) = decode::<PeerMsg>(buf) {
			self.handle_peer(msg, from).await;
		}
	}

	async fn handle_relay(self: &Arc<Self>, msg: RelayMsg) {
		match msg {
			RelayMsg::Registered { id, token } => {
				let mut g = self.inner.lock().await;
				g.self_id = Some(id);
				g.token = Some(token);
				drop(g);
				self.registered.notify_waiters();
				self.registered.notify_one();
			}
			RelayMsg::HeartbeatAck => {}
			RelayMsg::Incoming {
				from,
				from_addr,
				session,
				hello,
			} => self.on_incoming(from, from_addr, session, hello).await,
			RelayMsg::PeerFound {
				target: _,
				target_addr,
				session,
				answer,
			} => {
				if answer.len() >= 32 {
					let mut key = [0u8; 32];
					key.copy_from_slice(&answer[..32]);
					let crypto = self.identity.session(key, Role::Initiator);
					let mut g = self.inner.lock().await;
					g.sessions.insert(
						session,
						SessionState {
							peer_addr: target_addr,
							crypto,
							transport: Transport::Relay,
							send_seq: 0,
							direct_ok: false,
							data_tx: None,
						},
					);
					if let Some(n) = g.peer_found.get(&session).cloned() {
						n.notify_one();
					}
				}
			}
			RelayMsg::RelayData { session, payload } => {
				// Relay-fallback inbound: unwrap the tunnelled PeerMsg.
				if let Ok(PeerMsg::Data { seq, payload, .. }) = decode::<PeerMsg>(&payload) {
					self.deliver(session, seq, &payload).await;
				}
			}
			RelayMsg::Error { code, message } => {
				tracing::warn!(?code, message, "relay error");
			}
		}
	}

	async fn on_incoming(
		self: &Arc<Self>,
		from: DeviceId,
		from_addr: SocketAddr,
		session: SessionId,
		hello: Vec<u8>,
	) {
		if hello.len() < 32 {
			return;
		}
		let mut key = [0u8; 32];
		key.copy_from_slice(&hello[..32]);
		let crypto = self.identity.session(key, Role::Responder);
		let (data_tx, data_rx) = mpsc::unbounded_channel();

		let (id, token) = {
			let mut g = self.inner.lock().await;
			g.sessions.insert(
				session,
				SessionState {
					peer_addr: from_addr,
					crypto,
					transport: Transport::Relay,
					send_seq: 0,
					direct_ok: false,
					data_tx: Some(data_tx),
				},
			);
			(g.self_id, g.token)
		};
		let (Some(id), Some(token)) = (id, token) else {
			return;
		};

		// Accept (sends our pubkey back as the answer) and start punching.
		let _ = self
			.sock
			.send_to(
				&encode(&ClientMsg::Accept {
					id,
					token,
					session,
					answer: self.identity.public_bytes().to_vec(),
				}),
				self.relay,
			)
			.await;
		for seq in 0..PUNCH_ATTEMPTS as u32 {
			let _ = self
				.sock
				.send_to(&encode(&PeerMsg::Punch { session, seq }), from_addr)
				.await;
		}

		let _ = self.incoming_tx.send(Session {
			id: session,
			peer: from,
			transport: Transport::Relay,
			node: self.clone(),
			data_rx,
		});
	}

	async fn handle_peer(self: &Arc<Self>, msg: PeerMsg, from: SocketAddr) {
		match msg {
			PeerMsg::Punch { session, seq } => {
				let _ = self
					.sock
					.send_to(&encode(&PeerMsg::PunchAck { session, seq }), from)
					.await;
				let mut g = self.inner.lock().await;
				if let Some(s) = g.sessions.get_mut(&session) {
					s.direct_ok = true;
					s.transport = Transport::Direct;
					s.peer_addr = from;
				}
			}
			PeerMsg::PunchAck { session, .. } => {
				let mut g = self.inner.lock().await;
				if let Some(s) = g.sessions.get_mut(&session) {
					s.direct_ok = true;
				}
				if let Some(n) = g.punched.get(&session).cloned() {
					n.notify_one();
				}
			}
			PeerMsg::Data {
				session,
				seq,
				payload,
			} => self.deliver(session, seq, &payload).await,
			PeerMsg::KeepAlive { .. } => {}
		}
	}

	/// Decrypt an inbound payload and hand it to the session's reader.
	async fn deliver(&self, session: SessionId, seq: u64, ciphertext: &[u8]) {
		let mut g = self.inner.lock().await;
		if let Some(s) = g.sessions.get_mut(&session) {
			if let Ok(plain) = s.crypto.open(seq, ciphertext) {
				if let Some(tx) = &s.data_tx {
					let _ = tx.send(plain);
				}
			}
		}
	}
}

async fn recv_loop(weak: Weak<Node>) {
	let sock = match weak.upgrade() {
		Some(n) => n.sock.clone(),
		None => return,
	};
	let mut buf = vec![0u8; 65_536];
	loop {
		let (n, from) = match sock.recv_from(&mut buf).await {
			Ok(x) => x,
			Err(_) => break,
		};
		let Some(node) = weak.upgrade() else { break };
		node.handle_datagram(&buf[..n], from).await;
	}
}

/// Periodically ping the relay so our registration isn't evicted. Exits when the
/// [`Node`] is dropped.
async fn heartbeat_loop(weak: Weak<Node>) {
	let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
	loop {
		tick.tick().await;
		let Some(node) = weak.upgrade() else { return };
		let creds = {
			let g = node.inner.lock().await;
			g.self_id.zip(g.token)
		};
		if let Some((id, token)) = creds {
			let _ = node
				.sock
				.send_to(&encode(&ClientMsg::Heartbeat { id, token }), node.relay)
				.await;
		}
	}
}

/// An established (encrypted) session with a peer.
pub struct Session {
	id: SessionId,
	peer: DeviceId,
	transport: Transport,
	node: Arc<Node>,
	data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Drop for Session {
	/// Remove this session's state from the node so its inbound-data sender is
	/// dropped — otherwise a host accepting many connections leaks one
	/// `SessionState` (with its channel) per session, forever.
	fn drop(&mut self) {
		let node = self.node.clone();
		let id = self.id;
		if let Ok(handle) = tokio::runtime::Handle::try_current() {
			handle.spawn(async move {
				node.inner.lock().await.sessions.remove(&id);
			});
		}
	}
}

impl Session {
	pub fn id(&self) -> SessionId {
		self.id
	}
	pub fn peer(&self) -> DeviceId {
		self.peer
	}
	pub fn transport(&self) -> Transport {
		self.transport
	}

	/// Encrypt and send a payload over whichever transport this session uses.
	pub async fn send(&self, payload: &[u8]) -> Result<(), ConnError> {
		let (frame, dest_relay, peer_addr, transport) = {
			let mut g = self.node.inner.lock().await;
			let s = g
				.sessions
				.get_mut(&self.id)
				.ok_or(ConnError::P2pFailed)?;
			let seq = s.send_seq;
			s.send_seq += 1;
			let ct = s.crypto.seal(seq, payload);
			let inner = PeerMsg::Data {
				session: self.id,
				seq,
				payload: ct,
			};
			let transport = s.transport;
			let peer_addr = s.peer_addr;
			match transport {
				Transport::Direct => (encode(&inner), None, peer_addr, transport),
				Transport::Relay => {
					let id = g.self_id.ok_or(ConnError::NotRegistered)?;
					let token = g.token.ok_or(ConnError::NotRegistered)?;
					(
						encode(&ClientMsg::RelayData {
							id,
							token,
							session: self.id,
							payload: encode(&inner),
						}),
						Some(self.node.relay),
						peer_addr,
						transport,
					)
				}
			}
		};
		let dest = match transport {
			Transport::Direct => peer_addr,
			Transport::Relay => dest_relay.unwrap(),
		};
		self.node.sock.send_to(&frame, dest).await?;
		Ok(())
	}

	/// Receive the next decrypted inbound payload.
	pub async fn recv(&mut self) -> Option<Vec<u8>> {
		self.data_rx.recv().await
	}

	/// Refresh the transport from the live session state (it may upgrade to
	/// `Direct` once a late hole-punch succeeds).
	pub async fn live_transport(&self) -> Transport {
		self.node
			.inner
			.lock()
			.await
			.sessions
			.get(&self.id)
			.map(|s| if s.direct_ok { Transport::Direct } else { s.transport })
			.unwrap_or(self.transport)
	}

	/// The peer's observed UDP address (for directing a media stream at it).
	pub async fn peer_addr(&self) -> Option<std::net::SocketAddr> {
		self.node
			.inner
			.lock()
			.await
			.sessions
			.get(&self.id)
			.map(|s| s.peer_addr)
	}
}
