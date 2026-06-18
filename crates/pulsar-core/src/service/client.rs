//! Client-side request helpers sent over an authenticated session (list/launch
//! games, start a stream, forward input, keepalive/bye, side-channel data) plus
//! the small decoders a caller that owns the read side uses.

use super::*;

/// Client: ask the peer for its game list.
pub async fn request_games(session: &mut Session) -> Result<Vec<GameInfo>, ConnError> {
	session.send(&enc(&Msg::ListGames)).await?;
	for _ in 0..MAX_WAIT_MSGS {
		match tokio::time::timeout(
			std::time::Duration::from_secs(3),
			session.recv(),
		)
		.await
		{
			Ok(Some(bytes)) => {
				if let Some(Msg::Games(games)) = dec(&bytes) {
					return Ok(games);
				}
			}
			// None = session closed, Err(_) = per-message 3 s deadline exceeded
			Ok(None) | Err(_) => break,
		}
	}
	Ok(Vec::new())
}

/// Client: ask the host what it can actually stream (validated codec + encoder caps,
/// best-first). Empty on timeout/old host — the caller falls back to H.264. Used to
/// resolve the client's "auto" codec BEFORE writing its decoder SDP (so the SDP and
/// the stream can never disagree) and to gate the session-menu options.
pub async fn query_stream_caps(session: &mut Session) -> Result<StreamCaps, ConnError> {
	session.send(&enc(&Msg::QueryStreamCaps)).await?;
	for _ in 0..MAX_WAIT_MSGS {
		match tokio::time::timeout(
			std::time::Duration::from_secs(3),
			session.recv(),
		)
		.await
		{
			Ok(Some(bytes)) => {
				if let Some(Msg::StreamCaps(caps)) = dec(&bytes) {
					return Ok(caps);
				}
			}
			// None = session closed, Err(_) = per-message 3 s deadline exceeded
			Ok(None) | Err(_) => break,
		}
	}
	Ok(StreamCaps::default())
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

/// Client: announce a graceful disconnect so the host's `serve_with` loop exits at
/// once (killing its ffmpeg + releasing held input) rather than waiting for the
/// keepalive timeout.
pub async fn send_bye(session: &mut Session) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Bye)).await
}

/// Either peer: send one side-channel data message (clipboard/chat/file/audio).
pub async fn send_data(session: &Session, msg: &DataMsg) -> Result<(), ConnError> {
	session.send(&enc(&Msg::Data(msg.clone()))).await
}

/// True if `bytes` is the host's `Pong` reply (lets the client time round-trips
/// without depending on the private `Msg` enum).
pub fn is_pong(bytes: &[u8]) -> bool {
	matches!(dec(bytes), Some(Msg::Pong))
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
