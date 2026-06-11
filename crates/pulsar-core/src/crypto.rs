//! End-to-end encryption for Pulsar sessions.
//!
//! Each device has a long-lived X25519 identity. To talk to a peer, both sides
//! perform an X25519 Diffie–Hellman and derive a shared ChaCha20-Poly1305 key
//! (the two public keys are sorted so both ends derive the *same* key). The
//! relay only ever forwards ciphertext, so it can't read traffic — the
//! "zero-knowledge" property from the design.
//!
//! **Per-session key binding.** The static X25519 DH alone is identical for every
//! session between the same two devices, so deriving the data key from it *only*
//! would make every session (reconnect / 2nd stream / connect-twice) reuse the
//! same key. Combined with `seq` restarting at 0 each session, that is catastrophic
//! ChaCha20 keystream reuse. To prevent it, each side mints a fresh random 32-byte
//! **session salt** at handshake time and exchanges it alongside its public key;
//! both salts plus the random per-connect `session_id` are folded into the key
//! derivation, so two sessions between the same identities derive *different* keys.
//! (Forward secrecy via ephemeral X25519 is a separate, future enhancement; the
//! salt+session_id binding already fully fixes the cross-session reuse.)
//!
//! Nonces embed a direction byte so the two halves of a session never collide
//! even though both peers count their own sequence numbers from zero.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

/// A fresh, random per-session salt mixed into the data key so the same device
/// pair derives a different key every session. Generate one per handshake and
/// send it alongside the static public key.
pub fn random_salt() -> [u8; 32] {
	let mut salt = [0u8; 32];
	OsRng.fill_bytes(&mut salt);
	salt
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
	#[error("decryption failed (bad key, nonce, or tampered ciphertext)")]
	Decrypt,
	#[error("replayed or out-of-window sequence number")]
	Replay,
}

/// Which side of the handshake we are. Determines the nonce direction so the two
/// data streams (initiator→responder and responder→initiator) never share a
/// (key, nonce) pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
	Initiator,
	Responder,
}

/// A device's long-lived X25519 identity.
#[derive(Clone)]
pub struct Identity {
	secret: StaticSecret,
	public: PublicKey,
}

impl Identity {
	/// Generate a fresh random identity.
	pub fn generate() -> Self {
		let secret = StaticSecret::random_from_rng(OsRng);
		let public = PublicKey::from(&secret);
		Self { secret, public }
	}

	/// Reconstruct from persisted secret bytes.
	pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
		let secret = StaticSecret::from(bytes);
		let public = PublicKey::from(&secret);
		Self { secret, public }
	}

	/// Load a persisted identity from `path` (32 raw secret bytes), or generate +
	/// save a new one. This is what makes the device's relay-assigned ID **stable
	/// across restarts**: the same identity → the relay hands back the same id. The
	/// file lives in the per-user app dir, so different OS users (e.g. ASTER seats)
	/// keep separate, independent identities.
	pub fn load_or_create(path: impl AsRef<std::path::Path>) -> Self {
		let path = path.as_ref();
		if let Ok(bytes) = std::fs::read(path) {
			if let Ok(b) = <[u8; 32]>::try_from(bytes.as_slice()) {
				return Self::from_secret_bytes(b);
			}
		}
		let id = Self::generate();
		if let Some(parent) = path.parent() {
			let _ = std::fs::create_dir_all(parent);
		}
		// Best-effort persist; on Unix tighten perms to the owner (it's a secret key).
		let _ = std::fs::write(path, id.secret_bytes());
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
		}
		id
	}

	pub fn secret_bytes(&self) -> [u8; 32] {
		self.secret.to_bytes()
	}

	pub fn public_bytes(&self) -> [u8; 32] {
		self.public.to_bytes()
	}

	/// Derive a symmetric [`Session`] with `peer_public`, bound to this specific
	/// connection by the per-connect `session_id` and the two fresh handshake salts
	/// (ours + the peer's). Both ends pass the same `session_id` and the same pair of
	/// salts (in either order — they're sorted internally), so they derive an
	/// identical key while two *different* sessions between the same identities get
	/// *different* keys.
	pub fn session(
		&self,
		peer_public: [u8; 32],
		role: Role,
		session_id: u64,
		our_salt: [u8; 32],
		peer_salt: [u8; 32],
	) -> Session {
		let peer = PublicKey::from(peer_public);
		let shared = self.secret.diffie_hellman(&peer);
		let key = derive_key(
			self.public.to_bytes(),
			peer_public,
			shared.as_bytes(),
			session_id,
			our_salt,
			peer_salt,
		);
		Session {
			cipher: ChaCha20Poly1305::new(&key),
			role,
			recv_window: ReplayWindow::default(),
		}
	}
}

