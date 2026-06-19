//! A client lists and launches the host's games over the real encrypted session.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulsar_core::service::{request_games, request_launch, serve, GameInfo};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn client_lists_and_launches_host_games() {
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

	// Host serves its games on the first incoming session.
	let launched = Arc::new(Mutex::new(Vec::<String>::new()));
	{
		let host = host.clone();
		let launched = launched.clone();
		tokio::spawn(async move {
			if let Some(session) = host.next_incoming().await {
				let games = || {
					vec![
						GameInfo {
							id: "g1".into(),
							title: "Elden Ring".into(),
							kind: "program".into(),
							image: String::new(),
						},
						GameInfo {
							id: "g2".into(),
							title: "Hades II".into(),
							kind: "program".into(),
							image: String::new(),
						},
					]
				};
				serve(
					session,
					games,
					move |id| launched.lock().unwrap().push(id),
					|_req, _addr| {},
					|_state| {},
				)
				.await;
			}
		});
	}

	// Client connects and lists the host's games.
	let mut sess = client.connect(host_id).await.unwrap();
	let games = timeout(Duration::from_secs(2), request_games(&mut sess))
		.await
		.expect("games in time")
		.unwrap();
	assert_eq!(games.len(), 2);
	assert_eq!(games[0].title, "Elden Ring");

	// Client asks the host to launch one.
	request_launch(&mut sess, "g2").await.unwrap();
	tokio::time::sleep(Duration::from_millis(150)).await;
	assert_eq!(launched.lock().unwrap().as_slice(), &["g2".to_string()]);
}
