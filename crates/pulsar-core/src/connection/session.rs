//! An established, end-to-end-encrypted [`Session`] with a peer.

use super::*;

/// An established (encrypted) session with a peer.
pub struct Session {
	pub(super) id: SessionId,
	pub(super) peer: DeviceId,
	pub(super) transport: Transport,
	pub(super) node: Arc<Node>,
	pub(super) data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
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
			let s = g.sessions.get_mut(&self.id).ok_or(ConnError::P2pFailed)?;
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
}
