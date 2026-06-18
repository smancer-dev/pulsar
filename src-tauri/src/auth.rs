//! Connection authorization: the host's Allow/Deny popup, the client's password
//! prompt, the race between them, and the small Tauri commands that resolve them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pulsar_core::service::{
	need_password, recv_client_auth, recv_host_auth, send_auth, ClientAuth, HostAuth,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::events::{AuthPrompt, SessionEvent};
use crate::state::AppState;

/// Constant-time secret equality for the connect/one-time passwords.
///
/// A remote peer can flood `Auth(password)` guesses, so the secret check must not
/// leak the shared-prefix length through reject latency the way `str`/`String`
/// `==` does (it short-circuits on the first differing byte). We hash both the
/// candidate and the stored secret to fixed-length SHA-256 digests and compare
/// those digests in constant time — digesting also hides the secret's length
/// (a raw byte-slice compare would short-circuit on a length mismatch first).
pub(crate) fn secret_eq(a: &str, b: &str) -> bool {
	use sha2::{Digest, Sha256};
	use subtle::ConstantTimeEq;
	let da = Sha256::digest(a.as_bytes());
	let db = Sha256::digest(b.as_bytes());
	da.ct_eq(&db).into()
}

/// Per-peer auth throttle (brute-force / prompt-spam protection). Both passwords
/// (the rotating one-time pw and the persistent connect password) and the
/// Allow/Deny popup are remotely triggerable, so a hostile peer could spam
/// attempts or popups. After [`MAX_FAILURES`] wrong passwords the peer is locked
/// out for [`LOCKOUT`]: further sessions are rejected *without* opening a popup.
/// Keyed by the peer label (relay id or socket addr) — process-lifetime map, tiny.
pub(crate) mod throttle {
	use std::collections::HashMap;
	use std::sync::{LazyLock, Mutex};
	use std::time::{Duration, Instant};

	const MAX_FAILURES: u32 = 5;
	const LOCKOUT: Duration = Duration::from_secs(300);
	/// Failure counts reset if the last failure is older than this (a genuinely
	/// forgetful user shouldn't accumulate into a lockout across hours).
	const WINDOW: Duration = Duration::from_secs(600);

	/// Total wrong guesses across ALL peers before the host force-rotates the
	/// one-time password. The per-peer [`MAX_FAILURES`] lockout is keyed by the
	/// peer label, which for a relay connect is the attacker-chosen 9-digit id —
	/// an attacker who registers many ids (relay registration is unauthenticated)
	/// gets a fresh per-peer bucket each time, defeating that lockout. This global
	/// counter is source-independent: after this many wrong guesses (over the
	/// window, against a static OTP) the credential being brute-forced is rotated
	/// out from under the attacker regardless of which ids they cycle through.
	const GLOBAL_MAX_FAILURES: u32 = 20;

	/// Minimum interval between attacker-triggered OTP rotations.
	///
	/// An id-cycling attacker can hit the global threshold in as little as 20
	/// lightweight relay connections (each contributing one wrong guess via a
	/// fresh peer bucket). Without a cooldown they can rotate the displayed OTP
	/// on every batch of 20 — churning the code faster than a legitimate user
	/// can read and type it (denial-of-access, C9).
	///
	/// With this gate an attacker-triggered rotation is suppressed if one already
	/// happened within the last `ROTATION_COOLDOWN` seconds: the counter is reset
	/// (the batch of wrong guesses is consumed), but the displayed code is NOT
	/// changed again. The legitimate user therefore has at least this many seconds
	/// to complete a connection with a code they already read off the screen.
	///
	/// The single-use rotation that happens after a CORRECT OTP auth (called
	/// directly via `rotate_session_password`, not through this path) is NOT
	/// rate-limited — it always fires so the same code can never unlock a second
	/// session.
	const ROTATION_COOLDOWN: Duration = Duration::from_secs(60);

	static ATTEMPTS: LazyLock<Mutex<HashMap<String, (u32, Instant)>>> =
		LazyLock::new(|| Mutex::new(HashMap::new()));

