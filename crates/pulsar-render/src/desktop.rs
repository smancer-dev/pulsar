//! Windows / macOS overlay backend — eframe (winit + wgpu: D3D12 on Windows, Metal on macOS).
//! Renders the SAME `overlay.rs` egui UI as the Linux egui_glow path, so the overlay looks
//! identical across all platforms. A transparent, borderless, always-on-top window; shown with
//! its full menu while the overlay is OPEN, and — while CLOSED — kept visible-but-click-through
//! so it can still paint the Parsec-style closed-state chrome (mini stats HUD, overlay-open
//! button with its unread-chat badge, and transient hint/toast) over whatever is behind it.
//!
//! Phase / parity note: on macOS this is an OVERLAY-ONLY window today — the live video is the
//! separate native `mpv` child the app spawns alongside us (`play.rs` mac branch). The video is
//! NOT composited inside this surface (that needs the Metal zero-copy renderer, a Mac-only later
//! phase). Everything else — the open/close protocol, the stdin control surface, the HUD/button/
//! hint chrome and the chat/files/audio state — matches the Linux (`linux.rs`) and Windows
//! (`win/mod.rs`) backends so the two-mode UX is identical. The overlay toggles on stdin
//! `open`/`close` (written by `session_cmds::set_overlay`); `hide`/`show` follow session
//! lifetime; the rest of the protocol mirrors `linux.rs` line-for-line where it makes sense for
//! an overlay-only window.

use crate::overlay::{self, Mode, OverlayCmd, OverlayState};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

static OPEN: AtomicBool = AtomicBool::new(false);
/// Set by a `hide` line on stdin (session ended) — the viewport is hidden entirely until `show`.
/// Unlike Linux (where staying resident avoids corrupting WebKit's shared Mali GL), this is a
/// plain hide: the eframe window just stops being visible while idle, and `show` brings it back.
static IDLE: AtomicBool = AtomicBool::new(false);
static STATS: Mutex<[f32; 4]> = Mutex::new([0.0; 4]); // fps, latency_ms, decode_ms, mbps
/// Network RTT in tenths of ms (app stdin `rtt <ms>` from the keepalive ping/pong) — the
/// overlay's "Gecikme" tile shows THIS rather than the local present-gap when it is fed.
static RTT_DMS: AtomicU32 = AtomicU32::new(0);
/// Frame-pacing toggle state (mirrored for the overlay's segmented control). This backend has no
/// local video pipeline to pace — the value is purely reflected to the UI and emitted on change
/// so the frontend persists it; the real pacing happens in the separate video renderer/mpv.
static PACE: AtomicBool = AtomicBool::new(true);
/// View-fit mode (overlay Görüntü section / frontend-persisted value) — reflected to the UI and
/// forwarded; the actual fit is applied by the video renderer, not this overlay-only window.
static FIT: Mutex<String> = Mutex::new(String::new());
/// The host's ACTIVE encode summary ("H.265 · VideoToolbox · 1080p · 60fps"), pushed over stdin
/// (`hostenc <label>`); shown faintly under the overlay's selectors so the user sees what is
/// REALLY in use.
static HOST_ENC: Mutex<String> = Mutex::new(String::new());
/// Always-on mini stats HUD while the overlay is closed (frontend-persisted toggle; synced via
/// stdin `statshud 0|1` / the caps line, and flippable from the overlay row).
static STATS_HUD: AtomicBool = AtomicBool::new(false);
/// Parsec-style overlay-open button while the overlay is closed (default ON; frontend-persisted,
/// synced via stdin `ovbtn 0|1` / the caps line).
static OVERLAY_BTN: AtomicBool = AtomicBool::new(true);
/// Overlay-open button top-left in egui points (frontend-persisted; drag-movable — synced via
/// stdin `ovbtnpos <x> <y>` and the caps line's `btnpos=x,y`).
static OVBTN_POS: Mutex<(f32, f32)> = Mutex::new(crate::overlay::BTN_POS_DEFAULT);
/// Transport label for the overlay header ("P2P"/"Relay"), from the caps line.
static CONN_LABEL: Mutex<String> = Mutex::new(String::new());
/// Host caps + active selections pushed by the app over stdin (`caps …` line):
/// (host_codecs, host_encoders, active codec, active encoder). Copied into the OverlayState the
/// next frame, then cleared (take()), exactly like linux.rs.
static CAPS: Mutex<Option<(Vec<String>, Vec<String>, String, String)>> = Mutex::new(None);
/// Session audio truth (app stdin `audio tx=1 mute=0 mic=0`) for the Ses section.
static AUDIO_TX: AtomicBool = AtomicBool::new(true);
static AUDIO_MUTE: AtomicBool = AtomicBool::new(false);
static MIC_ON: AtomicBool = AtomicBool::new(false);
/// Transient bottom-center helper tooltip (text + when it was armed + its visible window in
/// seconds; drawn while the overlay is CLOSED). Armed locally on overlay close ("click to
/// control") and by the app over stdin (`hint engage|click`, `toast <text>`).
static HINT: Mutex<Option<(String, std::time::Instant, f32)>> = Mutex::new(None);
/// Visible window + fade-out tail of the tooltip (seconds). Chat toasts pass a longer duration.
const HINT_SECS: f32 = 3.0;
const TOAST_SECS: f32 = 6.0;
const HINT_FADE: f32 = 0.5;
/// Chat log fed over stdin (`chat in|out <text>`, same protocol as linux.rs) — shown in the
/// overlay's Chat view while it is open.
static CHAT_LOG: Mutex<Vec<(bool, String)>> = Mutex::new(Vec::new());
/// Host messages not yet seen in the overlay Chat view — badge on the open button.
static CHAT_UNREAD: AtomicUsize = AtomicUsize::new(0);
/// Files view, REMOTE pane: (HOME-relative path, rows) from `fsjson …` (same one-line JSON as
/// linux.rs) — copied into the overlay state each frame.
static FS_REMOTE: Mutex<(String, Vec<overlay::FsRow>)> = Mutex::new((String::new(), Vec::new()));
/// Key input relayed by the webview while the overlay is open (`kin t <text>` / `kin k <name>`).
/// This backend has REAL keyboard focus (its own eframe window), so this is belt-and-braces —
/// the app may still relay keys (e.g. if the webview captured them first); merge them in too.
static KEY_IN: Mutex<Vec<egui::Event>> = Mutex::new(Vec::new());
/// Relayed Enter — consumed by the Chat composer as "send" (in addition to the real Enter this
/// window's own keyboard focus produces).
static ENTER_IN: AtomicBool = AtomicBool::new(false);

