//! A silently-dead client (app closed/killed — no graceful Bye, UDP has no FIN)
//! must be detected by the host: `serve` returns within the peer timeout so the
//! host can tear down screen capture + input injection.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pulsar_core::service::{send_input, serve, InputEvent};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn host_detects_silent_client_death() {
	let relay = Relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
	let relay_addr: SocketAddr = relay.local_addr().unwrap();
	tokio::spawn(relay.run());

	let host = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto).await.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto).await.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	// Host serves the incoming session and flips `done` when serve() returns.
	let done = Arc::new(AtomicBool::new(false));
	{
		let host = host.clone();
		let done = done.clone();
		tokio::spawn(async move {
			if let Some(session) = host.next_incoming().await {
				serve(session, Vec::new, |_| {}, |_, _| {}, |_| {}).await;
				done.store(true, Ordering::SeqCst);
			}
		});
	}

	let mut sess = client.connect(host_id).await.unwrap();
	send_input(&mut sess, &InputEvent::PointerMotion { x: 0.5, y: 0.5 }).await.unwrap();

	// Simulate the client dying: drop the session and the whole client node, with
	// no Bye and no further keepalives.
	drop(sess);
	drop(client);

	// The host's serve() must return within the peer timeout (6s) + slack.
	let detected = timeout(Duration::from_secs(9), async {
		loop {
			if done.load(Ordering::SeqCst) {
				return;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	})
	.await;
	assert!(detected.is_ok(), "host must detect the dead client and serve() must return");
}
