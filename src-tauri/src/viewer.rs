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
	/// UDP port the host should stream RTP to.
	pub media_port: u16,
	/// Loopback WebSocket port the webview connects to.
	pub ws_port: u16,
	tasks: Vec<JoinHandle<()>>,
}

impl Viewer {
	pub fn stop(self) {
		for t in self.tasks {
			t.abort();
		}
	}
}

/// Start the relay: bind an ephemeral UDP port (for the host's RTP) and a
/// loopback WebSocket, forwarding each datagram to all connected webview clients.
pub async fn start() -> std::io::Result<Viewer> {
	// `0.0.0.0` so the host can reach us over LAN too, not just loopback.
	let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
	let media_port = udp.local_addr()?.port();
	let ws_listener = TcpListener::bind("127.0.0.1:0").await?;
	let ws_port = ws_listener.local_addr()?.port();

	let (tx, _rx) = broadcast::channel::<Vec<u8>>(2048);

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
	let t_ws = tokio::spawn(async move {
		while let Ok((stream, _)) = ws_listener.accept().await {
			let mut rx = tx.subscribe();
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
						// Drop backlog rather than disconnect if the webview lags.
						Err(broadcast::error::RecvError::Lagged(_)) => continue,
						Err(broadcast::error::RecvError::Closed) => break,
					}
				}
			});
		}
	});

	Ok(Viewer { media_port, ws_port, tasks: vec![t_udp, t_ws] })
}

#[cfg(test)]
mod tests {
	use super::*;
	use futures_util::StreamExt;
	use std::time::Duration;

	#[tokio::test]
	async fn forwards_udp_datagrams_to_the_websocket() {
		let v = start().await.unwrap();
		let (mut ws, _) =
			tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", v.ws_port))
				.await
				.unwrap();

		// Let the WS connection subscribe to the broadcast before sending.
		tokio::time::sleep(Duration::from_millis(150)).await;
		let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
		sender.send_to(b"rtp-payload", ("127.0.0.1", v.media_port)).await.unwrap();

		let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
			.await
			.expect("a forwarded message in time")
			.expect("stream open")
			.expect("valid frame");
		assert_eq!(msg.into_data(), b"rtp-payload");
		v.stop();
	}
}