/// Arm the transient helper hint for `kind` ("engage"/"click"), localized via the i18n catalog
/// — same text + lifetime as linux.rs `arm_hint`.
fn arm_hint(kind: &str) {
	let text = crate::i18n::t(if kind == "engage" {
		"hint.engage"
	} else {
		"hint.click"
	});
	*HINT.lock().unwrap() = Some((text.to_string(), std::time::Instant::now(), HINT_SECS));
}

/// Read control + stats from stdin (host/app → overlay). The accepted lines mirror linux.rs's
/// stdin reader, restricted to what an overlay-only window can act on: `open`/`close`/`hide`/
/// `show` (visibility), `stat`/`rtt` (HUD), `pace`/`fit`/`statshud`/`ovbtn`/`ovbtnpos` (reflected
/// settings), `caps`/`hostenc` (host capability + active-encode seed), `audio`/`chat`/`fsjson`/
/// `kin`/`hint`/`toast`. Video/cursor side-channel lines (`cursor*`, `reopen`, `engaged`) are
/// tolerated and ignored — they belong to the separate native video renderer, not this overlay.
fn stdin_control() {
	use std::io::BufRead;
	let stdin = std::io::stdin();
	for line in stdin.lock().lines() {
		let Ok(line) = line else { break };
		let line = line.trim_end().to_string();
		let mut it = line.split_whitespace();
		match it.next() {
			// Overlay visibility toggle (written by session_cmds::set_overlay over stdin).
			Some("open") => OPEN.store(true, Ordering::SeqCst),
			Some("close") => OPEN.store(false, Ordering::SeqCst),
			// Session ended / resumed: hide / show the whole overlay window. (No GL-context
			// caveat here — unlike Linux there's no shared WebKit Mali GL to protect.)
			Some("hide") => IDLE.store(true, Ordering::SeqCst),
			Some("show") => IDLE.store(false, Ordering::SeqCst),
			// Live HUD stats: `stat <fps> <lat> <dec> <mbps>` (render_stats.rs's stat writer).
			Some("stat") => {
				let v: Vec<f32> = it.filter_map(|x| x.parse().ok()).collect();
				if v.len() >= 4 {
					*STATS.lock().unwrap() = [v[0], v[1], v[2], v[3]];
				}
			}
			// Network RTT from the keepalive ping/pong (ms, the "Gecikme" tile).
			Some("rtt") => {
				if let Some(ms) = it.next().and_then(|v| v.parse::<f32>().ok()) {
					RTT_DMS.store((ms * 10.0) as u32, Ordering::Relaxed);
				}
			}
			// Frame-pacing toggle (reflected to the overlay's segmented control; the real
			// pacing lives in the video renderer — here we only mirror + re-emit on change).
			Some("pace") => {
				if let Some(v) = it.next() {
					PACE.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
				}
			}
			// View-fit mode (overlay Görüntü section / frontend-persisted value).
			Some("fit") => {
				if let Some(v) = it.next() {
					*FIT.lock().unwrap() = v.to_string();
				}
			}
			// The host's active encode summary (rest of the line, verbatim).
			Some("hostenc") => {
				*HOST_ENC.lock().unwrap() = line.splitn(2, ' ').nth(1).unwrap_or("").to_string();
			}
			// Always-on stats HUD toggle (frontend setting / overlay row).
			Some("statshud") => {
				if let Some(v) = it.next() {
					STATS_HUD.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
				}
			}
			// Overlay-open button toggle (frontend setting / overlay row).
			Some("ovbtn") => {
				if let Some(v) = it.next() {
					OVERLAY_BTN.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
				}
			}
			// Overlay-open button position in egui points (webview hotspot drag).
			Some("ovbtnpos") => {
				if let (Some(x), Some(y)) = (
					it.next().and_then(|v| v.parse::<f32>().ok()),
					it.next().and_then(|v| v.parse::<f32>().ok()),
				) {
					*OVBTN_POS.lock().unwrap() = (x, y);
				}
			}
			// Host caps + active request from the app:
			// `caps codecs=h264,h265 encoders=auto,software codec=h264 encoder=auto conn=P2P …`
			// (same parse + seeded statics as linux.rs's `caps` arm).
			Some("caps") => {
				let (mut codecs, mut encoders) = (Vec::new(), Vec::new());
				let (mut codec, mut encoder) = (String::new(), String::new());
				for kv in it {
					if let Some((k, v)) = kv.split_once('=') {
						match k {
							"codecs" => codecs = v.split(',').map(str::to_string).collect(),
							"encoders" => encoders = v.split(',').map(str::to_string).collect(),
							"codec" => codec = v.to_string(),
							"encoder" => encoder = v.to_string(),
							"conn" => *CONN_LABEL.lock().unwrap() = v.to_string(),
							"statshud" => STATS_HUD
								.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst),
							// Persisted overlay-button state rides the caps line so a respawn
							// (live codec switch) re-seeds it with everything else.
							"ovbtn" => OVERLAY_BTN
								.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst),
							"btnpos" => {
								if let Some((x, y)) = v.split_once(',') {
									if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
										*OVBTN_POS.lock().unwrap() = (x, y);
									}
								}
							}
							_ => {}
						}
					}
				}
				*CAPS.lock().unwrap() = Some((codecs, encoders, codec, encoder));
			}
			// Session audio truth from the app (`audio tx=1 mute=0 mic=0`) so the overlay's
			// Ses toggles highlight the real state — same format as linux.rs.
			Some("audio") => {
				for kv in it {
					if let Some((k, v)) = kv.split_once('=') {
						let on = v == "1" || v == "on";
						match k {
							"tx" => AUDIO_TX.store(on, Ordering::SeqCst),
							"mute" => AUDIO_MUTE.store(on, Ordering::SeqCst),
							"mic" => MIC_ON.store(on, Ordering::SeqCst),
							_ => {}
						}
					}
				}
			}
			// Chat line for the native Chat view: `chat in <text>` (host) / `chat out <text>`
			// (our own, echoed by the app — single source of truth).
			Some("chat") => {
				let dir = it.next().unwrap_or("in");
				let text = line.splitn(3, ' ').nth(2).unwrap_or("").trim().to_string();
				if !text.is_empty() {
					let mut log = CHAT_LOG.lock().unwrap();
					log.push((dir == "out", text));
					if log.len() > 200 {
						log.remove(0);
					}
					if dir != "out" {
						CHAT_UNREAD.fetch_add(1, Ordering::SeqCst);
					}
				}
			}
			// Remote file listing for the Files view: one-line JSON
			// {"path":"…","entries":[{"name":…,"dir":…,"size":…},…]}.
			Some("fsjson") => {
				let json = line.splitn(2, ' ').nth(1).unwrap_or("");
				if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
					let path = v["path"].as_str().unwrap_or("").to_string();
					let rows = v["entries"]
						.as_array()
						.map(|a| {
							a.iter()
								.filter_map(|e| {
									Some(overlay::FsRow {
										name: e["name"].as_str()?.to_string(),
										dir: e["dir"].as_bool().unwrap_or(false),
										size: e["size"].as_u64().unwrap_or(0),
									})
								})
								.collect()
						})
						.unwrap_or_default();
					*FS_REMOTE.lock().unwrap() = (path, rows);
				}
			}
			// Relayed keyboard for the Chat composer (`kin t <text>` / `kin k <name>`). Belt-and-
			// braces on this backend (it has real keyboard focus), mirroring linux.rs exactly.
			Some("kin") => match it.next() {
				Some("t") => {
					let text = line.splitn(3, ' ').nth(2).unwrap_or("").to_string();
					if !text.is_empty() {
						KEY_IN.lock().unwrap().push(egui::Event::Text(text));
					}
				}
				Some("k") => {
					let key = match it.next() {
						Some("backspace") => Some(egui::Key::Backspace),
						Some("enter") => {
							ENTER_IN.store(true, Ordering::SeqCst);
							None
						}
						Some("left") => Some(egui::Key::ArrowLeft),
						Some("right") => Some(egui::Key::ArrowRight),
						_ => None,
					};
					if let Some(key) = key {
						let mut q = KEY_IN.lock().unwrap();
						for pressed in [true, false] {
							q.push(egui::Event::Key {
								key,
								physical_key: None,
								pressed,
								repeat: false,
								modifiers: egui::Modifiers::default(),
							});
						}
					}
				}
				_ => {}
			},
			// Transient helper tooltip ("hint engage|click") — engage/release edges are
			// app-side knowledge (input capture), so the app pushes them here.
			Some("hint") => arm_hint(it.next().unwrap_or("click")),
			// Free-text toast (rest of the line verbatim): inbound chat etc.
			Some("toast") => {
				let rest = line.splitn(2, ' ').nth(1).unwrap_or("").trim();
				if !rest.is_empty() {
					*HINT.lock().unwrap() =
						Some((rest.to_string(), std::time::Instant::now(), TOAST_SECS));
				}
			}
			// Anything else (cursor side-channel, reopen, engaged …) belongs to the separate
			// video renderer on this platform — tolerated and ignored here.
			_ => {}
		}
	}
}

