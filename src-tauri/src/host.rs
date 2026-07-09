//! Host role: bind the node, register with the relay, and serve incoming sessions
//! (auth → games → stream → input → side-channels). `go_online` is the single
//! long-lived entry point; the per-session stream/file/audio handlers (and the
//! Windows WASAPI loopback helper) live in the `handlers` submodule.

use std::net::SocketAddr;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::input::{create_virtual_pad_target, EmulationTarget, GamepadKind, ResolvedTarget, VirtualGamepad};

/// Spawn a thread that forwards the game's rumble from a virtual pad back to the client's
/// physical controller (as [`DataMsg::Rumble`]). No-op when the backend has no rumble
/// reader (non-DS4). The thread exits when the pad is unplugged (`next()` → `None`) or the
/// outbound channel closes (session ended).
fn spawn_rumble_forward(
	pad: &dyn VirtualGamepad,
	slot: u8,
	tx: tokio::sync::mpsc::Sender<pulsar_core::service::DataMsg>,
) {
	use pulsar_core::service::DataMsg;
	if let Some(mut reader) = pad.rumble_reader() {
		tracing::info!(slot, "rumble: DS4 notifier thread started (host)");
		std::thread::spawn(move || {
			while let Some((large, small)) = reader.next() {
				tracing::info!(slot, large, small, "rumble: host got from game → forwarding");
				if tx.blocking_send(DataMsg::Rumble { slot, large, small }).is_err() {
					break;
				}
			}
			tracing::info!(slot, "rumble: DS4 notifier thread ended (host)");
		});
	} else {
		tracing::info!("rumble: pad has NO rumble_reader (not DS4 backend?)");
	}
}
use pulsar_core::pipeline::{self, CaptureMethod, HwEncoder, StreamPlan};
use pulsar_core::proto::DeviceId;
use pulsar_core::service::{
	accept, need_password, recv_auth, reject, send_bye_via, serve_with, DataHandlers, DataMsg,
	GameInfo, InputEvent, QualityPref, StreamReq,
};
use pulsar_core::{Discovery, Node};
use tauri::{AppHandle, Emitter, Manager as _, State};
use tokio::sync::oneshot;

use crate::audio_io::spawn_audio_player;
use crate::events::{AvatarPayload, DataPayload, FilePayload, ReverseReq, SessionEvent};
use crate::files::{sanitize_filename, save_received_file_chunks};
use crate::process::{
	capture_from_str, codec_from_str, encoder_from_str, ffmpeg_bin, launch_host_game, no_window,
	probe_ddagrab_zerocopy, spawn_tracked,
};
use crate::state::AppState;
use crate::util::{config_path, display_rotation, identity_path, resolve_relay, DDAGRAB_ZEROCOPY};

mod handlers;
#[cfg(target_os = "linux")]
pub(crate) mod cursor;
use handlers::{make_on_audio, make_on_file, make_on_stream};

/// Transport features this host advertises in its `StreamCaps` reply: it can carry
/// the RTP media inside the session (single socket) and honors NACK retransmits.
fn media_features() -> Vec<String> {
	use pulsar_core::service::media::{FEAT_MOS, FEAT_NACK};
	vec![FEAT_MOS.to_string(), FEAT_NACK.to_string()]
}

/// RAII guard that ensures per-session resources are released when the session task
/// ends — either naturally (session disconnect) or forcibly (via `JoinHandle::abort()`
/// in `go_online`'s drain loop on a reconnect/settings change).
///
/// Without this, `abort()` cancels the future at the current `.await` point inside
/// `serve_with` and skips the post-`tokio::select!` cleanup block, leaving:
///   - ffmpeg encoder children running orphaned (GPU leak / NVENC 100% failure mode)
///   - on Linux: the XDG ScreenCast portal session live (compositor keeps showing
///     "your screen is being shared" even though nothing is captured)
///   - host audio redirected until the next `go_online` calls `reset_redirect_all`
///   - `incoming`/`host_out`/`active`/`peer_meta` maps retaining stale entries for
///     the aborted peer (ghost connection leaking ~50-70 KB avatar data-URL + a
///     dead mpsc sender per ghost)
///   - the Connections window showing the now-dead peer indefinitely (it only
///     removes a row on a `disconnected` SessionEvent)
///   - `hostSessions` in +page.svelte permanently non-empty (the boot/idle
///     auto-updater is gated on `hostSessions.length === 0` and never fires again
///     until the app is fully restarted)
///
/// `Drop` performs the critical synchronous parts immediately; the Linux portal
/// close (async D-Bus call) is fire-and-forget spawned so the runtime cleans it up
/// in the background. All operations are idempotent so double-calling (guard drop +
/// normal teardown) is safe.
struct SessionCleanupGuard {
	procs: Arc<Mutex<Vec<Child>>>,
	/// The RTP forwarder tasks for this session's current stream (vh/ah).  Each holds
	/// a strong `Arc<Node>` clone (via `SessionSender`), so without an abort here
	/// a `JoinHandle::abort()` of the *session* task cancels the session future but
	/// leaves the forwarders running indefinitely — they block in `vsock/asock.recv()`
	/// and never observe the session going away, so the old `Node` (and its bound UDP
	/// socket) is never dropped.  We abort + do NOT await (same rationale as `procs`
	/// kill: blocking Drop stalls the executor thread running `abort()`; the async
	/// runtime will poll each aborted task to completion and release the `Arc<Node>`
	/// shortly after this Drop returns).
	fwd_slot: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
	#[cfg(target_os = "linux")]
	cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>>,
	/// This session's capture generation counter (shared with on_stream). Bumped in
	/// Drop so a capture::start task still in the portal picker — when cap_slot is
	/// empty — observes the change and aborts, dismissing the dialog.
	#[cfg(target_os = "linux")]
	cap_gen: Arc<std::sync::atomic::AtomicU64>,
	/// Display-mode watcher task; aborted here so an abort()-path teardown (e.g.
	/// go_online reconnect) cancels it immediately instead of waiting up to ~1.5 s
	/// for the next restream_tx.is_closed() check.  On Windows this watcher is
	/// active only for ffmpeg-path sessions (native DXGI self-heals via ACCESS_LOST;
	/// the watcher fires only when last_req_store is populated, which on_stream does
	/// only when native_started=false).
	mode_watcher: tokio::task::JoinHandle<()>,
	sid: u64,
	/// Bookkeeping maps that must be cleaned up even on the abort() path so the
	/// Connections window and +page.svelte's `hostSessions` do not show phantom
	/// peers and so the auto-updater liveness gate is not permanently suppressed.
	/// These are all the same Arcs captured in the session closure.
	incoming: Arc<Mutex<std::collections::HashMap<u64, (String, tokio::sync::oneshot::Sender<()>)>>>,
	host_out: Arc<Mutex<std::collections::HashMap<u64, (String, tokio::sync::mpsc::Sender<pulsar_core::service::DataMsg>)>>>,
	active: Arc<Mutex<std::collections::HashMap<u64, crate::state::ConnInfo>>>,
	peer_meta: Arc<Mutex<std::collections::HashMap<String, (Option<String>, Option<String>)>>>,
	peer: String,
	/// Same-host co-op pre-auth bookkeeping: the peer's pubkey + whether we registered a
	/// pre-auth entry for it. Dropping the LAST live session for this pubkey removes the
	/// entry (so a future connect from this device must re-authenticate). Removal is done
	/// HERE (the guard always drops, on both the normal and abort paths) — exactly once.
	preauth_pubkey: Option<[u8; 32]>,
	/// Per-session screen-adaptation state. If this session switched the captured monitor's
	/// display mode for a split pane (Windows, `StreamReq::adapt`), the original mode is stored
	/// here; Drop reverts it so the host's resolution is ALWAYS restored on teardown — on both
	/// the normal end and the abort() path (Drop covers both). `take()`n so it's idempotent (a
	/// second drop / a prior revert via an `adapt: None` restream sees `None` and no-ops). The
	/// same Arc is held by the on_stream handler, which applies/updates it during the session.
	adapt_state: Arc<Mutex<crate::display_mode::AdaptState>>,
	app_handle: AppHandle,
}

impl Drop for SessionCleanupGuard {
	fn drop(&mut self) {
		// Kill any live ffmpeg encoder children. `std::process::Child::drop` does NOT
		// kill the OS process, so the orphan-NVENC-100%-GPU failure mode requires an
		// explicit `kill()`. After draining, the normal teardown block finds the vec
		// empty and is a no-op (safe double-call).
		for mut child in self.procs.lock().unwrap().drain(..) {
			let _ = child.kill();
			// wait() right after SIGKILL returns immediately (the process is already
			// dead) and reaps the zombie entry from the kernel process table.
			// Skipping wait() would leave a <defunct> zombie per encoder for the
			// entire app lifetime — Unix does NOT auto-reap children unless the
			// parent calls wait() or exits; Tokio has no SIGCHLD reaper for
			// std::process::Child zombies (confirmed: no SIGCHLD handler in codebase).
			// This mirrors the normal teardown block at the tokio::select! exit path.
			let _ = child.wait();
		}
		// Abort the RTP media-forwarder tasks (vh/ah).  Each holds a strong
		// `Arc<Node>` clone (via `SessionSender`) and blocks in `vsock/asock.recv()`;
		// without this abort they keep the old `Node` alive after the session task is
		// cancelled, pinning its UDP socket and relay heartbeat indefinitely.
		// We do NOT await here (same reasoning as the `procs` kill above: Drop must
		// not block the executor thread).  The runtime will poll each aborted task to
		// completion shortly after this Drop returns, releasing the `Arc<Node>`.
		// After draining, the normal teardown block finds the vec empty — safe no-op.
		for h in self.fwd_slot.lock().unwrap().drain(..) {
			h.abort();
		}
		// Abort the display-mode watcher immediately so it does not linger for up
		// to ~1.5 s waiting for its next restream_tx.is_closed() check.
		// abort() is safe to call after the task has already finished.
		self.mode_watcher.abort();
		// Screen adaptation (Parsec-style): if this session switched the captured monitor's
		// display mode for a split pane, restore the ORIGINAL mode now so the host is never
		// left stuck at a wrong resolution. This is the teardown guarantee — it fires on BOTH
		// the normal end and the abort() path (Drop covers both). `take()` makes it idempotent
		// (a second drop, or a prior revert from an `adapt: None` restream, finds None). No-op
		// off Windows (the stored PrevMode is always None there).
		if let Some(prev) = self.adapt_state.lock().unwrap().prev.take() {
			crate::display_mode::revert(prev);
		}
		// Linux: close the XDG ScreenCast portal session so the compositor's
		// "your screen is being shared" indicator disappears.
		// `WaylandCapture::stop` is async, so we fire-and-forget a background task.
		// The runtime is still live when this Drop runs: abort() fires from within the
		// running Tokio executor, not from its shutdown path.
		#[cfg(target_os = "linux")]
		{
			// Bump FIRST so a capture::start STILL SITTING IN THE PORTAL PICKER (nothing
			// in cap_slot yet) sees the stale generation and drops its start future,
			// which dismisses the picker (capture.rs's SessionCloseGuard). Covers the
			// abort() teardown path, where serve_with's own cleanup (which also bumps
			// cap_gen) is skipped.
			self.cap_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			if let Some(cap) = self.cap_slot.lock().unwrap().take() {
				let _ = tokio::spawn(cap.stop());
			}
		}
		// Release per-session redirect ownership; idempotent if already released
		// by the normal teardown block.
		handlers::release_redirect(self.sid);
		// Same-host co-op pre-auth: drop this session's pre-auth refcount. Done HERE only
		// (the guard always drops, on both the normal and abort paths) so the decrement
		// happens exactly once — the normal teardown tail does NOT also remove it. When the
		// LAST session for this device ends the entry is removed, so a future connect from
		// it must re-authenticate. None when this was an unattended (no-auth) session.
		if let Some(pk) = self.preauth_pubkey {
			crate::auth::preauth::remove(&pk);
		}
		// C3: Emit a `disconnected` SessionEvent and clean up the bookkeeping maps
		// even when this session task was cancelled by `JoinHandle::abort()` (the
		// go_online reconnect path). Without this, the abort() path skips the entire
		// post-tokio::select! cleanup tail, leaving:
		//   - stale (sid, dead-sender) entries in incoming/host_out/active/peer_meta
		//   - the Connections window showing a phantom peer row forever
		//   - +page.svelte's `hostSessions` permanently non-empty → auto-updater wedged
		// All operations below are idempotent (sid-guarded compare-and-remove), so
		// double-execution with the normal teardown path is safe: the normal path runs
		// first (or may already have run), finds the maps empty / entries already gone,
		// and is a no-op.
		let sid = self.sid;
		let peer = self.peer.clone();
		// Keyed by sid now: this entry is uniquely ours, so a plain remove() is correct —
		// a same-DEVICE co-op session has its OWN sid key and is never touched here.
		self.incoming.lock().unwrap().remove(&sid);
		self.host_out.lock().unwrap().remove(&sid);
		// peer_still_live: any OTHER active session sharing this peer (couch co-op) — used
		// to refcount the per-DEVICE identity cache so session B's teardown doesn't blank
		// session A's still-live name/avatar.
		let (removed, peer_still_live, conns_emptied) = {
			let mut g = self.active.lock().unwrap();
			let removed = g.remove(&sid).is_some();
			let peer_still_live = g.values().any(|ci| ci.peer == peer);
			(removed, peer_still_live, g.is_empty())
		};
		if removed && !peer_still_live {
			// Last session for this device left — release the ~50-70 KB avatar data-URL
			// and peer name cached for it; a reconnect will re-push them.
			self.peer_meta.lock().unwrap().remove(&peer);
		}
		if conns_emptied {
			crate::connections::close(&self.app_handle);
		}
		// Emit the `disconnected` SessionEvent (carrying this session's sid) so both the
		// Connections window and +page.svelte's `hostSessions` drop THIS session's row.
		// Gated on `removed` (we actually held the slot): the row is keyed by sid, so this
		// only drops our own pane — a same-device co-op session's row survives.
		if removed {
			let _ = self.app_handle.emit(
				"session",
				crate::events::SessionEvent {
					kind: "disconnected".into(),
					peer,
					sid,
					detail: String::new(),
				},
			);
		}
	}
}

