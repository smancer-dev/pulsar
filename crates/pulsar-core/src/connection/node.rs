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
	/// Signalled by the `Registered` handler when a RE-registration returns a
	/// DIFFERENT id than we already held — e.g. the relay restarted/lost its
	/// `by_pubkey` map and minted a fresh 9-digit id. Lets the app re-advertise
	/// the new id (UI + LAN beacon) instead of broadcasting the dead old one.
	/// `Arc` so a watcher can await it (`id_changed_handle`) without pinning the
	/// `Node` alive across the await (mirrors `shutdown`).
	pub(super) id_changed: Arc<Notify>,
	/// Signalled when the relay rejects a re-registration from an already-online
	/// node with `IncompatibleVersion` (relay was redeployed with a newer protocol
	/// version). The heartbeat will keep bouncing; the node is effectively stranded.
	/// Lets the app surface an "update required" error and go offline cleanly
	/// instead of silently advertising a dead id forever.
	pub(super) version_error: Arc<Notify>,
	pub(super) inner: Mutex<Inner>,
	pub(super) incoming_tx: mpsc::UnboundedSender<Session>,
	pub(super) incoming_rx: Mutex<mpsc::UnboundedReceiver<Session>>,
	/// Signalled by `Drop` so `recv_loop` exits immediately (see below).
	pub(super) shutdown: Arc<Notify>,
}