fn derive_key(
	a: [u8; 32],
	b: [u8; 32],
	shared: &[u8; 32],
	session_id: u64,
	salt_a: [u8; 32],
	salt_b: [u8; 32],
) -> Key {
	// Order the two public keys so both peers derive an identical key.
	let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
	// Sort the salts the same role-independent way, so both ends agree on the key
	// without having to know who is initiator vs responder.
	let (lo_salt, hi_salt) = if salt_a <= salt_b {
		(salt_a, salt_b)
	} else {
		(salt_b, salt_a)
	};
	let mut h = Sha256::new();
	h.update(b"pulsar-session-key-v2");
	h.update(lo);
	h.update(hi);
	h.update(shared);
	h.update(lo_salt);
	h.update(hi_salt);
	h.update(session_id.to_be_bytes());
	let digest = h.finalize();
	*Key::from_slice(digest.as_slice())
}

/// RFC 6479-style sliding-window anti-replay over the per-direction receive
/// sequence numbers. UDP datagrams legitimately arrive out of order, so a strict
/// "reject seq <= last" check would wrongly drop reordered packets; instead we
/// track the highest accepted seq and a bitmap of the most recent [`WINDOW`] seqs.
struct ReplayWindow {
	/// Highest seq accepted so far (the right edge of the window). `None` until the
	/// first datagram is accepted.
	high: Option<u64>,
	/// Bitmap of seen seqs in `[high - (WINDOW-1) ..= high]`; bit 0 == `high`.
	bitmap: u64,
}

const WINDOW: u64 = 64;

impl Default for ReplayWindow {
	fn default() -> Self {
		Self {
			high: None,
			bitmap: 0,
		}
	}
}

impl ReplayWindow {
	/// Check whether `seq` is acceptable, and if so record it. Returns `false` for a
	/// duplicate (already seen) or a seq older than the window (so the caller drops
	/// the datagram); returns `true` for a fresh seq (in-order, reordered-within-
	/// window, or ahead of the window), having recorded it.
	fn accept(&mut self, seq: u64) -> bool {
		match self.high {
			None => {
				// First datagram of this direction; it becomes the window's right edge.
				self.high = Some(seq);
				self.bitmap = 1; // bit 0 == high == seq
				true
			}
			Some(high) if seq > high => {
				// Ahead of the window: slide right by the gap, then mark the new edge.
				let shift = seq - high;
				self.bitmap = if shift >= WINDOW {
					0
				} else {
					self.bitmap << shift
				};
				self.bitmap |= 1; // bit 0 == new high == seq
				self.high = Some(seq);
				true
			}
			Some(high) => {
				// seq <= high: within or behind the window.
				let offset = high - seq;
				if offset >= WINDOW {
					return false; // too old — outside the window
				}
				let mask = 1u64 << offset;
				if self.bitmap & mask != 0 {
					return false; // duplicate — already seen
				}
				self.bitmap |= mask;
				true
			}
		}
	}
}

/// A symmetric, authenticated session between two peers.
pub struct Session {
	cipher: ChaCha20Poly1305,
	role: Role,
	/// Anti-replay state for the *receive* direction.
	recv_window: ReplayWindow,
}

impl Session {
	fn nonce(dir: u8, seq: u64) -> Nonce {
		let mut n = [0u8; 12];
		n[0] = dir;
		n[4..12].copy_from_slice(&seq.to_be_bytes());
		*Nonce::from_slice(&n)
	}

	fn send_dir(&self) -> u8 {
		match self.role {
			Role::Initiator => 0,
			Role::Responder => 1,
		}
	}

	fn recv_dir(&self) -> u8 {
		1 - self.send_dir()
	}