struct Overlay {
	state: OverlayState,
	// Frame-persistent overlay UI state (current page, chat composer, local file pane).
	ov_ui: overlay::UiState,
	was_open: bool,
	was_idle: bool,
}

impl Overlay {
	/// Pull the latest stdin-fed state into `self.state` (run every frame, open or closed, so the
	/// closed-state chrome — HUD, button badge, hint — stays live too).
	fn sync_state(&mut self) {
		// HUD numbers: prefer the network RTT for the latency tile (the local present-gap would
		// read as fake "lag"); fall back to the stat line's latency before the first pong.
		let s = *STATS.lock().unwrap();
		if s[0] > 0.0 {
			self.state.fps = s[0];
			self.state.decode_ms = s[2];
			self.state.mbps = s[3];
		}
		let rtt = RTT_DMS.load(Ordering::Relaxed) as f32 / 10.0;
		self.state.latency_ms = if rtt > 0.0 { rtt } else { s[1] };

		// Host caps + active selections (take(), seeded once per `caps` line).
		if let Some((codecs, encoders, codec, encoder)) = CAPS.lock().unwrap().take() {
			self.state.host_codecs = codecs;
			self.state.host_encoders = encoders;
			if !codec.is_empty() {
				self.state.codec = codec;
			}
			if !encoder.is_empty() {
				self.state.encoder = encoder;
			}
		}
		{
			let he = HOST_ENC.lock().unwrap();
			if self.state.host_active != *he {
				self.state.host_active = he.clone();
			}
		}
		{
			let cl = CONN_LABEL.lock().unwrap();
			if !cl.is_empty() && self.state.conn_label != *cl {
				self.state.conn_label = cl.clone();
			}
		}
		{
			let fit = FIT.lock().unwrap();
			if !fit.is_empty() && self.state.fit != *fit {
				self.state.fit = fit.clone();
			}
		}
		self.state.pace = PACE.load(Ordering::SeqCst);
		self.state.stats_hud = STATS_HUD.load(Ordering::SeqCst);
		self.state.overlay_btn = OVERLAY_BTN.load(Ordering::SeqCst);
		self.state.btn_pos = *OVBTN_POS.lock().unwrap();
		self.state.audio_tx = AUDIO_TX.load(Ordering::SeqCst);
		self.state.audio_mute = AUDIO_MUTE.load(Ordering::SeqCst);
		self.state.mic_on = MIC_ON.load(Ordering::SeqCst);
		self.state.chat = CHAT_LOG.lock().unwrap().clone();
		// Reading the chat clears the unread badge (the badge shows on the closed-state open
		// button); new arrivals while the Chat view is on screen count as read immediately.
		if self.state.open && self.ov_ui.view == overlay::View::Chat {
			CHAT_UNREAD.store(0, Ordering::SeqCst);
		}
		self.state.chat_unread = CHAT_UNREAD.load(Ordering::SeqCst);
		{
			let (p, rows) = &*FS_REMOTE.lock().unwrap();
			self.state.fs_remote_path = p.clone();
			self.state.fs_remote = rows.clone();
		}
	}
}

