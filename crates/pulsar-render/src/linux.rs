//! Linux/X11 single-surface renderer: rkmpp video + egui overlay in ONE GL context, in a CHILD
//! window of the Tauri window (`--wid`). Because it's a child window the overlay moves/clips/
//! stacks WITH the app automatically (the override-redirect-top-level approach desynced on move
//! and floated above other apps). Video is drawn every frame; egui is composited on top of it in
//! the same framebuffer while the overlay is open — no compositor transparency needed.

use crate::overlay::{self, Mode, OverlayCmd, OverlayState};
use crate::video;
use std::ffi::c_void;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use x11::xlib;

use khronos_egl as egl;

static OPEN: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// Set by a `hide` line on stdin (sent by the app when a session ends). While idle the render
/// UNMAPS its video window — revealing the WebKitGTK webview underneath — but keeps the process
/// + EGL context ALIVE. Destroying this GL context (process exit) corrupts WebKit's shared Mali
/// GL on RK3588 and wedges the webview; staying resident-but-hidden avoids that. `show` re-maps.
static IDLE: AtomicBool = AtomicBool::new(false);
/// Stream-selection seeds pushed by the app after a renderer respawn (`res <val>`,
/// `fps <val>`, `bitrate <val>`, `quality <val>`, `display <idx>` stdin lines).
/// Stored as `Option` so we only overwrite the OverlayState on the first frame after
/// a respawn seed — not on every frame — leaving the overlay free to update them live
/// thereafter (via emit_cmd). Uses take-once semantics like `CAPS`.
static RES_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static FPS_SEL_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static BITRATE_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static QUALITY_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// `u32::MAX` = not set (sentinel); any real display index is < MAX.
static DISPLAY_IDX_SEED: std::sync::atomic::AtomicU32 =
	std::sync::atomic::AtomicU32::new(u32::MAX);
/// Host caps + active selections pushed by the app over stdin (`caps …` line):
/// (host_codecs, host_encoders, active codec, active encoder). The render loop
/// copies them into the OverlayState each frame.
static CAPS: std::sync::Mutex<Option<(Vec<String>, Vec<String>, String, String)>> =
	std::sync::Mutex::new(None);
/// Host monitors `(idx, label)` from the caps line `displays=` field — the overlay's
/// Display-section screen picker. Empty / single = no picker. Copied into OverlayState.
static DISPLAYS: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());
/// The host's ACTIVE encode summary ("H.265 · Rockchip MPP · 1080p · 60fps"),
/// pushed by the app over stdin (`hostenc <label>`); shown faintly under the
/// overlay's selectors so the user sees what is REALLY in use.
static HOST_ENC: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Always-on mini stats HUD while the overlay is closed (frontend-persisted toggle;
/// synced via stdin `statshud 0|1` and flippable from the overlay row).
static STATS_HUD: AtomicBool = AtomicBool::new(false);
/// Parsec-style overlay-open button while the overlay is closed (default ON;
/// frontend-persisted, synced via stdin `ovbtn 0|1`).
static OVERLAY_BTN: AtomicBool = AtomicBool::new(true);
/// Overlay-open button top-left in egui points (frontend-persisted; drag-movable —
/// synced via stdin `ovbtnpos <x> <y>` and the caps line's `btnpos=x,y`).
static OVBTN_POS: std::sync::Mutex<(f32, f32)> =
	std::sync::Mutex::new(crate::overlay::BTN_POS_DEFAULT);
/// Transport label for the overlay header ("P2P"/"Relay"), from the caps line.
static CONN_LABEL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Transient bottom-center helper tooltip (text + when it was armed; drawn for ~3 s
/// while the overlay is closed). Set locally on overlay close ("click to control")
/// and by the app over stdin (`hint engage|click`) on engage/release transitions.
static HINT: std::sync::Mutex<Option<(String, std::time::Instant, f32)>> =
	std::sync::Mutex::new(None);
/// Visible window (helper hints) + fade-out tail of the tooltip (seconds). Chat
/// toasts pass their own longer duration.
const HINT_SECS: f32 = 3.0;
const TOAST_SECS: f32 = 6.0;
const HINT_FADE: f32 = 0.5;
/// Network RTT in tenths of ms (app stdin `rtt <ms>` from the keepalive ping/pong) —
/// the overlay's "Gecikme" tile shows THIS, not the local present-gap.
static RTT_DMS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Whether the evdev capture is ENGAGED (app stdin `engaged 0|1`). The local cursor
/// hides over the video ONLY while engaged — after Ctrl+Alt+Z the user has their
/// mouse back, so it must be visible again.
static ENGAGED_R: AtomicBool = AtomicBool::new(false);
/// Session audio truth (app stdin `audio tx=1 mute=0 mic=0`) for the Ses section.
static AUDIO_TX: AtomicBool = AtomicBool::new(true);
static AUDIO_MUTE: AtomicBool = AtomicBool::new(false);
static MIC_ON: AtomicBool = AtomicBool::new(false);
/// Native Chat view state: the conversation (me, text) fed over stdin
/// (`chat in|out <text>` — the app echoes BOTH directions, single source of truth).
static CHAT_LOG: std::sync::Mutex<Vec<(bool, String)>> = std::sync::Mutex::new(Vec::new());
/// Host messages not yet seen in the overlay Chat view — badge on the open button.
static CHAT_UNREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Native Files view, REMOTE pane: (HOME-relative path, rows) from `fsjson …`.
static FS_REMOTE: std::sync::Mutex<(String, Vec<crate::overlay::FsRow>)> =
	std::sync::Mutex::new((String::new(), Vec::new()));
/// Key input relayed by the webview while the overlay is open (`kin t <text>` /
/// `kin k <name>`): this child window can't take X focus (it would kill the
/// focus-gated combos), so typing for the Chat composer arrives here.
static KEY_IN: std::sync::Mutex<Vec<egui::Event>> = std::sync::Mutex::new(Vec::new());
/// Relayed Enter — consumed by the Chat composer as "send".
static ENTER_IN: AtomicBool = AtomicBool::new(false);

/// Cursor side-channel state (Moonlight model): the host captured WITHOUT a hardware cursor
/// (KMS zero-copy), so it streams the pointer out-of-band and WE draw it over the video. Fed by
/// the app over stdin (`cursor <x> <y>` normalized 0..1, `cursorimg w h hx hy <b64png>`,
/// `cursorhide`). `None` position = nothing to draw (no side-channel cursor this session).
static CURSOR_POS: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);
/// The latest cursor SHAPE: decoded RGBA + dims + hotspot, replaced on each `cursorimg` line.
/// `egui::TextureHandle` can't be a static (needs the ctx), so we keep raw pixels here and the
/// render loop (re)uploads them to a texture when the generation counter changes.
static CURSOR_IMG: std::sync::Mutex<Option<CursorImg>> = std::sync::Mutex::new(None);
/// Bumped on every new `cursorimg` so the render loop knows to re-upload the egui texture.
static CURSOR_IMG_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Session mode set at spawn via `--mode` and live-updatable via the `mode game|remote` stdin
/// command. The render loop reads this each frame so a cross-session mode change (game→remote
/// or vice-versa) takes effect without a renderer restart. `false` = Game, `true` = Remote.
static MODE_REMOTE: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct CursorImg {
	w: usize,
	h: usize,
	hot_x: f32,
	hot_y: f32,
	rgba: Vec<u8>,
}

/// Decode a base64'd RGBA PNG (the cursor side-channel shape) to raw RGBA8 bytes, verifying it
/// matches the announced `w`x`h`. Returns `None` on any malformed input — a bad cursor frame must
/// never panic the renderer (it just keeps the previous shape / no shape).
fn decode_cursor_png(b64: &str, w: usize, h: usize) -> Option<Vec<u8>> {
	if w == 0 || h == 0 || w > 256 || h > 256 {
		return None;
	}
	let bytes = base64_decode(b64)?;
	let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
	let mut reader = decoder.read_info().ok()?;
	// Reject a PNG whose HEADER dimensions don't match the announced (already ≤256²)
	// w×h BEFORE allocating — `output_buffer_size()` is derived from the header's own
	// dimensions, so a tiny PNG declaring e.g. 40000×40000 would otherwise allocate
	// multiple GB and OOM/crash the renderer.
	let (hw, hh) = reader.info().size();
	if hw as usize != w || hh as usize != h {
		return None;
	}
	let mut buf = vec![0u8; reader.output_buffer_size()];
	let info = reader.next_frame(&mut buf).ok()?;
	if info.color_type != png::ColorType::Rgba
		|| info.width as usize != w
		|| info.height as usize != h
	{
		return None;
	}
	buf.truncate(info.buffer_size());
	Some(buf)
}

/// Minimal standard-base64 decoder (no padding-strictness, ignores whitespace). Kept inline so
/// the renderer doesn't pull a base64 crate just for the tiny cursor-shape payload.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
	fn val(c: u8) -> Option<u8> {
		match c {
			b'A'..=b'Z' => Some(c - b'A'),
			b'a'..=b'z' => Some(c - b'a' + 26),
			b'0'..=b'9' => Some(c - b'0' + 52),
			b'+' => Some(62),
			b'/' => Some(63),
			_ => None,
		}
	}
	let mut out = Vec::with_capacity(s.len() / 4 * 3);
	let mut acc = 0u32;
	let mut bits = 0u32;
	for &c in s.as_bytes() {
		if c == b'=' || c.is_ascii_whitespace() {
			continue;
		}
		let v = val(c)?;
		acc = (acc << 6) | v as u32;
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((acc >> bits) as u8);
		}
	}
	Some(out)
}

