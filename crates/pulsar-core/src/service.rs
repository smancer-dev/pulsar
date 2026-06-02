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
use crate::input::GamepadState;

/// If a connected peer sends nothing (not even a keepalive) for this long, treat
/// it as gone and tear the session down. Clients send a keepalive every ~2s.
const PEER_TIMEOUT: Duration = Duration::from_secs(6);

/// A game/app the host exposes to clients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameInfo {
	pub id: String,
	pub title: String,
	pub kind: String,
}

/// One control event a client sends to drive the host (mouse / keyboard / pad).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
	/// Controller state (game streaming).
	Gamepad(GamepadState),
	/// Absolute pointer position, normalized 0..1 within the streamed screen.
	PointerMotion { x: f64, y: f64 },
	/// Mouse button (0=left, 1=right, 2=middle) pressed/released.
	PointerButton { button: u8, down: bool },
	/// Smooth scroll delta.
	Scroll { dx: f64, dy: f64 },
	/// Keyboard evdev keycode pressed/released.
	Key { code: u32, down: bool },
}

/// A client's request to start receiving a video stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamReq {
	/// UDP port the client is listening on for the media stream.
	pub port: u16,
	/// Codec the client prefers (`h264` / `h265` / `av1`).
	pub codec: String,
	/// Encoder hint for the host (`auto` / `nvenc` / `vaapi` / …).
	pub encoder: String,
}

/// A bidirectional data-channel message exchanged over a live session for the
/// session "side channels": clipboard sync, text chat, file transfer, and mic
/// audio. Either peer can send any of these. NOTE: the session transport is UDP
/// (unordered, lossy) — fine on LAN/loopback; file transfer tags chunks so the
/// receiver can detect a gap and report an incomplete transfer rather than
/// silently corrupting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DataMsg {
	/// Clipboard text pushed to the peer (peer sets its system clipboard).
	Clipboard(String),
	/// A chat line.
	Chat(String),
	/// Start of a file: name + total byte length + number of chunks to expect.
	FileBegin { name: String, size: u64, chunks: u32 },
	/// One file chunk with its 0-based index (for gap detection).
	FileChunk { index: u32, data: Vec<u8> },
	/// All chunks for the current file have been sent.
	FileEnd,
	/// One frame of raw PCM mic audio (s16le, 48kHz mono).
	Audio(Vec<u8>),
	/// The mic stream stopped.
	AudioEnd,
}

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
	Ok,
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

/// The host's verdict on an access request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
	/// Host approved — proceed.
	Accepted,
	/// Host rejected the session.
	Denied,
	/// A password is required; prompt the user and call `authenticate` again.
	NeedPassword,
}

/// Client: send the access request (with the one-time password, which may be
/// empty) and wait for the host's verdict — the host may pop an Allow/Deny prompt,
/// so this can block until the host user decides. Call right after connecting,
/// before any other request.
pub async fn authenticate(session: &mut Session, password: &str) -> Result<AuthOutcome, ConnError> {
	session.send(&enc(&Msg::Auth(password.to_string()))).await?;
	loop {
		match session.recv().await {
			Some(bytes) => match dec(&bytes) {
				Some(Msg::Ok) => return Ok(AuthOutcome::Accepted),
				Some(Msg::Denied) => return Ok(AuthOutcome::Denied),
				Some(Msg::NeedPassword) => return Ok(AuthOutcome::NeedPassword),
				_ => continue,
			},
			None => return Ok(AuthOutcome::Denied),
		}
	}
}

/// Host: read the client's opening access request and return the password it sent
/// (empty string if none). `None` if the peer sent something else or hung up.
pub async fn recv_auth(session: &mut Session) -> Option<String> {
	match session.recv().await.as_deref().and_then(dec) {
		Some(Msg::Auth(pw)) => Some(pw),
		_ => None,
	}
}

/// Client → host message during the auth handshake (host side).
pub enum ClientAuth {
	/// The client (re)submitted a password.
	Password(String),
	/// Keepalive or other non-auth message — ignore, keep waiting.
	Keepalive,
	/// The client went away.
	Gone,
}

