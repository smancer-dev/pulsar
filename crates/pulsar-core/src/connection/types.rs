//! Shared per-session and per-node state used across the connection submodules.

use super::*;

pub(super) struct SessionState {
	pub(super) peer_addr: SocketAddr,
	pub(super) crypto: Crypto,
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
}