fn arm_hint(kind: &str) {
	let text = crate::i18n::t(if kind == "engage" {
		"hint.engage"
	} else {
		"hint.click"
	});
	*HINT.lock().unwrap() = Some((text.to_string(), std::time::Instant::now(), HINT_SECS));
}

extern "C" fn on_usr(sig: libc::c_int) {
	OPEN.store(sig == libc::SIGUSR1, Ordering::SeqCst);
}
extern "C" fn on_stop(_: libc::c_int) {
	STOP.store(true, Ordering::SeqCst);
	// Async-signal-safe: only set atomics here. The decode loop checks video::STOP and exits;
	// the actual drain+free (video::stop_decode) is done from the main thread after the render
	// loop ends (see real_run) — calling it here can deadlock on the MBX mutex the main thread
	// holds every frame in Presenter::draw.
	video::signal_stop();
}

pub fn run() {
	let args: Vec<String> = std::env::args().collect();
	let mut wid: u64 = 0;
	let mut mode = Mode::Game;
	let mut sdp = String::new();
	// Frame-pacing startup default: ON (Moonlight per-vblank metering — kills the gap→teleport
	// and beats the old EMA pacer). `PULSAR_PACE=0` forces newest-wins for A/B; the `--pace`
	// flag and the live `pace 0|1` stdin toggle still override. The frontend persists the choice.
	let mut pace = std::env::var("PULSAR_PACE")
		.map(|v| v == "1" || v == "on" || v == "true")
		.unwrap_or(true);
	let mut i = 1;
	while i < args.len() {
		match args[i].as_str() {
			"--wid" => {
				if let Some(s) = args.get(i + 1) {
					let s = s.trim_start_matches("0x");
					wid = u64::from_str_radix(s, 16)
						.or_else(|_| s.parse())
						.unwrap_or(0);
					i += 1;
				}
			}
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
			"--pace" => {
				if let Some(s) = args.get(i + 1) {
					pace = s == "on" || s == "1" || s == "true";
					i += 1;
				}
			}
			a if !a.starts_with("--") && sdp.is_empty() => sdp = a.to_string(),
			_ => {}
		}
		i += 1;
	}

	unsafe {
		libc::signal(libc::SIGUSR1, on_usr as *const () as usize);
		libc::signal(libc::SIGUSR2, on_usr as *const () as usize);
		libc::signal(libc::SIGINT, on_stop as *const () as usize);
		libc::signal(libc::SIGTERM, on_stop as *const () as usize);
	}

	video::set_pace(pace);
	// Seed the MODE_REMOTE atomic so the render loop and the live `mode` command see the
	// correct initial mode parsed from `--mode`.
	MODE_REMOTE.store(mode == Mode::Remote, Ordering::SeqCst);
	// Adaptive pacing ceiling by mode: game prizes min latency (buffer ≤2), remote tolerates one
	// more frame for smoothness (≤3). Within the QCAP hard cap; the pacer trims toward 1 at rest.
	video::set_pace_ceiling(match mode {
		Mode::Game => 2,
		Mode::Remote => 3,
	});
	// Live pacing toggles arrive as `pace 0|1` lines on stdin (the same stdin the HUD `stat …`
	// lines use); read them on a side thread so the frontend Settings/overlay can flip pacing
	// with no respawn. Non-`pace` lines (HUD stat, etc.) are tolerated and ignored.
	std::thread::spawn(|| {
		use std::io::BufRead;
		let stdin = std::io::stdin();
		for line in stdin.lock().lines() {
			let line = match line {
				Ok(l) => l,
				Err(_) => break,
			};
			let mut it = line.split_whitespace();
			match it.next() {
				Some("pace") => {
					if let Some(v) = it.next() {
						video::set_pace(v == "1" || v == "on" || v == "true");
					}
				}
				// Session ended: hide the video window (reveal the webview) + idle, WITHOUT
				// exiting — keeps our EGL context alive so WebKit's shared Mali GL isn't corrupted.
				Some("hide") => IDLE.store(true, Ordering::SeqCst),
				Some("show") => IDLE.store(false, Ordering::SeqCst),
				// Transient helper tooltip ("hint engage|click") — engage/release edges
				// are app-side knowledge (evdev capture), so the app pushes them here.
				Some("hint") => arm_hint(it.next().unwrap_or("click")),
				// Chat line for the native Chat view: `chat in <text>` (from the host)
				// or `chat out <text>` (our own, echoed by the app after sending).
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
										Some(crate::overlay::FsRow {
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
				// Relayed keyboard for the Chat composer (`kin t <text>` / `kin k <name>`).
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
				// Free-text toast (rest of the line verbatim): inbound chat etc — the
				// webview is occluded by the video, so this is the visible channel.
				Some("toast") => {
					let rest = line.splitn(2, ' ').nth(1).unwrap_or("").trim();
					if !rest.is_empty() {
						*HINT.lock().unwrap() =
							Some((rest.to_string(), std::time::Instant::now(), TOAST_SECS));
					}
				}
				// Live engage state (cursor visibility) — same app-side edges.
				Some("engaged") => {
					if let Some(v) = it.next() {
						ENGAGED_R.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
					}
				}
				// Cursor side-channel position (normalized 0..1 in the streamed screen). The
				// host captured without a hardware cursor (KMS), so we draw it over the video.
				Some("cursor") => {
					if let (Some(x), Some(y)) = (
						it.next().and_then(|v| v.parse::<f32>().ok()),
						it.next().and_then(|v| v.parse::<f32>().ok()),
					) {
						*CURSOR_POS.lock().unwrap() = Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
					}
				}
				// Cursor side-channel shape: `cursorimg w h hot_x hot_y <base64 png>`. Decode
				// to RGBA + stash; the render loop re-uploads it to an egui texture on change.
				Some("cursorimg") => {
					let w = it.next().and_then(|v| v.parse::<usize>().ok());
					let h = it.next().and_then(|v| v.parse::<usize>().ok());
					let hx = it.next().and_then(|v| v.parse::<f32>().ok());
					let hy = it.next().and_then(|v| v.parse::<f32>().ok());
					let b64 = it.next().unwrap_or("");
					if let (Some(w), Some(h), Some(hx), Some(hy)) = (w, h, hx, hy) {
						if let Some(rgba) = decode_cursor_png(b64, w, h) {
							*CURSOR_IMG.lock().unwrap() = Some(CursorImg {
								w,
								h,
								hot_x: hx,
								hot_y: hy,
								rgba,
							});
							CURSOR_IMG_GEN.fetch_add(1, Ordering::SeqCst);
						}
					}
				}
				// The host pointer is hidden / left the screen — stop drawing the side cursor.
				Some("cursorhide") => *CURSOR_POS.lock().unwrap() = None,
				// Network RTT from the keepalive ping/pong (ms, the "Gecikme" tile).
				Some("rtt") => {
					if let Some(ms) = it.next().and_then(|v| v.parse::<f32>().ok()) {
						RTT_DMS.store((ms * 10.0) as u32, Ordering::Relaxed);
					}
				}
				// Live mode switch for cross-session reconnects (game→remote or remote→game).
				// Updates the MODE_REMOTE atomic; the render loop re-reads it each frame so
				// OverlayState.mode and the pace ceiling update on the very next frame paint
				// without a renderer restart.
				Some("mode") => {
					if let Some(v) = it.next() {
						let remote = v == "remote";
						MODE_REMOTE.store(remote, Ordering::SeqCst);
						video::set_pace_ceiling(if remote { 3 } else { 2 });
					}
				}
				// Live codec switch / resident-reuse: reopen the demuxer+decoder on a
				// rewritten SDP IN PLACE — this process must survive (killing it corrupts
				// WebKit's shared Mali GL). When we are REUSED for a brand-new session we
				// must also discard the PREVIOUS session's overlay state — chat log, unread
				// badge, remote file listing, and the side-channel cursor — so the new
				// session starts clean and host-A's private data is never shown to host-B.
				Some("reopen") => {
					if let Some(p) = it.next() {
						// Reset per-session overlay statics before the new session begins.
						CHAT_LOG.lock().unwrap().clear();
						CHAT_UNREAD.store(0, Ordering::SeqCst);
						*FS_REMOTE.lock().unwrap() = (String::new(), Vec::new());
						*CURSOR_POS.lock().unwrap() = None;
						*CURSOR_IMG.lock().unwrap() = None;
						// Bump the generation so the render loop detects the clear and drops
						// the stale GPU texture — otherwise host-A's pointer bitmap stays
						// uploaded and is drawn over host-B's video on the resident-reuse path.
						CURSOR_IMG_GEN.fetch_add(1, Ordering::SeqCst);
						video::request_reopen(p);
					}
				}
				// View-fit mode (overlay Görüntü section / frontend persisted value).
				Some("fit") => {
					if let Some(v) = it.next() {
						video::set_fit(v);
					}
				}
				// Session audio truth from the app (atx/amute/mic 0|1) so the overlay's
				// Ses toggles highlight the real state.
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
				// Always-on stats HUD toggle (frontend setting / overlay row).
				Some("statshud") => {
					if let Some(v) = it.next() {
						STATS_HUD.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
					}
				}
				// The host's active encode summary (rest of the line, verbatim).
				Some("hostenc") => {
					let rest = line.splitn(2, ' ').nth(1).unwrap_or("").to_string();
					*HOST_ENC.lock().unwrap() = rest;
				}
				// Host caps + active request from the app:
				// `caps codecs=h264,h265 encoders=auto,software codec=h264 encoder=auto`
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
								"displays" => {
									*DISPLAYS.lock().unwrap() = crate::overlay::parse_displays(v)
								}
								"statshud" => STATS_HUD
									.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst),
								// Persisted overlay-button state rides the caps line so a
								// respawn (live codec switch) re-seeds it with everything else.
								"ovbtn" => OVERLAY_BTN
									.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst),
								"btnpos" => {
									if let Some((x, y)) = v.split_once(',') {
										if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>())
										{
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
				// Stream-selection seeds pushed by the app after a respawn (C14): the
				// renderer starts with OverlayState::default, so the app re-sends the
				// user's last picks over these verbs so the overlay shows the live state.
				// Take-once: stored in Option statics; the render loop drains them once.
				Some("res") => {
					if let Some(v) = it.next() {
						*RES_SEED.lock().unwrap() = Some(v.to_string());
					}
				}
				Some("fps") => {
					if let Some(v) = it.next() {
						*FPS_SEL_SEED.lock().unwrap() = Some(v.to_string());
					}
				}
				Some("bitrate") => {
					if let Some(v) = it.next() {
						*BITRATE_SEED.lock().unwrap() = Some(v.to_string());
					}
				}
				Some("quality") => {
					if let Some(v) = it.next() {
						*QUALITY_SEED.lock().unwrap() = Some(v.to_string());
					}
				}
				Some("display") => {
					if let Some(v) = it.next() {
						if let Ok(idx) = v.parse::<u32>() {
							DISPLAY_IDX_SEED.store(idx, Ordering::SeqCst);
						}
					}
				}
				_ => {}
			}
		}
	});
	if !sdp.is_empty() {
		video::start_decode(&sdp);
	}
	unsafe { real_run(wid, mode) };
}

/// Walk up the X window tree to the WM-level toplevel (the child of root) that contains
/// `w`. With the in-app container the `--wid` parent is a mid-tree child window — focusing
/// IT would yank X input focus off the GTK toplevel and kill the app's focus-gated combos,
/// so overlay clicks must refocus the real toplevel instead.
unsafe fn toplevel_of(xd: *mut xlib::Display, mut w: u64) -> u64 {
	loop {
		let (mut root, mut parent) = (0u64, 0u64);
		let mut children: *mut u64 = std::ptr::null_mut();
		let mut n: u32 = 0;
		if xlib::XQueryTree(xd, w, &mut root, &mut parent, &mut children, &mut n) == 0 {
			return w;
		}
		if !children.is_null() {
			xlib::XFree(children as *mut c_void);
		}
		if parent == 0 || parent == root {
			return w;
		}
		w = parent;
	}
}

unsafe fn win_size(xd: *mut xlib::Display, win: u64) -> (u32, u32) {
	let mut root = 0u64;
	let (mut x, mut y, mut bw, mut depth) = (0i32, 0i32, 0u32, 0u32);
	let (mut w, mut h) = (0u32, 0u32);
	if xlib::XGetGeometry(
		xd, win, &mut root, &mut x, &mut y, &mut w, &mut h, &mut bw, &mut depth,
	) != 0
	{
		(w.max(1), h.max(1))
	} else {
		(1, 1)
	}
}

/// Non-fatal Xlib error handler. Xlib's DEFAULT handler prints "X Error of failed
/// request" and EXITS the process — which killed the renderer mid-session whenever
/// an async X call hit a stale window id (proven live: `XSetInputFocus` on the
/// embedding toplevel after a fullscreen toggle re-created it → BadWindow → the
/// video froze and the overlay stopped taking clicks; only Ctrl+Shift+Q worked).
/// Every XID we touch (parent, focus_top) is owned by ANOTHER process (the Tauri
/// GTK window) and can die/respawn at any time, so a stale-id race is unavoidable —
/// log and carry on; the per-frame geometry/focus logic self-heals next frame.
unsafe extern "C" fn x_error_ignore(
	_xd: *mut xlib::Display,
	ev: *mut xlib::XErrorEvent,
) -> std::os::raw::c_int {
	let (code, req) = if ev.is_null() {
		(0, 0)
	} else {
		((*ev).error_code, (*ev).request_code)
	};
	eprintln!("pulsar-render: X error ignored (code={code} request={req})");
	0
}

unsafe fn real_run(wid: u64, mode: Mode) {
	let xd = xlib::XOpenDisplay(std::ptr::null());
	if xd.is_null() {
		eprintln!("pulsar-render: XOpenDisplay failed");
		return;
	}
	// Survive stale-XID races instead of dying with Xlib's exit-on-error default.
	xlib::XSetErrorHandler(Some(x_error_ignore));
	// No `--wid` = STANDALONE: the X11 embed into the app window failed (e.g. the client
	// runs native Wayland, so the Tauri toplevel has no XID). Then this is its own normal,
	// WM-managed window — WINDOWED by default (the user maximizes/fullscreens it themself),
	// NOT a root-sized surface covering the whole desktop. It also reports its focus and
	// video clicks on stdout (`ov focus 0|1` / `ov engage`) so the app's evdev capture can
	// gate on them (focusing this toplevel unfocuses the Tauri window).
	let standalone = wid == 0;
	let parent = if wid != 0 {
		wid
	} else {
		xlib::XDefaultRootWindow(xd)
	};
	// Embedded: the GTK toplevel that ultimately owns us (see toplevel_of) — the refocus
	// target for overlay clicks. The container parent itself must NOT take input focus.
	let focus_top = if standalone {
		0
	} else {
		toplevel_of(xd, parent)
	};
	let (mut w, mut h) = if standalone {
		(1280u32, 720u32)
	} else {
		win_size(xd, parent)
	};

	let egl = egl::Instance::new(egl::Static);
	let display = egl
		.get_display(xd as egl::NativeDisplayType)
		.expect("egl display");
	egl.initialize(display).expect("egl init");

	let attribs = [
		egl::SURFACE_TYPE,
		egl::WINDOW_BIT,
		egl::RENDERABLE_TYPE,
		egl::OPENGL_ES2_BIT,
		egl::RED_SIZE,
		8,
		egl::GREEN_SIZE,
		8,
		egl::BLUE_SIZE,
		8,
		egl::NONE,
	];
	let config = egl
		.choose_first_config(display, &attribs)
		.expect("choose")
		.expect("no config");

	// Child window uses the EGL config's native (opaque) visual — like the vidsink. Opaque is
	// correct here: video + egui share THIS framebuffer, there's no sibling to composite against.
	let vid = egl
		.get_config_attrib(display, config, egl::NATIVE_VISUAL_ID)
		.unwrap_or(0) as u64;
	let mut tmpl: xlib::XVisualInfo = std::mem::zeroed();
	tmpl.visualid = vid;
	let mut nret = 0i32;
	let vinfo = xlib::XGetVisualInfo(xd, xlib::VisualIDMask, &mut tmpl, &mut nret);
	let (visual, depth) = if !vinfo.is_null() && nret > 0 {
		((*vinfo).visual, (*vinfo).depth)
	} else {
		let s = xlib::XDefaultScreen(xd);
		(xlib::XDefaultVisual(xd, s), xlib::XDefaultDepth(xd, s))
	};
	let cmap = xlib::XCreateColormap(xd, parent, visual, xlib::AllocNone);
	if !vinfo.is_null() {
		xlib::XFree(vinfo as *mut c_void);
	}

	let mut swa: xlib::XSetWindowAttributes = std::mem::zeroed();
	swa.colormap = cmap;
	swa.background_pixel = 0;
	swa.border_pixel = 0;
	// EMBEDDED: start with NO pointer events (overlay closed) — a click on the video then
	// propagates to GTK → Pulsar refocuses → the evdev grab re-engages. Pointer events are
	// added when the overlay opens (egui needs them) and removed again on close.
	// STANDALONE: there is no parent to propagate to, so always select buttons (clicks =
	// engage) + focus changes (reported to the app, which can't see this window's focus).
	let base_mask = xlib::ExposureMask
		| xlib::StructureNotifyMask
		| if standalone {
			xlib::FocusChangeMask | xlib::ButtonPressMask | xlib::ButtonReleaseMask
		} else {
			0
		};
	swa.event_mask = base_mask;
	let valuemask = xlib::CWColormap | xlib::CWEventMask | xlib::CWBackPixel | xlib::CWBorderPixel;
	let win = xlib::XCreateWindow(
		xd,
		parent,
		0,
		0,
		w,
		h,
		0,
		depth,
		xlib::InputOutput as u32,
		visual,
		valuemask,
		&mut swa,
	);
	// Standalone: behave like a regular application window — title + close-button support
	// (WM_DELETE_WINDOW → clean stop instead of an X connection kill).
	let mut wm_delete: xlib::Atom = 0;
	if standalone {
		let title = std::ffi::CString::new("Pulsar").unwrap();
		xlib::XStoreName(xd, win, title.as_ptr());
		// WM_CLASS: without it taskbars fall back to the process name
		// ("pulsar-render"); match the app's class so the window groups as Pulsar.
		let mut class = xlib::XClassHint {
			res_name: b"pulsar\0".as_ptr() as *mut _,
			res_class: b"Pulsar\0".as_ptr() as *mut _,
		};
		xlib::XSetClassHint(xd, win, &mut class);
		wm_delete = xlib::XInternAtom(xd, b"WM_DELETE_WINDOW\0".as_ptr() as *const _, xlib::False);
		if wm_delete != 0 {
			xlib::XSetWMProtocols(xd, win, &mut wm_delete, 1);
		}
	}
	xlib::XMapWindow(xd, win);

	// Invisible cursor for the video window. While the overlay is CLOSED (gameplay) the local
	// pointer must not show over the video (input is forwarded to the host); while OPEN we restore
	// the default arrow so the user can click the egui overlay. STANDALONE keeps the normal
	// cursor: capture is click-to-engage there, and while disengaged the user is just hovering
	// a regular window — a vanishing pointer would read as broken.
	let mut blank: xlib::XColor = std::mem::zeroed();
	let zero = [0u8];
	// `.cast()`: c_char is u8 on aarch64 but i8 on x86_64 — keep both building.
	let pix = xlib::XCreateBitmapFromData(xd, win, zero.as_ptr().cast(), 1, 1);
	let invisible = xlib::XCreatePixmapCursor(xd, pix, pix, &mut blank, &mut blank, 0, 0);
	xlib::XFreePixmap(xd, pix);
	// The cursor starts VISIBLE: it only hides while input is ENGAGED (forwarded to the
	// host) with the overlay closed — see the per-frame cursor sync in the loop.
	xlib::XSync(xd, xlib::False);

	egl.bind_api(egl::OPENGL_ES_API).ok();
	let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
	let context = egl
		.create_context(display, config, None, &ctx_attribs)
		.expect("ctx");
	let surface = egl
		.create_window_surface(display, config, win as egl::NativeWindowType, None)
		.expect("surface");
	egl.make_current(display, Some(surface), Some(surface), Some(context))
		.expect("make_current");
	// VSync: 1 = sync to vblank (default). Under the GNOME/mutter compositor a windowed GL
	// app's own vblank-sync can beat against the compositor's redraw → periodic judder
	// ("mouse jumps") on smooth motion. PULSAR_VSYNC=0 lets us present without blocking so the
	// compositor paces us instead — A/B knob for the windowed-stutter investigation.
	let vsync: i32 = std::env::var("PULSAR_VSYNC")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(1);
	egl.swap_interval(display, vsync).ok();

	let get_proc = |s: &str| -> *const c_void {
		match egl.get_proc_address(s) {
			Some(p) => p as *const c_void,
			None => std::ptr::null(),
		}
	};
	let gl = Arc::new(glow::Context::from_loader_function(|s| get_proc(s)));

	let egui_ctx = egui::Context::default();
	overlay::apply_theme(&egui_ctx);
	let ppp = 1.25_f32;
	egui_ctx.set_pixels_per_point(ppp);
	let mut painter = egui_glow::Painter::new(gl.clone(), "", None, false).expect("painter");

	let dpy_ptr = display.as_ptr();
	let mut presenter = video::Presenter::new(&gl, dpy_ptr, &get_proc);

	let mut state = OverlayState {
		mode,
		open: false,
		id: "—".into(),
		conn_label: "P2P".into(),
		..Default::default()
	};

	let mut pointer = egui::pos2(0.0, 0.0);
	// Cursor side-channel egui texture: (re)uploaded from CURSOR_IMG when the generation moves.
	let mut cursor_tex: Option<egui::TextureHandle> = None;
	let mut cursor_tex_gen = 0u64;
	let mut cursor_hot = (0.0f32, 0.0f32);
	let mut cursor_dims = (0.0f32, 0.0f32);
	let mut geom_tick = 0u32;
	let mut last_stat = std::time::Instant::now();
	let mut prev_open = false;
	let mut prev_idle = false;
	let mut cursor_hidden = false;
	// Overlay UI state (page, chat composer, local file pane) — page resets to the
	// hub each time the overlay opens; the rest persists across opens.
	let mut ov_ui = overlay::UiState::default();

	while !STOP.load(Ordering::SeqCst) {
		// Idle (post-disconnect): unmap the video window so the WebKitGTK webview underneath is
		// revealed + interactive, but keep this process + its EGL context resident (exiting would
		// destroy the GL context and corrupt WebKit's shared Mali GL → the webview wedges with no
		// way back short of a reboot). Drain X events + sleep a frame; never draw/swap while idle.
		let idle = IDLE.load(Ordering::SeqCst);
		if idle != prev_idle {
			if idle {
				xlib::XUnmapWindow(xd, win);
			} else {
				xlib::XMapWindow(xd, win);
			}
			xlib::XSync(xd, xlib::False);
			prev_idle = idle;
		}
		if idle {
			while xlib::XPending(xd) > 0 {
				let mut ev: xlib::XEvent = std::mem::zeroed();
				xlib::XNextEvent(xd, &mut ev);
			}
			std::thread::sleep(std::time::Duration::from_millis(16));
			continue;
		}
		let open = OPEN.load(Ordering::SeqCst);
		state.open = open;
		// Reflect the live pacing state (set by the egui toggle OR a `pace 0|1` from the
		// frontend) so the overlay's segmented control highlights the real value.
		state.pace = video::pace_on();
		// Sync host caps + active selections (stdin `caps` line) and the ACTUAL decoder
		// (read-only) into the overlay state.
		if let Some((codecs, encoders, codec, encoder)) = CAPS.lock().unwrap().take() {
			state.host_codecs = codecs;
			state.host_encoders = encoders;
			if !codec.is_empty() {
				state.codec = codec;
			}
			if !encoder.is_empty() {
				state.encoder = encoder;
			}
		}
		// Host monitor list (persists; cloned each frame so the Display picker stays populated).
		state.displays = DISPLAYS.lock().unwrap().clone();
		// Stream-selection respawn seeds (C14): apply once when the app seeds a fresh
		// renderer; cleared after the first apply so the overlay can update them live.
		if let Some(v) = RES_SEED.lock().unwrap().take() {
			state.res = v;
		}
		if let Some(v) = FPS_SEL_SEED.lock().unwrap().take() {
			state.fps_sel = v;
		}
		if let Some(v) = BITRATE_SEED.lock().unwrap().take() {
			state.bitrate = v;
		}
		if let Some(v) = QUALITY_SEED.lock().unwrap().take() {
			state.quality = v;
		}
		{
			let idx = DISPLAY_IDX_SEED.load(Ordering::SeqCst);
			if idx != u32::MAX {
				state.display_idx = idx;
				DISPLAY_IDX_SEED.store(u32::MAX, Ordering::SeqCst);
			}
		}
		{
			let dec = video::DEC_LABEL.lock().unwrap();
			if !dec.is_empty() && state.decoder != *dec {
				state.decoder = dec.clone();
			}
		}
		{
			let he = HOST_ENC.lock().unwrap();
			if state.host_active != *he {
				state.host_active = he.clone();
			}
		}
		// Live mode switch: `mode game|remote` on stdin flips MODE_REMOTE; we mirror it
		// into state.mode each frame so the overlay menu boxes + look update immediately.
		state.mode = if MODE_REMOTE.load(Ordering::SeqCst) {
			Mode::Remote
		} else {
			Mode::Game
		};
		state.stats_hud = STATS_HUD.load(Ordering::SeqCst);
		state.overlay_btn = OVERLAY_BTN.load(Ordering::SeqCst);
		state.btn_pos = *OVBTN_POS.lock().unwrap();
		state.fit = video::fit_label().to_string();
		state.audio_tx = AUDIO_TX.load(Ordering::SeqCst);
		state.audio_mute = AUDIO_MUTE.load(Ordering::SeqCst);
		state.mic_on = MIC_ON.load(Ordering::SeqCst);
		state.chat = CHAT_LOG.lock().unwrap().clone();
		// Reading the chat clears the unread badge; new arrivals while the Chat view
		// is on screen count as read immediately.
		if open && ov_ui.view == overlay::View::Chat {
			CHAT_UNREAD.store(0, Ordering::SeqCst);
		}
		state.chat_unread = CHAT_UNREAD.load(Ordering::SeqCst);
		{
			let (p, rows) = &*FS_REMOTE.lock().unwrap();
			state.fs_remote_path = p.clone();
			state.fs_remote = rows.clone();
		}
		state.chat_enter = ENTER_IN.swap(false, Ordering::SeqCst);
		{
			let cl = CONN_LABEL.lock().unwrap();
			if !cl.is_empty() && state.conn_label != *cl {
				state.conn_label = cl.clone();
			}
		}
		if open != prev_open {
			// Drop keystrokes relayed across the edge (e.g. racing the close combo) so
			// they can't replay into the chat composer on the next open — the win
			// backend clears its EGUI_EVENTS the same way.
			KEY_IN.lock().unwrap().clear();
			ENTER_IN.store(false, Ordering::SeqCst);
			if open {
				// Fresh open always lands on the hub, not a stale section; the Files
				// view re-requests the remote listing once per visit.
				ov_ui.view = overlay::View::Root;
				ov_ui.remote_requested = false;
				// Select pointer events so egui gets clicks/motion while the overlay is
				// open. KEYBOARD stays with the GTK toplevel (focusing this child would
				// kill the focus-gated combos): the webview captures keydowns while the
				// overlay is open and relays them over stdin (`kin …`) for the Chat
				// composer — see Session.svelte.
				xlib::XSelectInput(
					xd,
					win,
					base_mask
						| xlib::ButtonPressMask
						| xlib::ButtonReleaseMask
						| xlib::PointerMotionMask,
				);
			} else {
				// Closing leaves the user DISENGAGED (the app released the grab when the
				// overlay opened) — prompt the click-to-control step.
				arm_hint("click");
				// Overlay CLOSED: back to the base mask. Embedded: no pointer events, so a click
				// on the video PROPAGATES to the GTK window → Pulsar refocuses → the evdev grab
				// re-engages (click-to-capture) — otherwise this opaque child window ate the
				// click and the user, once unfocused, could never click back into the session.
				// Standalone: buttons/focus stay selected (clicks engage via `ov engage`).
				xlib::XSelectInput(xd, win, base_mask);
			}
			prev_open = open;
		}
		// Cursor visibility tracks ENGAGE state, not the overlay: hidden only while input
		// is captured + forwarded (no local cursor over the video) — after a release
		// (Ctrl+Alt+Z / 3×RightCtrl) the user has their mouse back and must SEE it.
		let want_hidden = !standalone && !open && ENGAGED_R.load(Ordering::SeqCst);
		if want_hidden != cursor_hidden {
			if want_hidden {
				xlib::XDefineCursor(xd, win, invisible);
			} else {
				xlib::XUndefineCursor(xd, win);
			}
			cursor_hidden = want_hidden;
		}

		// EMBEDDED: track the parent size EVERY frame (XGetGeometry is ~µs) so the video follows
		// a fullscreen toggle INSTANTLY — the old 30-frame poll left the child the windowed size
		// for ~0.3 s after going fullscreen (visible glitch / wrong-size video). Moonlight-style:
		// the renderer fills whatever the app window currently is, with no lag.
		// STANDALONE: the WM resizes this window directly; size updates arrive as
		// ConfigureNotify events in the loop below (the "parent" is the root window — following
		// it would snap back to a desktop-sized surface).
		let _ = geom_tick;
		if !standalone {
			let (nw, nh) = win_size(xd, parent);
			if nw != w || nh != h {
				w = nw;
				h = nh;
				xlib::XResizeWindow(xd, win, w, h);
			}
		}
		geom_tick = geom_tick.wrapping_add(1);

		// X events → egui input.
		let mut events: Vec<egui::Event> = Vec::new();
		while xlib::XPending(xd) > 0 {
			let mut ev: xlib::XEvent = std::mem::zeroed();
			xlib::XNextEvent(xd, &mut ev);
			match ev.get_type() {
				xlib::MotionNotify => {
					let m = ev.motion;
					pointer = egui::pos2(m.x as f32 / ppp, m.y as f32 / ppp);
					events.push(egui::Event::PointerMoved(pointer));
				}
				// Standalone window resized/moved by the WM — adopt the new size (the EGL
				// surface tracks the X window automatically; w/h drive the GL viewport).
				xlib::ConfigureNotify => {
					let c = ev.configure;
					let (nw, nh) = ((c.width.max(1)) as u32, (c.height.max(1)) as u32);
					if standalone && (nw != w || nh != h) {
						w = nw;
						h = nh;
					}
				}
				// Standalone: report focus to the app — this toplevel's focus is invisible to
				// Tauri, and the evdev capture gates on "app OR render window focused".
				xlib::FocusIn | xlib::FocusOut if standalone => {
					println!(
						"ov focus {}",
						if ev.get_type() == xlib::FocusIn { 1 } else { 0 }
					);
					let _ = std::io::stdout().flush();
				}
				// Standalone: the WM close button → clean stop (same as SIGTERM).
				xlib::ClientMessage => {
					if standalone
						&& wm_delete != 0 && ev.client_message.data.get_long(0) as xlib::Atom
						== wm_delete
					{
						STOP.store(true, Ordering::SeqCst);
						video::signal_stop();
					}
				}
				xlib::ButtonPress | xlib::ButtonRelease => {
					let b = ev.button;
					let pressed = ev.get_type() == xlib::ButtonPress;
					// Overlay CLOSED (standalone only — embedded selects no buttons then): a
					// left-click on the video is the click-to-engage trigger. Tell the app so
					// the evdev capture takes the devices; the click itself stays local.
					// EXCEPT on the drawn overlay-open button: there is no webview hotspot
					// over this separate toplevel, so the renderer hit-tests the click itself
					// and asks the app to open the overlay instead of grabbing input.
					if !open {
						if standalone && pressed && b.button == 1 {
							let on_btn = OVERLAY_BTN.load(Ordering::SeqCst)
								&& overlay::btn_rect(
									*OVBTN_POS.lock().unwrap(),
									egui::Rect::from_min_size(
										egui::pos2(0.0, 0.0),
										egui::vec2(w as f32 / ppp, h as f32 / ppp),
									),
								)
								.contains(egui::pos2(b.x as f32 / ppp, b.y as f32 / ppp));
							println!("{}", if on_btn { "ov toggle" } else { "ov engage" });
							let _ = std::io::stdout().flush();
						}
						continue;
					}
					// A click into the overlay reaches THIS child window (it grabs pointer events
					// while open), so it never propagates to GTK → the app stays unfocused if it
					// was (e.g. the user alt-tabbed away). Without focus, FOCUSED stays false: the
					// close combo (Ctrl+Shift+M, focus-gated) can't fire and, on close, the evdev
					// grab never re-engages → the session is stranded with no input. Refocus the
					// parent (Tauri GTK toplevel) on press so Focused(true) fires; SUSPENDED keeps
					// the grab released while the overlay is open, so this is safe. Embedded only —
					// standalone has no embedding parent (root), and IS the focused window already.
					if pressed && wid != 0 && focus_top != 0 {
						xlib::XSetInputFocus(
							xd,
							focus_top,
							xlib::RevertToParent,
							xlib::CurrentTime,
						);
					}
					pointer = egui::pos2(b.x as f32 / ppp, b.y as f32 / ppp);
					let button = match b.button {
						1 => Some(egui::PointerButton::Primary),
						2 => Some(egui::PointerButton::Middle),
						3 => Some(egui::PointerButton::Secondary),
						// X11 wheel "buttons" 4-7: translate the press edge to an egui
						// scroll line so the overlay's ScrollAreas (chat log, file panes)
						// wheel-scroll — like the win backend's WM_MOUSEWHEEL arm.
						4..=7 if pressed => {
							events.push(egui::Event::MouseWheel {
								unit: egui::MouseWheelUnit::Line,
								delta: match b.button {
									4 => egui::vec2(0.0, 1.0),
									5 => egui::vec2(0.0, -1.0),
									6 => egui::vec2(1.0, 0.0),
									_ => egui::vec2(-1.0, 0.0),
								},
								modifiers: egui::Modifiers::default(),
							});
							None
						}
						_ => None,
					};
					if let Some(button) = button {
						events.push(egui::Event::PointerButton {
							pos: pointer,
							button,
							pressed,
							modifiers: egui::Modifiers::default(),
						});
					}
				}
				_ => {}
			}
		}

		// Live stats from the video presenter ([fps, mbit, max_gap_ms]). Previously only fps +
		// mbps were fed, so the overlay's "Gecikme ms" + "Çözme ms" tiles were stuck at 0.
		{
			let s = *video::FPS.lock().unwrap();
			if s[0] > 0.0 {
				state.fps = s[0];
			}
			// Bitrate: measured received-stream Mbit/s.
			state.mbps = s[1];
			// Latency: the NETWORK RTT from the keepalive ping/pong when the app feeds
			// it (stdin `rtt`); the local present-gap only as a fallback before the
			// first pong (it used to show render jitter and read as fake "lag").
			let rtt = RTT_DMS.load(Ordering::Relaxed) as f32 / 10.0;
			state.latency_ms = if rtt > 0.0 { rtt } else { s[2] };
			// Decode: measured per-frame decode time (µs → ms).
			state.decode_ms = video::DEC_US.load(Ordering::Relaxed) as f32 / 1000.0;
		}

		// ---- Render: video first (opaque), then egui overlay on top (blended) ----
		use glow::HasContext;
		gl.disable(glow::BLEND);
		gl.viewport(0, 0, w as i32, h as i32);
		gl.clear_color(0.0, 0.0, 0.0, 1.0);
		gl.clear(glow::COLOR_BUFFER_BIT);
		let have_video = presenter.draw(&gl, w as i32, h as i32);

		// Cursor side-channel: (re)upload the host pointer shape to an egui texture when it
		// changed, and resolve the screen-point to draw at from the video's letterbox rect.
		let cur_gen = CURSOR_IMG_GEN.load(Ordering::SeqCst);
		if cur_gen != cursor_tex_gen {
			cursor_tex_gen = cur_gen;
			if let Some(img) = CURSOR_IMG.lock().unwrap().clone() {
				let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba);
				cursor_tex = Some(egui_ctx.load_texture("pulsar-cursor", ci, egui::TextureOptions::LINEAR));
				cursor_hot = (img.hot_x, img.hot_y);
				cursor_dims = (img.w as f32, img.h as f32);
			} else {
				// CURSOR_IMG was cleared (e.g. reopen for a new host): drop the stale
				// GPU texture so we cannot paint the previous host's pointer shape.
				cursor_tex = None;
			}
		}
		// Where to draw it (egui points, top-left origin): map the normalized host pointer into
		// the video's letterbox rect. VIDEO_RECT is in framebuffer pixels with a BOTTOM-left GL
		// origin → flip Y. `None`/no-video/empty-rect = nothing to draw this frame.
		let cursor_draw: Option<(egui::Pos2, egui::TextureId, egui::Vec2)> = (|| {
			let (nx, ny) = (*CURSOR_POS.lock().unwrap())?;
			let tex = cursor_tex.as_ref()?;
			let r = *video::VIDEO_RECT.lock().unwrap();
			let (vx, vyb, vw, vh) = (r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32);
			if vw < 1.0 || vh < 1.0 || !have_video {
				return None;
			}
			// Host-pixel → displayed-pixel scale: the side-channel cursor bitmap/hotspot arrive
			// in raw host pixels, so its drawn size AND hotspot must scale by the same factor the
			// video is shown at, or the tip mis-aligns / the shape is the wrong size whenever the
			// stream isn't presented 1:1 (downscale = too big, upscale = too small).
			let src = *video::VIDEO_SRC.lock().unwrap();
			let (sw, sh) = (src[0] as f32, src[1] as f32);
			let (kx, ky) = (vw / sw.max(1.0), vh / sh.max(1.0));
			// GL bottom-left rect → top-left pixel rect, then px → egui points (÷ ppp).
			let vy_top = h as f32 - (vyb + vh);
			let px = vx + nx * vw;
			let py = vy_top + ny * vh;
			let pos = egui::pos2(
				(px - cursor_hot.0 * kx) / ppp,
				(py - cursor_hot.1 * ky) / ppp,
			);
			let size = egui::vec2(cursor_dims.0 * kx / ppp, cursor_dims.1 * ky / ppp);
			Some((pos, tex.id(), size))
		})();

		// Draw the side-channel cursor with egui's painter (its own layer, on top of the video).
		let paint_cursor = |ctx: &egui::Context| {
			if let Some((pos, id, size)) = cursor_draw {
				let painter = ctx.layer_painter(egui::LayerId::new(
					egui::Order::Foreground,
					egui::Id::new("pulsar-cursor-layer"),
				));
				painter.image(
					id,
					egui::Rect::from_min_size(pos, size),
					egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
					egui::Color32::WHITE,
				);
			}
		};

		if open {
			// Merge the webview-relayed keyboard (the Chat composer's only input
			// channel — see the `kin` stdin arm) into this frame's events.
			events.append(&mut KEY_IN.lock().unwrap());
			let raw_input = egui::RawInput {
				screen_rect: Some(egui::Rect::from_min_size(
					egui::pos2(0.0, 0.0),
					egui::vec2(w as f32 / ppp, h as f32 / ppp),
				)),
				events,
				..Default::default()
			};
			let mut cmds: Vec<OverlayCmd> = Vec::new();
			let full = egui_ctx.run(raw_input, |ctx| {
				cmds = overlay::draw(ctx, &state, &mut ov_ui);
				// "Switching screen…" indicator over the held last frame during a switch.
				if video::SWITCHING.load(Ordering::Relaxed) {
					overlay::draw_switching(ctx);
				}
				paint_cursor(ctx);
			});
			for c in cmds {
				emit_cmd(&mut state, c);
			}
			gl.viewport(0, 0, w as i32, h as i32);
			let prims = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
			painter.paint_and_update_textures(
				[w, h],
				full.pixels_per_point,
				&prims,
				&full.textures_delta,
			);
		} else {
			// Expire the transient helper hint after its window; fade it out smoothly
			// over the final HINT_FADE seconds (alpha 1→0).
			let hint: Option<(String, f32)> = {
				let mut g = HINT.lock().unwrap();
				let mut out = None;
				if let Some((text, t0, secs)) = g.as_ref() {
					let el = t0.elapsed().as_secs_f32();
					if el >= *secs {
						*g = None;
					} else {
						let alpha = ((secs - el) / HINT_FADE).min(1.0);
						out = Some((text.clone(), alpha));
					}
				}
				out
			};
			let hint_text = hint;
			let switching = video::SWITCHING.load(Ordering::Relaxed);
			if state.stats_hud || state.overlay_btn || hint_text.is_some() || cursor_draw.is_some() || switching
			{
				// Closed-state chrome: the mini stats HUD, the Parsec-style open button
				// and/or the helper tooltip. Display-only on Linux (the container is input
				// pass-through — the matching CLICK hotspot lives in the webview).
				// Also paints the "Switching screen…" spinner when a monitor/codec switch
				// is in progress, regardless of whether any other chrome is active.
				let raw_input = egui::RawInput {
					screen_rect: Some(egui::Rect::from_min_size(
						egui::pos2(0.0, 0.0),
						egui::vec2(w as f32 / ppp, h as f32 / ppp),
					)),
					events: Vec::new(),
					..Default::default()
				};
				let full = egui_ctx.run(raw_input, |ctx| {
					if switching {
						overlay::draw_switching(ctx);
					}
					if state.stats_hud {
						overlay::draw_hud(ctx, &state);
					}
					if state.overlay_btn {
						let _ = overlay::draw_open_button(ctx, &state);
					}
					if let Some((text, alpha)) = &hint_text {
						overlay::draw_hint(ctx, text, *alpha);
					}
					paint_cursor(ctx);
				});
				gl.viewport(0, 0, w as i32, h as i32);
				let prims = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
				painter.paint_and_update_textures(
					[w, h],
					full.pixels_per_point,
					&prims,
					&full.textures_delta,
				);
			}
		}

		egl.swap_buffers(display, surface).ok();
		let _ = have_video;

		// Emit a stats line for the host (~1 Hz): same format the lib.rs parser reads.
		if last_stat.elapsed().as_millis() >= 1000 {
			let s = *video::FPS.lock().unwrap();
			println!(
				"vidsink-fps {:.1} {}x{} {:.1} {:.0}",
				s[0], w, h, s[1], s[2]
			);
			let _ = std::io::stdout().flush();
			last_stat = std::time::Instant::now();
		}
	}

	// The render loop has exited, so the main thread no longer holds MBX: do the real
	// drain+free here (the SIGINT/SIGTERM handler only set the atomic — see on_stop).
	video::stop_decode();
	painter.destroy();
	egl.make_current(display, None, None, None).ok();
	xlib::XDestroyWindow(xd, win);
	xlib::XCloseDisplay(xd);
}

fn emit_cmd(state: &mut OverlayState, c: OverlayCmd) {
	match c {
		OverlayCmd::Set(field, val) => {
			match field {
				"codec" => state.codec = val.clone(),
				"encoder" => state.encoder = val.clone(),
				"decoder" => state.decoder = val.clone(),
				"res" => state.res = val.clone(),
				"fps" => state.fps_sel = val.clone(),
				"bitrate" => state.bitrate = val.clone(),
				"quality" => state.quality = val.clone(),
				// Host monitor pick: update the selection optimistically; the emitted
				// `ov set display <idx>` drives the app's set_play_monitor (host restream).
				"display" => {
					if let Ok(idx) = val.parse::<u32>() {
						state.display_idx = idx;
					}
				}
				// Pacing flips the renderer locally AT ONCE (instant feel) and is also emitted
				// so the frontend persists it as the default for next session.
				"pace" => {
					state.pace = val == "on";
					video::set_pace(val == "on");
				}
				// Local-immediate like pacing; the frontend persists it and re-syncs
				// over stdin on later sessions.
				"statshud" => {
					state.stats_hud = val == "on";
					STATS_HUD.store(val == "on", Ordering::SeqCst);
				}
				"ovbtn" => {
					state.overlay_btn = val == "on";
					OVERLAY_BTN.store(val == "on", Ordering::SeqCst);
				}
				// View fit is renderer-local (instant) + forwarded so the frontend
				// mirrors it (its own canvas path / persistence).
				"fit" => video::set_fit(&val),
				// Audio toggles apply optimistically; the app's `audio …` line
				// re-syncs the truth after the host acknowledges.
				"atx" => AUDIO_TX.store(val == "on", Ordering::SeqCst),
				"amute" => AUDIO_MUTE.store(val == "on", Ordering::SeqCst),
				"mic" => MIC_ON.store(val == "on", Ordering::SeqCst),
				// Voice call = mic + host audio together (paired optimistic update).
				// ON enables BOTH; OFF drops ONLY the mic and leaves host audio as-is
				// (it has its own `atx` row). The overlay highlight derives from MIC_ON
				// alone (overlay::draw_audio), so highlight + state stay in sync.
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
		// Native Chat / Files traffic — rest-of-line payloads the app parses
		// (`render_stats`): chat line out, remote ls/download, local upload.
		OverlayCmd::Chat(text) => println!("ov chat {}", text.replace('\n', " ")),
		OverlayCmd::FsLs(p) => println!("ov fsls {p}"),
		OverlayCmd::FsGet(p) => println!("ov fsget {p}"),
		OverlayCmd::FsSend(p) => println!("ov fssend {p}"),
		OverlayCmd::OpenFiles => println!("ov files"),
	}
	let _ = std::io::stdout().flush();
}
