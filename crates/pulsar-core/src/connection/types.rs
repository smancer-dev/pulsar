//! Shared per-session and per-node state used across the connection submodules.

use super::*;

pub(super) struct SessionState {
	pub(super) peer_addr: SocketAddr,
	/// The peer's same-LAN candidate from its handshake blob tail (see
	/// `parse_lan_candidate`): punched ALONGSIDE the relay-observed public addr,
	/// so same-NAT peers converge on the true LAN path instead of the lossy
	/// router hairpin. `None` for old peers / cross-network connects.
	pub(super) peer_lan: Option<SocketAddr>,
	pub(super) crypto: Crypto,
	/// The peer's static X25519 public key that this session's key was derived
	/// against. Exposed via `Session::peer_pubkey` so the layer above can pin it
	/// to the requested id (TOFU) and detect a later key change for a known id —
	/// the relay-path identity assurance the relay itself can't provide.
	pub(super) peer_pubkey: PublicKey,
	pub(super) transport: Transport,
	pub(super) send_seq: u64,
	pub(super) direct_ok: bool,
	pub(super) data_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
	/// Our per-session handshake salt. Kept so a `Hello`/`HelloAck` retransmit
	/// re-sends the *same* salt (otherwise the peer would derive a mismatched key).
	pub(super) our_salt: [u8; 32],
}

#[derive(Default)]
pub(super) struct Inner {
	pub(super) self_id: Option<DeviceId>,
	pub(super) token: Option<Token>,
	/// A relay [`RelayMsg::Error`] that arrived while we were still unregistered
	/// (no `self_id` yet) — e.g. `IncompatibleVersion`. `register()` wakes on the
	/// `registered` notify and reads this to fail with a specific [`ConnError`]
	/// instead of a generic `RelayTimeout`. Cleared on a successful `Registered`.
	pub(super) register_error: Option<ErrCode>,
	pub(super) sessions: HashMap<SessionId, SessionState>,
	pub(super) peer_found: HashMap<SessionId, Arc<Notify>>,
	pub(super) punched: HashMap<SessionId, Arc<Notify>>,
	/// Direct-IP connects waiting for the peer's `HelloAck` (in-band key exchange).
	pub(super) hello_done: HashMap<SessionId, Arc<Notify>>,
	/// Our outgoing handshake salt for sessions where we send our `hello`/`Hello`
	/// *before* we receive the peer's reply (and thus derive the key later): the
	/// relay-path requester (`connect` → `PeerFound`) and the direct-IP initiator
	/// (`connect_direct` → `HelloAck`). Removed once the key is derived.
	pub(super) pending_salt: HashMap<SessionId, [u8; 32]>,
	/// The relay's per-session forwarding rate cap (kbit/s, 0 = uncapped) parsed from a
	/// `PeerFound`, held until `connect` builds the [`Session`] and copies it in. Only set on
	/// the relay-fallback rendezvous path; consumed when the session is returned.
	pub(super) pending_rate_cap: HashMap<SessionId, u32>,
	/// The peer's EXPECTED X25519 public key for direct-IP connects where the caller
	/// already knows it (`connect_direct(_, Some(pk))`). The `HelloAck` handler rejects
	/// (drops) an ack whose `pubkey` doesn't match this, so a MITM that terminates the
	/// in-band key exchange with its own key can't silently substitute itself. Absent
	/// for the typed-IP-with-no-known-key path (in-band/TOFU). Removed once consumed.
	pub(super) expected_pubkey: HashMap<SessionId, PublicKey>,
	/// Set by the `PeerFound` / `HelloAck` handlers when the answering key differs from
	/// the pin stored in `expected_pubkey` (TOFU mismatch). `connect_pinned` reads this
	/// after its rendezvous timeout so it can return `IdentityChanged` instead of the
	/// generic `TargetUnreachable` when the peer DID answer but with a rotated key.
	pub(super) identity_mismatch: HashMap<SessionId, Arc<std::sync::atomic::AtomicBool>>,
}