	/// Encrypt + authenticate a payload for the given (per-direction) sequence number.
	pub fn seal(&self, seq: u64, plaintext: &[u8]) -> Vec<u8> {
		self.cipher
			.encrypt(&Self::nonce(self.send_dir(), seq), plaintext)
			.expect("ChaCha20-Poly1305 encryption never fails for valid inputs")
	}

	/// Decrypt + verify a payload received from the peer, enforcing sliding-window
	/// anti-replay on `seq`.
	///
	/// The transport is UDP and datagrams legitimately arrive **out of order**, so a
	/// strict monotonic check is wrong. We accept any seq that is fresh within an
	/// RFC 6479-style sliding window (in-order, reordered-within-window, or ahead of
	/// the window) and reject a duplicate or a seq older than the window with
	/// [`CryptoError::Replay`]. The window is advanced **only after** the ciphertext
	/// authenticates, so a forged/garbage seq can never poison it.
	pub fn open(&mut self, seq: u64, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
		let plain = self
			.cipher
			.decrypt(&Self::nonce(self.recv_dir(), seq), ciphertext)
			.map_err(|_| CryptoError::Decrypt)?;
		if !self.recv_window.accept(seq) {
			return Err(CryptoError::Replay);
		}
		Ok(plain)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A connected pair sharing one `session_id` + both salts, the way the real
	/// handshake wires them up (each side mints its own salt, exchanges it, then both
	/// fold the pair in — order-independent).
	fn pair() -> (Session, Session) {
		let a = Identity::generate();
		let b = Identity::generate();
		let session_id = 0x1234_5678_9abc_def0;
		let salt_a = random_salt();
		let salt_b = random_salt();
		let a_sess = a.session(
			b.public_bytes(),
			Role::Initiator,
			session_id,
			salt_a,
			salt_b,
		);
		let b_sess = b.session(
			a.public_bytes(),
			Role::Responder,
			session_id,
			salt_b,
			salt_a,
		);
		(a_sess, b_sess)
	}

	#[test]
	fn both_directions_round_trip() {
		let (mut a, mut b) = pair();
		// initiator -> responder
		let ct = a.seal(0, b"merhaba dunya");
		assert_eq!(b.open(0, &ct).unwrap(), b"merhaba dunya");
		// responder -> initiator
		let ct2 = b.seal(0, b"selam");
		assert_eq!(a.open(0, &ct2).unwrap(), b"selam");
	}

	#[test]
	fn sequence_numbers_are_independent_per_direction() {
		let (a, mut b) = pair();
		for seq in 0..32u64 {
			let msg = format!("frame {seq}");
			let ct = a.seal(seq, msg.as_bytes());
			assert_eq!(b.open(seq, &ct).unwrap(), msg.as_bytes());
		}
	}

	#[test]
	fn tampered_ciphertext_is_rejected() {
		let (a, mut b) = pair();
		let mut ct = a.seal(1, b"secret");
		ct[0] ^= 0xFF;
		assert!(b.open(1, &ct).is_err());
	}

	#[test]
	fn wrong_peer_cannot_decrypt() {
		let a = Identity::generate();
		let b = Identity::generate();
		let eve = Identity::generate();
		let session_id = 99;
		let salt_a = random_salt();
		let salt_eve = random_salt();
		let a_to_b = a.session(
			b.public_bytes(),
			Role::Initiator,
			session_id,
			salt_a,
			salt_eve,
		);
		let mut eve_as_b = eve.session(
			a.public_bytes(),
			Role::Responder,
			session_id,
			salt_eve,
			salt_a,
		);
		let ct = a_to_b.seal(0, b"top secret");
		assert!(eve_as_b.open(0, &ct).is_err());
	}

	#[test]
	fn two_sessions_same_pair_derive_different_keys() {
		// The confirmed bug: two sessions between the SAME identity pair must NOT
		// share a key, or reconnect/2nd-stream reuse the same (key, nonce) stream.
		let a = Identity::generate();
		let b = Identity::generate();

		// Session A: its own session_id + salts.
		let sid_a = 0xAAAA_AAAA_AAAA_AAAA;
		let (sa1, sa2) = (random_salt(), random_salt());
		let a_in_sa = a.session(b.public_bytes(), Role::Initiator, sid_a, sa1, sa2);

		// Session B: a different session_id + different salts (as a fresh handshake
		// would produce), responder side.
		let sid_b = 0xBBBB_BBBB_BBBB_BBBB;
		let (sb1, sb2) = (random_salt(), random_salt());
		let mut b_in_sb = b.session(a.public_bytes(), Role::Responder, sid_b, sb2, sb1);

		// A ciphertext sealed under session A must NOT open under session B.
		let ct = a_in_sa.seal(0, b"frame from session A");
		assert!(
			b_in_sb.open(0, &ct).is_err(),
			"distinct sessions reused the same key — keystream reuse bug is back"
		);

		// Sanity: differing only in session_id (same salts) still differs.
		let (s1, s2) = (random_salt(), random_salt());
		let a_x = a.session(b.public_bytes(), Role::Initiator, 1, s1, s2);
		let mut b_y = b.session(a.public_bytes(), Role::Responder, 2, s2, s1);
		let ct2 = a_x.seal(0, b"same salts, different session_id");
		assert!(b_y.open(0, &ct2).is_err());
	}

	#[test]
	fn anti_replay_rejects_dup_but_tolerates_reordering() {
		let (a, mut b) = pair();
		// Pre-seal a run of frames so we can feed them in any order.
		let ct: Vec<Vec<u8>> = (0..10u64)
			.map(|seq| a.seal(seq, format!("frame {seq}").as_bytes()))
			.collect();

		// Deliver out of order (reordered, NOT duplicated): 0, 3, 1, 2, 5, 4.
		// Every one of these must be ACCEPTED — dropping reordered UDP would break
		// the data path.
		for &seq in &[0u64, 3, 1, 2, 5, 4] {
			assert_eq!(
				b.open(seq, &ct[seq as usize]).unwrap(),
				format!("frame {seq}").as_bytes(),
				"reordered (non-duplicate) seq {seq} was wrongly dropped"
			);
		}

		// Replaying an already-delivered seq must be REJECTED.
		assert!(
			matches!(b.open(2, &ct[2]), Err(CryptoError::Replay)),
			"a duplicate seq must be rejected as a replay"
		);
		assert!(matches!(b.open(0, &ct[0]), Err(CryptoError::Replay)));

		// A brand-new higher seq still goes through.
		assert_eq!(
			b.open(9, &ct[9]).unwrap(),
			b"frame 9",
			"a fresh ahead-of-window seq must be accepted"
		);
		// …and replaying it is then rejected.
		assert!(matches!(b.open(9, &ct[9]), Err(CryptoError::Replay)));
	}

	#[test]
	fn anti_replay_drops_seq_older_than_window() {
		let (a, mut b) = pair();
		// Accept a high seq to advance the window far forward.
		let hi = a.seal(200, b"far ahead");
		assert_eq!(b.open(200, &hi).unwrap(), b"far ahead");
		// A seq more than WINDOW behind the high-water mark is outside the window and
		// must be dropped (can't prove it isn't a replay).
		let old = a.seal(10, b"ancient");
		assert!(matches!(b.open(10, &old), Err(CryptoError::Replay)));
		// But a seq just inside the window (200 - 63 = 137) is still accepted.
		let edge = a.seal(137, b"window edge");
		assert_eq!(b.open(137, &edge).unwrap(), b"window edge");
	}

	#[test]
	fn identity_is_recoverable_from_secret_bytes() {
		let id = Identity::generate();
		let restored = Identity::from_secret_bytes(id.secret_bytes());
		assert_eq!(id.public_bytes(), restored.public_bytes());
	}

	#[test]
	fn load_or_create_persists_then_reloads_same_identity() {
		let dir = std::env::temp_dir().join(format!("pulsar-id-test-{}", std::process::id()));
		let path = dir.join("identity.key");
		let _ = std::fs::remove_file(&path);
		// First call creates + saves; second call reloads the SAME identity (stable id).
		let first = Identity::load_or_create(&path);
		let second = Identity::load_or_create(&path);
		assert_eq!(first.public_bytes(), second.public_bytes());
		// A bad/short file falls back to a fresh identity rather than panicking.
		std::fs::write(&path, b"too short").unwrap();
		let _ = Identity::load_or_create(&path); // must not panic
		let _ = std::fs::remove_dir_all(&dir);
	}
}