/// Bind the node and register with the configured relay; returns this device's
/// grouped ID. Fails (so the UI shows "offline") when the relay is unreachable.
#[tauri::command]
pub(crate) async fn go_online(
	app: AppHandle,
	state: State<'_, AppState>,
) -> Result<String, String> {
	// Pre-warm ALL encoder probes off the hot path: the first QueryStreamCaps must
	// answer within the client's 2 s window, but a cold probe chain (one-frame ffmpeg
	// encodes per backend×codec + the gst pipelines) takes several seconds. Results
	// are cached per process, so this makes the first caps reply instant. (Verified
	// failure mode on the Pi: cold probes > 2 s → client timed out → auto codec fell
	// back to H.264 even though MPP HEVC was available.)
	{
		let ffmpeg = crate::process::ffmpeg_bin(&app);
		let vaapi = state.stream_cfg.lock().unwrap().vaapi_device.clone();
		std::thread::spawn(move || {
			let _ = crate::process::validated_encoders(&ffmpeg, &vaapi);
			#[cfg(target_os = "linux")]
			let _ = crate::process::validated_gst_encoders();
		});
	}

	let cfg = state.config.lock().unwrap().clone();
	// go_online is re-runnable (startup, manual retry, relay/network settings change).
	// Tear down any previous serve loop + node FIRST so we don't leak a stale node
	// (its UDP socket, relay heartbeat, serve task, LAN beacon) on every reconnect.
	// Aborting the serve loop drops its Arc<Node> clone; taking state.node drops ours,
	// so the old node reaches strong-count 0 and its recv_loop/heartbeat_loop exit.
	// Collect ALL live task handles before aborting so we can await them below.
	// Awaiting is necessary when cfg.node_port is pinned: abort() only SCHEDULES
	// cancellation; each aborted task holds a strong Arc<Node> clone (its Session +
	// SessionSender keep the node alive), and that Arc — together with its UDP
	// socket — is not dropped until the runtime actually polls the cancelled future
	// to completion. If we don't await, the only yield point before rebinding the
	// pinned port is `resolve_relay().await`, which returns synchronously (no poll
	// of the reactor) when the relay string is already a SocketAddr (IP literal,
	// e.g. 192.168.1.5:21116 — the normal LAN-relay case). On a current-thread or
	// busy executor the aborted tasks may not have run their drop yet, leaving the
	// old socket bound and the rebind failing with address-in-use.
	let mut teardown_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
	if let Some(h) = state.serve_task.lock().unwrap().take() {
		h.abort();
		teardown_handles.push(h);
	}
	// Abort the independently-spawned per-session tasks too: each owns a strong
	// Arc<Node> (its Session + SessionSender), so without this they'd keep the old
	// node at strong-count > 0 — its UDP socket stays bound (a re-bind to a pinned
	// node_port would then fail address-in-use) and its relay heartbeat keeps
	// pinging the OLD relay.
	for h in state.session_tasks.lock().unwrap().drain(..) {
		h.abort();
		teardown_handles.push(h);
	}
	// Await all aborted tasks to completion — this ensures their Arc<Node> clones
	// (and thus the old UDP socket) are fully dropped BEFORE we rebind the port.
	// `abort()` + `await` resolves immediately with `Err(JoinError::cancelled())`;
	// we ignore that error. This is the only deterministic way to guarantee the
	// socket is free when cfg.node_port is pinned.
	for h in teardown_handles {
		let _ = h.await;
	}
	// Second drain — close the teardown-accept race (C20): after serve_task has
	// been awaited its final synchronous iteration (accept→spawn→push) has
	// completed, so any session handle pushed after the first drain is now
	// visible.  Abort + await those too so no orphan Arc<Node> (and its UDP
	// socket) escapes teardown.  Collect into a local Vec first so the
	// MutexGuard is dropped before the await points (the guard is !Send).
	let late_handles: Vec<_> = state.session_tasks.lock().unwrap().drain(..).collect();
	for h in late_handles {
		h.abort();
		let _ = h.await;
	}
	let _ = state.node.lock().unwrap().take();
	// The old node (and its port) is gone: clear the advertised port now, so a
	// go_online that fails below doesn't leave Home showing a copyable ip:port
	// that no longer accepts connections. The success path re-publishes the
	// real port further down.
	state
		.node_port
		.store(0, std::sync::atomic::Ordering::SeqCst);
	let _ = app.emit("node-port", 0u16);
	// A previous serve loop's sessions may not have torn down cleanly (independent
	// spawns survive the accept-loop abort) — never carry a stale sink-redirect over.
	handlers::reset_redirect_all();
	// Likewise clear any stranded same-host co-op pre-auth entries: a fresh serve loop has
	// no live sessions, so no device should remain pre-authorized from the prior loop.
	crate::auth::preauth::clear_all();
	// Crash-restore for the Sunshine-style sink redirect: a prior process that
	// redirected the default render endpoint to the virtual sink and died before its
	// guard dropped left the host on the sinkless sink (real speakers silent). Restore
	// the saved default from the on-disk marker (no-op for a clean previous exit).
	handlers::restore_stale_redirect();
	// Crash-restore for the Linux/macOS host-silent null-sink / output redirect: a
	// prior process that crashed/SIGKILL'd before the HostSilentGuard::drop ran left
	// PulseAudio (Linux) pointing at the `pulsar_silent` null sink as the default
	// (real speakers silent) or macOS pointing at a virtual output device. Restore the
	// saved default from the on-disk marker and (Linux) unload the stranded null sink.
	// No-op for a clean previous exit or on Windows (Windows uses restore_stale_redirect).
	handlers::restore_stale_host_silent();
	// Crash-restore for the endpoint-mute fallback (no-virtual-sink / no-null-sink
	// case): a prior process that fell back to endpoint-mute and died before the
	// session ended left the host output muted. Unmute iff our marker says we set it
	// (never touches a mute the user set independently). No-op for a clean exit.
	handlers::restore_stale_mute_fallback();
	tracing::info!(relay = %cfg.relay, "go_online: resolving relay");
	let relay = resolve_relay(&cfg.relay)
		.await
		.ok_or_else(|| format!("{}: {}", crate::i18n::t("err.relayResolve"), cfg.relay))?;
	tracing::info!(%relay, "go_online: binding node + registering");
	let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
	// Identity advertised on the network: the user's chosen device name, or — when
	// it's the generic default — the OS user's name, so relay-less peers are still
	// recognizable ("Ahmet Enes Duruer" instead of "Pulsar Cihazı").
	let announce_name = {
		let n = cfg.device_name.trim();
		if n.is_empty() || n == "Pulsar Cihazı" {
			pulsar_core::discovery::os_display_name()
		} else {
			n.to_string()
		}
	};
	// Port policy: an explicitly configured port (Settings → Ağ) is binding — if it's
	// already taken, FAIL with a clear error instead of silently sliding to another
	// port (the user pinned it for a firewall rule / port-forward; a silent ephemeral
	// fallback made those rules quietly useless). Unset (0) = a RANDOM ephemeral port
	// every launch — the LAN beacon and the Home screen's "ip:port" always carry the
	// real port, so discovery/direct connects keep working.
	let preferred = SocketAddr::new(local.ip(), cfg.node_port);
	// Persisted per-user identity → the relay hands back the SAME 9-digit ID every
	// launch (stable device ID). Different OS users keep separate identity files.
	let identity = pulsar_core::crypto::Identity::load_or_create(identity_path(&app));
	let node = match Node::bind_with_identity(
		preferred,
		relay,
		cfg.network_mode,
		announce_name.clone(),
		identity.clone(),
	)
	.await
	{
		Ok(n) => n,
		Err(e) if cfg.node_port != 0 => {
			return Err(format!(
				"{} ({}): {e}",
				crate::i18n::t("err.portInUse"),
				cfg.node_port
			));
		}
		Err(e) => return Err(e.to_string()),
	};

	// Start LAN discovery BEFORE registering so it works even when the relay is
	// unreachable (offline mode): we announce ourselves (id-less) and find peers on
	// the local network regardless of relay state. Replaces any prior beacon.
	let node_port = node.local_addr().map(|a| a.port()).unwrap_or(0);
	// Surface the live port to the UI (Home shows "ip:port" for direct connects):
	// state for late mounts + an event for screens already up.
	state
		.node_port
		.store(node_port, std::sync::atomic::Ordering::SeqCst);
	let _ = app.emit("node-port", node_port);
	let discovery =
		match Discovery::start(announce_name.clone(), node_port, node.public_key(), None).await {
			Ok(d) => {
				tracing::info!(port = node_port, name = %announce_name, "LAN discovery beacon started");
				// Honor the current serve gate: a beacon (re)started while in gaming mode must
				// stay silent (don't advertise a pure client on the LAN).
				if state
					.hosting_disabled
					.load(std::sync::atomic::Ordering::Relaxed)
				{
					d.set_paused(true);
				}
				*state.discovery.lock().unwrap() = Some(d.clone());
				Some(d)
			}
			Err(e) => {
				tracing::warn!(%e, "LAN discovery failed to start");
				None
			}
		};

	// Issue a fresh one-time password for this online session (unless unattended
	// access is on, in which case no password is required).
	let require_auth = !cfg.unattended_access;
	let password = if require_auth {
		pulsar_core::service::gen_password()
	} else {
		String::new()
	};
	*state.password.lock().unwrap() = password;

	// Host role: serve published games, start streams, and surface activity.
	let games = state.games.clone();
	let stream_cfg = state.stream_cfg.clone();
	// Read the live password per connection (so `new_password` takes effect).
	let password_store = state.password.clone();
	let pending = state.pending.clone();
	let next_req = state.next_req.clone();
	let incoming = state.incoming.clone();
	let host_out = state.host_out.clone();
	let active = state.active.clone();
	// Track every per-session task so a later `go_online` can abort them and release
	// the strong Arc<Node> they each hold (otherwise the old node never drops).
	let session_tasks = state.session_tasks.clone();
	#[cfg(target_os = "linux")]
	let restore_token = state.restore_token.clone();
	let serve_node = node.clone();
	let app_h = app.clone();
	// Our display name, pushed to every connecting client (PeerName decoration).
	let self_name = announce_name.clone();
	let serve_handle = tokio::spawn(async move {
		while let Some(session) = serve_node.next_incoming().await {
			let self_name = self_name.clone();
			let games = games.clone();
			let stream_cfg = stream_cfg.clone();
			// ffmpeg children for THIS session live here and are killed on teardown
			// below — never in a global pool, so a client's exit can't orphan them.
			let procs: Arc<Mutex<Vec<Child>>> = Arc::new(Mutex::new(Vec::new()));
			// Native DXGI+NVENC capture handle for this session (Windows), when the native path
			// is used instead of ffmpeg. Stopped at the same drain sites as `procs`.
			#[cfg(windows)]
			let native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>> = Arc::new(Mutex::new(None));
			let password_store = password_store.clone();
			let pending = pending.clone();
			let next_req = next_req.clone();
			let incoming = incoming.clone();
			let host_out = host_out.clone();
			let active = active.clone();
			// PER-SESSION restore token (was a shared AppState clone): start each new
			// incoming connection with `None` so the Wayland ScreenCast picker prompts
			// on EVERY new connection (re-consent per remote viewer). The token the
			// portal returns is still reused WITHIN this session for re-streams
			// (resolution/codec/monitor changes) so those don't re-prompt.
			#[cfg(target_os = "linux")]
			let restore_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
			let app_h = app_h.clone();
			let peer = {
				let id = session.peer();
				if id.0 >= DeviceId::MIN {
					id.grouped()
				} else {
					// Direct (relay-less) connect has no relay id — key by the address.
					session
						.peer_addr()
						.await
						.map(|a| a.to_string())
						.unwrap_or_else(|| "direct".into())
				}
			};
			// This session's id: used so a same-peer reconnection that replaced our
			// `incoming`/`host_out` entries isn't evicted when THIS (older) session tears
			// down (both maps are keyed by `peer`, which collides across reconnects).
			let sid = session.id();
			// The peer's static X25519 public key — the no-MITM-safe identity the same-host
			// co-op pre-auth set is keyed on (NOT the spoofable relay id). Captured here while
			// the session is still owned; `None` only if the session state was already torn
			// down (then pre-auth simply doesn't apply and normal auth runs).
			let peer_pubkey = session.peer_pubkey().await;
			let session_handle = tokio::spawn(async move {
				let mut session = session;
				// The client's first message is its access request (password may be
				// empty). Auto-allow no-auth hosts or a correct password; otherwise
				// pop an attention-grabbing Allow/Deny window for the host user.
				// Bounded wait: a peer that establishes a session but never sends
				// Auth would otherwise pin this task (and its SessionState +
				// unbounded channel) forever — UDP gives no close.
				let provided = match tokio::time::timeout(
					std::time::Duration::from_secs(60),
					recv_auth(&mut session),
				)
				.await
				{
					Ok(Some(p)) => p,
					_ => return,
				};
				// Gaming mode (pure client): the user put this device into a personality
					// where it may NOT be a host, so refuse every inbound connection right
					// here — before any Allow/Deny popup or password race — and tell the UI.
					// Registration with the relay stays alive (outbound connects still work);
					// only inbound serving is gated. Active sessions are kicked separately by
					// the UI when the mode is entered.
					if app_h
						.state::<AppState>()
						.hosting_disabled
						.load(std::sync::atomic::Ordering::Relaxed)
					{
						let _ = reject(&mut session).await;
						tracing::info!(%peer, "inbound refused — hosting disabled (gaming mode)");
						let _ = app_h.emit(
							"session",
							SessionEvent {
								kind: "rejected".into(),
								peer: peer.clone(),
								sid,
								detail: String::new(),
							},
						);
						return;
					}
					// Auth: a correct up-front password is accepted immediately. Otherwise
				// the host's Allow/Deny popup AND the client's password prompt appear
				// at the SAME time; accept on whichever lands first (so the host can
				// approve passwordlessly). Unattended hosts auto-allow. The persistent
				// connect password (Settings → Güvenlik) is accepted alongside the
				// one-time password; wrong attempts are rate-limited per peer, and a
				// locked-out peer is rejected up front WITHOUT an Allow/Deny popup
				// (otherwise repeated connects could spam attention-grabbing windows).
				//
				// Read unattended_access LIVE from the current config so a toggle in
				// Settings → Güvenlik takes effect immediately — without needing a
				// go_offline/go_online cycle.  go_online only set require_auth once at
				// startup; if the user disabled unattended access while online the
				// captured bool would be stale and new connections would still bypass
				// auth.  Symmetrically, enabling unattended access while online would
				// still demand a password until a reconnect.
				let require_auth = !app_h
					.state::<AppState>()
					.config
					.lock()
					.unwrap()
					.unattended_access;
				// If auth just became required but no OTP exists yet (because
				// unattended_access was ON when go_online ran, so we set the password
				// to ""), lazily generate one now and emit it so the Home screen can
				// display it.  This is idempotent: if another connection already
				// generated it, we find it non-empty and skip.
				if require_auth {
					let mut pw_guard = password_store.lock().unwrap();
					if pw_guard.is_empty() {
						let fresh = pulsar_core::service::gen_password();
						*pw_guard = fresh.clone();
						drop(pw_guard);
						let _ = app_h.emit("session-password", fresh);
					}
				}
				// Same-host co-op pre-auth: if this DEVICE already has an authorized live
				// session, treat the new same-device session as pre-authorized — skip the
				// single-use OTP consume AND the Allow/Deny popup. Required because the OTP
				// rotates on consume, so a 2nd pane could never present a valid code. Keyed on
				// the no-MITM-safe pubkey (a relay can't substitute a peer behind an id).
				// Unattended hosts never reach here (require_auth=false → auto-allow below).
				let preauthorized = require_auth
					&& peer_pubkey
						.as_ref()
						.is_some_and(crate::auth::preauth::is_authorized);
				if preauthorized {
					tracing::info!(%peer, "same-device session pre-authorized (existing authorized session) — skipping OTP + popup");
				}
				// Set to true only when the operator picks "Allow view-only" in the popup —
				// the session starts with control revoked (see the ConnInfo insert below).
				let mut approved_view_only = false;
				let approved = if require_auth && !preauthorized {
					if let Some(rem) = crate::auth::throttle::locked_out(&peer) {
						tracing::warn!(%peer, secs = rem.as_secs(), "auth throttled: rejecting without prompt");
						false
					} else {
						let host_pw = password_store.lock().unwrap().clone();
						let custom_pw = app_h
							.state::<crate::state::AppState>()
							.config
							.lock()
							.unwrap()
							.connect_password
							.clone();
						let accepted: Vec<String> = [host_pw.clone(), custom_pw.clone()]
							.into_iter()
							.filter(|p| !p.is_empty())
							.collect();
						// Distinguish the two credentials: the ONE-TIME password (`host_pw`)
						// is single-use and must be rotated the moment it authenticates a
						// connection, so the same code never unlocks a second one. The
						// persistent connect password (Settings → Güvenlik) is intentionally
						// reusable and is NEVER rotated.
						// For the OTP, use try_consume_otp which atomically matches AND
						// rotates under one lock, eliminating the read→compare→rotate TOCTOU
						// race where two concurrent tasks could both match the same live OTP.
						let otp_accepted = crate::commands::try_consume_otp(&app_h, &provided);
						let custom_accepted = !custom_pw.is_empty()
							&& crate::auth::secret_eq(&provided, &custom_pw);
						if !accepted.is_empty() && (otp_accepted || custom_accepted) {
							true
						} else {
							// Count a NON-EMPTY wrong up-front guess here: otherwise an
							// attacker who sends one wrong code and then goes silent never
							// enters the race (it just times out recording nothing), so the
							// lockout never accumulates — a free uncounted attempt per connect.
							// An EMPTY provided is the client's automatic "I have no password
							// yet" probe (the real attempt comes from the password prompt →
							// race), so it is NOT counted, which also avoids double-counting a
							// genuine human attempt (client sends empty up-front, then the
							// typed code in the race).
							if !provided.is_empty() {
								crate::auth::throttle::record_failure(&peer);
								// Count it against the source-independent global limit
								// too: a relay attacker who registers many ids gets a
								// fresh per-peer bucket each time, so rotate the OTP
								// after enough TOTAL wrong guesses regardless of source.
								if crate::auth::throttle::note_global_failure() {
									crate::commands::rotate_session_password(&app_h);
								}
							}
							let _ = need_password(&mut session).await;
							let outcome = crate::auth::race_host_auth(
								&mut session,
								&app_h,
								&pending,
								&next_req,
								&peer,
								&accepted,
								&host_pw,
							)
							.await;
							// If the race accepted via the one-time password, rotate it
							// (single-use) — race_host_auth reports which credential matched.
							if outcome.matched_one_time {
								crate::commands::rotate_session_password(&app_h);
							}
							approved_view_only = outcome.view_only;
							outcome.approved
						}
					}
				} else {
					true
				};
				if approved {
					crate::auth::throttle::clear(&peer);
					// Same-host co-op: mark this DEVICE authorized so a subsequent same-pubkey
					// session (2nd pane) skips the OTP + popup. Only on password-gated hosts —
					// unattended (!require_auth) hosts auto-allow and don't need pre-auth. The
					// matching `preauth::remove` runs on this session's teardown (both paths).
					if require_auth {
						if let Some(pk) = peer_pubkey {
							crate::auth::preauth::add(pk);
						}
					}
				}
				if !approved {
					let _ = reject(&mut session).await;
					tracing::info!(%peer, "connection rejected");
					let _ = app_h.emit(
						"session",
						SessionEvent {
							kind: "rejected".into(),
							peer: peer.clone(),
							sid,
							detail: String::new(),
						},
					);
					return;
				}
				let _ = accept(&mut session).await;
				tracing::info!(%peer, "incoming session connected");
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "connected".into(),
						peer: peer.clone(),
						sid,
						detail: String::new(),
					},
				);
				// Connection time for the connections window.
				let since_ms = std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.map(|d| d.as_millis() as u64)
					.unwrap_or(0);
				// stop channel: the receiver drives the tokio::select! below; the sender is
				// registered in `incoming` so the host UI can kick this peer at any time.
				let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

				// Side channels: a queue the host UI drains to push chat/clipboard back
				// to this client (registered by peer id in `host_out` so `host_send_*`
				// can find it).
				let (out_tx, out_rx) = tokio::sync::mpsc::channel::<DataMsg>(256);
				// A clone for on_stream to push the encode summary to the client.
				let stats_out = out_tx.clone();
				// A clone for the file-manager handler's replies (FsEntries / file stream).
				let fs_out = out_tx.clone();
				// A clone the on_input gamepad path uses to forward the game's rumble back to
				// the client (one dedicated thread per emulated DS4 pad).
				let rumble_out = out_tx.clone();

				// C9: Register this session at accept time (not deferred to the first
				// StartStream) so the host operator can always see and kick any
				// authenticated peer — including data-channel-only, scripted, or
				// malicious clients that never send StartStream.
				//
				// PHASE 1 (same-host co-op): the maps are keyed by SESSION id now, so a
				// new session from a peer that's ALREADY connected NO LONGER collides — it
				// always registers under its own sid. There is no stop_tx to clobber, so the
				// old "skip-if-peer-present shadow" (which existed only to avoid overwriting a
				// live session's `incoming[peer]` and tearing it down) is gone: two panes from
				// one client device now hold two independent sessions, neither evicting the
				// other. make_on_stream therefore never re-inserts `incoming` (it always
				// receives None for stop_tx); it only upgrades the `active` mode + re-inserts
				// `host_out[sid]` on the first StartStream.
				//
				// Mode defaults to Remote; make_on_stream upgrades it to Game/Remote and
				// OPENS the connections window on the first StartStream (the correct mode
				// is only known then). We do NOT open the window here at accept time: a
				// metadata-only connection — the mobile game-library fetch (auth +
				// request_games + Bye, never a StartStream) — would otherwise flash the
				// window open and shut on every fetch, though no real session forms. The
				// session is still REGISTERED here (active/incoming/host_out), so any peer
				// remains listed + kickable the moment the window is open for a real session.
				let stop_tx_opt: Option<oneshot::Sender<()>> = {
					active.lock().unwrap().insert(
						sid,
						crate::state::ConnInfo {
							peer: peer.clone(),
							since_ms,
							// Unknown until StartStream; make_on_stream will overwrite.
							mode: crate::state::ConnMode::Remote,
							// "Allow view-only" from the popup starts the session with control
							// revoked; every other accept path grants control (false).
							view_only: approved_view_only,
						},
					);
					incoming.lock().unwrap().insert(sid, (peer.clone(), stop_tx));
					host_out.lock().unwrap().insert(sid, (peer.clone(), out_tx.clone()));
					None
				};

				// Media-over-session: a send-only session handle for the RTP forwarder
				// tasks (they transmit concurrently with the serve loop's recv), and the
				// NACK channel slot the active video forwarder registers itself into.
				let media_tx = session.sender();
				let nack_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>> =
					Arc::new(Mutex::new(None));
				// The running RTP forwarder tasks for this session's CURRENT stream; a
				// re-stream aborts + replaces them (same lifecycle as `procs`).
				let fwd_slot: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
					Arc::new(Mutex::new(Vec::new()));

				// Per-session: hold the screen capture so it can be stopped when this
				// client disconnects. (Input injection is via uinput in `on_input`.)
				#[cfg(target_os = "linux")]
				let cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>> =
					Arc::new(Mutex::new(None));
				// Generation guard for the async portal-capture task: capture::start can
				// sit in the portal dialog for seconds, racing teardown and overlapping
				// re-streams. Every (re-)stream bumps + captures it, teardown bumps it,
				// and a task whose generation went stale STOPS its fresh capture instead
				// of storing it into a dead/superseded session (orphaned portal cast).
				#[cfg(target_os = "linux")]
				let cap_gen: Arc<std::sync::atomic::AtomicU64> = Arc::new(std::sync::atomic::AtomicU64::new(0));
				// Records the latest StreamReq so the host-side display-mode watcher can
				// re-issue it to restart capture at the new geometry.  On Windows this is
				// populated only when the ffmpeg fallback path is active (native_started=false);
				// native DXGI sessions self-heal via ACCESS_LOST in pulsar-capture.
				let last_req_store: Arc<Mutex<Option<StreamReq>>> = Arc::new(Mutex::new(None));
				// Per-session screen-adaptation state (Parsec-style): when a StreamReq carries
				// `adapt: Some((paneW,paneH))` the on_stream handler switches the captured monitor
				// to the best-fit mode and stores the original here; the SessionCleanupGuard reverts
				// it on teardown (normal end AND abort()). Shared by Arc between the two so the
				// handler applies and the guard restores. Windows-only effect (no-op stubs else).
				let adapt_state: Arc<Mutex<crate::display_mode::AdaptState>> =
					Arc::new(Mutex::new(crate::display_mode::AdaptState::default()));
				// Producer half of the re-stream channel feeding `serve_with`'s restream branch:
				// when the HOST's own display mode changes mid-session the watcher re-sends the
				// last StreamReq so capture restarts at the new geometry (ffmpeg/Wayland/gdigrab
				// have no DXGI ACCESS_LOST signal).  On Windows this only fires for ffmpeg-path
				// sessions because on_stream only populates last_req_store when native_started=false.
				let (restream_tx, restream_rx) = tokio::sync::mpsc::channel::<StreamReq>(4);
				// Also handed to on_nack: on the ffmpeg fallback path (no live CaptureHandle to force
				// an IDR on) a client keyframe-request — MediaNack([0]) — re-issues the last StreamReq
				// here so capture restarts with a fresh IDR (see on_nack below for the gating).
				let nack_restream_tx = restream_tx.clone();
				// Poll the host's own display geometry; on a STABLE change to the streamed
				// display's size, re-issue the stored StreamReq once so capture rebuilds at the
				// new size. Inert when nothing has streamed yet (no stored req) and on platforms
				// where `host_displays()` is empty (Wayland w/o Mutter, macOS) — there it just
				// loops without ever firing. Exits when the channel closes (session teardown).
				let mode_watcher = {
					let last_req_store = last_req_store.clone();
					tokio::spawn(async move {
						// Baseline size of the currently-streamed display; re-baselined on a monitor
						// switch. `None` until the first stream request establishes a display_idx.
						let mut baseline: Option<(u32, (u32, u32))> = None;
						loop {
							tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
							// Stop once the consumer (serve_with) dropped its receiver.
							if restream_tx.is_closed() {
								break;
							}
							let Some(req) = last_req_store.lock().unwrap().clone() else {
								// Nothing has streamed yet — keep waiting for the first request.
								continue;
							};
							let idx = req.display_idx;
							// host_displays() may shell out (xrandr / gdbus), so keep it off the runtime.
							let size = tokio::task::spawn_blocking(move || {
								crate::process::host_displays()
									.into_iter()
									.find(|d| d.idx == idx)
									.map(|d| (d.width, d.height))
							})
							.await
							.ok()
							.flatten();
							let Some(size) = size else {
								continue; // display not enumerable (empty list / disconnected) — skip
							};
							match baseline {
								// First observation, or the streamed monitor changed: (re-)baseline,
								// don't fire (a monitor switch already restarts capture on its own).
								None => baseline = Some((idx, size)),
								Some((bidx, _)) if bidx != idx => baseline = Some((idx, size)),
								Some((_, bsize)) if bsize != size => {
									// Confirm the new size is STABLE across one more poll before acting,
									// so we don't restart mid-transition (some drivers report an
									// intermediate mode while applying a change).
									tokio::time::sleep(std::time::Duration::from_millis(700)).await;
									let confirm = tokio::task::spawn_blocking(move || {
										crate::process::host_displays()
											.into_iter()
											.find(|d| d.idx == idx)
											.map(|d| (d.width, d.height))
									})
									.await
									.ok()
									.flatten();
									if confirm == Some(size) {
										// Re-read the freshest StreamReq AFTER the confirm sleep so
										// we don't clobber a client request that arrived during those
										// 700 ms (e.g. monitor switch, codec change, resolution pick).
										let fresh_req =
											last_req_store.lock().unwrap().clone();
										let Some(fresh_req) = fresh_req else {
											// Session was torn down during the sleep — nothing to do.
											continue;
										};
										// If the client switched to a different display during the
										// confirm window, skip this watcher-triggered restream
										// entirely: the client's own StartStream already restarted
										// capture on the new monitor; firing again with the old idx
										// would override that switch.
										if fresh_req.display_idx != idx {
											// The client switched to a different display during the
											// confirm window. Clear the baseline so the next poll
											// does a clean first-observation re-baseline for the new
											// monitor (B). Using `size` here would be wrong: `size`
											// was measured for the OLD monitor A, not B, so
											// `(B_idx, A_size)` would cause a spurious bsize!=size
											// mismatch on the very next poll and trigger a needless
											// capture restart.
											baseline = None;
											continue;
										}
										tracing::info!(
											display_idx = idx,
											w = size.0,
											h = size.1,
											"host display mode changed -> restarting capture"
										);
										// Channel full (a prior restart still in flight) → skip
										// this send but keep looping; next poll re-detects delta.
										// Closed → session torn down; exit the watcher.
										match restream_tx.try_send(fresh_req) {
											Ok(_) => {
												baseline = Some((idx, size));
											}
											Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
												// Don't update baseline — next poll must still see the delta.
											}
											Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
												break;
											}
										}
									}
								}
								_ => {}
							}
						}
					})
				};
				// The monitor (`display_idx`) the current stream is capturing, so the input path
				// can map an absolute (webview) pointer onto the streamed screen instead of always
				// the primary. `on_stream` publishes the selected idx here; `on_input` reads it.
				// Windows-only (the native multi-monitor absolute-pointer mapping).
				#[cfg(windows)]
				let cur_display: Arc<std::sync::atomic::AtomicU32> =
					Arc::new(std::sync::atomic::AtomicU32::new(0));
				// On the native (DXGI+NVENC) path the input closure reads the capture thread's OWN
				// current_output Arc instead of cur_display. cur_display is set synchronously at
				// switch-request time (before the thread rebuilds), so it reflects the OLD monitor
				// for the entire rebuild window — causing absolute pointer events to land on the
				// wrong screen after a one-shot monitor switch (C4). The capture thread writes its
				// current_output atom only AFTER each successful build (including reverts), so
				// reading from it gives the monitor the thread is ACTUALLY streaming, never the
				// optimistically-requested one. native_out_arc is Some while a CaptureHandle is
				// active and None otherwise (ffmpeg path / session teardown). Windows-only.
				#[cfg(windows)]
				let native_out_arc: Arc<Mutex<Option<Arc<std::sync::atomic::AtomicU32>>>> =
					Arc::new(Mutex::new(None));
				// Monotonic build-generation counter from the CaptureHandle (C8): the capture
				// thread bumps this after EVERY successful build, including same-index resolution-
				// change rebuilds. The input closure re-resolves display_rect()/set_monitor()
				// whenever this advances — catching host resolution changes that leave the monitor
				// index unchanged but shift the virtual-desktop geometry. Windows-only.
				#[cfg(windows)]
				let native_gen_arc: Arc<Mutex<Option<Arc<std::sync::atomic::AtomicU32>>>> =
					Arc::new(Mutex::new(None));

				// Per-window (WGC) capture target discovered when THIS session launched a game in
				// game mode (Phase 2b co-op): `on_launch` spawns a resolver that polls the launched
				// app's PID for its top-level window and stores the HWND (as i64) here; `make_on_stream`
				// reads it and, when set, builds a per-window WGC CaptureConfig instead of duplicating
				// the whole monitor — so two same-host panes can each launch+capture their OWN game on
				// one monitor. `None` until a launch resolves a window (or the launch had none, e.g.
				// the Desktop entry), in which case the stream takes the normal display path.
				let launched_hwnd: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
				// The HWND the CURRENT stream is ACTUALLY per-window (WGC) capturing, published by
				// `make_on_stream` (Bug 0). `launched_hwnd` only records the resolver's discovered
				// window; the stream may still fall back to WHOLE-MONITOR capture (no NVENC / HDR /
				// 4:4:4 / PULSAR_FFMPEG_CAPTURE / a capture-start error), in which case the video
				// shows the monitor, not the window. `on_input` must gate its per-window PostMessage
				// routing on THIS (the confirmed capture target) — NOT on `launched_hwnd` — so kb/
				// mouse never diverts into a window the client can't even see. Set to the target only
				// when the native WGC path started with that window; cleared to `None` on every other
				// path. Windows-only (WGC + WindowInput are Windows-only).
				#[cfg(windows)]
				let win_capture_hwnd: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
				let provider = {
					let games = games.clone();
					let ffmpeg = crate::process::ffmpeg_bin(&app_h);
					move || {
						games
							.lock()
							.unwrap()
							.iter()
							.map(|h| {
								// The built-in Desktop card shows a FRESH live thumbnail of the
								// host's screen ("what you'd stream now"); every other game uses
								// its configured cover image (if any).
								let image = if h.id == "desktop" {
									crate::process::desktop_thumb_data_url(&ffmpeg)
										.unwrap_or_default()
								} else {
									h.image.clone()
								};
								GameInfo {
									id: h.id.clone(),
									title: h.title.clone(),
									kind: h.kind.clone(),
									image,
								}
							})
							.collect::<Vec<_>>()
					}
				};
				let on_launch = {
					let games = games.clone();
					let app_h = app_h.clone();
					let peer = peer.clone();
					let launched_hwnd = launched_hwnd.clone();
					move |id: String| {
						// Match by id first, then tolerantly by title (case-insensitive) so a
						// CLI `--app <name>` works. An unmatched app (incl. "Desktop"/"Masaüstü")
						// launches nothing — the host still streams the whole desktop.
						let found = {
							let g = games.lock().unwrap();
							g.iter()
								.find(|h| h.id == id)
								.or_else(|| {
									g.iter().find(|h| h.title.eq_ignore_ascii_case(id.trim()))
								})
								.cloned()
						};
						if let Some(g) = found {
							let _ = app_h.emit(
								"session",
								SessionEvent {
									kind: "launch".into(),
									peer: peer.clone(),
									sid,
									detail: g.title.clone(),
								},
							);
							// Launch the app and capture its PID. In GAME mode, resolve its top-level
							// window off-thread (games create windows async / launchers re-parent) and
							// stash the HWND so this session's next StartStream WGC-captures that window
							// instead of the whole monitor — per-app co-op capture (Phase 2b). The
							// resolver is best-effort + bounded (~10 s); on no-window it leaves the slot
							// None and the stream falls back to the display path. Reset the slot before
							// each launch so a re-launch (or a launch after a prior game closed) can't
							// capture a stale window. The Desktop entry launches nothing (pid None) →
							// slot stays None → whole-desktop capture, unchanged.
							*launched_hwnd.lock().unwrap() = None;
							if let Some(pid) = launch_host_game(&g) {
								let slot = launched_hwnd.clone();
								std::thread::spawn(move || {
									if let Some(hwnd) = crate::process::resolve_launched_window(pid) {
										*slot.lock().unwrap() = Some(hwnd);
									}
								});
							}
						}
					}
				};
				let on_stream = make_on_stream(
					stream_cfg.clone(),
					procs.clone(),
					active.clone(),
					incoming.clone(),
					host_out.clone(),
					stop_tx_opt,
					out_tx,
					since_ms,
					sid,
					self_name.clone(),
					#[cfg(windows)]
					native_slot.clone(),
					#[cfg(windows)]
					cur_display.clone(),
					#[cfg(windows)]
					native_out_arc.clone(),
					#[cfg(windows)]
					native_gen_arc.clone(),
					#[cfg(windows)]
					win_capture_hwnd.clone(),
					launched_hwnd.clone(),
					stats_out.clone(),
					app_h.clone(),
					peer.clone(),
					media_tx.clone(),
					nack_slot.clone(),
					fwd_slot.clone(),
					#[cfg(target_os = "linux")]
					restore_token.clone(),
					#[cfg(target_os = "linux")]
					cap_slot.clone(),
					#[cfg(target_os = "linux")]
					cap_gen.clone(),
					last_req_store.clone(),
					adapt_state.clone(),
				);
				// Route the client's input: controllers into a virtual gamepad, and
				// mouse/keyboard into a uinput desktop injector — both created lazily.
				let on_input = {
					// One virtual pad per player slot (0-based). The legacy `Gamepad` variant
						// maps to slot 0; `GamepadSlot`/`GamepadDisconnect` address slots directly.
						// Pads are created lazily on the first frame for a slot and dropped on
						// disconnect so the host releases the emulated device (ViGEm/uinput).
						let mut pads: std::collections::HashMap<u8, (ResolvedTarget, Box<dyn VirtualGamepad>)> =
							std::collections::HashMap::new();
					let mut desktop: Option<pulsar_core::input::DesktopInput> = None;
					let mut tried = false;
					// PHASE 3B (same-host co-op): per-WINDOW input target. When THIS session captures
					// a specific window (a launched game / picked app — Phase 2b stashed its HWND in
					// `launched_hwnd`), its kb/mouse is delivered to THAT window's message pump via
					// `WindowInput` (PostMessage) instead of the OS-global `SendInput` desktop cursor —
					// so two co-op panes on one host don't fight over the single system focus. The HWND
					// resolves asynchronously after launch, so we (re)build the injector lazily when the
					// stashed target changes. `None` HWND (display capture / Desktop entry / unresolved)
					// keeps the global `DesktopInput` path, unchanged. HONEST CEILING: PostMessage reaches
					// Win32 message-pump apps only, NOT DirectInput/RawInput/GetAsyncKeyState games — see
					// `pulsar_core::input::WindowInput` docs; those need OS multiseat (out of scope).
					// Gate per-window routing on the CONFIRMED capture target (Bug 0): the HWND the
					// current stream is actually WGC-capturing, published by make_on_stream. Reading
					// `launched_hwnd` directly here routed kb/mouse into the window even when the stream
					// fell back to whole-monitor capture (non-NVENC host / HDR / 4:4:4 / capture error) —
					// clicks remapped onto an invisible window while the video showed the monitor.
					#[cfg(windows)]
					let win_capture_hwnd_in = win_capture_hwnd.clone();
					#[cfg(windows)]
					let mut window_input: Option<pulsar_core::input::WindowInput> = None;
					// The HWND currently bound to `window_input` (so we only rebuild when it changes).
					#[cfg(windows)]
					let mut bound_hwnd: Option<i64> = None;
					// Which monitor's geometry is currently applied to the desktop injector, so we
					// only re-resolve the virtual-desktop rect when the streamed monitor changes.
					// `u32::MAX` = "none applied yet" (forces the first resolve).
					#[cfg(windows)]
					let cur_display = cur_display.clone();
					#[cfg(windows)]
					let native_out_arc = native_out_arc.clone();
					#[cfg(windows)]
					let native_gen_arc = native_gen_arc.clone();
					#[cfg(windows)]
					let mut applied_display: u32 = u32::MAX;
					// Last build-generation we resolved geometry for. Starts at u32::MAX so
					// the first event always resolves. On a same-index resolution change the
					// capture thread bumps the generation without changing the index — this
					// sentinel ensures we detect that and re-call display_rect/set_monitor (C8).
					#[cfg(windows)]
					let mut applied_gen: u32 = u32::MAX;
					// "Sadece izleme" gate: read per-event (cheap map lookup) so the
					// Connections-window toggle takes effect mid-session, sid-guarded
					// against a same-peer reconnection's newer entry.
					let view_active = active.clone();
					// Tracks the view-only state we last saw so we can detect the
					// FALSE->TRUE edge and flush any input held at the instant control was
					// revoked: the gate below then drops the matching key-up/button-up, so
					// without this a held Shift/Ctrl or mouse button (drag-select) stays
					// stuck on the host until teardown.
					let mut was_view_only = false;
					// Input is injected WITHOUT any pointer rotation. The host video is always presented
					// UPRIGHT (the native capture bakes the display rotation into the frame, or a rotated
					// ffmpeg stream is un-rotated by the client), and Windows SendInput addresses the same
					// logical desktop coordinate space the upright video shows — coords inject as-is.
					// (Rotating here would DOUBLE-correct vs the baked-upright video → 180°-mirrored clicks.)
					move |ev: InputEvent| {
						// View-only: drop EVERY input event for this session (gamepad too)
						// while the host user has control revoked.
						let view_only = view_active
							.lock()
							.unwrap()
							.get(&sid)
							.map(|ci| ci.view_only)
							.unwrap_or(false);
						if view_only {
							// On the FALSE->TRUE edge, release whatever was held at the instant
							// control was revoked (every later up-event is about to be dropped):
							// keys/buttons on the desktop injector and the virtual gamepad to neutral.
							if !was_view_only {
								was_view_only = true;
								if let Some(d) = desktop.as_mut() {
									d.flush_held();
								}
								// Release anything held on the per-window injector too (Phase 3B), so a
								// window-captured session can't latch a key/drag in the target window
								// when control is revoked mid-press.
								#[cfg(windows)]
								if let Some(w) = window_input.as_mut() {
									w.flush_held();
								}
								for (_, p) in pads.values_mut() {
									p.apply(&pulsar_core::input::GamepadState::default());
								}
							}
							return;
						}
						was_view_only = false;
						// Maximum virtual gamepads per session: matches the client's play.rs
						// append_idx.min(3) ceiling so an authenticated peer cannot create more
						// than 4 virtual gamepads or storm plug/unplug by cycling the target field.
						const MAX_PADS: u8 = 4;
						match ev {
							// Legacy single-pad variant → Player 1 (slot 0), Xbox emulation.
							InputEvent::Gamepad(state) => {
								pads.entry(0)
									.or_insert_with(|| (ResolvedTarget::Xbox360, create_virtual_pad_target(GamepadKind::Xbox, EmulationTarget::Auto)))
									.1.apply(&state);
							}
							// Slot-tagged controller (multi-pad). The `target` field carries the
							// client's chosen emulation target (Auto/Xbox360/Ds4). create_virtual_pad_target
							// now honors the resolved (kind, target) — DS4 target gives a DS4 backend,
							// Xbox360 (or Auto+Xbox) gives Xbox360. The pad is recreated only when the
							// resolved target changes, so ViGEm/uinput replug is bounded and rare.
							InputEvent::GamepadSlot { slot, kind, target, state } => {
								if slot >= MAX_PADS {
									return;
								}
								let want = target.resolve(kind);
								let need_create = match pads.get(&slot) {
									Some((have, _)) => *have != want,
									None => true,
								};
								if need_create {
									if pads.len() >= MAX_PADS as usize && !pads.contains_key(&slot) {
										// At the cap: refuse to create a BRAND-NEW slot's
										// virtual device. An in-place recreate of an EXISTING
										// slot (its emulation target changed) does NOT grow the
										// map (insert replaces), so it must proceed — otherwise
										// that pad returns before apply() every tick and goes
										// permanently input-dead after a target switch.
										return;
									}
									let pad = create_virtual_pad_target(kind, target);
									// Forward the game's rumble back to the client's physical pad
									// (DS4 backend only; no-op for others). The thread ends when
									// this pad is dropped (its notification IOCTL aborts).
									spawn_rumble_forward(pad.as_ref(), slot, rumble_out.clone());
									pads.insert(slot, (want, pad));
								}
								pads.get_mut(&slot).unwrap().1.apply(&state);
							}
							// A client controller went away: neutralize to all-zero but keep the
							// Box alive so the emulated device stays registered on the host —
							// avoids reconnect churn when the client briefly re-enumerates pads.
							InputEvent::GamepadDisconnect { slot } => {
								if slot >= MAX_PADS {
									return;
								}
								if let Some((_, p)) = pads.get_mut(&slot) {
									p.apply(&pulsar_core::input::GamepadState::default());
								}
							}
							other => {
								// PHASE 3B: pick the per-session input target. If this session captured a
								// specific window (Phase 2b stashed its HWND in `launched_hwnd`), route the
								// kb/mouse to that window's message pump (`WindowInput`/PostMessage) instead
								// of the global desktop (`DesktopInput`/SendInput). The HWND resolves async
								// after launch; rebuild the injector when the stashed target changes, and
								// fall back to the global path if the window is gone or none is set.
								#[cfg(windows)]
								let mut routed_to_window = false;
								#[cfg(windows)]
								{
									let target = *win_capture_hwnd_in.lock().unwrap();
									if target != bound_hwnd {
										// Target changed (resolved / re-launched / cleared): rebuild. Before
										// routing kb/mouse to the window path, release everything the client is
										// holding on the GLOBAL desktop injector (SendInput): its up-strokes are
										// about to be delivered to the window instead, so without this a held
										// Shift/Ctrl or a drag button stays latched on the host desktop until
										// teardown (stuck-modifier — Bug 2). flush_held is idempotent and a
										// no-op when nothing is held. The OLD window injector (Some(a)→Some(b)/
										// →None) self-releases on Drop when window_input is reassigned below.
										if let Some(d) = desktop.as_mut() {
											d.flush_held();
										}
										bound_hwnd = target;
										window_input = match target {
											Some(h) => pulsar_core::input::WindowInput::new(h),
											None => None,
										};
									}
									// Drop a dead window's injector so a closed game falls back to global.
									if window_input.as_ref().map(|w| !w.is_alive()).unwrap_or(false) {
										window_input = None;
									}
									if let Some(w) = window_input.as_mut() {
										routed_to_window = true;
										match &other {
											InputEvent::PointerMotion { x, y } => w.pointer(*x, *y),
											InputEvent::PointerRelative { dx, dy } => {
												w.pointer_relative(*dx, *dy)
											}
											InputEvent::PointerButton { button, down } => {
												w.button(*button, *down)
											}
											InputEvent::Scroll { dx, dy } => w.scroll(*dx, *dy),
											InputEvent::Key { code, down } => w.key(*code, *down),
											InputEvent::Char(c) => w.type_char(*c),
											// Controllers never reach the desktop/window injector.
											InputEvent::Gamepad(_)
											| InputEvent::GamepadSlot { .. }
											| InputEvent::GamepadDisconnect { .. } => {
												routed_to_window = false;
											}
										}
									}
								}
								// Global desktop path: used when there's no window target (display capture)
								// OR on non-Windows (per-window injection is Windows-only). Skipped when the
								// event was already delivered to the target window above.
								#[cfg(windows)]
								if routed_to_window {
									return;
								}
								if !tried {
									tried = true;
									match pulsar_core::input::DesktopInput::new() {
										Ok(d) => desktop = Some(d),
										Err(e) => tracing::warn!("desktop input unavailable: {e}"),
									}
								}
								if let Some(d) = desktop.as_mut() {
									match other {
										InputEvent::PointerMotion { x, y } => {
											// Absolute (webview) pointer: map onto the streamed monitor's place in the
											// virtual desktop. Re-resolve only when the captured monitor changed — a bare
											// ABSOLUTE move would otherwise always land on the PRIMARY display.
											#[cfg(windows)]
											{
												// On the native (DXGI+NVENC) path, prefer the capture thread's own
												// current_output Arc over cur_display. cur_display is written at
												// switch-request time (before the thread rebuilds), so it reflects the OLD
												// monitor for the entire rebuild window — causing pointer events to land on
												// the wrong screen after a one-shot menu switch (C4). The thread writes
												// its current_output atom only AFTER each confirmed build (including
												// reverts), so reading it gives the monitor actually being streamed. When
												// no native handle is active (ffmpeg path), fall back to cur_display. (C4)
												let (idx, gen) = {
													let out_guard = native_out_arc.lock().unwrap();
													let gen_guard = native_gen_arc.lock().unwrap();
													let idx = match out_guard.as_ref() {
														Some(arc) => arc.load(std::sync::atomic::Ordering::Relaxed),
														None => cur_display.load(std::sync::atomic::Ordering::Relaxed),
													};
													let gen = match gen_guard.as_ref() {
														Some(arc) => arc.load(std::sync::atomic::Ordering::Relaxed),
														None => 0,
													};
													(idx, gen)
												};
												// Re-resolve the monitor geometry when the index changes (different
												// monitor selected) OR when the build generation advances (same-index
												// resolution change — the virtual-desktop layout shifts and the old
												// mon_width/virt_* are stale → offset clicks on multi-monitor — C8).
												// applied_display/applied_gen are only advanced when display_rect()
												// returns Some — a transient None (DXGI enumeration momentarily failing
												// during a TDR/hotplug/mode-switch) is retried on the next pointer event
												// instead of latching the primary-only fallback permanently (C23).
												if idx != applied_display || gen != applied_gen {
													if let Some(r) = pulsar_capture::display_rect(idx) {
														applied_display = idx;
														applied_gen = gen;
														d.set_monitor(Some(pulsar_core::input::MonitorRect {
															mon_left: r.mon_left,
															mon_top: r.mon_top,
															mon_width: r.mon_width,
															mon_height: r.mon_height,
															virt_left: r.virt_left,
															virt_top: r.virt_top,
															virt_width: r.virt_width,
															virt_height: r.virt_height,
														}));
													}
													// If display_rect returned None, we leave applied_display/applied_gen
													// unchanged so the next PointerMotion retries the resolve.
												}
											}
											d.pointer(x, y)
										}
										InputEvent::PointerRelative { dx, dy } => {
											// Resolve the captured monitor's geometry HERE too — not only on absolute
											// PointerMotion. A native-renderer / game client sends ONLY relative motion, so
											// without this `d.monitor` stays None, pointer_relative's cursor clamp is
											// skipped, and the host cursor drifts off the streamed monitor onto another one.
											// Mirrors the PointerMotion resolve above (same C4/C8/C23 reasoning).
											#[cfg(windows)]
											{
												let (idx, gen) = {
													let out_guard = native_out_arc.lock().unwrap();
													let gen_guard = native_gen_arc.lock().unwrap();
													let idx = match out_guard.as_ref() {
														Some(arc) => arc.load(std::sync::atomic::Ordering::Relaxed),
														None => cur_display.load(std::sync::atomic::Ordering::Relaxed),
													};
													let gen = match gen_guard.as_ref() {
														Some(arc) => arc.load(std::sync::atomic::Ordering::Relaxed),
														None => 0,
													};
													(idx, gen)
												};
												if idx != applied_display || gen != applied_gen {
													if let Some(r) = pulsar_capture::display_rect(idx) {
														applied_display = idx;
														applied_gen = gen;
														d.set_monitor(Some(pulsar_core::input::MonitorRect {
															mon_left: r.mon_left,
															mon_top: r.mon_top,
															mon_width: r.mon_width,
															mon_height: r.mon_height,
															virt_left: r.virt_left,
															virt_top: r.virt_top,
															virt_width: r.virt_width,
															virt_height: r.virt_height,
														}));
													}
												}
											}
											let (rdx, rdy) = (dx, dy);
											d.pointer_relative(rdx, rdy)
										}
										InputEvent::PointerButton { button, down } => {
											d.button(button, down)
										}
										InputEvent::Scroll { dx, dy } => d.scroll(dx, dy),
										InputEvent::Key { code, down } => d.key(code, down),
										InputEvent::Char(c) => d.type_char(c),
										// Controller variants are routed to virtual pads in the outer
											// match and never reach the desktop injector.
											InputEvent::Gamepad(_)
											| InputEvent::GamepadSlot { .. }
											| InputEvent::GamepadDisconnect { .. } => {}
									}
								}
							}
						}
					}
				};
				// Side channels (clipboard / chat / file / mic audio) from this client.
				let on_clipboard = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					move |text: String| {
						let _ = app_h.emit(
							"clipboard",
							DataPayload {
								peer: peer.clone(),
								text,
							},
						);
					}
				};
				let on_chat = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let chat_log = tauri::Manager::state::<AppState>(&app_h).chat_log.clone();
					move |text: String| {
						// Backlog first (the connections window may be CLOSED — events
						// broadcast only to live windows), then surface the window: the
						// connections window's message modal is the host chat UI now.
						// Capped: the log lives for the (tray-resident) app's lifetime.
						{
							let mut log = chat_log.lock().unwrap();
							log.push((peer.clone(), text.clone(), false));
							let excess = log.len().saturating_sub(500);
							if excess > 0 {
								log.drain(..excess);
							}
						}
						crate::connections::open_or_update(&app_h, crate::connections::Surface::Forward);
						let _ = app_h.emit(
							"host-chat",
							DataPayload {
								peer: peer.clone(),
								text,
							},
						);
					}
				};
				let on_file = make_on_file(app_h.clone(), peer.clone(), sid);
				let on_audio = make_on_audio();
				let on_reverse = {
					let app_h = app_h.clone();
					move |id: String| {
						// The controlling peer asked us to reverse roles: surface it so the
						// host UI can connect back to `id` (it must be online/serving).
						let _ = app_h.emit("reverse-request", ReverseReq { id });
					}
				};
				// What this host can ACTUALLY stream, best-first — answers the client's
				// `QueryStreamCaps` so its "auto" codec resolves to what we will really
				// send (the client writes its decoder SDP before the stream starts).
				// Wayland captures via the GStreamer x264 path → H.264 only; otherwise
				// run the same validated encoder/codec resolution the stream start uses
				// (probes are cached, so this is cheap after the first call).
				let stream_caps = {
					let stream_cfg = stream_cfg.clone();
					let app_h = app_h.clone();
					move || {
						use pulsar_core::pipeline::{HwEncoder, VCodec};
						use pulsar_core::service::StreamCaps;
						// Startup-probed caps: derive the reply instantly when available
						// (the background probe at launch ran the SAME validation chain).
						let probed = tauri::Manager::state::<AppState>(&app_h)
							.local_caps
							.lock()
							.unwrap()
							.clone();
						if let Some(lc) = probed {
							// `capture` is a Linux-only module in pulsar-core — gate the
							// call (same pattern as the gst probe below).
							#[cfg(target_os = "linux")]
							let wayland = pulsar_core::capture::is_wayland();
							#[cfg(not(target_os = "linux"))]
							let wayland = false;
							// Wayland encodes ONLY through gst: keep gst-backed families
							// (+ software, which gst's x264 covers too).
							let usable = |e: &crate::caps::EncoderCap| {
								!wayland || e.backend == "gst" || e.id == "software"
							};
							let mut encoders: Vec<String> = lc
								.encoders
								.iter()
								.filter(|e| usable(e))
								.map(|e| e.id.clone())
								.collect();
							if encoders.is_empty() {
								encoders.push("software".to_string());
							}
							let hw_h265 = lc.encoders.iter().any(|e| {
								usable(e)
									&& e.id != "software" && e.codecs.iter().any(|c| c == "h265")
							});
							// AV1 only from a validated HARDWARE encoder (software realtime AV1
							// isn't viable on the hosts we target — same rule as hw_h265). Mirrors
							// the inline fallback so the probed and inline paths advertise the same
							// codecs; without this AV1 is never negotiated even when both ends support it.
							let hw_av1 = lc.encoders.iter().any(|e| {
								usable(e)
									&& e.id != "software" && e.codecs.iter().any(|c| c == "av1")
							});
							// Quality-descending, best-first.
							let mut codecs = Vec::new();
							if hw_av1 {
								codecs.push("av1".to_string());
							}
							if hw_h265 {
								codecs.push("h265".to_string());
							}
							codecs.push("h264".to_string());
							return StreamCaps {
								codecs,
								encoders,
								features: media_features(),
								displays: crate::process::host_displays(),
							};
						}
						// Fallback (probe still running): compute inline, same chain.
						// Validated gst families (Linux): the Wayland path encodes through gst
						// exclusively, and on X11 they cover HW encoders ffmpeg lacks (Orange Pi
						// MPP). hw_h265 = any gst HARDWARE family validated for HEVC.
						#[cfg(target_os = "linux")]
						let gst = crate::process::validated_gst_encoders();
						#[cfg(not(target_os = "linux"))]
						let gst: Vec<(pulsar_core::pipeline::gst::GstEncoder, Vec<VCodec>)> = Vec::new();
						let gst_hw_h265 = gst.iter().any(|(e, codecs)| {
							*e != pulsar_core::pipeline::gst::GstEncoder::X264
								&& codecs.contains(&VCodec::H265)
						});
						// Wayland: gst is the ONLY encode path — caps come from it alone.
						#[cfg(target_os = "linux")]
						let wayland = pulsar_core::capture::is_wayland();
						#[cfg(not(target_os = "linux"))]
						let wayland = false;
						if wayland {
							let mut encoders: Vec<String> =
								gst.iter().map(|(e, _)| e.wire_id().to_string()).collect();
							if encoders.is_empty() {
								encoders.push("software".to_string());
							}
							let codecs = if gst_hw_h265 {
								vec!["h265".to_string(), "h264".to_string()]
							} else {
								vec!["h264".to_string()]
							};
							return StreamCaps {
								codecs,
								encoders,
								features: media_features(),
								displays: crate::process::host_displays(),
							};
						}
						let cfg = stream_cfg.lock().unwrap().clone();
						let ffmpeg = crate::process::ffmpeg_bin(&app_h);
						// Encoder backends that really work here (cached one-frame probes),
						// merged with the gst HARDWARE families (same wire vocabulary, so e.g.
						// "rkmpp" appears once whether ffmpeg-rockchip or gst serves it).
						let mut encoders: Vec<String> =
							crate::process::validated_encoders(&ffmpeg, &cfg.vaapi_device)
								.into_iter()
								.map(|e| crate::process::encoder_wire_id(e).to_string())
								.collect();
						for (e, _) in gst
							.iter()
							.filter(|(e, _)| *e != pulsar_core::pipeline::gst::GstEncoder::X264)
						{
							let id = e.wire_id().to_string();
							if !encoders.contains(&id) {
								// HW families ahead of the terminal software entry.
								let pos = encoders.len().saturating_sub(1);
								encoders.insert(pos, id);
							}
						}
						// The encoder the host would pick for its configured preference — drives
						// which codecs we can promise. Software realtime HEVC isn't viable on the
						// hosts we target, so H.265 is offered only from a hardware encoder
						// (ffmpeg-validated or a gst HW family).
						let enc_text = crate::process::encoders_text(&ffmpeg);
						let encoder = pulsar_core::pipeline::resolve(
							crate::process::encoder_from_str(&cfg.encoder),
							&pulsar_core::pipeline::detect(&enc_text),
						);
						#[cfg(not(windows))]
						let encoder = crate::process::resolve_encoder_validated(
							&ffmpeg,
							encoder,
							&enc_text,
							&cfg.vaapi_device,
						);
						let ffmpeg_hw = |c: VCodec| {
							!matches!(encoder, HwEncoder::Software)
								&& crate::process::resolve_codec_validated(
									&ffmpeg,
									encoder,
									c,
									&cfg.vaapi_device,
								) == c
						};
						// Quality-descending; H.265/AV1 only from validated HW encoders.
						let mut codecs = Vec::new();
						if ffmpeg_hw(VCodec::Av1) {
							codecs.push("av1".to_string());
						}
						if ffmpeg_hw(VCodec::H265) || gst_hw_h265 {
							codecs.push("h265".to_string());
						}
						codecs.push("h264".to_string());
						tracing::info!(?codecs, ?encoders, "stream caps reply");
						StreamCaps {
							codecs,
							encoders,
							features: media_features(),
							displays: crate::process::host_displays(),
						}
					}
				};
				// The client pushed its identity image: surface it to every window — the
				// connections list renders it next to this peer's id — and remember it in
				// peer_meta so a LATER-opened connections window's snapshot still has it.
				let peer_meta = tauri::Manager::state::<AppState>(&app_h).peer_meta.clone();
				let on_avatar = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let peer_meta = peer_meta.clone();
					move |png: Vec<u8>| {
						let url = crate::avatar::data_url(&png);
						peer_meta
							.lock()
							.unwrap()
							.entry(peer.clone())
							.or_insert((None, None))
							.1 = Some(url.clone());
						let _ = app_h.emit(
							"peer-avatar",
							AvatarPayload {
								peer: peer.clone(),
								data_url: url,
							},
						);
					}
				};
				// Same for the pushed display name (DataMsg::PeerName).
				let on_peer_name = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let peer_meta = peer_meta.clone();
					move |name: String| {
						peer_meta
							.lock()
							.unwrap()
							.entry(peer.clone())
							.or_insert((None, None))
							.0 = Some(name.clone());
						let _ = app_h.emit("peer-name", (peer.clone(), name));
					}
				};
				// The client pushed its OWN device ID (DataMsg::PeerId): remember it keyed
				// by this connection's peer-key and broadcast it so every connections window
				// shows the client's id instead of its ip:port (direct/same-LAN connects).
				let on_peer_id = {
					let app_h = app_h.clone();
					let peer = peer.clone();
					let peer_ids = tauri::Manager::state::<AppState>(&app_h).peer_ids.clone();
					move |id: String| {
						peer_ids.lock().unwrap().insert(peer.clone(), id.clone());
						let _ = app_h.emit("peer-id", (peer.clone(), id));
					}
				};
				// NACK requests from the client → the active video forwarder's channel.
				let on_nack = {
					let nack_slot = nack_slot.clone();
					#[cfg(windows)]
					let native_idr = native_slot.clone();
					// ffmpeg-fallback keyframe recovery for the MediaNack([0]) sentinel below:
					// last_req_store is Some only on the ffmpeg path (native sessions clear it and
					// use request_idr above); the restart channel re-issues that req so a fresh
					// ffmpeg/capture opens on an IDR, and last_kf_restart debounces the client's
					// ~400 ms nack cadence so it can't restart-storm.
					let last_req_store = last_req_store.clone();
					let mut last_kf_restart: Option<std::time::Instant> = None;
					move |seqs: Vec<u16>| {
						// MediaNack([0]) is the client's keyframe-request sentinel (its decoder
						// was rebuilt mid-session and needs an IDR now, not at the next scheduled
						// GOP). seq 0 is ~never in the retransmit ring, so before this the sentinel
						// was a silent no-op.
						if seqs.contains(&0) {
							// Native (NVENC) path: force an immediate IDR on the live encoder.
							#[cfg(windows)]
							if let Some(h) = native_idr.lock().unwrap().as_ref() {
								h.request_idr();
							}
							// ffmpeg fallback (no live CaptureHandle): a spawned CLI ffmpeg has no
							// runtime force-keyframe hook, so re-issue the last StreamReq — a fresh
							// ffmpeg/capture opens on an IDR. Gated to DESKTOP sessions (game mode's
							// ~0.25 s GOP self-heals faster than a restart's rebuild, so a restart
							// there would only regress it) and debounced (1.5 s) so the client's
							// ~400 ms nack cadence can't restart-storm. last_req_store is populated
							// only on the ffmpeg path, so native sessions skip this branch entirely.
							if let Some(req) = last_req_store.lock().unwrap().clone() {
								let due = last_kf_restart
									.map(|t| t.elapsed() >= std::time::Duration::from_millis(1500))
									.unwrap_or(true);
								if !req.game_mode && due {
									// Full (a restart already queued / in flight) or Closed (session
									// torn down) → skip; the next nack or the natural GOP still recovers.
									if nack_restream_tx.try_send(req).is_ok() {
										last_kf_restart = Some(std::time::Instant::now());
									}
								}
							}
						}
						if let Some(tx) = nack_slot.lock().unwrap().as_ref() {
							let _ = tx.send(seqs);
						}
					}
				};
				let handlers = DataHandlers {
					outbound: Some(out_rx),
					on_clipboard: Box::new(on_clipboard),
					on_chat: Box::new(on_chat),
					on_file: Box::new(on_file),
					on_audio: Box::new(on_audio),
					on_reverse: Box::new(on_reverse),
					stream_caps: Box::new(stream_caps),
					// The host's visible top-level windows the client can pick as a per-window
					// (WGC) capture target (Phase 2b co-op) — answers the client's QueryWindows.
					// Cheap (one EnumWindows pass), called on demand when the client opens its
					// window picker. Empty on non-Windows (no per-window source) → no picker.
					windows: Box::new(|| {
						// pulsar-capture (and its WGC/EnumWindows window source) is a Windows-only
						// dependency; off Windows there is no per-window source, so reply empty.
						#[cfg(windows)]
						{
							pulsar_capture::list_capture_windows()
								.into_iter()
								.map(|(hwnd, title)| pulsar_core::service::WindowInfo {
									hwnd: hwnd as i64,
									title,
								})
								.collect()
						}
						#[cfg(not(windows))]
						{
							Vec::<pulsar_core::service::WindowInfo>::new()
						}
					}),
					on_nack: Box::new(on_nack),
					on_avatar: Box::new(on_avatar),
					on_peer_name: Box::new(on_peer_name),
					on_peer_id: Box::new(on_peer_id),
					// File manager: FsList/FsGet from this client, answered through the
					// same outbound queue (HOME-jailed; see fs_browse).
					on_fs: Box::new(crate::fs_browse::make_on_fs(fs_out)),
					// Host-initiated re-stream: fed by the display-mode watcher above.
					// On Windows this fires only for ffmpeg-path sessions (native DXGI self-heals
					// via ACCESS_LOST; last_req_store is only populated when native_started=false).
					restream: Some(restream_rx),
				};
				// Guard that runs critical cleanup even if this task is cancelled via
				// `JoinHandle::abort()` (e.g. by `go_online` on a reconnect). Abort
				// skips the post-`tokio::select!` block below, so we duplicate the
				// essential teardown inside `Drop` as a safety net. The normal path still
				// runs the block below; every operation is idempotent so double-execution
				// is safe (procs drain finds an empty vec, cap_slot.take() returns None,
				// release_redirect is a no-op when already removed, and the map removals
				// are sid-guarded so they silently no-op when already cleared).
				let _cleanup_guard = SessionCleanupGuard {
					procs: procs.clone(),
					fwd_slot: fwd_slot.clone(),
					#[cfg(target_os = "linux")]
					cap_slot: cap_slot.clone(),
					#[cfg(target_os = "linux")]
					cap_gen: cap_gen.clone(),
					mode_watcher,
					sid,
					incoming: incoming.clone(),
					host_out: host_out.clone(),
					active: active.clone(),
					peer_meta: peer_meta.clone(),
					peer: peer.clone(),
					// We registered a pre-auth entry iff this is a password-gated host (the
					// add() above had the same condition) — drop it on the guard's teardown.
					preauth_pubkey: if require_auth { peer_pubkey } else { None },
					// Same Arc the on_stream handler applies adaptation into — Drop reverts it.
					adapt_state: adapt_state.clone(),
					app_handle: app_h.clone(),
				};
				tokio::select! {
					_ = serve_with(session, provider, on_launch, on_stream, on_input, handlers) => {}
					// Host kicked this client from the UI (disconnect_peer / _all / go-offline
					// all fire stop_tx). serve_with — and thus the live Session — is still
					// alive inside THIS branch (select! only drops the other future once the
					// block finishes), so send an explicit Bye NOW over the cloned sender. Over
					// a relay the client's recv never returns None when we vanish, so without
					// this it hangs on the frozen last frame until its silence watchdog; the Bye
					// ends it at once. A natural serve_with end (the other arm) = the client
					// already left, so no Bye is sent there.
					_ = &mut stop_rx => {
						let _ = send_bye_via(&media_tx).await;
					}
				}
				// Session ended (peer gone or host kicked): kill this session's ffmpeg
				// so capture/encode stops at once and the GPU is freed. Held mouse
				// buttons / modifier keys are released by DesktopInput's Drop (the
				// on_input closure is dropped when serve_with's future ends above).
				// (SessionCleanupGuard above also covers these for the abort() path.)
				for mut child in procs.lock().unwrap().drain(..) {
					let _ = child.kill();
					let _ = child.wait();
				}
				// Stop the media-over-session forwarder tasks (their session is gone).
				for h in fwd_slot.lock().unwrap().drain(..) {
					h.abort();
				}
				// The host display-mode watcher is aborted by SessionCleanupGuard::drop
				// (both on the normal path here and on the abort() path from go_online).
				// Stop the native capture thread (releases the NVENC session + DXGI duplication).
				#[cfg(windows)]
				if let Some(h) = native_slot.lock().unwrap().take() {
					h.stop();
				}
				// Drop this session's Sunshine-style sink redirect request so the default
				// render endpoint is restored when this was the last owner (a same-peer
				// reconnect keeps it redirected through the OLD session's delayed teardown).
				handlers::release_redirect(sid);
				// Compare-and-remove: only drop the entries if they still belong to THIS
				// session. A same-peer reconnection may have already overwritten them with
				// its own (newer) sid; removing unconditionally would kill the live one.
				// Keyed by sid now: this entry is uniquely ours, so a plain remove() is
				// correct — a same-DEVICE co-op session has its OWN sid key and is untouched.
				incoming.lock().unwrap().remove(&sid);
				host_out.lock().unwrap().remove(&sid);
				tracing::info!(%peer, "session disconnected");
				// Drop this session's row from the connections window's list; close the
				// window once the LAST connection ends. peer_still_live = any OTHER active
				// session sharing this device (couch co-op), used to refcount the per-device
				// identity cache so a co-op sibling's teardown keeps our name/avatar.
				let (removed, peer_still_live, conns_emptied) = {
					let mut g = active.lock().unwrap();
					let removed = g.remove(&sid).is_some();
					let peer_still_live = g.values().any(|ci| ci.peer == peer);
					(removed, peer_still_live, g.is_empty())
				};
				if removed && !peer_still_live {
					// Last session for this device left — release its cached ~50-70 KB
					// avatar data-URL + name; a reconnect re-pushes them.
					peer_meta.lock().unwrap().remove(&peer);
				}
				if conns_emptied {
					crate::connections::close(&app_h);
				}
				// Stop this session's screen capture — closes the portal session so
				// KDE/GNOME stops showing "screen is being shared".
				#[cfg(target_os = "linux")]
				{
					// Bump FIRST: an in-flight capture::start (portal dialog can take
					// seconds) then sees the stale generation and stops its fresh
					// capture instead of storing it into this dead session.
					cap_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
					let cap = cap_slot.lock().unwrap().take();
					if let Some(cap) = cap {
						cap.stop().await;
					}
				}
				// Tell the UI "disconnected" (carrying this session's sid) when we actually
				// held the slot. The Connections window keys rows by sid, so this drops only
				// THIS session's row — a same-device co-op session's row survives.
				if removed {
					let _ = app_h.emit(
						"session",
						SessionEvent {
							kind: "disconnected".into(),
							peer,
							sid,
							detail: String::new(),
						},
					);
				}
			});
			// Track this session task so the next go_online can abort it (releasing the
			// strong Arc<Node> it holds). Prune already-finished handles first so the
			// list cannot grow without bound across many short-lived connections.
			{
				let mut tasks = session_tasks.lock().unwrap();
				tasks.retain(|h| !h.is_finished());
				tasks.push(session_handle);
			}
		}
	});

	// Node + serve loop go live BEFORE registering: LAN discovery already
	// announces this host (started pre-register, by design), so direct LAN
	// connects must find a consumer behind `next_incoming()` even when the
	// relay is unreachable — otherwise the documented offline-LAN flow hangs
	// every connecting client at auth.
	*state.node.lock().unwrap() = Some(node.clone());
	*state.serve_task.lock().unwrap() = Some(serve_handle);

	// Register with the relay. If it's unreachable we stay "offline" for the UI
	// (the Err) but keep the node + serve loop + LAN discovery running so
	// same-network devices still appear AND can connect.
	let id = match node.register().await {
		Ok(id) => id,
		Err(e) => {
			tracing::info!(error = %e, "relay unreachable — staying offline, LAN discovery + serving still active");
			return Err(e.to_string());
		}
	};
	tracing::info!(%id, "go_online: registered with relay");
	// Now that we have a relay id, advertise it on the LAN too.
	if let Some(d) = &discovery {
		d.set_id(Some(id)).await;
	}

	// Watch for an ID ROTATION: a full relay restart loses its pubkey→id map and
	// re-registration mints a DIFFERENT 9-digit id. Without this the Home screen
	// and the LAN beacon keep advertising the dead old id forever (connects fail
	// with TargetOffline) until the user toggles offline/online. Re-advertise the
	// new id to both. Holds a Weak<Node> + the id-change signal handle so it does
	// NOT pin the node alive: it exits when go_online tears the node down (the next
	// call's `state.node.take()` drops the last strong ref → upgrade() fails).
	{
		let id_signal = node.id_changed_handle();
		let weak = std::sync::Arc::downgrade(&node);
		let watch_app = app.clone();
		let watch_disc = discovery.clone();
		tokio::spawn(async move {
			loop {
				id_signal.notified().await;
				let Some(n) = weak.upgrade() else { return };
				let new_id = n.self_id().await;
				drop(n);
				let Some(new_id) = new_id else { continue };
				tracing::warn!(id = %new_id, "relay reissued a new device ID — re-advertising");
				if let Some(d) = &watch_disc {
					d.set_id(Some(new_id)).await;
				}
				let _ = watch_app.emit("node-id", new_id.grouped());
			}
		});
	}

	// Watch for a post-registration INCOMPATIBLE VERSION: the relay was redeployed
	// with a newer protocol version while this node was already online. The node is
	// stranded (every heartbeat and re-register attempt will be refused). Surface
	// "update required" to the UI and go offline cleanly so the user sees the error
	// instead of silently advertising a dead id. Same Weak<Node> pattern: exits when
	// go_online tears the node down (next call's `state.node.take()` drops it).
	{
		let ver_signal = node.version_error_handle();
		let weak = std::sync::Arc::downgrade(&node);
		let watch_app = app.clone();
		let watch_disc = discovery.clone();
		tokio::spawn(async move {
			ver_signal.notified().await;
			// If the node is already gone (normal go_online teardown), nothing to do.
			if weak.upgrade().is_none() {
				return;
			}
			tracing::warn!("relay rejected re-registration: incompatible protocol version — going offline");
			// Clear the relay id from the LAN beacon so it stops advertising a dead id.
			if let Some(d) = &watch_disc {
				d.set_id(None).await;
			}
			// Perform the same deterministic teardown that go_online does at its top so
			// the serve task, all per-session tasks, and the node (with its UDP socket +
			// heartbeat) are actually released — not just the AppState strong reference.
			// A bare node.take() only drops ONE Arc while serve_handle and every live
			// session task each hold their own strong clone; those keep the heartbeat
			// pinging an incompatible relay and the UDP socket bound indefinitely.
			let state = watch_app.state::<AppState>();
			let mut teardown_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
			if let Some(h) = state.serve_task.lock().unwrap().take() {
				h.abort();
				teardown_handles.push(h);
			}
			for h in state.session_tasks.lock().unwrap().drain(..) {
				h.abort();
				teardown_handles.push(h);
			}
			for h in teardown_handles {
				let _ = h.await;
			}
			// Second drain — close the teardown-accept race: after serve_task has been
			// awaited its last synchronous accept→spawn may have pushed a new handle.
			let late_handles: Vec<_> = state.session_tasks.lock().unwrap().drain(..).collect();
			for h in late_handles {
				h.abort();
				let _ = h.await;
			}
			let _ = state.node.lock().unwrap().take();
			// Emit the version-error event — the UI sets online=false and shows the
			// "update required" message (same text as the initial-register path).
			let _ = watch_app.emit("node-version-error", ());
		});
	}

	Ok(id.grouped())
}
