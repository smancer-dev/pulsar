//! Stream synthetic video frames over a real encrypted Pulsar session and verify
//! they arrive and decode losslessly — the capture→encode→transport→decode→present
//! path running over the actual P2P channel.

use std::net::SocketAddr;
use std::time::Duration;

use pulsar_core::media::{
	CollectingSink, EncodedPacket, FrameSink, FrameSource, RleDecoder, RleEncoder,
	SolidColorSource, VideoDecoder, VideoEncoder,
};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

#[tokio::test]
async fn frames_stream_over_an_encrypted_session() {
	// Relay + two nodes.
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

	let client_sess = client.connect(host_id).await.unwrap();
	let mut host_sess = timeout(Duration::from_secs(2), host.next_incoming())
		.await
		.unwrap()
		.unwrap();

	const N: u32 = 6;

	// Host receives + decodes in the background.
	let recv_task = tokio::spawn(async move {
		let mut dec = RleDecoder;
		let mut sink = CollectingSink::default();
		for _ in 0..N {
			let bytes = timeout(Duration::from_secs(2), host_sess.recv())
				.await
				.expect("frame in time")
				.expect("session open");
			let packet: EncodedPacket = serde_json::from_slice(&bytes).unwrap();
			let frame = dec.decode(&packet).expect("decodes");
			sink.present(&frame);
		}
		sink.frames
	});

	// Client captures synthetic frames, encodes, and streams them.
	let mut src = SolidColorSource::new(16, 16, N, 60);
	let mut enc = RleEncoder;
	let mut sent = Vec::new();
	while let Some(frame) = src.next_frame() {
		let packet = enc.encode(&frame);
		client_sess
			.send(&serde_json::to_vec(&packet).unwrap())
			.await
			.unwrap();
		sent.push(frame);
		tokio::time::sleep(Duration::from_millis(5)).await;
	}

	let received = recv_task.await.unwrap();
	assert_eq!(received.len(), N as usize, "all frames should arrive");
	assert_eq!(
		received, sent,
		"frames decode losslessly after the round trip"
	);
}
