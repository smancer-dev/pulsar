//! A client streams controller state to the host over the encrypted session;
//! the host receives the exact frames (which it would inject into a virtual pad).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulsar_core::input::{button, GamepadState};
use pulsar_core::service::{send_input, serve, InputEvent};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn controller_frames_reach_the_host() {
	let relay = Relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
	let relay_addr: SocketAddr = relay.local_addr().unwrap();
	tokio::spawn(relay.run());

	let host = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto).await.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay_addr, NetworkMode::Auto).await.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	let received = Arc::new(Mutex::new(Vec::<GamepadState>::new()));
	{
		let host = host.clone();
		let received = received.clone();
		tokio::spawn(async move {
			if let Some(session) = host.next_incoming().await {
				serve(
					session,
					Vec::new,
					|_id| {},
					|_req, _addr| {},
					move |ev| {
						if let InputEvent::Gamepad(state) = ev {
							received.lock().unwrap().push(state);
						}
					},
				)
				.await;
			}
		});
	}

	let mut sess = client.connect(host_id).await.unwrap();
	let mut frame = GamepadState::default();
	frame.set(button::A, true);
	frame.left_x = -12000;
	frame.right_trigger = 200;
	send_input(&mut sess, &InputEvent::Gamepad(frame)).await.unwrap();

	// wait for the host to process the frame
	let got = timeout(Duration::from_secs(2), async {
		loop {
			if let Some(f) = received.lock().unwrap().first().copied() {
				return f;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("host should receive the controller frame");

	assert_eq!(got, frame);
	assert!(got.is_pressed(button::A));
}
