//! An established, end-to-end-encrypted [`Session`] with a peer.

use super::*;

/// An established (encrypted) session with a peer.
pub struct Session {
	pub(super) id: SessionId,
	pub(super) peer: DeviceId,
	pub(super) transport: Transport,
	pub(super) node: Arc<Node>,
	pub(super) data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
	/// The relay's per-session forwarding rate cap (kbit/s; `0` = uncapped). Set from the
	/// `PeerFound` on the relay-fallback rendezvous path, `0` on direct/host-accept paths.
	/// Meaningful only when [`transport`](Self::transport) is `Relay` (then the media rides
	/// the relay and its cap applies) — the caller clamps its stream bitrate accordingly.
	pub(super) rate_cap_kbps: u32,
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
	/// The relay's per-session forwarding rate cap in kbit/s (`0` = uncapped). Apply it as a
	/// stream-bitrate ceiling only when [`transport`](Self::transport) is `Relay`.
	pub fn rate_cap_kbps(&self) -> u32 {
		self.rate_cap_kbps
	}

	/// Encrypt and send a payload over whichever transport this session uses.
	pub async fn send(&self, payload: &[u8]) -> Result<(), ConnError> {
		send_payload(&self.node, self.id, payload).await
	}

	/// A cloneable send-only handle to this session, so other tasks (the media
	/// forwarder) can transmit concurrently while the owner keeps `recv()`.
	pub fn sender(&self) -> SessionSender {
		SessionSender {
			node: self.node.clone(),
			id: self.id,
		}
	}

	/// Receive the next decrypted inbound payload.
	pub async fn recv(&mut self) -> Option<Vec<u8>> {
		self.data_rx.recv().await
	}

	/// Non-blocking receive of an already-queued inbound payload, or `None` if the
	/// queue is momentarily empty. Lets a reader drain a burst backlog (and drop
	/// stale frames) instead of decoding a FIFO queue at real-time speed forever.
	pub fn try_recv(&mut self) -> Option<Vec<u8>> {
		self.data_rx.try_recv().ok()
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
			.map(|s| {
				if s.direct_ok {
					Transport::Direct
				} else {
					s.transport
				}
			})
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

	/// The peer's static X25519 public key that this session's E2E key was derived
	/// against. The caller can pin it to the requested id on first connect (TOFU)
	/// and pass it back as `connect_pinned`'s `expected` on later connects so a
	/// malicious relay can't silently substitute a different peer behind a known
	/// id. `None` only if the session state was already torn down.
	pub async fn peer_pubkey(&self) -> Option<PublicKey> {
		self.node
			.inner
			.lock()
			.await
			.sessions
			.get(&self.id)
			.map(|s| s.peer_pubkey)
	}
}

/// A cloneable, send-only handle to an established session (see [`Session::sender`]).
/// Sends the same sealed frames over the same transport; safe to use concurrently
/// with the owning `Session` (the per-session crypto seq is serialized internally).
#[derive(Clone)]
pub struct SessionSender {
	node: Arc<Node>,
	id: SessionId,
}

impl SessionSender {
	pub async fn send(&self, payload: &[u8]) -> Result<(), ConnError> {
		send_payload(&self.node, self.id, payload).await
	}
}

/// Shared seal+route body for [`Session::send`] / [`SessionSender::send`].
async fn send_payload(node: &Arc<Node>, id: SessionId, payload: &[u8]) -> Result<(), ConnError> {
	// Hold node.inner ONLY to snapshot what we need: clone the (Arc-backed) crypto
	// handle, read+bump the send seq, and grab the route. The ChaCha20-Poly1305 seal
	// then runs AFTER the lock is released so a concurrent recv_loop isn't blocked
	// behind a full encrypt — the lock hold shrinks to ~one Arc clone + a HashMap
	// lookup. `send_seq` stays a plain counter bumped under this brief lock.
	let (crypto, seq, transport, peer_addr, relay_creds) = {
		let mut g = node.inner.lock().await;
		// Copy the relay creds out before the mutable session borrow (only the relay
		// path needs them; the direct path leaves them unused, as before).
		let self_id = g.self_id;
		let token = g.token;
		let s = g.sessions.get_mut(&id).ok_or(ConnError::P2pFailed)?;
		let seq = s.send_seq;
		s.send_seq += 1;
		let crypto = s.crypto.clone();
		let transport = s.transport;
		let peer_addr = s.peer_addr;
		let relay_creds = match transport {
			Transport::Direct => None,
			Transport::Relay => Some((
				self_id.ok_or(ConnError::NotRegistered)?,
				token.ok_or(ConnError::NotRegistered)?,
			)),
		};
		(crypto, seq, transport, peer_addr, relay_creds)
	};
	// Seal off-lock, then frame for the chosen transport.
	let inner = PeerMsg::Data {
		session: id,
		seq,
		payload: crypto.seal(seq, payload),
	};
	let (frame, dest) = match transport {
		Transport::Direct => (encode(&inner), peer_addr),
		Transport::Relay => {
			let (self_id, token) = relay_creds.expect("relay_creds set on the relay path");
			(
				encode(&ClientMsg::RelayData {
					id: self_id,
					token,
					session: id,
					payload: encode(&inner),
				}),
				node.relay,
			)
		}
	};
	node.sock.send_to(&frame, dest).await?;
	Ok(())
}
