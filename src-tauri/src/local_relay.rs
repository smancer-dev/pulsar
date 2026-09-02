//! A relay server run INSIDE this app.
//!
//! Settings → Ağ has a toggle: turn it on and this device stops using a remote rendezvous
//! server and runs one itself, which the app then registers with automatically. There is
//! no address to type — the relay is local by definition, so `go_online` points at
//! `127.0.0.1:<port>` and the relay-address field is ignored while the toggle is on. Other
//! machines on the LAN can use it too, by pointing their own relay setting at this
//! device's LAN address.
//!
//! This is the same `pulsar_relay::Relay` the standalone binary and `pulsar --relay` run,
//! just hosted on a tokio task in-process, so it starts and stops with the toggle.

use std::net::SocketAddr;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

/// The task hosting the running relay, plus the port it bound.
struct RelayTask {
	handle: tauri::async_runtime::JoinHandle<std::io::Result<()>>,
	port: u16,
}

/// Managed state: `Some` while the in-app relay is running.
#[derive(Default)]
pub(crate) struct LocalRelay(Mutex<Option<RelayTask>>);

/// What the UI shows for the toggle.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayStatus {
	pub running: bool,
	/// The UDP port it bound (0 when not running).
	pub port: u16,
	/// The address other devices on the LAN would point their relay setting at.
	pub lan_addr: String,
}

impl RelayStatus {
	fn stopped() -> Self {
		Self {
			running: false,
			port: 0,
			lan_addr: String::new(),
		}
	}
}

/// The address this device is reachable on from the LAN, for the "others can use it too"
/// hint. Routing probe only — a UDP connect sends no packets. Falls back to the port alone.
fn lan_addr(port: u16) -> String {
	let ip = std::net::UdpSocket::bind("0.0.0.0:0")
		.and_then(|s| {
			s.connect("8.8.8.8:80")?;
			s.local_addr()
		})
		.ok()
		.map(|a| a.ip().to_string());
	match ip {
		Some(ip) if !ip.starts_with("127.") => format!("{ip}:{port}"),
		_ => format!(":{port}"),
	}
}

/// Lock the state, tolerating a previous panic.
///
/// A poisoned mutex used to make this permanently unusable: one panic inside the guard
/// (there WAS one — an accidental `block_on` inside the async command) left every later
/// `lock().unwrap()` panicking too, so the toggle could never be switched on again for the
/// rest of the run. The data behind it is a plain `Option<RelayTask>`, which a panic cannot
/// leave half-written, so recovering the inner value is safe and strictly better than
/// bricking the feature.
fn lock(state: &LocalRelay) -> std::sync::MutexGuard<'_, Option<RelayTask>> {
	state.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// The status of a currently-running relay, if there is one.
fn running_status<R: Runtime>(app: &AppHandle<R>) -> Option<RelayStatus> {
	let state = app.state::<LocalRelay>();
	let mut guard = lock(&state);
	match guard.as_ref() {
		Some(t) if !t.handle.inner().is_finished() => Some(RelayStatus {
			running: true,
			port: t.port,
			lan_addr: lan_addr(t.port),
		}),
		// The task exited on its own (bind lost, runtime shutdown) — forget it so the next
		// start rebinds instead of reporting a relay that is not there.
		Some(_) => {
			*guard = None;
			None
		}
		None => None,
	}
}

/// Start the in-app relay if it isn't already running. Idempotent: a second call just
/// reports the running one rather than fighting for the port.
///
/// `async` on purpose: the bind is awaited, never `block_on`-ed. This runs on the Tauri
/// async runtime, and blocking on it from inside itself panics with "Cannot start a
/// runtime from within a runtime". The lock is likewise never held across the await.
pub(crate) async fn ensure_running<R: Runtime>(
	app: &AppHandle<R>,
	port: u16,
) -> Result<RelayStatus, String> {
	if let Some(st) = running_status(app) {
		return Ok(st);
	}
	let port = if port == 0 {
		pulsar_core::proto::DEFAULT_RELAY_PORT
	} else {
		port
	};
	let addr = SocketAddr::from(([0, 0, 0, 0], port));
	// Bind before taking the lock: a port clash is then reported to the toggle as an error
	// instead of failing later inside a detached task the UI never hears from.
	let relay = pulsar_relay::Relay::bind(addr)
		.await
		.map_err(|e| format!("{}: {e}", crate::i18n::t("err.localRelayBind")))?;
	let bound = relay.local_addr().map(|a| a.port()).unwrap_or(port);
	let handle = tauri::async_runtime::spawn(relay.run());

	let state = app.state::<LocalRelay>();
	let mut guard = lock(&state);
	// Someone started one while we were binding — keep theirs, drop ours.
	if let Some(t) = guard.as_ref() {
		if !t.handle.inner().is_finished() {
			handle.abort();
			return Ok(RelayStatus {
				running: true,
				port: t.port,
				lan_addr: lan_addr(t.port),
			});
		}
	}
	tracing::info!(port = bound, "in-app relay started");
	*guard = Some(RelayTask {
		handle,
		port: bound,
	});
	Ok(RelayStatus {
		running: true,
		port: bound,
		lan_addr: lan_addr(bound),
	})
}

/// Stop the in-app relay. Safe to call when it isn't running.
pub(crate) fn stop<R: Runtime>(app: &AppHandle<R>) {
	let state = app.state::<LocalRelay>();
	let mut guard = lock(&state);
	if let Some(t) = guard.take() {
		t.handle.abort();
		tracing::info!(port = t.port, "in-app relay stopped");
	}
}

/// Turn the in-app relay on or off. Persisted in the config, so the choice survives a
/// restart and `go_online` knows to register against the local one.
#[tauri::command]
pub(crate) async fn set_local_relay(
	app: AppHandle,
	state: tauri::State<'_, crate::state::AppState>,
	enabled: bool,
) -> Result<RelayStatus, String> {
	let status = if enabled {
		ensure_running(&app, 0).await?
	} else {
		stop(&app);
		RelayStatus::stopped()
	};
	// Persist only after the relay actually came up, so a failed bind doesn't leave the
	// app configured to use a relay that isn't there.
	{
		let path = crate::util::config_path(&app);
		let mut cfg = state.config.lock().unwrap();
		cfg.use_local_relay = enabled;
		let snapshot = cfg.clone();
		drop(cfg);
		if let Err(e) = snapshot.save(&path) {
			tracing::warn!(error = %e, "could not persist the local-relay setting");
		}
	}
	Ok(status)
}

/// Current state of the in-app relay (for the toggle on mount).
#[tauri::command]
pub(crate) async fn local_relay_status(app: AppHandle) -> Result<RelayStatus, String> {
	Ok(running_status(&app).unwrap_or_else(RelayStatus::stopped))
}
