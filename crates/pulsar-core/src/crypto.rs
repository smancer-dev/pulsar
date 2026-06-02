//! End-to-end encryption for Pulsar sessions.
//!
//! Each device has a long-lived X25519 identity. To talk to a peer, both sides
//! perform an X25519 Diffie–Hellman and derive a shared ChaCha20-Poly1305 key
//! (the two public keys are sorted so both ends derive the *same* key). The
//! relay only ever forwards ciphertext, so it can't read traffic — the
//! "zero-knowledge" property from the design.
//!
//! Nonces embed a direction byte so the two halves of a session never collide
//! even though both peers count their own sequence numbers from zero.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
	#[error("decryption failed (bad key, nonce, or tampered ciphertext)")]
	Decrypt,
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

	pub fn secret_bytes(&self) -> [u8; 32] {
		self.secret.to_bytes()
	}

	pub fn public_bytes(&self) -> [u8; 32] {
		self.public.to_bytes()
	}

	/// Derive a symmetric [`Session`] with `peer_public`, given our handshake role.
	pub fn session(&self, peer_public: [u8; 32], role: Role) -> Session {
		let peer = PublicKey::from(peer_public);
		let shared = self.secret.diffie_hellman(&peer);
		let key = derive_key(self.public.to_bytes(), peer_public, shared.as_bytes());
		Session {
			cipher: ChaCha20Poly1305::new(&key),
			role,
		}
	}
}

fn derive_key(a: [u8; 32], b: [u8; 32], shared: &[u8; 32]) -> Key {
	// Order the two public keys so both peers derive an identical key.
	let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
	let mut h = Sha256::new();
	h.update(b"pulsar-session-key-v1");
	h.update(lo);
	h.update(hi);
	h.update(shared);
	let digest = h.finalize();
	*Key::from_slice(digest.as_slice())
}

/// A symmetric, authenticated session between two peers.
pub struct Session {
	cipher: ChaCha20Poly1305,
	role: Role,
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

	/// Decrypt + verify a payload received from the peer.
	pub fn open(&self, seq: u64, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
		self.cipher
			.decrypt(&Self::nonce(self.recv_dir(), seq), ciphertext)
			.map_err(|_| CryptoError::Decrypt)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pair() -> (Session, Session) {
		let a = Identity::generate();
		let b = Identity::generate();
		let a_sess = a.session(b.public_bytes(), Role::Initiator);
		let b_sess = b.session(a.public_bytes(), Role::Responder);
		(a_sess, b_sess)
	}

	#[test]
	fn both_directions_round_trip() {
		let (a, b) = pair();
		// initiator -> responder
		let ct = a.seal(0, b"merhaba dunya");
		assert_eq!(b.open(0, &ct).unwrap(), b"merhaba dunya");
		// responder -> initiator
		let ct2 = b.seal(0, b"selam");
		assert_eq!(a.open(0, &ct2).unwrap(), b"selam");
	}

	#[test]
	fn sequence_numbers_are_independent_per_direction() {
		let (a, b) = pair();
		for seq in 0..32u64 {
			let msg = format!("frame {seq}");
			let ct = a.seal(seq, msg.as_bytes());
			assert_eq!(b.open(seq, &ct).unwrap(), msg.as_bytes());
		}
	}

	#[test]
	fn tampered_ciphertext_is_rejected() {
		let (a, b) = pair();
		let mut ct = a.seal(1, b"secret");
		ct[0] ^= 0xFF;
		assert!(b.open(1, &ct).is_err());
	}

	#[test]
	fn wrong_peer_cannot_decrypt() {
		let a = Identity::generate();
		let b = Identity::generate();
		let eve = Identity::generate();
		let a_to_b = a.session(b.public_bytes(), Role::Initiator);
		let eve_as_b = eve.session(a.public_bytes(), Role::Responder);
		let ct = a_to_b.seal(0, b"top secret");
		assert!(eve_as_b.open(0, &ct).is_err());
	}

	#[test]
	fn identity_is_recoverable_from_secret_bytes() {
		let id = Identity::generate();
		let restored = Identity::from_secret_bytes(id.secret_bytes());
		assert_eq!(id.public_bytes(), restored.public_bytes());
	}
}
