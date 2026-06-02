//! The host gates each incoming connection: empty password → ask the client to
//! prompt (NeedPassword); correct → accept; wrong → reject.

use std::net::SocketAddr;
use std::time::Duration;

use pulsar_core::service::{accept, authenticate, need_password, recv_auth, reject, AuthOutcome};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn host_prompts_then_accepts_correct_password() {
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

	// Host: empty → NeedPassword, "sezam" → accept, anything else → reject.
	{
		let host = host.clone();
		tokio::spawn(async move {
			while let Some(mut session) = host.next_incoming().await {
				let provided = recv_auth(&mut session).await.unwrap_or_default();
				if provided.is_empty() {
					let _ = need_password(&mut session).await;
				} else if provided == "sezam" {
					let _ = accept(&mut session).await;
				} else {
					let _ = reject(&mut session).await;
				}
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		});
	}

	let mut s = client.connect(host_id).await.unwrap();
	let v = timeout(Duration::from_secs(2), authenticate(&mut s, ""))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(v, AuthOutcome::NeedPassword, "empty password must prompt");

	let mut s = client.connect(host_id).await.unwrap();
	let v = timeout(Duration::from_secs(2), authenticate(&mut s, "yanlis"))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(v, AuthOutcome::Denied, "wrong password must be rejected");

	let mut s = client.connect(host_id).await.unwrap();
	let v = timeout(Duration::from_secs(2), authenticate(&mut s, "sezam"))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		v,
		AuthOutcome::Accepted,
		"correct password must be accepted"
	);
}