impl eframe::App for Overlay {
	fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
		[0.0, 0.0, 0.0, 0.0] // transparent so the video/desktop below shows through
	}

	fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
		// Session-idle: hide the whole overlay window until `show` (post-disconnect). While idle
		// we draw nothing and just keep ticking so the next `show` brings it straight back.
		let idle = IDLE.load(Ordering::SeqCst);
		if idle != self.was_idle {
			ctx.send_viewport_cmd(egui::ViewportCommand::Visible(!idle));
			self.was_idle = idle;
		}
		if idle {
			ctx.request_repaint_after(std::time::Duration::from_millis(100));
			return;
		}

		let open = OPEN.load(Ordering::SeqCst);
		self.state.open = open;
		// Visibility + click-through follow the open state. KEY DIFFERENCE vs. the old code: the
		// window stays VISIBLE while closed (so the closed-state chrome can paint) but is mouse
		// PASS-THROUGH then, so clicks fall to whatever is behind it (the video / desktop). When
		// open it grabs the mouse so egui receives clicks for the full menu.
		if open != self.was_open {
			ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
			ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!open));
			if open {
				// Fresh open always lands on the hub, not a stale section; the Files view
				// re-requests the remote listing once per visit.
				self.ov_ui.view = overlay::View::Root;
				self.ov_ui.remote_requested = false;
			} else {
				// Closing leaves the user prompted to click-to-control again.
				arm_hint("click");
			}
			// Drop any keystrokes relayed across the edge so they can't replay into the chat
			// composer on the next open (the other backends clear their relay queue the same way).
			KEY_IN.lock().unwrap().clear();
			ENTER_IN.store(false, Ordering::SeqCst);
			self.was_open = open;
		}

		self.sync_state();

		if open {
			// This window has REAL keyboard focus, so Enter in the chat composer arrives as a
			// normal egui key event — but ALSO honor a relayed Enter (`kin k enter`) so the
			// composer still "sends" if the webview captured the key first.
			let real_enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
			self.state.chat_enter = real_enter || ENTER_IN.swap(false, Ordering::SeqCst);
			// This window has real keyboard focus, so typing reaches the composer through egui's
			// own input — the relayed `kin t/k …` queue is only a fallback for keys the webview
			// grabbed first. Drain it each open frame so it can't pile up; relayed Enter is
			// already folded into `chat_enter` above. (Drained, not replayed: egui has already
			// processed this frame's input by the time `update` runs, so re-injecting text here
			// would land a frame late — the real focus path covers the normal case.)
			KEY_IN.lock().unwrap().clear();
			for c in overlay::draw(ctx, &self.state, &mut self.ov_ui) {
				self.emit_cmd(c);
			}
		} else {
			// CLOSED-state chrome (display-only, same as linux.rs): the mini stats HUD, the
			// Parsec-style open button (+ unread-chat badge) and the transient hint/toast. The
			// window is mouse pass-through while closed, so a click here normally falls through to
			// the video; the button click is wired belt-and-braces (`ov toggle`) for the cases
			// where the OS does deliver it (e.g. a future non-passthrough hotspot region).
			let hint = self.expire_hint();
			if self.state.stats_hud {
				overlay::draw_hud(ctx, &self.state);
			}
			if self.state.overlay_btn && overlay::draw_open_button(ctx, &self.state) {
				// The renderer's own open button was clicked — ask the app to open the overlay
				// (it round-trips back as a `open` stdin line via set_overlay).
				println!("ov toggle");
				use std::io::Write;
				let _ = std::io::stdout().flush();
			}
			if let Some((text, alpha)) = hint {
				overlay::draw_hint(ctx, &text, alpha);
			}
		}
		// Repaint continuously while open (live menu) or whenever there is closed-state chrome to
		// animate (HUD ticks, hint fade); otherwise a slow idle tick keeps stdin-driven state
		// (e.g. a freshly-armed toast or a new unread badge) appearing promptly.
		if open || self.state.stats_hud || self.state.overlay_btn || HINT.lock().unwrap().is_some() {
			ctx.request_repaint();
		} else {
			ctx.request_repaint_after(std::time::Duration::from_millis(100));
		}
	}
}

