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
use std::sync::Arc;
use std::time::Duration;

use pulsar_proto::{
	decode, encode, ClientMsg, DeviceId, PeerMsg, PublicKey, RelayMsg, SessionId, Token,
	PROTOCOL_VERSION,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::timeout;

use crate::config::NetworkMode;
use crate::crypto::{random_salt, Identity, Role, Session as Crypto};

mod handlers;
mod node;
mod session;
mod types;

pub use node::Node;
pub use session::Session;

use types::{Inner, SessionState};

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

/// Parse a relay-path handshake blob `pubkey(32) || salt(32)` into its two halves.
///
/// Returns `None` (so the caller fails the connection gracefully) if the blob is
/// the wrong length — never index-slices an untrusted, attacker-influenced buffer.
fn split_handshake(blob: &[u8]) -> Option<([u8; 32], [u8; 32])> {
	if blob.len() != 64 {
		return None;
	}
	let mut pubkey = [0u8; 32];
	let mut salt = [0u8; 32];
	pubkey.copy_from_slice(&blob[..32]);
	salt.copy_from_slice(&blob[32..64]);
	Some((pubkey, salt))
}