	/// Source-independent wrong-guess counter: (count, first-failure instant, last-rotation instant).
	///
	/// The third field tracks when we last performed an attacker-triggered rotation so
	/// we can enforce [`ROTATION_COOLDOWN`] (C9 fix).
	static GLOBAL_FAILURES: LazyLock<Mutex<(u32, Instant, Option<Instant>)>> =
		LazyLock::new(|| Mutex::new((0, Instant::now(), None)));

	/// Throttle key: a relay id passes through, but a direct peer's label is
	/// "ip:port" with a NEW EPHEMERAL PORT per connection — keyed verbatim, an
	/// attacker reconnecting per guess would never accumulate failures. Strip
	/// the port so the counter sticks to the address.
	///
	/// Parse with `SocketAddr` so the port is stripped ONLY for an unambiguous
	/// host:port — this handles IPv4 ("1.2.3.4:9000" → "1.2.3.4") AND bracketed
	/// IPv6 ("[fe80::1]:9000" → "fe80::1"). The old naive `rsplit_once(':')`
	/// mangled BARE IPv6 ("fe80::1:9000" → "fe80::1") differently each reconnect
	/// (the ephemeral port looks like just another hextet), so an IPv6 attacker
	/// never accumulated failures → no lockout. Anything that isn't a real
	/// socket address (a bare hostname, a relay id) is keyed verbatim.
	fn key(peer: &str) -> String {
		if let Ok(sa) = peer.parse::<std::net::SocketAddr>() {
			return sa.ip().to_string();
		}
		peer.to_string()
	}

	/// Remaining lockout, if the peer is currently locked out.
	pub(crate) fn locked_out(peer: &str) -> Option<Duration> {
		let peer = key(peer);
		let g = ATTEMPTS.lock().unwrap();
		let (n, at) = g.get(&peer)?;
		if *n >= MAX_FAILURES {
			let end = *at + LOCKOUT;
			let now = Instant::now();
			if now < end {
				return Some(end - now);
			}
		}
		None
	}

	/// Record a wrong-password attempt; returns true when the peer just crossed
	/// into lockout (callers should stop prompting and deny).
	pub(crate) fn record_failure(peer: &str) -> bool {
		let peer = key(peer);
		let mut g = ATTEMPTS.lock().unwrap();
		let e = g.entry(peer).or_insert((0, Instant::now()));
		// Reset the counter if the failure window has expired OR if a previous
		// lockout has fully elapsed.  Without the second condition a user who
		// retries exactly once after the 300 s lockout expires — while still
		// within the 600 s WINDOW — is instantly re-locked on that single wrong
		// guess because the count is still >= MAX_FAILURES (C15 fix).
		let lockout_served = e.0 >= MAX_FAILURES && e.1.elapsed() >= LOCKOUT;
		if e.1.elapsed() > WINDOW || lockout_served {
			*e = (0, Instant::now());
		}
		e.0 += 1;
		e.1 = Instant::now();
		if e.0 >= MAX_FAILURES {
			tracing::warn!(failures = e.0, "auth throttle: peer locked out");
			true
		} else {
			false
		}
	}

	/// Count a wrong guess against the source-independent global counter; returns
	/// true exactly when the global threshold is crossed AND enough time has
	/// elapsed since the last attacker-triggered rotation, meaning the host should
	/// force-rotate the one-time password (and resets the counter so the next
	/// rotation needs a fresh batch of failures).
	///
	/// This defeats the relay-id rotation that sidesteps the per-peer
	/// [`record_failure`] lockout: no matter how many ids an attacker cycles, the
	/// OTP they are guessing changes after [`GLOBAL_MAX_FAILURES`] total wrong
	/// attempts.
	///
	/// The [`ROTATION_COOLDOWN`] gate (C9 fix) prevents an id-cycling attacker
	/// from churning the displayed OTP faster than a legitimate user can act: if
	/// the threshold is crossed but a rotation already happened within the cooldown
	/// window, the counter is reset (the bad-guess batch is consumed) but `false`
	/// is returned — the code the user already read stays valid until the cooldown
	/// expires.
	pub(crate) fn note_global_failure() -> bool {
		let mut g = GLOBAL_FAILURES.lock().unwrap();
		if g.1.elapsed() > WINDOW {
			*g = (0, Instant::now(), g.2);
		}
		g.0 += 1;
		if g.0 >= GLOBAL_MAX_FAILURES {
			// Consume the batch unconditionally (reset count) so the attacker
			// doesn't "save up" a rotation that fires the moment the cooldown
			// expires.
			g.0 = 0;
			g.1 = Instant::now();
			// Enforce the cooldown: only signal rotation if we haven't done one
			// recently. This is the C9 availability fix.
			let cooldown_elapsed = g.2.map_or(true, |t| t.elapsed() >= ROTATION_COOLDOWN);
			if cooldown_elapsed {
				tracing::warn!("auth throttle: global wrong-guess limit — rotating one-time password");
				g.2 = Some(Instant::now());
				true
			} else {
				tracing::warn!(
					"auth throttle: global wrong-guess limit reached but rotation suppressed \
					 (cooldown active — C9 protection); attacker batch consumed, OTP unchanged"
				);
				false
			}
		} else {
			false
		}
	}

