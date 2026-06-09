//! The one-time-password auth handshake that opens a session: the client sends
//! its access request and the host returns a verdict (accept / deny / prompt).

use super::*;

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
