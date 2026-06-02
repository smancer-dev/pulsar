//! The bidirectional side channels (clipboard / chat / file / audio) ride the
//! same encrypted session: a client sends a chat line + a clipboard push to the
//! host, and the host pushes a chat reply back to the client.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulsar_core::service::{decode_data, send_data, serve_with, DataHandlers, DataMsg};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::sync::mpsc;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn side_channels_round_trip() {
	let relay = Relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
	let relay_addr: SocketAddr = relay.local_addr().unwrap();
	tokio::spawn(relay.run());

	let host = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto)
		.await
		.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	let chats = Arc::new(Mutex::new(Vec::<String>::new()));
	let clips = Arc::new(Mutex::new(Vec::<String>::new()));
	// Host → client outbound queue.
	let (out_tx, out_rx) = mpsc::channel::<DataMsg>(8);
	{
		let host = host.clone();
		let chats = chats.clone();
		let clips = clips.clone();
		tokio::spawn(async move {
			if let Some(session) = host.next_incoming().await {
				let handlers = DataHandlers {
					outbound: Some(out_rx),
					on_chat: Box::new(move |s| chats.lock().unwrap().push(s)),
					on_clipboard: Box::new(move |s| clips.lock().unwrap().push(s)),
					..Default::default()
				};
				serve_with(session, Vec::new, |_| {}, |_, _| {}, |_| {}, handlers).await;
			}
		});
	}

	let mut sess = client.connect(host_id).await.unwrap();
	send_data(&sess, &DataMsg::Chat("merhaba".into()))
		.await
		.unwrap();
	send_data(&sess, &DataMsg::Clipboard("gizli-sifre".into()))
		.await
		.unwrap();

	// Host received both.
	let got = timeout(Duration::from_secs(2), async {
		loop {
			if !chats.lock().unwrap().is_empty() && !clips.lock().unwrap().is_empty() {
				return;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await;
	assert!(got.is_ok(), "host must receive chat + clipboard");
	assert_eq!(chats.lock().unwrap().first().unwrap(), "merhaba");
	assert_eq!(clips.lock().unwrap().first().unwrap(), "gizli-sifre");

	// Host pushes a chat reply to the client.
	out_tx
		.send(DataMsg::Chat("hosttan-cevap".into()))
		.await
		.unwrap();
	let reply = timeout(Duration::from_secs(2), async {
		loop {
			if let Some(bytes) = sess.recv().await {
				if let Some(DataMsg::Chat(s)) = decode_data(&bytes) {
					return s;
				}
			} else {
				return String::new();
			}
		}
	})
	.await
	.expect("client must receive the host reply");
	assert_eq!(reply, "hosttan-cevap");
}