	/// Successful auth clears the per-peer slate.
	///
	/// NOTE: we intentionally do NOT reset [`GLOBAL_FAILURES`] here. That counter
	/// is source-independent (it accumulates wrong guesses from ANY peer) and exists
	/// to cap sustained brute-forcing by an attacker who cycles relay ids to defeat
	/// the per-peer lockout. Resetting it on every successful auth — including a
	/// passwordless operator Allow or a connect-password login — would let an attacker
	/// interleave their id-cycling batches with normal legitimate traffic and keep
	/// the counter at zero indefinitely, completely disarming the OTP-rotation guard.
	/// The only intended reset is the 600 s WINDOW elapsed check inside
	/// [`note_global_failure`]: after that quiet period the counter expires naturally.
	pub(crate) fn clear(peer: &str) {
		ATTEMPTS.lock().unwrap().remove(&key(peer));
	}

	/// Reset the global failure counter to a clean state (count=0, no prior rotation).
	/// Only available in test builds to ensure test isolation (the static is process-global).
	#[cfg(test)]
	pub(crate) fn reset_global_for_test() {
		*GLOBAL_FAILURES.lock().unwrap() = (0, Instant::now(), None);
	}
}

/// Maximum number of Allow/Deny popups that may be open at the same time.
///
/// An id-cycling attacker who sends a fresh connection on each of N relay ids
/// would otherwise open N always-on-top, focused, Critical-attention windows —
/// a desktop-level denial of service. This cap limits the damage: once this
/// many popups are open, new auth races are denied immediately without opening
/// another window.
const MAX_CONCURRENT_POPUPS: usize = 3;

/// Count of currently-open Allow/Deny popup windows.
static OPEN_POPUP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that holds one slot in [`OPEN_POPUP_COUNT`] and releases it — plus
/// closes the popup window if it is still open — on drop.
///
/// Owning this guard for the full lifetime of [`race_host_auth`] (including across
/// every `.await` point) ensures the count is ALWAYS decremented, even when:
///   - the operator closes the decorated popup via the OS title-bar X / Alt+F4
///     (the `WindowEvent::Destroyed` handler resolves the pending oneshot, the race
///     exits normally, and the guard drops);
///   - the per-session task is aborted (e.g. `go_online` is re-run while a race is
///     in progress): Tokio cancels the future at the current `.await` point, which
///     drops the guard and triggers `Drop::drop`, releasing the slot even though the
///     code after the `select!` loop never runs (C2 fix).
///
/// `close_approval_window` now only closes the OS window; the counter update is
/// exclusively managed here, preventing any double-decrement.
pub(crate) struct PopupSlotGuard {
	app: AppHandle,
	id: u64,
}

impl Drop for PopupSlotGuard {
	fn drop(&mut self) {
		// Release the popup slot unconditionally. Saturating to guard against any
		// hypothetical double-drop (shouldn't happen, but safe).
		OPEN_POPUP_COUNT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
			Some(n.saturating_sub(1))
		})
		.ok();
		// Close the window if it is still open (it may already be gone if the
		// operator dismissed it with the OS chrome — that's fine).
		if let Some(win) = self.app.get_webview_window(&format!("approve-{}", self.id)) {
			let _ = win.close();
		}
	}
}

