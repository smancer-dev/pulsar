//! End-to-end flow tests: a real relay + two real `Node`s over loopback.
//!
//! These cover the requirements directly:
//!  * an ID only exists once the relay assigns one (relay down → no ID),
//!  * `P2pOnly` establishes a *direct* path and survives the relay going away,
//!  * `RelayOnly` carries traffic through the relay,
//!  * `Auto` works end to end,
//! all with payloads that must decrypt correctly on the other side.

use std::net::SocketAddr;
use std::time::Duration;

use pulsar_core::{NetworkMode, Node, Transport};
use pulsar_relay::Relay;
use tokio::task::JoinHandle;
use tokio::time::timeout;

async fn start_relay() -> (SocketAddr, JoinHandle<std::io::Result<()>>) {
	let relay = Relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
	let addr = relay.local_addr().unwrap();
	let handle = tokio::spawn(relay.run());
	(addr, handle)
}

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn registering_without_a_relay_yields_no_id() {
	// Point at a port with nothing listening.
	let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
	let node = Node::bind(LOCAL.parse().unwrap(), dead, NetworkMode::Auto)
		.await
		.unwrap();
	assert!(node.register().await.is_err(), "register must fail with no relay");
	assert!(node.self_id().await.is_none(), "no ID without a relay");
}

/// Drive a full connect + bidirectional encrypted exchange, returning the
/// transports each side observed.
async fn exchange(mode: NetworkMode) -> (Transport, Transport) {
	let (relay, _h) = start_relay().await;

	let host = Node::bind(LOCAL.parse().unwrap(), relay, mode).await.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay, mode).await.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	// Client connects to the host.
	let mut client_sess = client.connect(host_id).await.unwrap();
	// The host receives the matching inbound session.
	let mut host_sess = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.expect("host should receive an incoming session")
		.unwrap();

	// client -> host
	client_sess.send(b"merhaba host").await.unwrap();
	let got = timeout(Duration::from_secs(2), host_sess.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"merhaba host");

	// host -> client
	host_sess.send(b"merhaba client").await.unwrap();
	let got = timeout(Duration::from_secs(2), client_sess.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"merhaba client");

	(client_sess.transport(), host_sess.live_transport().await)
}

#[tokio::test]
async fn p2p_only_establishes_a_direct_path() {
	let (client_t, host_t) = exchange(NetworkMode::P2pOnly).await;
	assert_eq!(client_t, Transport::Direct);
	assert_eq!(host_t, Transport::Direct);
}

#[tokio::test]
async fn relay_only_tunnels_through_the_relay() {
	let (client_t, _host_t) = exchange(NetworkMode::RelayOnly).await;
	assert_eq!(client_t, Transport::Relay);
}

#[tokio::test]
async fn auto_mode_connects_end_to_end() {
	// On loopback hole-punching succeeds, so Auto lands on Direct.
	let (client_t, _host_t) = exchange(NetworkMode::Auto).await;
	assert_eq!(client_t, Transport::Direct);
}

#[tokio::test]
async fn direct_p2p_survives_the_relay_being_taken_down() {
	let (relay, relay_handle) = start_relay().await;
	let host = Node::bind(LOCAL.parse().unwrap(), relay, NetworkMode::P2pOnly)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay, NetworkMode::P2pOnly)
		.await
		.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	let client_sess = client.connect(host_id).await.unwrap();
	let mut host_sess = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(client_sess.transport(), Transport::Direct);

	// Take the relay down — a direct session must keep working.
	relay_handle.abort();
	tokio::time::sleep(Duration::from_millis(50)).await;

	client_sess.send(b"relay yokken bile calisir").await.unwrap();
	let got = timeout(Duration::from_secs(2), host_sess.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"relay yokken bile calisir");
}
