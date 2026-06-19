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
		let (peer_addr, peer_lan) = match self.inner.lock().await.sessions.get(&session) {
			Some(s) => (s.peer_addr, s.peer_lan),
			None => return Err(ConnError::P2pFailed),
		};

		let pk = Arc::new(Notify::new());
		self.inner.lock().await.punched.insert(session, pk.clone());
		for seq in 0..PUNCH_ATTEMPTS as u32 {
			let _ = self
				.sock
				.send_to(&encode(&PeerMsg::Punch { session, seq }), peer_addr)
				.await;
			// Same-NAT peer: also punch its PRIVATE candidate — the public addr
			// above is a router hairpin there (lossy); whichever path answers
			// first wins `peer_addr` (LAN preferred, see the Punch handler).
			if let Some(lan) = peer_lan {
				let _ = self
					.sock
					.send_to(&encode(&PeerMsg::Punch { session, seq }), lan)
					.await;
			}
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
	pub(super) async fn establish_transport_direct(
		&self,
		session: SessionId,
		peer_addr: SocketAddr,
	) {
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
				// A re-register (NotRegistered → register_msg) after a relay restart
				// that lost `by_pubkey` mints a DIFFERENT id. Detect that rotation so
				// the app can re-advertise it; the UI/LAN beacon snapshot the id once
				// and would otherwise keep broadcasting the dead old one forever.
				let rotated = matches!(g.self_id, Some(prev) if prev != id);
				g.self_id = Some(id);
				g.token = Some(token);
				// Clear any stale register error: we're registered now.
				g.register_error = None;
				drop(g);
				self.registered.notify_waiters();
				self.registered.notify_one();
				if rotated {
					// notify_one stores a permit, so a rotation that lands while the
					// watcher is mid-processing (not parked in `notified()`) isn't lost.
					self.id_changed.notify_one();
				}
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
				rate_cap_kbps,
			} => {
				// answer = peer pubkey(32) || peer salt(32) || optional LAN candidate.
				// Reject a malformed blob rather than index-slicing past the end.
				let Some((key, peer_salt)) = split_handshake(&answer) else {
					tracing::warn!(session, "PeerFound answer blob too short");
					return;
				};
				let peer_lan = parse_lan_candidate(&answer);
				let mut g = self.inner.lock().await;
				// If the caller pinned the expected identity for this id (TOFU /
				// known-host), DROP a PeerFound whose key differs: the relay only
				// proves it enforced `target == id`, NOT which pubkey owns that id, so
				// a malicious/compromised relay (the stated threat model) or an
				// attacker that won the pubkey→id registration race could otherwise
				// answer in the target's place and terminate the E2E with its own key.
				// Leave the waiter/salt untouched so a legitimate retransmitted
				// PeerFound can still complete within the connect timeout; if none
				// arrives the connect fails `TargetUnreachable` (mirrors `HelloAck`).
				if let Some(&expected) = g.expected_pubkey.get(&session) {
					if key != expected {
						tracing::warn!(
							session,
							"PeerFound pubkey != expected (pinned) key — dropping (relay/MITM identity mismatch)"
						);
						// Signal the mismatch flag so `connect_pinned` can return
						// `IdentityChanged` instead of the generic `TargetUnreachable`.
						if let Some(flag) = g.identity_mismatch.get(&session) {
							flag.store(true, std::sync::atomic::Ordering::Release);
						}
						return;
					}
				}
				// Pair the peer's salt with the one we minted in `connect`.
				let Some(our_salt) = g.pending_salt.remove(&session) else {
					tracing::warn!(session, "PeerFound for an unknown/expired connect");
					return;
				};
				// The key is accepted (matched the pin, or none was set) — the pin has
				// served its purpose for this session.
				g.expected_pubkey.remove(&session);
				let crypto =
					self.identity
						.session(key, Role::Initiator, session, our_salt, peer_salt);
				g.sessions.insert(
					session,
					SessionState {
						peer_addr: target_addr,
						peer_lan,
						crypto,
						peer_pubkey: key,
						transport: Transport::Relay,
						send_seq: 0,
						direct_ok: false,
						data_tx: None,
						our_salt,
					},
				);
				// Stash the relay's per-session cap (kbit/s, 0 = uncapped) so `connect` can copy
				// it onto the returned Session — used to clamp the encoder when this session ends
				// up relayed (the relay forwards the media, so its per-session cap applies).
				g.pending_rate_cap.insert(session, rate_cap_kbps);
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
				// A relay refusal that arrives while we're still UNREGISTERED (initial
				// `register()` in flight, no id yet) is fatal for registration — most
				// notably `IncompatibleVersion`. Record it and wake the `register()`
				// waiter so it fails with a specific `ConnError` (e.g. a clear "update
				// required" message) instead of waiting out the full `REGISTER_TIMEOUT`
				// and reporting a generic `RelayTimeout`.
				{
					let mut g = self.inner.lock().await;
					if g.self_id.is_none() {
						g.register_error = Some(code);
						drop(g);
						self.registered.notify_waiters();
						self.registered.notify_one();
					} else if code == ErrCode::IncompatibleVersion
						|| (code == ErrCode::Protocol
							&& message == "incompatible protocol version")
					{
						// Already registered but the relay now rejects a re-Register with a
						// version-mismatch error (relay was redeployed with a newer
						// PROTOCOL_VERSION). The heartbeat will keep bouncing; every future
						// Register attempt will be refused too. Signal the version-error
						// watcher so the app can surface an "update required" message and go
						// offline — instead of silently advertising a dead id forever.
						//
						// IMPORTANT — only treat `ErrCode::Protocol` as a version error when
						// the message is exactly "incompatible protocol version" (the relay's
						// dedicated string for Register-version rejections). The relay also
						// emits `ErrCode::Protocol` for benign stale-session conditions on
						// RelayData/Accept ("no such session", "not a session member",
						// "not the session target") — those arrive in relay-fallback sessions
						// whenever the relay restarts and loses its in-memory sessions map or
						// the session is GC'd at SESSION_TTL. Treating those as a version
						// error would falsely strand a still-registered host offline with a
						// permanent "update required" banner.
						//
						// Wire-compat note: the relay emits `ErrCode::Protocol` (not
						// `IncompatibleVersion`) for version-mismatch so old builds that
						// pre-date `IncompatibleVersion` can decode the error. We match both
						// so the future two-phase rollout (flip the relay to `IncompatibleVersion`
						// once all deployed clients can decode it) needs no client change.
						drop(g);
						self.version_error.notify_one();
					}
				}
				// The relay evicted us (>TTL outage / relay restart): our id+token
				// are stale, every heartbeat keeps bouncing, and this device is
				// unreachable by ID until re-registered. Re-send Register — if the
				// relay still has our pubkey → id mapping it reissues the SAME
				// 9-digit ID; but a full relay restart loses `by_pubkey` and mints
				// a DIFFERENT id, so the `Registered` arm signals `id_changed` and
				// the app re-advertises the new id. Bounded by the heartbeat
				// cadence: at most one retry per bounce.
				if code == ErrCode::NotRegistered {
					let _ = self
						.sock
						.send_to(&encode(&self.register_msg()), self.relay)
						.await;
				}
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
		// hello = requester pubkey(32) || requester salt(32) || optional LAN
		// candidate. Reject a malformed blob.
		let Some((key, peer_salt)) = split_handshake(&hello) else {
			tracing::warn!(session, "incoming hello blob too short");
			return;
		};
		let peer_lan = parse_lan_candidate(&hello);
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
					self.push_lan_candidate(&mut answer);
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

		// Read self_id + token BEFORE inserting the session so we never leak a
		// dangling SessionState + mpsc channel when the node is unregistered.
		// (An attacker-chosen session id on an unregistered node would otherwise
		// leave one SessionState per packet, growing unboundedly — conn-r15-2.)
		let (id, token) = {
			let g = self.inner.lock().await;
			(g.self_id, g.token)
		};
		let (Some(id), Some(token)) = (id, token) else {
			return;
		};

		{
			let mut g = self.inner.lock().await;
			g.sessions.insert(
				session,
				SessionState {
					peer_addr: from_addr,
					peer_lan,
					crypto,
					peer_pubkey: key,
					transport: Transport::Relay,
					send_seq: 0,
					direct_ok: false,
					data_tx: Some(data_tx),
					our_salt,
				},
			);
		}

		// Accept: send back our pubkey(32) || our salt(32) || optional LAN candidate
		// as the answer, then punch.
		let mut answer = self.identity.public_bytes().to_vec();
		answer.extend_from_slice(&our_salt);
		self.push_lan_candidate(&mut answer);
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
		// carries the traffic. Other modes probe to upgrade to a direct path —
		// including the peer's PRIVATE candidate (same-NAT: the public addr is a
		// lossy router hairpin; the LAN path wins via the Punch-handler preference).
		// Paced like establish_transport's burst, in its own task: the first probes
		// beat the requester's PeerFound by one relay RTT and are dropped (no
		// session there yet) — the paced retransmits land after it exists, and
		// recv_loop keeps draining meanwhile.
		if self.mode != NetworkMode::RelayOnly {
			let sock = self.sock.clone();
			tokio::spawn(async move {
				for seq in 0..PUNCH_ATTEMPTS as u32 {
					let _ = sock
						.send_to(&encode(&PeerMsg::Punch { session, seq }), from_addr)
						.await;
					if let Some(lan) = peer_lan {
						let _ = sock
							.send_to(&encode(&PeerMsg::Punch { session, seq }), lan)
							.await;
					}
					tokio::time::sleep(Duration::from_millis(20)).await;
				}
			});
		}

		let _ = self.incoming_tx.send(Session {
			id: session,
			peer: from,
			transport: Transport::Relay,
			node: self.clone(),
			data_rx,
			rate_cap_kbps: 0, // host side accepts; the client is the one that requests bitrate
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
						// SECURITY: only adopt the data path if `from` is one of this
						// session's known punch candidates (the relay-observed public addr
						// or the LAN candidate we were given during rendezvous). Without
						// this gate, any party that learns the SessionId (a compromised
						// relay, an on-LAN sniff) can redirect the victim's data path to
						// itself and suppress relay fallback. Mirrors the PunchAck guard
						// at the PunchAck arm below.
						if from == s.peer_addr || Some(from) == s.peer_lan {
							// PREFER THE PRIVATE PATH: with both the LAN route and the
							// router-hairpin route alive, punch retransmits would otherwise
							// flap `peer_addr` between them — and the hairpin route is the
							// lossy one. Once a private source is locked in, a public
							// arrival no longer overwrites it.
							if !(s.direct_ok && is_private_v4(s.peer_addr)) || is_private_v4(from) {
								s.peer_addr = from;
							}
							s.direct_ok = true;
							s.transport = Transport::Direct;
						}
					}
				}
			}
			PeerMsg::PunchAck { session, .. } => {
				// Mirror of the Punch arm's mode guard: a RelayOnly node never
				// punches, so any ack is gratuitous — and must not flip Direct.
				if self.mode == NetworkMode::RelayOnly {
					return;
				}
				let mut g = self.inner.lock().await;
				let Some(s) = g.sessions.get_mut(&session) else {
					return;
				};
				// Only honor acks from one of THIS session's punch candidates:
				// every Pulsar binds the same well-known port and acks probes for
				// unknown sessions, so a stale private candidate can resolve to a
				// DIFFERENT device on our LAN — its ack would lock Direct while
				// `peer_addr` still points at an unpunchable public addr,
				// suppressing the relay fallback.
				if from != s.peer_addr && Some(from) != s.peer_lan {
					return;
				}
				// The ack's source IS a verified working path — adopt it, with the
				// same prefer-the-private-path rule as the Punch arm (otherwise the
				// initiator keeps the relay-observed public addr and its data
				// blackholes on non-hairpin NATs / rides the lossy hairpin).
				if !(s.direct_ok && is_private_v4(s.peer_addr)) || is_private_v4(from) {
					s.peer_addr = from;
				}
				s.direct_ok = true;
				s.transport = Transport::Direct;
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
				// SECURITY (conn-r15-1): reject a Hello whose session id matches one of
				// OUR in-flight outbound connect_direct attempts.  An on-LAN attacker
				// that snoops (or receives) our Hello{session, our_pk, our_salt} knows
				// the random session id and can race us with its own Hello{session,
				// attacker_pk, ...}; without this gate our Hello handler would derive
				// Responder crypto against the attacker's key, insert a session, and
				// emit a Session — completing a full MITM before any pin check runs.
				// `pending_salt` and `hello_done` together uniquely identify a session
				// id we own as the initiator; a legitimate inbound initiator from a
				// different machine cannot have either entry for a freshly-random id.
				if g.pending_salt.contains_key(&session) || g.hello_done.contains_key(&session) {
					tracing::warn!(
						session,
						"inbound Hello for an in-flight outbound connect_direct — dropping (possible MITM)"
					);
					return;
				}
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
						peer_lan: None, // direct-IP: the typed address IS the path
						crypto,
						peer_pubkey: pubkey,
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
					rate_cap_kbps: 0, // direct-IP host accept: no relay, no cap
				});
			}
			PeerMsg::HelloAck {
				session,
				pubkey,
				salt: peer_salt,
			} => {
				let mut g = self.inner.lock().await;
				if !g.sessions.contains_key(&session) {
					// If the caller pinned the peer's key (beacon/known-host path),
					// DROP an ack carrying any other key: an attacker that answers our
					// Hello in the peer's place would otherwise terminate the E2E with
					// its own key (silent MITM). We don't notify the waiter or consume
					// our salt, so a legitimate retransmitted ack can still complete
					// within the connect timeout; if none arrives it fails P2pFailed.
					if let Some(&expected) = g.expected_pubkey.get(&session) {
						if pubkey != expected {
							tracing::warn!(
								session,
								"HelloAck pubkey != expected (pinned) key — dropping (possible MITM)"
							);
							// Signal the mismatch flag so the caller can return
							// `IdentityChanged` instead of the generic `P2pFailed`.
							if let Some(flag) = g.identity_mismatch.get(&session) {
								flag.store(true, std::sync::atomic::Ordering::Release);
							}
							return;
						}
					}
					// Pair the peer's salt with the one we minted in `connect_direct`.
					let Some(our_salt) = g.pending_salt.remove(&session) else {
						tracing::warn!(session, "HelloAck for an unknown/expired direct connect");
						return;
					};
					// The key is accepted (matched the pin, or none was set) — the
					// pin has served its purpose for this session.
					g.expected_pubkey.remove(&session);
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
							peer_lan: None, // direct-IP: the typed address IS the path
							crypto,
							peer_pubkey: pubkey,
							transport: Transport::Direct,
							send_seq: 0,
							direct_ok: false,
							data_tx: None,
							our_salt,
						},
					);
				} else {
					// SECURITY (conn-r15-1, defense-in-depth): the session already
					// exists — meaning the inbound Hello path raced the HelloAck and
					// the MITM guard above (Part 1) didn't fire (e.g. timing edge).
					// If this was a pinned connect, verify the existing session's
					// peer_pubkey matches the pin; on mismatch evict the session so
					// connect_direct returns IdentityChanged instead of handing the
					// caller a session keyed to the attacker.
					if let Some(&expected) = g.expected_pubkey.get(&session) {
						let actual = g.sessions.get(&session).map(|s| s.peer_pubkey);
						if actual != Some(expected) {
							tracing::warn!(
								session,
								"HelloAck: existing session peer_pubkey != pinned expected key — evicting (possible MITM)"
							);
							g.sessions.remove(&session);
							if let Some(flag) = g.identity_mismatch.get(&session) {
								flag.store(true, std::sync::atomic::Ordering::Release);
							}
							return;
						}
					}
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

#[cfg(test)]
mod tests {
	use super::*;

	const SESSION: SessionId = 42;

	fn addr(s: &str) -> SocketAddr {
		s.parse().unwrap()
	}

	/// A node whose handlers we drive directly — the relay addr is a black hole
	/// (no relay involved) and outbound acks/punches go nowhere; only the state
	/// transitions matter here.
	async fn test_node() -> Arc<Node> {
		Node::bind(
			addr("127.0.0.1:0"),
			addr("198.51.100.1:21116"),
			NetworkMode::Auto,
		)
		.await
		.expect("bind test node")
	}

	/// `PeerFound` answer blob: peer pubkey(32) || peer salt(32) || LAN tail.
	fn answer_with_lan(lan: SocketAddr) -> Vec<u8> {
		let mut blob = vec![7u8; 32];
		blob.extend_from_slice(&[9u8; 32]);
		if let SocketAddr::V4(a) = lan {
			blob.extend_from_slice(&a.ip().octets());
			blob.extend_from_slice(&a.port().to_be_bytes());
		}
		blob
	}

	/// Stand in for `connect()`'s preamble (salt minted before the rendezvous),
	/// then deliver the relay's `PeerFound` so the session exists.
	async fn rendezvous(node: &Arc<Node>, public: SocketAddr, lan: SocketAddr) {
		node.inner
			.lock()
			.await
			.pending_salt
			.insert(SESSION, random_salt());
		node.handle_relay(RelayMsg::PeerFound {
			target: DeviceId(1),
			target_addr: public,
			session: SESSION,
			answer: answer_with_lan(lan),
			rate_cap_kbps: 0,
		})
		.await;
	}

	/// The initiator's punch state machine: an early responder punch (it beats
	/// `PeerFound` by one relay RTT) must not create state; a `PunchAck` is only
	/// honored from this session's punch candidates and ADOPTS the validated
	/// source as the data path, preferring the private (LAN) one over the
	/// public router hairpin.
	#[tokio::test]
	async fn punch_state_machine_adopts_validated_private_path() {
		let node = test_node().await;
		let public = addr("203.0.113.7:21118"); // relay-observed peer addr
		let lan = addr("192.168.77.5:21118"); // peer's LAN candidate
		let foreign = addr("192.168.77.99:21118"); // another Pulsar on our LAN

		// Responder punch arriving BEFORE PeerFound: acked, but no session yet.
		node.handle_peer(
			PeerMsg::Punch {
				session: SESSION,
				seq: 0,
			},
			lan,
		)
		.await;
		assert!(!node.inner.lock().await.sessions.contains_key(&SESSION));

		// PeerFound: the session starts on the relay-observed public addr.
		rendezvous(&node, public, lan).await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).expect("session after PeerFound");
			assert_eq!(s.peer_addr, public);
			assert_eq!(s.peer_lan, Some(lan));
			assert!(!s.direct_ok);
		}

		// An ack from a FOREIGN device (a stale candidate resolved to another
		// Pulsar on the well-known port) must not lock Direct.
		node.handle_peer(
			PeerMsg::PunchAck {
				session: SESSION,
				seq: 0,
			},
			foreign,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert!(!s.direct_ok, "foreign ack must not prove the direct path");
			assert_eq!(s.peer_addr, public);
		}

		// Ack from the public (hairpin) path: verified — adopted as the data path.
		node.handle_peer(
			PeerMsg::PunchAck {
				session: SESSION,
				seq: 0,
			},
			public,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert!(s.direct_ok);
			assert_eq!(s.transport, Transport::Direct);
			assert_eq!(s.peer_addr, public, "the ack source becomes the data path");
		}

		// Ack from the LAN candidate: the private path wins over the hairpin.
		node.handle_peer(
			PeerMsg::PunchAck {
				session: SESSION,
				seq: 1,
			},
			lan,
		)
		.await;
		assert_eq!(node.inner.lock().await.sessions[&SESSION].peer_addr, lan);

		// A late punch retransmit via the public path must NOT flap the locked
		// private path back to the hairpin.
		node.handle_peer(
			PeerMsg::Punch {
				session: SESSION,
				seq: 3,
			},
			public,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert_eq!(s.peer_addr, lan);
			assert!(s.direct_ok);
		}
	}

	/// A Punch from a foreign address (not a known session candidate) must NOT
	/// mutate peer_addr, direct_ok, or transport — closing the session-hijack /
	/// relay-suppression vector (security regression test for conn-1).
	#[tokio::test]
	async fn punch_from_foreign_addr_does_not_adopt_data_path() {
		let node = test_node().await;
		let public = addr("203.0.113.7:21118"); // relay-observed peer addr
		let lan = addr("192.168.77.5:21118"); // peer's LAN candidate
		let foreign = addr("10.0.0.77:21118"); // attacker / unrelated device

		// Session is in relay-fallback state (direct_ok==false, transport==Relay).
		rendezvous(&node, public, lan).await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).expect("session after PeerFound");
			assert_eq!(s.peer_addr, public);
			assert!(!s.direct_ok);
			assert_eq!(s.transport, Transport::Relay);
		}

		// Punch arriving from a foreign address: must be ACKed (the send fires)
		// but MUST NOT redirect the data path.
		node.handle_peer(
			PeerMsg::Punch {
				session: SESSION,
				seq: 0,
			},
			foreign,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert_eq!(
				s.peer_addr, public,
				"foreign Punch must not overwrite peer_addr"
			);
			assert!(!s.direct_ok, "foreign Punch must not set direct_ok");
			assert_eq!(
				s.transport,
				Transport::Relay,
				"foreign Punch must not flip transport to Direct"
			);
		}

		// A legitimate Punch from the relay-observed public addr IS allowed.
		node.handle_peer(
			PeerMsg::Punch {
				session: SESSION,
				seq: 1,
			},
			public,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert!(s.direct_ok, "legitimate Punch (public) must set direct_ok");
			assert_eq!(s.transport, Transport::Direct);
			assert_eq!(s.peer_addr, public);
		}

		// And a Punch from the LAN candidate is also legitimate.
		node.handle_peer(
			PeerMsg::Punch {
				session: SESSION,
				seq: 2,
			},
			lan,
		)
		.await;
		{
			let g = node.inner.lock().await;
			let s = g.sessions.get(&SESSION).unwrap();
			assert_eq!(
				s.peer_addr, lan,
				"Punch from LAN candidate should prefer private path"
			);
		}
	}

	/// Only a candidate-validated ack wakes the `punched` waiter that gates
	/// `establish_transport` — a foreign ack must leave it parked (so Auto can
	/// still fall back to the relay).
	#[tokio::test]
	async fn punch_ack_notifies_only_from_session_candidates() {
		let node = test_node().await;
		let public = addr("203.0.113.7:21118");
		let lan = addr("192.168.77.5:21118");
		rendezvous(&node, public, lan).await;

		let pk = Arc::new(Notify::new());
		node.inner.lock().await.punched.insert(SESSION, pk.clone());

		node.handle_peer(
			PeerMsg::PunchAck {
				session: SESSION,
				seq: 0,
			},
			addr("10.0.0.9:21118"),
		)
		.await;
		assert!(
			timeout(Duration::from_millis(50), pk.notified())
				.await
				.is_err(),
			"foreign ack must not unblock the punch waiter"
		);

		node.handle_peer(
			PeerMsg::PunchAck {
				session: SESSION,
				seq: 0,
			},
			public,
		)
		.await;
		assert!(timeout(Duration::from_millis(50), pk.notified())
			.await
			.is_ok());
	}
}