/// Spawn the Allow/Deny popup as a separate, focused, always-on-top window that
/// requests the user's attention (they may be in another app).
///
/// Returns a [`PopupSlotGuard`] on success. The caller MUST hold the guard for the
/// entire duration of the auth race — dropping it (including on task cancellation)
/// unconditionally releases the popup slot and closes the window.
pub(crate) fn open_approval_window(
	app: &AppHandle,
	id: u64,
	peer: &str,
	pw_status: &str,
) -> Option<PopupSlotGuard> {
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
	.title(crate::i18n::t("title.approve"))
	.inner_size(400.0, 300.0)
	.resizable(false)
	.always_on_top(true)
	.center()
	.focused(true)
	.build()
	{
		Ok(win) => {
			OPEN_POPUP_COUNT.fetch_add(1, Ordering::Relaxed);
			let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
			Some(PopupSlotGuard { app: app.clone(), id })
		}
		Err(e) => {
			tracing::warn!(%e, "approval window failed to open");
			None
		}
	}
}

/// Close the approval popup window.
///
/// The counter decrement is handled exclusively by [`PopupSlotGuard::drop`]; this
/// function only closes the OS window so the caller does not need to hold a window
/// handle when signalling early exit (e.g. idle timeout).
fn close_approval_window(app: &AppHandle, id: u64) {
	// Close the window if it is still open (it may already be gone if the
	// operator dismissed it with the OS chrome — that's fine, we just skip).
	if let Some(win) = app.get_webview_window(&format!("approve-{id}")) {
		let _ = win.close();
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

/// Result of [`race_host_auth`]: whether the connection was approved, and (when
/// approved by a password) whether that password was the single-use ONE-TIME
/// password — the caller rotates the OTP in that case, but never for the
/// reusable persistent connect password or a passwordless Allow.
pub(crate) struct RaceOutcome {
	pub approved: bool,
	pub matched_one_time: bool,
}

/// Host: open the Allow/Deny popup AND, at the same time, race it against a correct
/// password arriving over the session. Accept on whichever lands first — so the
/// host can approve passwordlessly while the client is still being asked for one.
/// `one_time_pw` is the rotating session password (may be empty); a password match
/// against it sets [`RaceOutcome::matched_one_time`] so the caller can rotate it.
pub(crate) async fn race_host_auth(
	session: &mut pulsar_core::Session,
	app: &AppHandle,
	pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
	next_req: &Arc<AtomicU64>,
	peer: &str,
	accepted_pws: &[String],
	one_time_pw: &str,
) -> RaceOutcome {
	// Cap concurrent popups: an id-cycling attacker opens a fresh connection on
	// each relay id, each reaching this race before the popup limit is known.
	// If the cap is already at the maximum, deny immediately — no new popup, no
	// new window, no new attention steal (C8 fix).
	if OPEN_POPUP_COUNT.load(Ordering::Relaxed) >= MAX_CONCURRENT_POPUPS {
		tracing::warn!(
			%peer,
			limit = MAX_CONCURRENT_POPUPS,
			"auth: concurrent popup cap reached — denying without opening a new window"
		);
		return RaceOutcome { approved: false, matched_one_time: false };
	}

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
	// Hold the guard for the entire race. Its Drop impl decrements OPEN_POPUP_COUNT
	// and closes the window regardless of how the race exits — normal return, timeout,
	// or future cancellation (task abort via JoinHandle::abort when go_online restarts).
	// This is the C2 fix: a cancelled task can no longer leak a popup slot.
	let _popup_guard = open_approval_window(app, id, peer, "wait");

	// Inactivity deadline: a client that sends NO password attempt within this
	// window is auto-denied. ONLY actual password submissions (ClientAuth::Password)
	// re-arm this timer — bare keepalives (Ping) must NOT re-arm it, because a
	// peer that keeps pinging without ever sending a password would otherwise pin
	// the popup open indefinitely (C8 root cause fix).
	const IDLE: std::time::Duration = std::time::Duration::from_secs(60);
	// Absolute budget: regardless of password retries the popup is always closed
	// and the peer is denied after this wall-clock cap. This bounds the popup
	// lifetime even for a peer that keeps submitting wrong passwords forever.
	const MAX_TOTAL: std::time::Duration = std::time::Duration::from_secs(120);

	let idle_deadline = tokio::time::sleep(IDLE);
	tokio::pin!(idle_deadline);
	let absolute_deadline = tokio::time::sleep(MAX_TOTAL);
	tokio::pin!(absolute_deadline);

	// (approved, matched_one_time): a passwordless Allow never rotates the OTP, a
	// password match reports whether it was the single-use OTP vs the reusable one.
	let result: (bool, bool) = loop {
		tokio::select! {
			biased;
			d = &mut rx => break (matches!(d, Ok(true)), false),
			// Idle timeout: client sent no useful message recently → deny.
			_ = &mut idle_deadline => {
				tracing::debug!(%peer, "auth: idle timeout — auto-denying");
				break (false, false);
			}
			// Absolute budget exhausted: deny regardless of activity.
			_ = &mut absolute_deadline => {
				tracing::warn!(%peer, "auth: absolute time budget exhausted — denying");
				break (false, false);
			}
			msg = recv_client_auth(session) => {
				match msg {
					ClientAuth::Password(pw) => {
						// A real password attempt: re-arm the idle deadline so the
						// operator has time to respond after a retry, but the absolute
						// cap above still applies regardless.
						idle_deadline.as_mut().reset(tokio::time::Instant::now() + IDLE);
						// Either the rotating one-time password or the persistent
						// connect password (Settings → Güvenlik) unlocks the session.
						// Reusable persistent connect password (Settings → Güvenlik):
						// a snapshot compare is fine since it is NEVER rotated. Exclude
						// the OTP snapshot (`one_time_pw`) here — the OTP is consumed
						// atomically below against the LIVE store.
						if accepted_pws
							.iter()
							.any(|a| !a.is_empty() && a.as_str() != one_time_pw && secret_eq(&pw, a))
						{
							break (true, false);
						}
						// One-time password: atomically match against the LIVE store and
						// rotate in a single critical section (try_consume_otp), so two
						// concurrent race tasks can never both consume the same live code
						// (closes the read→compare→rotate TOCTOU on the race path too).
						// It rotates internally on success → report matched_one_time=false
						// so the caller does NOT rotate again.
						if crate::commands::try_consume_otp(app, &pw) {
							break (true, false);
						}
						// Wrong: only count non-empty submissions. An empty pw is the
						// client's automatic "I have no password yet" probe — the same
						// no-op the up-front path skips (host.rs). Counting it would
						// let any peer drive per-peer + global throttle counters for
						// free by spamming empty Auth frames, and would burn a
						// legitimate user's attempt for accidentally submitting an empty
						// field (asymmetric with the deliberate up-front empty-skip).
						let locked = if !pw.is_empty() {
							let locked = throttle::record_failure(peer);
							// Also count it against the source-independent global limit:
							// a relay attacker who cycles ids gets a fresh per-peer bucket
							// each time, so the per-peer lockout alone can't stop sustained
							// guessing — rotate the OTP after enough TOTAL wrong guesses.
							if throttle::note_global_failure() {
								crate::commands::rotate_session_password(app);
							}
							locked
						} else {
							false
						};
						if locked {
							break (false, false);
						}
						let _ = need_password(session).await; // wrong → ask client to retry
					}
					// Keepalive / Ping: do NOT re-arm the idle deadline. A peer that
					// only sends pings without ever submitting a password would
					// otherwise hold the popup open forever (C8 root cause). The
					// absolute deadline above is the only bound for a pure-keepalive
					// attacker; the idle deadline auto-denies once keepalives stop.
					ClientAuth::Keepalive => {}
					ClientAuth::Gone => break (false, false),
				}
			}
		}
	};
	pending.lock().unwrap().remove(&id);
	// close_approval_window closes the OS window; _popup_guard.drop() (below) handles
	// OPEN_POPUP_COUNT. Call it explicitly here so the window closes before we return
	// (the guard will still decrement the counter when it drops at end-of-scope).
	close_approval_window(app, id);
	RaceOutcome {
		approved: result.0,
		matched_one_time: result.1,
	}
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

/// Unit tests for the throttle module.
#[cfg(test)]
mod tests {
	use super::throttle;
	use std::sync::Mutex;

	/// Serializes the two global-counter tests so they don't race on
	/// `GLOBAL_FAILURES` (a process-global static).  Per-peer tests do not need
	/// this lock — they use distinct peer keys and `clear()` is idempotent.
	static GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

	/// Interleaving `clear()` calls (which happen on every approved connection,
	/// including passwordless Allow and connect_password logins) must NOT reset the
	/// source-independent global counter — otherwise an attacker who cycles relay ids
	/// and waits for any legitimate login between batches would permanently prevent
	/// the global threshold from being crossed and the OTP would never rotate.
	#[test]
	fn global_counter_not_reset_by_clear() {
		let _guard = GLOBAL_TEST_LOCK.lock().unwrap();
		// Ensure clean state: a prior test or a prior rotation in this process
		// must not leave a `last_rotation` that suppresses this test's rotation.
		throttle::reset_global_for_test();
		// Record GLOBAL_MAX_FAILURES - 1 wrong guesses from different "peers"
		// (simulating relay-id cycling), interleaved with successful auths from
		// unrelated peers via clear().  The counter must NOT revert to zero.
		// GLOBAL_MAX_FAILURES = 20; we drive it to 19 with interspersed clears.
		let mut rotated = false;
		for i in 0..19u32 {
			// Simulate a legitimate user getting approved between every two guesses.
			if i % 2 == 0 {
				throttle::clear(&format!("legit-peer-{i}"));
			}
			// Wrong guess from an attacker cycling ids.
			if throttle::note_global_failure() {
				rotated = true;
				break;
			}
		}
		// After 19 failures (none of which were reset by the clear() calls) the
		// counter should be at 19, not zero.  One more failure must cross the
		// threshold and signal rotation.
		assert!(!rotated, "OTP rotated too early (expected exactly 20 failures)");
		let triggered = throttle::note_global_failure(); // 20th failure
		assert!(triggered, "global OTP-rotation guard did not fire after 20 total failures interleaved with successful auths");
	}

	/// C9 fix: a second attacker batch of 20 wrong guesses that arrives immediately
	/// after the first rotation must NOT trigger a second rotation — the cooldown
	/// gate must suppress it so the legitimate user's displayed code stays valid.
	#[test]
	fn global_rotation_cooldown_prevents_rapid_rechurn() {
		let _guard = GLOBAL_TEST_LOCK.lock().unwrap();
		// Start from a state where a rotation just happened (last_rotation = now).
		throttle::reset_global_for_test();
		// Drive 20 failures to trigger the first rotation (no prior rotation → fires).
		for _ in 0..20u32 {
			throttle::note_global_failure();
		}
		// At this point last_rotation = Some(now). Drive another 20 failures immediately.
		let mut second_rotation = false;
		for _ in 0..20u32 {
			if throttle::note_global_failure() {
				second_rotation = true;
			}
		}
		// The cooldown must have suppressed the second rotation.
		assert!(
			!second_rotation,
			"C9: attacker triggered a second OTP rotation within the cooldown window; \
			 the displayed code was churned away from the legitimate user"
		);
	}

	/// A clear() from an approved peer must still reset THAT peer's per-peer
	/// attempt counter (this is the intended behaviour — not a regression).
	#[test]
	fn clear_resets_per_peer_counter() {
		let peer = "per-peer-clear-test";
		// Record some failures for the peer.
		throttle::record_failure(peer);
		throttle::record_failure(peer);
		// Should not be locked out yet (threshold is 5).
		assert!(throttle::locked_out(peer).is_none());
		// Clearing removes the entry.
		throttle::clear(peer);
		// After clear, locked_out returns None (no entry).
		assert!(throttle::locked_out(peer).is_none());
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
pub(crate) async fn disconnect_peer(
	state: State<'_, AppState>,
	peer: String,
) -> Result<(), String> {
	if let Some((_, tx)) = state.incoming.lock().unwrap().remove(&peer) {
		let _ = tx.send(());
	}
	Ok(())
}
