//! Connection authorization: the host's Allow/Deny popup, the client's password
//! prompt, the race between them, and the small Tauri commands that resolve them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::service::{
	need_password, recv_client_auth, recv_host_auth, send_auth, ClientAuth, HostAuth,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::events::{AuthPrompt, SessionEvent};
use crate::state::AppState;

/// Spawn the Allow/Deny popup as a separate, focused, always-on-top window that
/// requests the user's attention (they may be in another app).
pub(crate) fn open_approval_window(app: &AppHandle, id: u64, peer: &str, pw_status: &str) {
	// A relay id is a grouped 9-digit string ("482 913 056"); normalize it to plain
	// digits for the popup. A DIRECT (relay-less) connect's `peer` is an address like
	// "192.168.1.5:9000" — digit-stripping that yields meaningless "192168159000", so
	// pass it through verbatim (the popup renders it as-is, ungrouped). Detect a relay
	// id by: despaced value is exactly 9 ASCII digits.
	let despaced: String = peer.chars().filter(|c| !c.is_whitespace()).collect();
	let is_relay_id = despaced.len() == 9 && despaced.chars().all(|c| c.is_ascii_digit());
	let peer_q: String = if is_relay_id {
		despaced
	} else {
		// Escape for safe injection into a JS double-quoted string literal.
		peer.replace('\\', "\\\\").replace('"', "\\\"")
	};
	// Inject the request details before the page loads (more reliable than a query
	// string surviving the asset URL).
	let init = format!("window.__APPROVE__={{id:{id},peer:\"{peer_q}\",pw:\"{pw_status}\"}};");
	match WebviewWindowBuilder::new(
		app,
		format!("approve-{id}"),
		WebviewUrl::App("index.html".into()),
	)
	.initialization_script(&init)
	.title("Pulsar — Bağlantı isteği")
	.inner_size(400.0, 300.0)
	.resizable(false)
	.always_on_top(true)
	.center()
	.focused(true)
	.build()
	{
		Ok(win) => {
			let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
		}
		Err(e) => tracing::warn!(%e, "approval window failed to open"),
	}
}

/// The approval popup's Allow/Deny buttons call this to resolve the request.
#[tauri::command]
pub(crate) async fn respond_request(
	state: State<'_, AppState>,
	id: u64,
	allow: bool,
) -> Result<(), String> {
	if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
		let _ = tx.send(allow);
	}
	Ok(())
}

/// Host: open the Allow/Deny popup AND, at the same time, race it against a correct
/// password arriving over the session. Accept on whichever lands first — so the
/// host can approve passwordlessly while the client is still being asked for one.
pub(crate) async fn race_host_auth(
	session: &mut pulsar_core::Session,
	app: &AppHandle,
	pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
	next_req: &Arc<AtomicU64>,
	peer: &str,
	host_pw: &str,
) -> bool {
	let id = next_req.fetch_add(1, Ordering::SeqCst);
	let (tx, mut rx) = oneshot::channel::<bool>();
	pending.lock().unwrap().insert(id, tx);
	let _ = app.emit(
		"session",
		SessionEvent {
			kind: "request".into(),
			peer: peer.into(),
			detail: "wait".into(),
		},
	);
	open_approval_window(app, id, peer, "wait");

	let result = loop {
		tokio::select! {
			biased;
			d = &mut rx => break matches!(d, Ok(true)),
			msg = recv_client_auth(session) => match msg {
				ClientAuth::Password(pw) => {
					if !host_pw.is_empty() && pw == host_pw {
						break true; // correct password → accept
					}
					let _ = need_password(session).await; // wrong → ask client to retry
				}
				ClientAuth::Keepalive => {}
				ClientAuth::Gone => break false,
			}
		}
	};
	pending.lock().unwrap().remove(&id);
	if let Some(win) = app.get_webview_window(&format!("approve-{id}")) {
		let _ = win.close();
	}
	result
}

/// Client: open a password prompt on the UI; returns the receiver for the answer.
fn open_pw_prompt(
	app: &AppHandle,
	pw_pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	next_auth: &Arc<AtomicU64>,
	peer: &str,
) -> (u64, oneshot::Receiver<Option<String>>) {
	let id = next_auth.fetch_add(1, Ordering::SeqCst);
	let (tx, rx) = oneshot::channel::<Option<String>>();
	pw_pending.lock().unwrap().insert(id, tx);
	let _ = app.emit(
		"auth-prompt",
		AuthPrompt {
			req: id,
			peer: peer.into(),
		},
	);
	(id, rx)
}

/// Client: authenticate over the session. Sends an empty request first (which makes
/// the host show its Allow/Deny popup + ask us to prompt), then races the host's
/// approval against the user typing the password. Returns `Ok(true)` if accepted.
pub(crate) async fn client_authenticate(
	sess: &mut pulsar_core::Session,
	app: &AppHandle,
	pw_pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	next_auth: &Arc<AtomicU64>,
	peer: &str,
) -> Result<bool, String> {
	send_auth(sess, "").await.map_err(|e| e.to_string())?;
	let mut pw_rx: Option<oneshot::Receiver<Option<String>>> = None;
	let mut cur_id: u64 = 0;
	let cleanup = |id: u64| {
		pw_pending.lock().unwrap().remove(&id);
	};
	loop {
		match pw_rx.take() {
			// Waiting for both the host's reply and the user's password.
			Some(mut rx) => {
				tokio::select! {
					biased;
					pw = &mut rx => {
						cleanup(cur_id);
						match pw {
							Ok(Some(p)) => send_auth(sess, &p).await.map_err(|e| e.to_string())?,
							_ => return Ok(false), // user cancelled
						}
					}
					out = recv_host_auth(sess) => match out {
						HostAuth::Ok => { cleanup(cur_id); return Ok(true); }
						HostAuth::Denied | HostAuth::Gone => { cleanup(cur_id); return Ok(false); }
						HostAuth::NeedPassword => {
							cleanup(cur_id);
							let (id, rx2) = open_pw_prompt(app, pw_pending, next_auth, peer);
							cur_id = id;
							pw_rx = Some(rx2);
						}
						HostAuth::Other => pw_rx = Some(rx), // keepalive: keep waiting
					}
				}
			}
			// Not prompting yet: just read the host's reply.
			None => match recv_host_auth(sess).await {
				HostAuth::Ok => return Ok(true),
				HostAuth::Denied | HostAuth::Gone => return Ok(false),
				HostAuth::NeedPassword => {
					let (id, rx) = open_pw_prompt(app, pw_pending, next_auth, peer);
					cur_id = id;
					pw_rx = Some(rx);
				}
				HostAuth::Other => {}
			},
		}
	}
}

/// The client password prompt replies here (`null` = cancelled).
#[tauri::command]
pub(crate) async fn submit_password(
	state: State<'_, AppState>,
	req: u64,
	password: Option<String>,
) -> Result<(), String> {
	if let Some(tx) = state.pw_pending.lock().unwrap().remove(&req) {
		let _ = tx.send(password);
	}
	Ok(())
}

/// Host: forcibly disconnect a connected client by its peer id.
#[tauri::command]
pub(crate) async fn disconnect_peer(state: State<'_, AppState>, peer: String) -> Result<(), String> {
	if let Some((_, tx)) = state.incoming.lock().unwrap().remove(&peer) {
		let _ = tx.send(());
	}
	Ok(())
}
