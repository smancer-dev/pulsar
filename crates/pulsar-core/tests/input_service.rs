//! A client streams controller state to the host over the encrypted session;
//! the host receives the exact frames (which it would inject into a virtual pad).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulsar_core::input::{button, EmulationTarget, GamepadKind, GamepadState};
use pulsar_core::service::{send_input, serve, InputEvent};
use pulsar_core::{NetworkMode, Node};
use pulsar_relay::Relay;
use tokio::time::timeout;

const LOCAL: &str = "127.0.0.1:0";

/// Roundtrip serde tests for the new slot-aware `InputEvent` variants.
/// Guards against silent drift between Serialize + Deserialize impls.
#[test]
fn gamepad_slot_roundtrip() {
	let mut state = GamepadState::default();
	state.set(button::B, true);
	state.left_x = 16000;

	let ev = InputEvent::GamepadSlot {
		slot: 1,
		kind: GamepadKind::Ds4,
		target: EmulationTarget::Auto,
		state,
	};
	let json = serde_json::to_string(&ev).expect("GamepadSlot must serialize");
	let back: InputEvent = serde_json::from_str(&json).expect("GamepadSlot must deserialize");
	assert_eq!(back, ev, "GamepadSlot roundtrip mismatch");
}

#[test]
fn gamepad_disconnect_roundtrip() {
	let ev = InputEvent::GamepadDisconnect { slot: 2 };
	let json = serde_json::to_string(&ev).expect("GamepadDisconnect must serialize");
	let back: InputEvent =
		serde_json::from_str(&json).expect("GamepadDisconnect must deserialize");
	assert_eq!(back, ev, "GamepadDisconnect roundtrip mismatch");
}

/// An old peer (e.g. a host running before T1 that only knows `Gamepad`) will
/// receive `{"GamepadSlot":{...}}` and silently drop it (serde_json returns an
/// error; `dec()` yields `None`). Verify that the new variant serializes to the
/// struct-variant JSON shape and that the legacy `Gamepad` variant is unchanged.
#[test]
fn gamepad_slot_json_shape_and_legacy_compat() {
	let mut state = GamepadState::default();
	state.set(button::A, true);

	// New variant serializes as {"GamepadSlot":{...}}, not the legacy tuple form.
	let slot_ev = InputEvent::GamepadSlot { slot: 0, kind: GamepadKind::Xbox, target: EmulationTarget::Auto, state };
	let slot_json = serde_json::to_string(&slot_ev).unwrap();
	assert!(
		slot_json.contains("\"GamepadSlot\""),
		"GamepadSlot must serialize as a struct variant, got: {slot_json}"
	);

	// Legacy Gamepad variant is a tuple variant (the shape old hosts expect).
	let legacy_ev = InputEvent::Gamepad(state);
	let legacy_json = serde_json::to_string(&legacy_ev).unwrap();
	assert!(
		legacy_json.contains("\"Gamepad\""),
		"legacy Gamepad must still serialize as a tuple variant, got: {legacy_json}"
	);
	let back: InputEvent = serde_json::from_str(&legacy_json).unwrap();
	assert_eq!(back, legacy_ev, "legacy Gamepad roundtrip must still work");
}

