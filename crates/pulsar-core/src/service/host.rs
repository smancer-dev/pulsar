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
	/// What this host can actually stream (validated codec + encoder caps, best-first)
	/// — the `QueryStreamCaps` reply. Default claims only H.264 / software.
	pub stream_caps: Box<dyn Fn() -> StreamCaps + Send>,
	/// The host's visible top-level windows the client may pick as a per-window capture
	/// target (Phase 2b co-op) — the `QueryWindows` reply. Cheap, called on demand.
	/// Default empty (a host with no per-window capture source advertises no windows).
	pub windows: Box<dyn Fn() -> Vec<WindowInfo> + Send>,
	/// Client reported missing RTP video seqs (media-over-session): re-send them from
	/// the retransmit ring. Default no-op (host without a media forwarder).
	pub on_nack: Box<dyn FnMut(Vec<u16>) + Send>,
	/// The client pushed its identity image (PNG bytes, see [`DataMsg::Avatar`]) so
	/// the host UI can show *who* connected. Default no-op.
	pub on_avatar: Box<dyn FnMut(Vec<u8>) + Send>,
	/// The client pushed its display name (see [`DataMsg::PeerName`]). Default no-op.
	pub on_peer_name: Box<dyn FnMut(String) + Send>,
	/// The client pushed its own relay device ID (see [`DataMsg::PeerId`]) so the host
	/// can show it instead of a bare `ip:port` on a direct connect. Default no-op.
	pub on_peer_id: Box<dyn FnMut(String) + Send>,
	/// File-manager requests from the client (`FsList` / `FsGet`). The handler
	/// replies through `outbound` (`FsEntries`, or the file as
	/// `FileBegin`/`FileChunk`/`FileEnd`). Default no-op — a host without the
	/// file manager just never answers.
	pub on_fs: Box<dyn FnMut(DataMsg) + Send>,
	/// Host-initiated re-stream injection. When the host detects its OWN display mode changed
	/// mid-session (resolution / refresh rate), a watcher re-sends the last [`StreamReq`] here so
	/// the capture restarts at the new geometry — the ffmpeg (x11grab/gdigrab/avfoundation) and
	/// Wayland paths have no DXGI `ACCESS_LOST` signal like the Windows native path, so without
	/// this they keep capturing the old size and the stream freezes until reconnect. `None`
	/// (default, and on Windows — the native path self-heals in `pulsar-capture`) → inert.
	pub restream: Option<mpsc::Receiver<StreamReq>>,
}

impl Default for DataHandlers {
	fn default() -> Self {
		Self {
			outbound: None,
			restream: None,
			on_clipboard: Box::new(|_| {}),
			on_chat: Box::new(|_| {}),
			on_file: Box::new(|_| {}),
			on_audio: Box::new(|_| {}),
			on_reverse: Box::new(|_| {}),
			stream_caps: Box::new(|| StreamCaps {
				codecs: vec!["h264".to_string()],
				encoders: vec!["software".to_string()],
				features: Vec::new(),
				displays: Vec::new(),
			}),
			windows: Box::new(Vec::new),
			on_nack: Box::new(|_| {}),
			on_avatar: Box::new(|_| {}),
			on_peer_name: Box::new(|_| {}),
			on_peer_id: Box::new(|_| {}),
			on_fs: Box::new(|_| {}),
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
		/// A host-side display-mode watcher asked to restart capture (re-issue this `StreamReq`).
		Restream(Option<StreamReq>),
	}
	let mut last_inbound = tokio::time::Instant::now();
	loop {
		// A dead client over UDP never closes the channel, so bound the wait: no
		// message (not even a keepalive) within `PEER_TIMEOUT` means it's gone.
		// The budget is anchored to the last INBOUND frame — the outbound branch
		// also completes iterations, and a long outbound stream (e.g. an FsGet
		// download) must not keep a silently-dead client "alive" while the host
		// streams into the void.
		let budget = PEER_TIMEOUT.saturating_sub(last_inbound.elapsed());
		if budget.is_zero() {
			break; // peer silent too long while we were busy sending
		}
		// Wait on the next inbound frame, an outbound side-channel message, OR a host-initiated
		// re-stream from the display-mode watcher — whichever lands first (only the chosen branch
		// then touches `session`). Both side channels are optional, so four select shapes.
		let ev = match (data.outbound.as_mut(), data.restream.as_mut()) {
			(Some(rx), Some(rs)) => tokio::select! {
				r = timeout(budget, session.recv()) => Ev::In(r),
				o = rx.recv() => Ev::Out(o),
				q = rs.recv() => Ev::Restream(q),
			},
			(Some(rx), None) => tokio::select! {
				r = timeout(budget, session.recv()) => Ev::In(r),
				o = rx.recv() => Ev::Out(o),
			},
			(None, Some(rs)) => tokio::select! {
				r = timeout(budget, session.recv()) => Ev::In(r),
				q = rs.recv() => Ev::Restream(q),
			},
			(None, None) => Ev::In(timeout(budget, session.recv()).await),
		};
		let bytes = match ev {
			Ev::In(Ok(Some(b))) => {
				last_inbound = tokio::time::Instant::now();
				b
			}
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
			// Host display mode changed → restart capture at the new geometry. Re-run on_stream
			// with the peer addr exactly as a client re-request would (reuses the full restart
			// path: kill old ffmpeg/native, re-clamp dims to the new display size, respawn).
			Ev::Restream(Some(req)) => {
				tracing::info!(display_idx = req.display_idx, "host re-streaming after display-mode change");
				if let Some(addr) = session.peer_addr().await {
					on_stream(req, addr);
				}
				continue;
			}
			// Watcher channel dropped (session ending / no watcher): stop selecting on it.
			Ev::Restream(None) => {
				data.restream = None;
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
			Some(Msg::QueryStreamCaps) => {
				let _ = session
					.send(&enc(&Msg::StreamCaps((data.stream_caps)())))
					.await;
			}
			Some(Msg::QueryWindows) => {
				let _ = session.send(&enc(&Msg::Windows((data.windows)()))).await;
			}
			Some(Msg::Launch(id)) => {
				on_launch(id);
				let _ = session.send(&enc(&Msg::Ok)).await;
			}
			Some(Msg::StartStream(req)) => {
				tracing::info!(display_idx = req.display_idx, "host received StartStream");
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
				m @ (DataMsg::FileBegin { .. }
				| DataMsg::FileChunk { .. }
				| DataMsg::FileEnd { .. }) => (data.on_file)(m),
				m @ (DataMsg::Audio(_) | DataMsg::AudioEnd) => (data.on_audio)(m),
				DataMsg::Stats(_) => {} // host→client only; ignore if echoed back
				DataMsg::DisplayRotation(_) => {} // host→client only; ignore if echoed back
				DataMsg::ReverseRequest(id) => (data.on_reverse)(id),
				DataMsg::MediaNack(seqs) => (data.on_nack)(seqs),
				DataMsg::Avatar(png) => (data.on_avatar)(png),
				DataMsg::PeerName(name) => (data.on_peer_name)(name),
				DataMsg::PeerId(id) => (data.on_peer_id)(id),
				m @ (DataMsg::FsList { .. } | DataMsg::FsGet { .. }) => (data.on_fs)(m),
				DataMsg::FsEntries { .. } => {} // host→client only; ignore if echoed back
				// Cursor side-channel + rumble are host→client only; ignore if echoed back.
				DataMsg::CursorPos { .. } | DataMsg::CursorShape { .. } | DataMsg::CursorHidden => {}
				DataMsg::Rumble { .. } => {}
			},
			_ => {}
		}
	}
}