/// Host: read the next message while racing the Allow/Deny popup against an
/// incoming password.
pub async fn recv_client_auth(session: &mut Session) -> ClientAuth {
	match session.recv().await.as_deref().and_then(dec) {
		Some(Msg::Auth(pw)) => ClientAuth::Password(pw),
		None => ClientAuth::Gone,
		_ => ClientAuth::Keepalive,
	}
}

/// Host → client reply during the auth handshake (client side).
pub enum HostAuth {
	Ok,
	Denied,
	NeedPassword,
	Gone,
	Other,
}

/// Client: read the host's next auth reply (used to race a host approval against
/// the user typing a password).
pub async fn recv_host_auth(session: &mut Session) -> HostAuth {
	match session.recv().await.as_deref().and_then(dec) {
		Some(Msg::Ok) => HostAuth::Ok,
		Some(Msg::Denied) => HostAuth::Denied,
		Some(Msg::NeedPassword) => HostAuth::NeedPassword,
		None => HostAuth::Gone,
		_ => HostAuth::Other,
	}
}

/// Client: send (or resubmit) the access password over an already-open session.
pub async fn send_auth(session: &mut Session, password: &str) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Auth(password.to_string()))).await
}

/// Host: tell the client a password is required (it will prompt and retry).
pub async fn need_password(session: &mut Session) -> Result<(), ConnError> {
	session.send(&enc(&Msg::NeedPassword)).await
}

/// Host: accept the connection (after approving the request).
pub async fn accept(session: &mut Session) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Ok)).await
}

/// Host: reject the connection.
pub async fn reject(session: &mut Session) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Denied)).await
}

/// Client: ask the peer for its game list.
pub async fn request_games(session: &mut Session) -> Result<Vec<GameInfo>, ConnError> {
	session.send(&enc(&Msg::ListGames)).await?;
	for _ in 0..MAX_WAIT_MSGS {
		match session.recv().await {
			Some(bytes) => {
				if let Some(Msg::Games(games)) = dec(&bytes) {
					return Ok(games);
				}
			}
			None => break,
		}
	}
	Ok(Vec::new())
}

/// Client: ask the peer to launch a game by id.
pub async fn request_launch(session: &mut Session, id: &str) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Launch(id.to_string()))).await
}

/// Client: ask the host to start streaming video to us.
pub async fn request_stream(session: &mut Session, req: &StreamReq) -> Result<(), ConnError> {
	session.send(&enc(&Msg::StartStream(req.clone()))).await
}

/// Client: send one control event (mouse / keyboard / controller) to the host.
pub async fn send_input(session: &mut Session, event: &InputEvent) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Input(*event))).await
}

/// Client: liveness keepalive so the host's `serve` doesn't block forever after a
/// silent disconnect. Send it every ~2s while a session is held open.
pub async fn send_keepalive(session: &mut Session) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Ping)).await
}

/// Either peer: send one side-channel data message (clipboard/chat/file/audio).
pub async fn send_data(session: &Session, msg: &DataMsg) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Data(msg.clone()))).await
}

/// Decode a received frame as a side-channel [`DataMsg`], if it is one. Lets a
/// caller that owns the session's read side (e.g. the client's hold loop) pull
/// out clipboard/chat/etc. without depending on the private `Msg` enum.
pub fn decode_data(bytes: &[u8]) -> Option<DataMsg> {
	match dec(bytes) {
		Some(Msg::Data(d)) => Some(d),
		_ => None,
	}
}

/// Host-side handlers for the bidirectional side channels, plus an optional
/// outbound queue the host drains to push messages *to* the client (chat replies,
/// clipboard). Defaults are no-ops so [`serve`] stays a thin wrapper.
pub struct DataHandlers {
	pub outbound: Option<mpsc::Receiver<DataMsg>>,
	pub on_clipboard: Box<dyn FnMut(String) + Send>,
	pub on_chat: Box<dyn FnMut(String) + Send>,
	pub on_file: Box<dyn FnMut(DataMsg) + Send>,
	pub on_audio: Box<dyn FnMut(DataMsg) + Send>,
}

