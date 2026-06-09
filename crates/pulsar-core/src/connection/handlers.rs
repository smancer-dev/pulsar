//! Inbound datagram dispatch: hole-punch transport establishment plus the relay
//! and peer message handlers that drive a [`Node`]'s session state machine.

use super::*;

impl Node {
	pub(super) async fn establish_transport(
		&self,
		session: SessionId,
	) -> Result<Transport, ConnError> {
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
		let res = timeout(PUNCH_TIMEOUT, pk.notified()).await;
		// The waiter is only needed during the punch — remove it on every path so the
		// `punched` map doesn't grow one stale `Arc<Notify>` per session, forever.
		self.inner.lock().await.punched.remove(&session);
		match res {
			Ok(()) => Ok(Transport::Direct),
			Err(_) if self.mode == NetworkMode::Auto => Ok(Transport::Relay),
			Err(_) => Err(ConnError::P2pFailed),
		}
	}

	/// Punch a direct path to a known addr. Never relay-falls-back (direct-IP has no
	/// relay). Best-effort: on a LAN the punch ack arrives; if not, data still flows
	/// to the user-provided (reachable) address.
	pub(super) async fn establish_transport_direct(&self, session: SessionId, peer_addr: SocketAddr) {
		let pk = Arc::new(Notify::new());
		self.inner.lock().await.punched.insert(session, pk.clone());
		for seq in 0..PUNCH_ATTEMPTS as u32 {
			let _ = self
				.sock
				.send_to(&encode(&PeerMsg::Punch { session, seq }), peer_addr)
				.await;
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
		let _ = timeout(PUNCH_TIMEOUT, pk.notified()).await;
		// Remove the waiter regardless of outcome so the `punched` map doesn't leak one
		// stale `Arc<Notify>` per direct connect.
		self.inner.lock().await.punched.remove(&session);
	}

	pub(super) async fn handle_datagram(self: &Arc<Self>, buf: &[u8], from: SocketAddr) {
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
				// answer = peer pubkey(32) || peer salt(32). Reject a malformed blob
				// rather than index-slicing past the end.
				let Some((key, peer_salt)) = split_handshake(&answer) else {
					tracing::warn!(session, "PeerFound answer blob too short");
					return;
				};
				let mut g = self.inner.lock().await;
				// Pair the peer's salt with the one we minted in `connect`.
				let Some(our_salt) = g.pending_salt.remove(&session) else {
					tracing::warn!(session, "PeerFound for an unknown/expired connect");
					return;
				};
				let crypto = self
					.identity
					.session(key, Role::Initiator, session, our_salt, peer_salt);
				g.sessions.insert(
					session,
					SessionState {
						peer_addr: target_addr,
						crypto,
						transport: Transport::Relay,
						send_seq: 0,
						direct_ok: false,
						data_tx: None,
						our_salt,
					},
				);
				if let Some(n) = g.peer_found.get(&session).cloned() {
					n.notify_one();
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
		// hello = requester pubkey(32) || requester salt(32). Reject a malformed blob.
		let Some((key, peer_salt)) = split_handshake(&hello) else {
			tracing::warn!(session, "incoming hello blob too short");
			return;
		};
		// Retransmit guard: a duplicated requester Connect (UDP can duplicate) makes the
		// relay emit a 2nd Incoming. If this session already exists, re-send Accept with
		// the SAME salt we already committed — minting a fresh salt would derive a
		// mismatched key (the requester only derives once) — and do NOT overwrite the
		// session or emit a second Session (mirrors the direct-IP Hello retransmit guard).
		{
			let g = self.inner.lock().await;
			if let Some(s) = g.sessions.get(&session) {
				let our_salt = s.our_salt;
				let (id, token) = (g.self_id, g.token);
				drop(g);
				if let (Some(id), Some(token)) = (id, token) {
					let mut answer = self.identity.public_bytes().to_vec();
					answer.extend_from_slice(&our_salt);
					let _ = self
						.sock
						.send_to(
							&encode(&ClientMsg::Accept {
								id,
								token,
								session,
								answer,
							}),
							self.relay,
						)
						.await;
				}
				return;
			}
		}
		// Mint our salt; both salts + the session id bind this specific session's key.
		let our_salt = random_salt();
		let crypto = self
			.identity
			.session(key, Role::Responder, session, our_salt, peer_salt);
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
					our_salt,
				},
			);
			(g.self_id, g.token)
		};
		let (Some(id), Some(token)) = (id, token) else {
			return;
		};

