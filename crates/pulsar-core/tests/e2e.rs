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
	assert!(
		node.register().await.is_err(),
		"register must fail with no relay"
	);
	assert!(node.self_id().await.is_none(), "no ID without a relay");
}

/// Drive a full connect + bidirectional encrypted exchange, returning the
/// transports each side observed.
async fn exchange(mode: NetworkMode) -> (Transport, Transport) {
	let (relay, _h) = start_relay().await;

	let host = Node::bind(LOCAL.parse().unwrap(), relay, mode)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay, mode)
		.await
		.unwrap();
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

	client_sess
		.send(b"relay yokken bile calisir")
		.await
		.unwrap();
	let got = timeout(Duration::from_secs(2), host_sess.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"relay yokken bile calisir");
}

#[tokio::test]
async fn direct_ip_connect_without_a_relay() {
	// A dead relay address proves the relay is never used on the direct-IP path.
	let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
	let host = Node::bind(LOCAL.parse().unwrap(), dead, NetworkMode::P2pOnly)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), dead, NetworkMode::P2pOnly)
		.await
		.unwrap();
	let host_addr = host.local_addr().unwrap();

	// In-band handshake (the host's key is learned via Hello/HelloAck).
	let mut c = client.connect_direct(host_addr, None).await.unwrap();
	let mut h = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.unwrap()
		.unwrap();
	c.send(b"merhaba host").await.unwrap();
	let got = timeout(Duration::from_secs(2), h.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"merhaba host");
	h.send(b"merhaba client").await.unwrap();
	let got = timeout(Duration::from_secs(2), c.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"merhaba client");
	assert_eq!(c.transport(), Transport::Direct);

	// Beacon shortcut: the host's public key is already known up front.
	let mut c2 = client
		.connect_direct(host_addr, Some(host.public_key()))
		.await
		.unwrap();
	let mut h2 = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.unwrap()
		.unwrap();
	c2.send(b"ping").await.unwrap();
	let got = timeout(Duration::from_secs(2), h2.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(got, b"ping");
}

/// Media-over-session: tagged RTP frames ride the SAME encrypted session as the
/// JSON control messages — through the RELAY (the single-socket promise must hold
/// on the worst-case transport), sent from a concurrent `SessionSender` while the
/// owner keeps using `send`/`recv`. The receiver must demux media vs control.
#[tokio::test]
async fn media_frames_ride_the_session_alongside_control() {
	use pulsar_core::service::media;

	let (relay, _h) = start_relay().await;
	let host = Node::bind(LOCAL.parse().unwrap(), relay, NetworkMode::RelayOnly)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), relay, NetworkMode::RelayOnly)
		.await
		.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	let host_id = host.self_id().await.unwrap();

	let mut client_sess = client.connect(host_id).await.unwrap();
	let host_sess = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.unwrap()
		.unwrap();

	// The host's media forwarder uses a cloned send-only handle, concurrently
	// with the serve loop owning the session itself.
	let media_tx = host_sess.sender();
	let mut rtp = vec![0x80u8, 96, 0x00, 0x2A, 0, 0, 0, 1]; // seq 42
	rtp.extend_from_slice(&[0xAB; 1200]); // MTU-sized video payload
	media_tx
		.send(&media::frame(media::TAG_VIDEO, &rtp))
		.await
		.unwrap();
	host_sess.send(b"{\"control\":true}").await.unwrap();
	media_tx
		.send(&media::frame(
			media::TAG_AUDIO,
			&[0x80, 97, 0, 1, 0, 0, 0, 2],
		))
		.await
		.unwrap();

	// Client demuxes: one video frame (seq intact), one control payload, one audio.
	let (mut vids, mut auds, mut ctrls) = (0, 0, 0);
	for _ in 0..3 {
		let bytes = timeout(Duration::from_secs(2), client_sess.recv())
			.await
			.unwrap()
			.unwrap();
		match media::parse(&bytes) {
			Some((media::TAG_VIDEO, body)) => {
				assert_eq!(media::rtp_seq(body), Some(42));
				assert_eq!(body.len(), rtp.len(), "video datagram intact");
				vids += 1;
			}
			Some((media::TAG_AUDIO, _)) => auds += 1,
			Some(_) => unreachable!("parse only yields known tags"),
			None => {
				assert_eq!(bytes, b"{\"control\":true}");
				ctrls += 1;
			}
		}
	}
	assert_eq!((vids, auds, ctrls), (1, 1, 1));
}