#[tokio::test]
async fn controller_frames_reach_the_host() {
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

	// Two Vecs: one for legacy Gamepad frames, one for GamepadSlot frames.
	let received = Arc::new(Mutex::new(Vec::<GamepadState>::new()));
	let received_slot = Arc::new(Mutex::new(Vec::<(u8, GamepadState)>::new()));
	{
		let host = host.clone();
		let received = received.clone();
		let received_slot = received_slot.clone();
		tokio::spawn(async move {
			if let Some(session) = host.next_incoming().await {
				serve(
					session,
					Vec::new,
					|_id| {},
					|_req, _addr| {},
					move |ev| {
						match ev {
							InputEvent::Gamepad(state) => {
								received.lock().unwrap().push(state);
							}
							InputEvent::GamepadSlot { slot, state, .. } => {
								received_slot.lock().unwrap().push((slot, state));
							}
							_ => {}
						}
					},
				)
				.await;
			}
		});
	}

	let mut sess = client.connect(host_id).await.unwrap();

	// Send a legacy Gamepad frame (Player 1 / Xbox — backward compat).
	let mut frame = GamepadState::default();
	frame.set(button::A, true);
	frame.left_x = -12000;
	frame.right_trigger = 200;
	send_input(&mut sess, &InputEvent::Gamepad(frame)).await.unwrap();

	// Also send a GamepadSlot frame (Player 2 / DS4 — new multi-pad path).
	let mut slot_frame = GamepadState::default();
	slot_frame.set(button::B, true);
	slot_frame.right_x = 8000;
	send_input(
		&mut sess,
		&InputEvent::GamepadSlot { slot: 1, kind: GamepadKind::Ds4, target: EmulationTarget::Auto, state: slot_frame },
	)
	.await
	.unwrap();

	// Wait for the host to receive the legacy frame.
	let got = timeout(Duration::from_secs(2), async {
		loop {
			if let Some(f) = received.lock().unwrap().first().copied() {
				return f;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("host should receive the legacy controller frame");

	assert_eq!(got, frame);
	assert!(got.is_pressed(button::A));

	// Wait for the host to receive the GamepadSlot frame.
	let got_slot = timeout(Duration::from_secs(2), async {
		loop {
			if let Some(entry) = received_slot.lock().unwrap().first().copied() {
				return entry;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("host should receive the GamepadSlot frame");

	assert_eq!(got_slot.0, 1, "slot must be 1");
	assert_eq!(got_slot.1, slot_frame, "GamepadSlot state must match");
	assert!(got_slot.1.is_pressed(button::B));
}

/// Send two `GamepadSlot` frames (slot 0 + slot 1) over one session and verify
/// the host receive-closure sees both frames with their distinct slot numbers.
/// This is the core fan-out check for T5 (host pads[0..4] dispatch).
#[tokio::test]
async fn two_gamepad_slots_reach_host_with_distinct_slots() {
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

	// Collect (slot, state) tuples for every GamepadSlot event.
	let received: Arc<Mutex<Vec<(u8, GamepadState)>>> = Arc::new(Mutex::new(Vec::new()));
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
						if let InputEvent::GamepadSlot { slot, state, .. } = ev {
							received.lock().unwrap().push((slot, state));
						}
					},
				)
				.await;
			}
		});
	}

	let mut sess = client.connect(host_id).await.unwrap();

	// Slot 0 — Xbox pad, A pressed.
	let mut frame0 = GamepadState::default();
	frame0.set(button::A, true);
	frame0.left_x = 10000;
	send_input(
		&mut sess,
		&InputEvent::GamepadSlot { slot: 0, kind: GamepadKind::Xbox, target: EmulationTarget::Auto, state: frame0 },
	)
	.await
	.unwrap();

	// Slot 1 — DS5 pad, B pressed.
	let mut frame1 = GamepadState::default();
	frame1.set(button::B, true);
	frame1.right_y = -8000;
	send_input(
		&mut sess,
		&InputEvent::GamepadSlot { slot: 1, kind: GamepadKind::Ds5, target: EmulationTarget::Auto, state: frame1 },
	)
	.await
	.unwrap();

	// Wait until the host accumulates at least two slot frames.
	let got = timeout(Duration::from_secs(2), async {
		loop {
			let v = received.lock().unwrap().clone();
			if v.len() >= 2 {
				return v;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("host should receive both GamepadSlot frames within 2 s");

	// Both slots must be present (order may differ due to async delivery).
	let slot0 = got.iter().find(|(s, _)| *s == 0).expect("slot 0 missing");
	let slot1 = got.iter().find(|(s, _)| *s == 1).expect("slot 1 missing");
	assert_eq!(slot0.1, frame0, "slot-0 state mismatch");
	assert!(slot0.1.is_pressed(button::A), "slot-0 A must be pressed");
	assert_eq!(slot1.1, frame1, "slot-1 state mismatch");
	assert!(slot1.1.is_pressed(button::B), "slot-1 B must be pressed");
	// The slots must be distinct (0 ≠ 1).
	assert_ne!(slot0.0, slot1.0, "slots must differ");
}