impl Overlay {
	/// Expire the transient helper hint after its window; fade it out over the final HINT_FADE
	/// seconds (alpha 1→0). Returns the live (text, alpha) to draw, or None. Mirrors linux.rs.
	fn expire_hint(&self) -> Option<(String, f32)> {
		let mut g = HINT.lock().unwrap();
		if let Some((text, t0, secs)) = g.as_ref() {
			let el = t0.elapsed().as_secs_f32();
			if el >= *secs {
				*g = None;
				None
			} else {
				let alpha = ((*secs - el) / HINT_FADE).min(1.0);
				Some((text.clone(), alpha))
			}
		} else {
			None
		}
	}

	/// Apply an overlay command to local state and emit its wire line on stdout — the SAME `ov …`
	/// lines render_stats.rs parses on every platform (so the frontend → host plumbing is shared).
	fn emit_cmd(&mut self, c: OverlayCmd) {
		match c {
			OverlayCmd::Set(field, val) => {
				match field {
					"codec" => self.state.codec = val.clone(),
					"encoder" => self.state.encoder = val.clone(),
					"decoder" => self.state.decoder = val.clone(),
					"res" => self.state.res = val.clone(),
					"fps" => self.state.fps_sel = val.clone(),
					"bitrate" => self.state.bitrate = val.clone(),
					"quality" => self.state.quality = val.clone(),
					// Settings reflected locally at once (instant feel) + emitted so the frontend
					// persists them; the video renderer applies the real effect of pace/fit.
					"pace" => {
						self.state.pace = val == "on";
						PACE.store(val == "on", Ordering::SeqCst);
					}
					"statshud" => {
						self.state.stats_hud = val == "on";
						STATS_HUD.store(val == "on", Ordering::SeqCst);
					}
					"ovbtn" => {
						self.state.overlay_btn = val == "on";
						OVERLAY_BTN.store(val == "on", Ordering::SeqCst);
					}
					"fit" => {
						self.state.fit = val.clone();
						*FIT.lock().unwrap() = val.clone();
					}
					// Audio toggles apply optimistically; the app's `audio …` line re-syncs the
					// truth after the host acknowledges.
					"atx" => AUDIO_TX.store(val == "on", Ordering::SeqCst),
					"amute" => AUDIO_MUTE.store(val == "on", Ordering::SeqCst),
					"mic" => MIC_ON.store(val == "on", Ordering::SeqCst),
					// Voice call = mic + host audio together (paired optimistic update).
					"call" => {
						let on = val == "on";
						MIC_ON.store(on, Ordering::SeqCst);
						if on {
							AUDIO_TX.store(true, Ordering::SeqCst);
						}
					}
					_ => {}
				}
				println!("ov set {field} {val}");
			}
			OverlayCmd::End => println!("ov end"),
			OverlayCmd::Close => {
				OPEN.store(false, Ordering::SeqCst);
				println!("ov close");
			}
			// Native Chat / Files traffic — rest-of-line payloads the app parses (render_stats).
			OverlayCmd::Chat(t) => println!("ov chat {}", t.replace('\n', " ")),
			OverlayCmd::FsLs(p) => println!("ov fsls {p}"),
			OverlayCmd::FsGet(p) => println!("ov fsget {p}"),
			OverlayCmd::FsSend(p) => println!("ov fssend {p}"),
			OverlayCmd::OpenFiles => println!("ov files"),
		}
		use std::io::Write;
		let _ = std::io::stdout().flush();
	}
}

