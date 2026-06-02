//! The host's Allow/Deny popup races the client's password: the host can approve
//! passwordlessly while the client is still being asked for a password, OR a
//! correct password auto-accepts. This exercises the protocol pieces the app's
//! `race_host_auth` + `client_authenticate` are built from.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pulsar_core::service::{
	accept, need_password, recv_auth, recv_client_auth, recv_host_auth, reject, send_auth,
	ClientAuth, HostAuth,
};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::sync::oneshot;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

/// Spawn a host that mimics `race_host_auth`: empty/wrong password → ask the client
/// to prompt AND race an Allow/Deny decision against a correct password.
async fn spawn_host(host: Arc<Node>, allow: oneshot::Receiver<bool>, accepted: Arc<AtomicBool>) {
	tokio::spawn(async move {
		let Some(mut session) = host.next_incoming().await else {
			return;
		};
		let provided = recv_auth(&mut session).await.unwrap_or_default();
		let ok = if provided == "sezam" {
			true
		} else {
			let _ = need_password(&mut session).await;
			let mut allow = allow;
			loop {
				tokio::select! {
					biased;
					d = &mut allow => break matches!(d, Ok(true)),
					m = recv_client_auth(&mut session) => match m {
						ClientAuth::Password(pw) => {
							if pw == "sezam" { break true; }
							let _ = need_password(&mut session).await;
						}
						ClientAuth::Keepalive => {}
						ClientAuth::Gone => break false,
					}
				}
			}
		};
		if ok {
			let _ = accept(&mut session).await;
		} else {
			let _ = reject(&mut session).await;
		}
		accepted.store(ok, Ordering::SeqCst);
		tokio::time::sleep(Duration::from_millis(50)).await;
	});
}

async fn fresh() -> (SocketAddr, Arc<Node>, Arc<Node>) {
	let relay = Relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
	let addr = relay.local_addr().unwrap();
	tokio::spawn(relay.run());
	let host = Node::bind(LOCAL.parse().unwrap(), addr, NetworkMode::Auto)
		.await
		.unwrap();
	let client = Node::bind(LOCAL.parse().unwrap(), addr, NetworkMode::Auto)
		.await
		.unwrap();
	host.register().await.unwrap();
	client.register().await.unwrap();
	(addr, host, client)
}

#[tokio::test]
async fn host_approves_without_password() {
	let (_a, host, client) = fresh().await;
	let host_id = host.self_id().await.unwrap();
	let (allow_tx, allow_rx) = oneshot::channel();
	let accepted = Arc::new(AtomicBool::new(false));
	spawn_host(host.clone(), allow_rx, accepted.clone()).await;

	let mut sess = client.connect(host_id).await.unwrap();
	send_auth(&mut sess, "").await.unwrap(); // empty → triggers prompt + popup
	assert!(matches!(
		recv_host_auth(&mut sess).await,
		HostAuth::NeedPassword
	));

	// Host clicks "Allow" — no password ever entered.
	allow_tx.send(true).unwrap();
	let v = timeout(Duration::from_secs(2), recv_host_auth(&mut sess))
		.await
		.unwrap();
	assert!(
		matches!(v, HostAuth::Ok),
		"host Allow must accept passwordlessly"
	);
}

#[tokio::test]
async fn correct_password_auto_accepts() {
	let (_a, host, client) = fresh().await;
	let host_id = host.self_id().await.unwrap();
	let (_allow_tx, allow_rx) = oneshot::channel();
	let accepted = Arc::new(AtomicBool::new(false));
	spawn_host(host.clone(), allow_rx, accepted.clone()).await;

	let mut sess = client.connect(host_id).await.unwrap();
	send_auth(&mut sess, "").await.unwrap();
	assert!(matches!(
		recv_host_auth(&mut sess).await,
		HostAuth::NeedPassword
	));

	// Client types the correct password (no host click).
	send_auth(&mut sess, "sezam").await.unwrap();
	let v = timeout(Duration::from_secs(2), recv_host_auth(&mut sess))
		.await
		.unwrap();
	assert!(matches!(v, HostAuth::Ok), "correct password must accept");
}
