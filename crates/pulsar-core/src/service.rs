//! Application service protocol that runs **over an established [`Session`]**:
//! a client lists and launches the host's games across the encrypted channel.
//!
//! This is the wire side of "Bağlan → oyun modu → girilen ID'nin host'undan
//! oyunları getir". The host runs [`serve`] on each incoming session; the client
//! calls [`request_games`] / [`request_launch`] on its session.

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::connection::{ConnError, Session};

mod auth;
mod client;
mod host;
pub mod media;
mod wire;

pub use auth::{
	accept, authenticate, need_password, recv_auth, recv_client_auth, recv_host_auth, reject,
	send_auth, AuthOutcome, ClientAuth, HostAuth,
};
pub use client::{
	decode_data, is_pong, query_stream_caps, request_games, request_launch, request_stream,
	send_bye, send_data, send_input, send_keepalive,
};
pub use host::{serve, serve_with, DataHandlers};
pub use wire::{
	DataMsg, DisplayInfo, FsEntry, GameInfo, InputEvent, QualityPref, StreamCaps, StreamReq,
};

/// If a connected peer sends nothing (not even a keepalive) for this long, treat
/// it as gone and tear the session down. Clients send a keepalive every ~2s.
const PEER_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Serialize, Deserialize)]
enum Msg {
	/// Client → host: the one-time password shown on the host (first message).
	Auth(String),
	/// Host → client: a password is required but none/empty was given — the client
	/// should prompt the user and retry.
	NeedPassword,
	/// Host → client: rejected; the host will not serve this session.
	Denied,
	ListGames,
	Games(Vec<GameInfo>),
	Launch(String),
	StartStream(StreamReq),
	/// Client → host control event (sent at the input rate).
	Input(InputEvent),
	/// Bidirectional side-channel data (clipboard / chat / file / audio).
	Data(DataMsg),
	/// Client → host liveness keepalive (so the host can detect a dead client over
	/// UDP, which has no connection teardown).
	Ping,
	/// Host → client reply to `Ping`, so the client can measure round-trip time.
	Pong,
	/// Client → host: graceful disconnect — the host should tear down at once
	/// (kill ffmpeg, release held input) instead of waiting out the keepalive timeout.
	Bye,
	Ok,
	/// Client → host: which codecs can this host actually stream (validated encode
	/// caps, best-first)? Lets the client resolve its "auto" codec to what the host
	/// will really send — the client writes its decoder SDP BEFORE the stream starts,
	/// so the two must never disagree.
	QueryStreamCaps,
	/// Host → client reply to `QueryStreamCaps`: validated codec + encoder ids,
	/// preference-ordered. An old host never replies (unknown message) — the client
	/// times out and falls back to H.264.
	StreamCaps(wire::StreamCaps),
}

/// A short, human-typable one-time password like `7yf2-qk` (no ambiguous chars).
pub fn gen_password() -> String {
	use rand::Rng;
	const CS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
	let mut rng = rand::thread_rng();
	let mut s = String::with_capacity(7);
	for i in 0..6 {
		if i == 4 {
			s.push('-');
		}
		s.push(CS[rng.gen_range(0..CS.len())] as char);
	}
	s
}

fn enc(m: &Msg) -> Vec<u8> {
	serde_json::to_vec(m).expect("service messages serialize")
}

fn dec(b: &[u8]) -> Option<Msg> {
	serde_json::from_slice(b).ok()
}

/// How many inbound messages to read while waiting for a specific reply.
const MAX_WAIT_MSGS: usize = 16;