pub fn run() {
	let args: Vec<String> = std::env::args().collect();
	let mut mode = Mode::Game;
	let mut i = 1;
	while i < args.len() {
		match args[i].as_str() {
			"--mode" => {
				if let Some(s) = args.get(i + 1) {
					mode = if s == "remote" {
						Mode::Remote
					} else {
						Mode::Game
					};
					i += 1;
				}
			}
			// The app passes its Config.language so this separate process localizes its strings
			// (hints/toasts/overlay labels) to match the rest of the UI — same flag as linux.rs.
			"--lang" => {
				if let Some(s) = args.get(i + 1) {
					crate::i18n::set_english(s == "en");
					i += 1;
				}
			}
			// `--pace on|off` startup default (reflected to the segmented control; the live value
			// then arrives via the `pace 0|1` stdin line / the frontend re-applies the persisted
			// choice). A bare SDP/positional arg is ignored here — this backend renders no video.
			"--pace" => {
				if let Some(s) = args.get(i + 1) {
					PACE.store(s == "on" || s == "1" || s == "true", Ordering::SeqCst);
					i += 1;
				}
			}
			_ => {}
		}
		i += 1;
	}

	std::thread::spawn(stdin_control);

	let options = eframe::NativeOptions {
		viewport: egui::ViewportBuilder::default()
			.with_transparent(true)
			.with_decorations(false)
			.with_always_on_top()
			.with_mouse_passthrough(true)
			.with_maximized(true),
		..Default::default()
	};

	let state = OverlayState {
		mode,
		open: false,
		id: "—".into(),
		conn_label: "P2P".into(),
		pace: PACE.load(Ordering::SeqCst),
		..Default::default()
	};
	let _ = eframe::run_native(
		"pulsar-render",
		options,
		Box::new(move |cc| {
			overlay::apply_theme(&cc.egui_ctx);
			Ok(Box::new(Overlay {
				state,
				ov_ui: Default::default(),
				was_open: false,
				was_idle: false,
			}))
		}),
	);
}
