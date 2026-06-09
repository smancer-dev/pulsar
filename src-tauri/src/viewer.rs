//! Local **UDP(RTP) → WebSocket** relay for the embedded video viewer.
//!
//! The host streams RTP/H.264 to a UDP port on this machine. The webview can't
//! read UDP, so we re-broadcast every datagram over a loopback WebSocket; the
//! SvelteKit `SessionView` connects to it, depacketizes the RTP in JS, and feeds
//! the frames to a `VideoDecoder` (WebCodecs) rendered on a `<canvas>` — no
//! separate `ffplay` window, low latency.

use std::sync::Arc;

use futures_util::SinkExt;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// A running relay. Holds the task handles so it can be torn down on stop.
pub struct Viewer {
	/// UDP port the host should stream **video** RTP to.
	pub media_port: u16,
	/// Loopback WebSocket port the webview connects to for **video**.
	pub ws_port: u16,
	/// UDP port the host should stream **audio** (Opus RTP) to.
	pub audio_port: u16,
	/// Loopback WebSocket port the webview connects to for **audio**.
	pub audio_ws_port: u16,
	/// Broadcast of received **audio** RTP datagrams. Webview clients read the WebSocket;
	/// the Linux native client subscribes here to feed a native player (WebKitGTK can't
	/// decode Opus via WebCodecs, so its webview audio path is silent).
	#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
	audio_tx: broadcast::Sender<Vec<u8>>,
	tasks: Vec<JoinHandle<()>>,
}

impl Viewer {
	pub fn stop(self) {
		for t in self.tasks {
			t.abort();
		}
	}

	/// Forward every received audio RTP datagram to a loopback UDP port (where a native
	/// ffmpeg/mpv decodes the Opus and plays it). The task ends when the viewer is dropped
	/// (the broadcast closes). Used on Linux, where the webview can't play the Opus stream.
	#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
	pub fn forward_audio_to_loopback(&mut self, dest_port: u16) {
		let mut rx = self.audio_tx.subscribe();
		self.tasks.push(tokio::spawn(async move {
			let sock = match UdpSocket::bind("127.0.0.1:0").await {
				Ok(s) => s,
				Err(_) => return,
			};
			let dest: std::net::SocketAddr = ([127, 0, 0, 1], dest_port).into();
			loop {
				match rx.recv().await {
					Ok(pkt) => {
						let _ = sock.send_to(&pkt, dest).await;
					}
					Err(broadcast::error::RecvError::Lagged(_)) => continue,
					Err(broadcast::error::RecvError::Closed) => break,
				}
			}
		}));
	}
}

/// Start the relays: one UDP→WS pair for video and one for audio. The webview
/// connects to each WebSocket, depacketizes the RTP in JS, and decodes (WebCodecs
/// for video, WebAudio for the Opus audio).
pub async fn start() -> std::io::Result<Viewer> {
	let mut tasks = Vec::new();
	let (media_port, ws_port, _video_tx) = relay(&mut tasks).await?;
	let (audio_port, audio_ws_port, audio_tx) = relay(&mut tasks).await?;
	Ok(Viewer {
		media_port,
		ws_port,
		audio_port,
		audio_ws_port,
		audio_tx,
		tasks,
	})
}

/// Bind one UDP→WebSocket relay (an ephemeral UDP port for the host's RTP + a
/// loopback WebSocket the webview reads), spawn its two pump tasks onto `tasks`,
/// and return `(udp_port, ws_port)`. Used once for video and once for audio.
async fn relay(tasks: &mut Vec<JoinHandle<()>>) -> std::io::Result<(u16, u16, broadcast::Sender<Vec<u8>>)> {
	// `0.0.0.0` so the host can reach us over LAN too, not just loopback. Bind via
	// socket2 to raise SO_RCVBUF: a 40 Mbps stream bursts hundreds of ~1400 B
	// datagrams per keyframe in a few ms, overflowing the default kernel recv buffer
	// and silently dropping packets — and dropped RTP corrupts H.264 until the next
	// keyframe. 8 MiB absorbs keyframe bursts + scheduling jitter.
	let sock = socket2::Socket::new(
		socket2::Domain::IPV4,
		socket2::Type::DGRAM,
		Some(socket2::Protocol::UDP),
	)?;
	sock.set_recv_buffer_size(8 * 1024 * 1024)?;
	sock.set_nonblocking(true)?; // required by UdpSocket::from_std
	sock.bind(&"0.0.0.0:0".parse::<std::net::SocketAddr>().unwrap().into())?;
	let udp = Arc::new(UdpSocket::from_std(sock.into())?);
	let media_port = udp.local_addr()?.port();
	let ws_listener = TcpListener::bind("127.0.0.1:0").await?;
	let ws_port = ws_listener.local_addr()?.port();

	let (tx, _rx) = broadcast::channel::<Vec<u8>>(8192);

	// UDP datagrams → broadcast.
	let udp_rx = udp.clone();
	let tx_udp = tx.clone();
	let t_udp = tokio::spawn(async move {
		let mut buf = vec![0u8; 65_536];
		loop {
			match udp_rx.recv(&mut buf).await {
				Ok(n) => {
					let _ = tx_udp.send(buf[..n].to_vec());
				}
				Err(_) => break,
			}
		}
	});

	// Accept webview WebSocket connections; each forwards the broadcast.
	let tx_ws = tx.clone();
	let t_ws = tokio::spawn(async move {
		while let Ok((stream, _)) = ws_listener.accept().await {
			let mut rx = tx_ws.subscribe();
			tokio::spawn(async move {
				let ws = match tokio_tungstenite::accept_async(stream).await {
					Ok(ws) => ws,
					Err(_) => return,
				};
				let (mut sink, _read) = futures_util::StreamExt::split(ws);
				loop {
					match rx.recv().await {
						Ok(pkt) => {
							if sink.send(Message::Binary(pkt)).await.is_err() {
								break;
							}
						}
						// Drop backlog rather than disconnect if the webview lags — but log
						// it: lagged drops lose RTP packets (the client resyncs on the next
						// keyframe), so this is the signal the webview is the bottleneck.
						Err(broadcast::error::RecvError::Lagged(n)) => {
							tracing::warn!("viewer ws lagged — dropped {n} datagrams");
							continue;
						}
						Err(broadcast::error::RecvError::Closed) => break,
					}
				}
			});
		}
	});

	tasks.push(t_udp);
	tasks.push(t_ws);
	Ok((media_port, ws_port, tx))
}

#[cfg(test)]
mod tests {
	use super::*;
	use futures_util::StreamExt;
	use std::time::Duration;

	#[tokio::test]
	async fn forwards_udp_datagrams_to_the_websocket() {
		let v = start().await.unwrap();
		let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", v.ws_port))
			.await
			.unwrap();

		// Let the WS connection subscribe to the broadcast before sending.
		tokio::time::sleep(Duration::from_millis(150)).await;
		let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
		sender
			.send_to(b"rtp-payload", ("127.0.0.1", v.media_port))
			.await
			.unwrap();

		let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
			.await
			.expect("a forwarded message in time")
			.expect("stream open")
			.expect("valid frame");
		assert_eq!(msg.into_data(), b"rtp-payload");
		v.stop();
	}
}