impl Drop for Node {
	/// Wake `recv_loop` so it exits: it holds a strong `Arc<UdpSocket>`, so an
	/// idle dropped node would otherwise keep its (well-known) port bound until
	/// the next stray datagram — and `go_online`'s re-bind to port 21118 would
	/// silently fall back to an ephemeral port.
	fn drop(&mut self) {
		// notify_one stores a permit, so the wakeup also lands if the loop is
		// mid-dispatch rather than parked in its select! right now.
		self.shutdown.notify_one();
		// Wake any id-rotation watcher (`id_changed_handle`) so it re-checks its
		// Weak<Node>, sees the node is gone, and exits instead of parking forever.
		self.id_changed.notify_one();
		// Wake any version-error watcher so it exits cleanly too.
		self.version_error.notify_one();
	}
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
		// Media-over-session rides THIS one socket: a 15 Mbit stream's IDR bursts
		// overflow the kernel-default ~208 KiB rcvbuf (UdpRcvbufErrors → silent
		// packet loss → broken reference chains / mosaic under motion). Ask for
		// 4 MiB each way before binding; the kernel clamps to rmem_max/wmem_max,
		// so this is best-effort (Pi has 16 MiB, stock desktops 4 MiB+).
		let sock = {
			use socket2::{Domain, Protocol, Socket, Type};
			let domain = if local.is_ipv4() {
				Domain::IPV4
			} else {
				Domain::IPV6
			};
			let s = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
			let _ = s.set_recv_buffer_size(4 << 20);
			let _ = s.set_send_buffer_size(4 << 20);
			s.set_nonblocking(true)?;
			s.bind(&local.into())?;
			UdpSocket::from_std(s.into())?
		};
		let sock = Arc::new(sock);
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		let node = Arc::new(Self {
			sock,
			identity,
			relay,
			mode,
			name,
			registered: Notify::new(),
			id_changed: Arc::new(Notify::new()),
			version_error: Arc::new(Notify::new()),
			inner: Mutex::new(Inner::default()),
			incoming_tx,
			incoming_rx: Mutex::new(incoming_rx),
			shutdown: Arc::new(Notify::new()),
		});
		let weak = Arc::downgrade(&node);
		tokio::spawn(recv_loop(weak));
		Ok(node)
	}

	pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
		self.sock.local_addr()
	}

	/// Our best same-LAN punch candidate: the OS-routed outbound v4 + the node's
	/// REAL bound port. Appended to the handshake blobs (`push_lan_candidate`) so
	/// a same-NAT peer can punch our private address instead of the lossy router
	/// hairpin. `None` when there's no usable v4 route (offline / v6-only).
	pub(super) fn lan_candidate(&self) -> Option<SocketAddr> {
		let port = self.sock.local_addr().ok()?.port();
		// Routing probe only — UDP connect() sends no packets.
		let probe = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
		probe.connect("8.8.8.8:80").ok()?;
		match probe.local_addr().ok()?.ip() {
			std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(SocketAddr::new(v4.into(), port)),
			_ => None,
		}
	}

	/// Append our LAN candidate (`v4 ip(4) || port(2)`, big-endian) to a handshake
	/// blob — the counterpart of `parse_lan_candidate`. No-op without a candidate.
	pub(super) fn push_lan_candidate(&self, blob: &mut Vec<u8>) {
		if let Some(SocketAddr::V4(a)) = self.lan_candidate() {
			blob.extend_from_slice(&a.ip().octets());
			blob.extend_from_slice(&a.port().to_be_bytes());
		}
	}

	pub fn public_key(&self) -> [u8; 32] {
		self.identity.public_bytes()
	}

	/// Our relay-assigned id, once registered.
	pub async fn self_id(&self) -> Option<DeviceId> {
		self.inner.lock().await.self_id
	}

	/// A handle to the id-rotation signal. Its `.notified()` resolves the next
	/// time our relay-assigned id ROTATES after the initial `register()` — i.e. a
	/// `NotRegistered` re-register got a brand-new id (a relay restart that lost
	/// `by_pubkey`). Await it in a loop and read [`self_id`] to re-advertise the
	/// fresh id to the UI / LAN beacon, otherwise both keep broadcasting the
	/// now-unreachable old id. Cloning the `Arc` lets a watcher await without
	/// pinning the `Node` alive across the await.
	pub fn id_changed_handle(&self) -> Arc<Notify> {
		self.id_changed.clone()
	}

	/// A handle to the post-registration version-error signal. Its `.notified()`
	/// resolves when the relay sends an `IncompatibleVersion` error to an already-
	/// registered node (relay redeployed with a newer protocol version). The node
	/// is stranded at this point — heartbeats will keep bouncing — so the app
	/// should surface an "update required" error and go offline. Cloning the `Arc`
	/// lets the watcher await without pinning the `Node` alive.
	pub fn version_error_handle(&self) -> Arc<Notify> {
		self.version_error.clone()
	}

	/// The relay registration message. One encoding for both the initial
	/// `register()` and the `NotRegistered` re-register (relay restart / >TTL
	/// outage): the relay maps pubkey → id, so re-sending this reissues the
	/// SAME 9-digit ID.
	pub(super) fn register_msg(&self) -> ClientMsg {
		ClientMsg::Register {
			version: PROTOCOL_VERSION,
			pubkey: self.identity.public_bytes(),
			name: Some(self.name.clone()),
		}
	}

	/// Register with the relay and obtain a [`DeviceId`]. Errors if the relay is
	/// unreachable (e.g. taken down) — without it there is no ID.
	pub async fn register(self: &Arc<Self>) -> Result<DeviceId, ConnError> {
		self.sock
			.send_to(&encode(&self.register_msg()), self.relay)
			.await?;
		timeout(REGISTER_TIMEOUT, self.registered.notified())
			.await
			.map_err(|_| ConnError::RelayTimeout)?;
		let id = {
			let g = self.inner.lock().await;
			match g.self_id {
				Some(id) => id,
				// The notify fired with no id: the relay refused our registration. A
				// version mismatch maps to a clear `IncompatibleVersion`; anything else
				// (or no recorded code) falls back to a timeout-style error.
				None => {
					return Err(match g.register_error {
						// The relay sends `ErrCode::Protocol` (not `IncompatibleVersion`)
						// for version-mismatch replies so that old builds that predate the
						// `IncompatibleVersion` variant can decode the error. During initial
						// registration both codes mean "update required" (no other relay
						// Protocol error is sent at this stage). Map both so the UI shows
						// the clear message instead of a generic relay-timeout / retry loop.
						Some(ErrCode::IncompatibleVersion) | Some(ErrCode::Protocol) => {
							ConnError::IncompatibleVersion
						}
						_ => ConnError::RelayTimeout,
					})
				}
			}
		};
		// Keep the registration alive: the relay drops devices that go silent for
		// `DEVICE_TTL`, after which `connect()` would fail with `BadToken`.
		tokio::spawn(heartbeat_loop(Arc::downgrade(self)));
		Ok(id)
	}

	/// Connect to a peer by id, following the configured [`NetworkMode`].
	pub async fn connect(self: &Arc<Self>, target: DeviceId) -> Result<Session, ConnError> {
		self.connect_pinned(target, None).await
	}

	/// Like [`Self::connect`] but with an optional **expected peer public key** to
	/// pin the relay-path identity. The relay maps pubkey → id but never proves to
	/// the requester WHICH pubkey owns the target id — it only enforces `target ==
	/// id` before emitting `PeerFound`. So a malicious/compromised relay (the
	/// stated self-hostable, ciphertext-only threat model) or an attacker that won
	/// the pubkey→id registration race after a TTL eviction could answer in the
	/// target's place with its OWN key. When `expected` is `Some(pk)` (e.g. the
	/// caller pinned the key on a prior connect — TOFU), the `PeerFound` handler
	/// drops any answer carrying a different key, mirroring the direct-path
	/// `connect_direct(_, Some(pk))` pin. `None` keeps the original TOFU-first
	/// behavior. Read [`Session::peer_pubkey`] after connecting to record the pin.
	pub async fn connect_pinned(
		self: &Arc<Self>,
		target: DeviceId,
		expected: Option<PublicKey>,
	) -> Result<Session, ConnError> {
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
		// When we have a pinned key, arm an identity-mismatch flag so we can
		// distinguish a legitimate-but-changed identity from a plain offline host.
		let im: Option<Arc<std::sync::atomic::AtomicBool>> =
			expected.map(|_| Arc::new(std::sync::atomic::AtomicBool::new(false)));
		// Mint our per-session salt and remember it: we send it now in `hello`, but
		// derive the key later (in the `PeerFound` handler) once we have the peer's.
		let our_salt = random_salt();
		{
			let mut g = self.inner.lock().await;
			g.peer_found.insert(session, pf.clone());
			g.pending_salt.insert(session, our_salt);
			// Pin the expected identity for this id (TOFU / known-host): the
			// `PeerFound` handler drops an answer carrying any other key so the relay
			// can't substitute a different peer behind the requested id. Absent ⇒
			// the original accept-any (TOFU-first) behavior.
			if let Some(pk) = expected {
				g.expected_pubkey.insert(session, pk);
			}
			if let Some(flag) = &im {
				g.identity_mismatch.insert(session, flag.clone());
			}
		}
		// Handshake blob: static pubkey(32) || fresh session salt(32) || optional
		// LAN candidate (same-NAT peers punch our PRIVATE addr too — see
		// `parse_lan_candidate`).
		let mut hello = self.identity.public_bytes().to_vec();
		hello.extend_from_slice(&our_salt);
		self.push_lan_candidate(&mut hello);
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
			g.identity_mismatch.remove(&session);
			// On failure the key is never derived, so drop the stashed salt too —
			// and a PeerFound landing between the timeout and this cleanup may
			// already have inserted the session; nothing will ever use it.
			if rv.is_err() {
				g.pending_salt.remove(&session);
				g.expected_pubkey.remove(&session);
				g.sessions.remove(&session);
			}
		}
		if rv.is_err() {
			// Was the timeout caused by a pinned-key mismatch? The handler sets the
			// `im` flag before dropping the answer, so if the flag is set the peer DID
			// answer but with a different key (legitimate identity rotation or re-mint)
			// — a recoverable `IdentityChanged`, not a plain "target offline".
			let mismatched = im
				.as_ref()
				.map(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
				.unwrap_or(false);
			return Err(if mismatched {
				ConnError::IdentityChanged(target)
			} else {
				ConnError::TargetUnreachable(target)
			});
		}

		// Decide the transport. On failure (P2pOnly punch miss) no `Session` is
		// ever built, so its Drop-based cleanup never runs — remove the state
		// here or one SessionState leaks per failed connect.
		let transport = match self.establish_transport(session).await {
			Ok(t) => t,
			Err(e) => {
				self.inner.lock().await.sessions.remove(&session);
				return Err(e);
			}
		};

		// Wire up an inbound data channel for the caller.
		let (data_tx, data_rx) = mpsc::unbounded_channel();
		let transport = {
			let mut g = self.inner.lock().await;
			match g.sessions.get_mut(&session) {
				Some(s) => {
					// The punch handlers may have proven the direct path while
					// our PunchAck waiter timed out — never downgrade such a
					// session to the relay (data would ride the relay while
					// `live_transport()` reports Direct).
					s.transport = if s.direct_ok {
						Transport::Direct
					} else {
						transport
					};
					s.data_tx = Some(data_tx);
					s.transport
				}
				None => transport,
			}
		};
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
		peer_pubkey: Option<PublicKey>,
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
		// When we have a pinned key, arm an identity-mismatch flag so we can
		// distinguish a legitimate-but-changed identity from a plain P2P failure
		// (mirrors the same plumbing in connect_pinned for the relay path).
		let im: Option<Arc<std::sync::atomic::AtomicBool>> =
			peer_pubkey.map(|_| Arc::new(std::sync::atomic::AtomicBool::new(false)));
		{
			let mut g = self.inner.lock().await;
			g.hello_done.insert(session, hp.clone());
			g.pending_salt.insert(session, our_salt);
			// When the caller already knows the peer's key, pin it: the `HelloAck`
			// handler drops an ack carrying any other key, so an attacker that answers
			// the Hello in the peer's place can't terminate the E2E with its own key
			// (silent MITM of the direct path). Absent ⇒ in-band/TOFU as before.
			if let Some(pk) = peer_pubkey {
				g.expected_pubkey.insert(session, pk);
			}
			if let Some(flag) = &im {
				g.identity_mismatch.insert(session, flag.clone());
			}
		}
		// The caller may race this whole future against its own (shorter) timeout
		// — the LAN fast path uses 1.5 s vs our 3 s — so if we're DROPPED at an
		// await point below, this guard sweeps the entries just inserted (plus
		// any half-built session) instead of leaking them per cancelled attempt.
		let mut guard = DirectGuard {
			node: self.clone(),
			session,
			armed: true,
		};
		for _ in 0..PUNCH_ATTEMPTS {
			let _ = self.sock.send_to(&hello(), peer_addr).await;
			tokio::time::sleep(Duration::from_millis(40)).await;
		}
		let hr = timeout(RENDEZVOUS_TIMEOUT, hp.notified()).await;
		if hr.is_err() {
			// Remove the waiter + stashed salt on the timeout path too so neither
			// map leaks one stale entry per failed direct connect — and a session
			// a late HelloAck may have inserted; nothing will ever use it.
			let mut g = self.inner.lock().await;
			g.hello_done.remove(&session);
			g.pending_salt.remove(&session);
			g.expected_pubkey.remove(&session);
			g.identity_mismatch.remove(&session);
			g.sessions.remove(&session);
			guard.armed = false;
			// Was the timeout caused by a pinned-key mismatch? The HelloAck handler
			// sets the `im` flag before dropping the ack, so if the flag is set the
			// peer DID answer but with a different key (legitimate identity rotation)
			// — return `IdentityChanged` so the caller can surface the actionable
			// "identity changed — forget and retry?" prompt instead of the generic
			// `P2pFailed`. Mirrors the same check in `connect_pinned`.
			let mismatched = im
				.as_ref()
				.map(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
				.unwrap_or(false);
			return Err(if mismatched {
				ConnError::IdentityChanged(DeviceId(0))
			} else {
				ConnError::P2pFailed
			});
		}
		{
			let mut g = self.inner.lock().await;
			g.hello_done.remove(&session);
			// The handshake succeeded — the mismatch flag is no longer needed.
			g.identity_mismatch.remove(&session);
			match g.sessions.get_mut(&session) {
				Some(s) => s.data_tx = Some(data_tx),
				None => return Err(ConnError::P2pFailed), // guard sweeps the salt
			}
		}

		// Hole-punch the direct path; no relay fallback (there is no relay here).
		self.establish_transport_direct(session, peer_addr).await;
		// From here the returned `Session`'s Drop owns the state cleanup.
		guard.armed = false;
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

/// Cancel-safety guard for [`Node::connect_direct`]: when the future is dropped
/// mid-flight (caller-side timeout), remove the session's waiter/salt entries —
/// and any half-built `SessionState` — that the in-line cleanup never reached.
/// Disarmed on every path that already cleaned up (or handed ownership to the
/// returned `Session`'s Drop).
struct DirectGuard {
	node: Arc<Node>,
	session: SessionId,
	armed: bool,
}

impl Drop for DirectGuard {
	fn drop(&mut self) {
		if !self.armed {
			return;
		}
		let node = self.node.clone();
		let session = self.session;
		// Same pattern as Session::drop — Drop can't await the inner lock.
		if let Ok(handle) = tokio::runtime::Handle::try_current() {
			handle.spawn(async move {
				let mut g = node.inner.lock().await;
				g.hello_done.remove(&session);
				g.pending_salt.remove(&session);
				g.expected_pubkey.remove(&session);
				g.identity_mismatch.remove(&session);
				g.punched.remove(&session);
				g.sessions.remove(&session);
			});
		}
	}
}

pub(super) async fn recv_loop(weak: Weak<Node>) {
	let (sock, shutdown) = match weak.upgrade() {
		Some(n) => (n.sock.clone(), n.shutdown.clone()),
		None => return,
	};
	let mut buf = vec![0u8; 65_536];
	loop {
		let recvd = tokio::select! {
			r = sock.recv_from(&mut buf) => r,
			// `Node::drop` signals here so this task releases its socket Arc
			// (and the bound port) immediately, not at the next stray datagram.
			_ = shutdown.notified() => return,
		};
		let (n, from) = match recvd {
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