impl Default for DataHandlers {
	fn default() -> Self {
		Self {
			outbound: None,
			on_clipboard: Box::new(|_| {}),
			on_chat: Box::new(|_| {}),
			on_file: Box::new(|_| {}),
			on_audio: Box::new(|_| {}),
		}
	}
}

/// Host: serve requests on an incoming session until the peer goes away.
/// `games` yields the current list; `on_launch` fires with a game id; `on_stream`
/// fires with the client's stream request + the peer's address (where to send
/// video); `on_input` fires with each controller frame (to inject into a virtual
/// pad).
pub async fn serve(
	session: Session,
	games: impl Fn() -> Vec<GameInfo>,
	on_launch: impl FnMut(String),
	on_stream: impl FnMut(StreamReq, SocketAddr),
	on_input: impl FnMut(InputEvent),
) {
	serve_with(session, games, on_launch, on_stream, on_input, DataHandlers::default()).await;
}

/// Like [`serve`], but also drives the bidirectional side channels: inbound
/// clipboard/chat/file/audio go to `data`'s handlers, and anything queued on
/// `data.outbound` is sent to the client. Full-duplex without splitting the
/// session: a `select!` waits on either the next inbound frame or the next
/// outbound message, and only the chosen branch then touches `session`.
pub async fn serve_with(
	mut session: Session,
	games: impl Fn() -> Vec<GameInfo>,
	mut on_launch: impl FnMut(String),
	mut on_stream: impl FnMut(StreamReq, SocketAddr),
	mut on_input: impl FnMut(InputEvent),
	mut data: DataHandlers,
) {
	enum Ev {
		In(Result<Option<Vec<u8>>, tokio::time::error::Elapsed>),
		Out(Option<DataMsg>),
	}
	loop {
		// A dead client over UDP never closes the channel, so bound the wait: no
		// message (not even a keepalive) within `PEER_TIMEOUT` means it's gone.
		let ev = match data.outbound.as_mut() {
			Some(rx) => tokio::select! {
				r = timeout(PEER_TIMEOUT, session.recv()) => Ev::In(r),
				o = rx.recv() => Ev::Out(o),
			},
			None => Ev::In(timeout(PEER_TIMEOUT, session.recv()).await),
		};
		let bytes = match ev {
			Ev::In(Ok(Some(b))) => b,
			Ev::In(Ok(None)) | Ev::In(Err(_)) => break, // closed or peer silent too long
			Ev::Out(Some(msg)) => {
				let _ = session.send(&enc(&Msg::Data(msg))).await;
				continue;
			}
			// Outbound queue dropped: stop selecting on it (avoid a busy loop).
			Ev::Out(None) => {
				data.outbound = None;
				continue;
			}
		};
		match dec(&bytes) {
			Some(Msg::Ping) => {} // keepalive — just refreshes the timeout
			Some(Msg::ListGames) => {
				let _ = session.send(&enc(&Msg::Games(games()))).await;
			}
			Some(Msg::Launch(id)) => {
				on_launch(id);
				let _ = session.send(&enc(&Msg::Ok)).await;
			}
			Some(Msg::StartStream(req)) => {
				if let Some(addr) = session.peer_addr().await {
					on_stream(req, addr);
				}
				let _ = session.send(&enc(&Msg::Ok)).await;
			}
			// Input events are high-rate and fire-and-forget (no reply).
			Some(Msg::Input(ev)) => on_input(ev),
			Some(Msg::Data(d)) => match d {
				DataMsg::Clipboard(s) => (data.on_clipboard)(s),
				DataMsg::Chat(s) => (data.on_chat)(s),
				m @ (DataMsg::FileBegin { .. } | DataMsg::FileChunk { .. } | DataMsg::FileEnd) => {
					(data.on_file)(m)
				}
				m @ (DataMsg::Audio(_) | DataMsg::AudioEnd) => (data.on_audio)(m),
			},
			_ => {}
		}
	}
}