		// Accept: send back our pubkey(32) || our salt(32) as the answer, then punch.
		let mut answer = self.identity.public_bytes().to_vec();
		answer.extend_from_slice(&our_salt);
		let _ = self
			.sock
			.send_to(
				&encode(&ClientMsg::Accept {
					id,
					token,
					session,
					answer,
				}),
				self.relay,
			)
			.await;
		// RelayOnly "skips punching entirely" (don't reveal our addr); the relay
		// carries the traffic. Other modes probe to upgrade to a direct path.
		if self.mode != NetworkMode::RelayOnly {
			for seq in 0..PUNCH_ATTEMPTS as u32 {
				let _ = self
					.sock
					.send_to(&encode(&PeerMsg::Punch { session, seq }), from_addr)
					.await;
			}
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
				// Ack the connectivity probe regardless (harmless). But a RelayOnly node
				// must NOT switch the data transport to Direct even if a peer in
				// Auto/P2pOnly punches us — otherwise traffic would bypass the relay.
				let _ = self
					.sock
					.send_to(&encode(&PeerMsg::PunchAck { session, seq }), from)
					.await;
				if self.mode != NetworkMode::RelayOnly {
					let mut g = self.inner.lock().await;
					if let Some(s) = g.sessions.get_mut(&session) {
						s.direct_ok = true;
						s.transport = Transport::Direct;
						s.peer_addr = from;
					}
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
			PeerMsg::Hello {
				session,
				pubkey,
				salt: peer_salt,
			} => {
				// Inbound direct-IP connect (no relay): derive crypto as Responder,
				// reply with our key + salt, punch, and emit a Session via next_incoming.
				let mut g = self.inner.lock().await;
				if let Some(s) = g.sessions.get(&session) {
					// Hello retransmit — re-ack with the SAME salt we already committed
					// to (a different salt would derive a mismatched key), so a lost
					// HelloAck still recovers.
					let our_salt = s.our_salt;
					drop(g);
					let _ = self
						.sock
						.send_to(
							&encode(&PeerMsg::HelloAck {
								session,
								pubkey: self.identity.public_bytes(),
								salt: our_salt,
							}),
							from,
						)
						.await;
					return;
				}
				let our_salt = random_salt();
				let crypto =
					self.identity
						.session(pubkey, Role::Responder, session, our_salt, peer_salt);
				let (data_tx, data_rx) = mpsc::unbounded_channel();
				g.sessions.insert(
					session,
					SessionState {
						peer_addr: from,
						crypto,
						transport: Transport::Direct,
						send_seq: 0,
						direct_ok: false,
						data_tx: Some(data_tx),
						our_salt,
					},
				);
				drop(g);
				let _ = self
					.sock
					.send_to(
						&encode(&PeerMsg::HelloAck {
							session,
							pubkey: self.identity.public_bytes(),
							salt: our_salt,
						}),
						from,
					)
					.await;
				for seq in 0..PUNCH_ATTEMPTS as u32 {
					let _ = self
						.sock
						.send_to(&encode(&PeerMsg::Punch { session, seq }), from)
						.await;
				}
				let _ = self.incoming_tx.send(Session {
					id: session,
					peer: DeviceId(0),
					transport: Transport::Direct,
					node: self.clone(),
					data_rx,
				});
			}
			PeerMsg::HelloAck {
				session,
				pubkey,
				salt: peer_salt,
			} => {
				let mut g = self.inner.lock().await;
				if !g.sessions.contains_key(&session) {
					// Pair the peer's salt with the one we minted in `connect_direct`.
					let Some(our_salt) = g.pending_salt.remove(&session) else {
						tracing::warn!(session, "HelloAck for an unknown/expired direct connect");
						return;
					};
					let crypto = self.identity.session(
						pubkey,
						Role::Initiator,
						session,
						our_salt,
						peer_salt,
					);
					g.sessions.insert(
						session,
						SessionState {
							peer_addr: from,
							crypto,
							transport: Transport::Direct,
							send_seq: 0,
							direct_ok: false,
							data_tx: None,
							our_salt,
						},
					);
				}
				if let Some(n) = g.hello_done.get(&session).cloned() {
					n.notify_one();
				}
			}
		}
	}

	/// Decrypt an inbound payload and hand it to the session's reader.
	///
	/// `crypto.open` both authenticates the ciphertext **and** enforces sliding-window
	/// anti-replay on `seq` (RFC 6479 style, reorder-tolerant since UDP datagrams
	/// arrive out of order). A failure — bad ciphertext or a replayed/too-old seq — is
	/// silently dropped, which is the right thing for UDP.
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
