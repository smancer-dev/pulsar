//! The [`Node`] endpoint: register → P2P → relay-fallback orchestration, plus
//! the background receive and heartbeat loops.

use std::sync::Weak;

use super::*;

/// A Pulsar endpoint: one UDP socket, one identity, talking to one relay.
pub struct Node {
	pub(super) sock: Arc<UdpSocket>,
	pub(super) identity: Identity,
	pub(super) relay: SocketAddr,
	pub(super) mode: NetworkMode,
	pub(super) name: String,
	pub(super) registered: Notify,
	pub(super) inner: Mutex<Inner>,
	pub(super) incoming_tx: mpsc::UnboundedSender<Session>,
	pub(super) incoming_rx: Mutex<mpsc::UnboundedReceiver<Session>>,
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
		Self::bind_with_identity(local, relay, mode, name, Identity::generate()).await
	}

	/// Like [`Self::bind_named`] but with a caller-provided [`Identity`]. The app
	/// passes a **persisted** identity here so the device's relay-assigned ID is
	/// stable across restarts (the relay maps pubkey → id).
	pub async fn bind_with_identity(
		local: SocketAddr,
		relay: SocketAddr,
		mode: NetworkMode,
		name: String,
		identity: Identity,
	) -> std::io::Result<Arc<Self>> {
		let sock = Arc::new(UdpSocket::bind(local).await?);
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		let node = Arc::new(Self {
			sock,
			identity,
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
		let id = self
			.inner
			.lock()
			.await
			.self_id
			.ok_or(ConnError::RelayTimeout)?;
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
		// Mint our per-session salt and remember it: we send it now in `hello`, but
		// derive the key later (in the `PeerFound` handler) once we have the peer's.
		let our_salt = random_salt();
		{
			let mut g = self.inner.lock().await;
			g.peer_found.insert(session, pf.clone());
			g.pending_salt.insert(session, our_salt);
		}
		// Handshake blob: static pubkey(32) || fresh session salt(32).
		let mut hello = self.identity.public_bytes().to_vec();
		hello.extend_from_slice(&our_salt);
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
		let rv = timeout(RENDEZVOUS_TIMEOUT, pf.notified()).await;
		// The waiter is only needed during rendezvous — remove it on every path so the
		// `peer_found` map doesn't grow one stale `Arc<Notify>` per connect, forever.
		{
			let mut g = self.inner.lock().await;
			g.peer_found.remove(&session);
			// On failure the key is never derived, so drop the stashed salt too.
			if rv.is_err() {
				g.pending_salt.remove(&session);
			}
		}
		rv.map_err(|_| ConnError::TargetUnreachable(target))?;

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

	/// Connect directly to a known peer address WITHOUT a relay (typed IP path).
	///
	/// `peer_pubkey` is `Some(pk)` when we already learned it from the LAN beacon,
	/// `None` learns it in-band. **Either way** we announce our key + fresh salt in a
	/// `Hello` and await the peer's `HelloAck` (which carries the peer's salt — both
	/// salts are needed to bind the per-session key), then derive as the initiator.
	/// Returns the same [`Session`] as [`connect`], so OTP auth + serve are identical.
	pub async fn connect_direct(
		self: &Arc<Self>,
		peer_addr: SocketAddr,
		_peer_pubkey: Option<PublicKey>,
	) -> Result<Session, ConnError> {
		let session: SessionId = rand::random();
		let (data_tx, data_rx) = mpsc::unbounded_channel();
		// Mint our salt and stash it; the `HelloAck` handler derives the key using it
		// plus the peer's salt from the ack.
		let our_salt = random_salt();
		let hello = || {
			encode(&PeerMsg::Hello {
				session,
				pubkey: self.identity.public_bytes(),
				salt: our_salt,
			})
		};

		// Announce ours, await the peer's HelloAck (carrying their pubkey + salt).
		let hp = Arc::new(Notify::new());
		{
			let mut g = self.inner.lock().await;
			g.hello_done.insert(session, hp.clone());
			g.pending_salt.insert(session, our_salt);
		}
		for _ in 0..PUNCH_ATTEMPTS {
			let _ = self.sock.send_to(&hello(), peer_addr).await;
			tokio::time::sleep(Duration::from_millis(40)).await;
		}
		let hr = timeout(RENDEZVOUS_TIMEOUT, hp.notified()).await;
		if hr.is_err() {
			// Remove the waiter + stashed salt on the timeout path too so neither map
			// leaks one stale entry per failed direct connect.
			let mut g = self.inner.lock().await;
			g.hello_done.remove(&session);
			g.pending_salt.remove(&session);
			return Err(ConnError::P2pFailed);
		}
		{
			let mut g = self.inner.lock().await;
			g.hello_done.remove(&session);
			match g.sessions.get_mut(&session) {
				Some(s) => s.data_tx = Some(data_tx),
				None => return Err(ConnError::P2pFailed),
			}
		}

		// Hole-punch the direct path; no relay fallback (there is no relay here).
		self.establish_transport_direct(session, peer_addr).await;
		Ok(Session {
			id: session,
			peer: DeviceId(0), // no relay id on a direct connect; UI shows the addr
			transport: Transport::Direct,
			node: self.clone(),
			data_rx,
		})
	}

	/// Await the next inbound (accepted) connection.
	pub async fn next_incoming(&self) -> Option<Session> {
		self.incoming_rx.lock().await.recv().await
	}
}

pub(super) async fn recv_loop(weak: Weak<Node>) {
	let sock = match weak.upgrade() {
		Some(n) => n.sock.clone(),
		None => return,
	};
	let mut buf = vec![0u8; 65_536];
	loop {
		let (n, from) = match sock.recv_from(&mut buf).await {
			Ok(x) => x,
			// Windows: a prior send_to that drew an ICMP port-unreachable makes the
			// next recv_from return WSAECONNRESET (10054). It is spurious for a
			// connectionless UDP socket — the socket is still fine, so keep looping.
			Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
			// Other ICMP-mapped transients seen on some stacks.
			Err(e)
				if matches!(
					e.kind(),
					std::io::ErrorKind::ConnectionRefused
						| std::io::ErrorKind::HostUnreachable
						| std::io::ErrorKind::NetworkUnreachable
				) =>
			{
				continue
			}
			// Truly unexpected: yield briefly so a hypothetical persistent error
			// can't spin the loop at 100% CPU, then keep going.
			Err(e) => {
				tracing::warn!(?e, "recv_loop: unexpected socket error");
				tokio::time::sleep(std::time::Duration::from_millis(50)).await;
				continue;
			}
		};
		let Some(node) = weak.upgrade() else { break };
		node.handle_datagram(&buf[..n], from).await;
	}
}

/// Periodically ping the relay so our registration isn't evicted. Exits when the
/// [`Node`] is dropped.
pub(super) async fn heartbeat_loop(weak: Weak<Node>) {
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
