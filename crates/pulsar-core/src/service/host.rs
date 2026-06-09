//! Host-side serve loop: drives requests on an incoming session (auth already
//! done) — game list, launch, stream start, input injection — and the optional
//! bidirectional side channels (clipboard / chat / file / audio / reverse).

use super::*;

/// Host-side handlers for the bidirectional side channels, plus an optional
/// outbound queue the host drains to push messages *to* the client (chat replies,
/// clipboard). Defaults are no-ops so [`serve`] stays a thin wrapper.
pub struct DataHandlers {
	pub outbound: Option<mpsc::Receiver<DataMsg>>,
	pub on_clipboard: Box<dyn FnMut(String) + Send>,
	pub on_chat: Box<dyn FnMut(String) + Send>,
	pub on_file: Box<dyn FnMut(DataMsg) + Send>,
	pub on_audio: Box<dyn FnMut(DataMsg) + Send>,
	/// Client asked to reverse direction; arg is the requester's connect id.
	pub on_reverse: Box<dyn FnMut(String) + Send>,
}

impl Default for DataHandlers {
	fn default() -> Self {
		Self {
			outbound: None,
			on_clipboard: Box::new(|_| {}),
			on_chat: Box::new(|_| {}),
			on_file: Box::new(|_| {}),
			on_audio: Box::new(|_| {}),
			on_reverse: Box::new(|_| {}),
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
	serve_with(
		session,
		games,
		on_launch,
		on_stream,
		on_input,
		DataHandlers::default(),
	)
	.await;
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
			Some(Msg::Ping) => {
				// Keepalive — refreshes the timeout; reply so the client can time RTT.
				let _ = session.send(&enc(&Msg::Pong)).await;
			}
			Some(Msg::Bye) => break, // client said goodbye — tear down immediately
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
				DataMsg::Stats(_) => {} // host→client only; ignore if echoed back
				DataMsg::DisplayRotation(_) => {} // host→client only; ignore if echoed back
				DataMsg::ReverseRequest(id) => (data.on_reverse)(id),
			},
			_ => {}
		}
	}
}
