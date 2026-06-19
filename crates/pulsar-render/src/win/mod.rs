//! Windows native renderer backend — the Win32 analogue of `linux.rs`.
//!
//! Moonlight-style, zero-copy: a CHILD HWND of the Tauri window (`--wid <hwnd>`, like Linux's
//! X11 `--wid`), a D3D11 device + DXGI swapchain on it, the host's RTP stream HW-decoded by
//! Media Foundation (DXVA → `ID3D11Texture2D`, no ffmpeg), colour-converted NV12→RGB by an
//! `ID3D11VideoProcessor`, and presented on the swapchain. The SAME `overlay.rs` egui UI is
//! composited on top. NO webview, NO GPU→CPU download.
//!
//! Build order (this file grows in layers, each compile-clean):
//!   1. [DONE] child window + D3D11 swapchain + clear-color present + geometry tracking + IPC.
//!   2. Media Foundation decode (`decode.rs`) → present the decoded texture.
//!   3. egui overlay on the swapchain.
//!
//! Usage: pulsar-render <stream.sdp> --wid <hwnd-decimal-or-0xhex> [--mode game|remote] [--pace on|off]
//! IPC (identical to linux.rs / desktop.rs so `play.rs` wiring is unchanged):
//!   stdin:  `open` / `close` (overlay), `stat <fps> <lat> <dec> <mbps>`, `pace 0|1`
//!   stdout: `vidsink-fps <fps> <w>x<h>` (HUD), `ov set <field> <val>` / `ov end` / `ov close`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub mod decode; // Media Foundation DXVA decode (Annex-B AU → NV12 ID3D11Texture2D)
pub mod egui_paint; // minimal D3D11 painter for the egui overlay
pub mod present; // NV12 → swapchain RGB via ID3D11VideoProcessor (letterbox)

// Shared (cross-backend) streaming types + RTP depacketizer.
use crate::stream::{self, parse_sdp, AccessUnit, Codec};

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
	D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
	D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
	IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC1,
	DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
	CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, PeekMessageW, RegisterClassW,
	SetWindowPos, TranslateMessage, CW_USEDEFAULT, HMENU, HWND_TOP, MSG, PM_REMOVE,
	SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WNDCLASSW, WS_CHILD, WS_VISIBLE,
};

// ---- shared control state (host → renderer over stdin) ---------------------------------
static OPEN: AtomicBool = AtomicBool::new(false);
static PACE: AtomicBool = AtomicBool::new(true);
static STATS: Mutex<[f32; 4]> = Mutex::new([0.0; 4]); // fps, latency_ms, decode_ms, mbps
/// Parent (Tauri) HWND passed via `--wid`; the child window is created under it.
static PARENT: AtomicU64 = AtomicU64::new(0);
/// In-app embed rect (stdin `viewrect <x> <y> <w> <h>`, PHYSICAL px in the parent's
/// client space): the frontend reports the session tab's content area so the video
/// renders INSIDE the app — chrome/tabs stay visible, like the Linux native container.
/// None = no report yet → fill the whole parent client area. A 0×0 rect hides the
/// child (tab inactive / session screen unmounted).
static VIEW_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

// ---- Win32 → egui input plumbing -------------------------------------------------------
// `wndproc` (a free fn) collects input into these statics; `paint_overlay` drains them into a
// `egui::RawInput` each frame. Only active while the overlay is OPEN (we SetCapture then).
static EGUI_EVENTS: Mutex<Vec<egui::Event>> = Mutex::new(Vec::new());
static POINTER: Mutex<(f32, f32)> = Mutex::new((0.0, 0.0));
/// Chat log fed over stdin (`chat in|out <text>`, same protocol as linux.rs) —
/// shown in the overlay's Chat view while it is open.
static CHAT_LOG: Mutex<Vec<(bool, String)>> = Mutex::new(Vec::new());
/// Host messages not yet seen in the overlay Chat view — badge on the open button.
static CHAT_UNREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Files view, REMOTE pane: (HOME-relative path, rows) from `fsjson …` (same
/// one-line JSON as linux.rs) — copied into the overlay state each frame.
static FS_REMOTE: Mutex<(String, Vec<crate::overlay::FsRow>)> =
	Mutex::new((String::new(), Vec::new()));
/// Relayed Enter (`kin k enter`) — consumed by the Chat composer as "send".
static ENTER_IN: AtomicBool = AtomicBool::new(false);
/// Network RTT in tenths of ms (app stdin `rtt <ms>`, from the keepalive ping/pong) —
/// the overlay's "Gecikme" tile (same as linux.rs RTT_DMS).
static RTT_DMS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
// ---- closed-state chrome (Linux parity: linux.rs statics of the same names) ----
/// Always-on mini stats HUD while the overlay is closed (`statshud 0|1` / caps seed).
static STATS_HUD: AtomicBool = AtomicBool::new(false);
/// Parsec-style overlay-open button while closed (`ovbtn 0|1` / caps seed). Default ON
/// like linux.rs — the frontend only sends `ovbtn` when the user turned it OFF.
static OVERLAY_BTN: AtomicBool = AtomicBool::new(true);
/// Overlay-open button top-left in egui POINTS (`ovbtnpos x y` / caps `btnpos=`).
static OVBTN_POS: Mutex<(f32, f32)> = Mutex::new(crate::overlay::BTN_POS_DEFAULT);
/// Live engage state from the app (`engaged 0|1`) — drives cursor visibility.
static ENGAGED_R: AtomicBool = AtomicBool::new(false);
/// Session audio truth (app stdin `audio tx=1 mute=0 mic=0`) for the overlay's Ses
/// section — same statics + defaults as linux.rs; synced into `ostate` each frame so
/// the Ses toggles highlight the real state (the app re-seeds these on respawn).
static AUDIO_TX: AtomicBool = AtomicBool::new(true);
static AUDIO_MUTE: AtomicBool = AtomicBool::new(false);
static MIC_ON: AtomicBool = AtomicBool::new(false);
/// Transient helper tooltip / toast: (text, armed-at, visible-secs).
static HINT: Mutex<Option<(String, std::time::Instant, f32)>> = Mutex::new(None);
/// Host's active encode summary (`hostenc <label>` stdin) — the overlay's per-field
/// host stats ("kodlama ms", target Mbit) parse out of this (overlay::host_parts).
static HOST_ENC: Mutex<String> = Mutex::new(String::new());
/// Caps line payload (codecs, encoders, active codec, active encoder, conn label) —
/// applied to the overlay state on the next paint.
#[allow(clippy::type_complexity)]
static CAPS_SEED: Mutex<Option<(Vec<String>, Vec<String>, String, String, String)>> =
	Mutex::new(None);
/// Stream-selection respawn seeds (C14): pushed by the app over stdin after a codec/monitor
/// switch respawn so the fresh renderer's overlay shows the user's last picks, not defaults.
/// Take-once (Option/sentinel) so the overlay can update them live after the initial seed.
static RES_SEED: Mutex<Option<String>> = Mutex::new(None);
static FPS_SEL_SEED: Mutex<Option<String>> = Mutex::new(None);
static BITRATE_SEED: Mutex<Option<String>> = Mutex::new(None);
static QUALITY_SEED: Mutex<Option<String>> = Mutex::new(None);
/// `u32::MAX` = not set (sentinel); any real display index is < MAX.
static DISPLAY_IDX_SEED: std::sync::atomic::AtomicU32 =
	std::sync::atomic::AtomicU32::new(u32::MAX);
// Cursor side-channel state (Moonlight model, mirrors linux.rs): the host captured
// WITHOUT a hardware cursor (KMS zero-copy) and streams the pointer out-of-band; WE
// draw it over the video. Fed over stdin: `cursor <x> <y>` (normalized 0..1),
// `cursorimg w h hx hy <b64png>`, `cursorhide`.
static CURSOR_POS: Mutex<Option<(f32, f32)>> = Mutex::new(None);
static CURSOR_IMG: Mutex<Option<CursorImg>> = Mutex::new(None);
static CURSOR_IMG_GEN: AtomicU64 = AtomicU64::new(0);
/// True while the stream is stalled (no AU for ≥ STALL_SECS and decoder was live).
/// The render loop sets this; it is cleared as soon as AUs start arriving again.
/// Mirrors video::STALLED on Linux — kept separate because video.rs is unix-only.
static STALLED: AtomicBool = AtomicBool::new(false);
/// Connected controllers pushed by the app over stdin (`ctrls slot:kind:name[:uuid:target],...`).
/// Game mode only; empty list in remote mode or when no pads are connected. Copied
/// into `ostate.controllers` each frame (paint_overlay + paint_closed parity).
/// Tuple: (slot, kind_label, device_name, uuid, target, rumble, disabled). Legacy short lines
/// default the missing tail. `rumble` = per-pad vibration level; `disabled` = pad toggled off.
static CONTROLLERS: Mutex<Vec<(u8, String, String, String, String, String, bool)>> = Mutex::new(Vec::new());
/// SPLIT MODE: pad uuids LOCKED to THIS session (this renderer == one pane), parsed from the
/// `ctrls` line's 8th per-pad field. Copied into `ostate.controllers_locked` each frame. Stored
/// as a Vec (const-constructible — `HashSet::new()` is not const) and collected on read. Empty
/// when split mode is off. Mirrors linux.rs's CONTROLLERS_LOCKED.
static CONTROLLERS_LOCKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct CursorImg {
	w: usize,
	h: usize,
	hot_x: f32,
	hot_y: f32,
	rgba: Vec<u8>,
}

/// Decode the side-channel cursor PNG (RGBA, ≤256², dims must match) — same
/// validation as linux.rs::decode_cursor_png.
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

/// Minimal standard-base64 decoder (same as linux.rs — no base64 crate for one payload).
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
	let mut out = Vec::with_capacity(s.len() * 3 / 4);
	let (mut acc, mut bits) = (0u32, 0u32);
	for &c in s.as_bytes() {
		if c == b'=' || c == b'\r' || c == b'\n' || c == b' ' {
			continue;
		}
		acc = (acc << 6) | val(c)? as u32;
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((acc >> bits) as u8);
		}
	}
	Some(out)
}

/// Live overlay-button drag (closed state): (down.x, down.y, btn0.x, btn0.y, moved) in
/// egui points. The webview drag hotspot is buried under this child on Windows, so the
/// renderer owns the drag: ≤3 pt = click (toggle), more = move + persist via
/// `ov set btnpos x,y`.
static BTN_DRAG: Mutex<Option<(f32, f32, f32, f32, bool)>> = Mutex::new(None);
const HINT_SECS: f32 = 3.0;
const HINT_FADE: f32 = 0.5;
const TOAST_SECS: f32 = 6.0;

fn arm_hint(kind: &str) {
	let text = crate::i18n::t(if kind == "engage" {
		"hint.engage"
	} else {
		"hint.click"
	});
	*HINT.lock().unwrap() = Some((text.to_string(), std::time::Instant::now(), HINT_SECS));
}

/// Overlay scale (egui points-per-physical-pixel). MUST match the Linux backend (`linux.rs`
/// ppp=1.25) so the overlay is the IDENTICAL physical size + look on every platform.
const OVERLAY_PPP: f32 = 1.25;

/// Mouse position from an `LPARAM`, in egui POINTS (physical px ÷ ppp), matching linux.rs.
fn lparam_xy(lp: LPARAM) -> egui::Pos2 {
	let x = (lp.0 & 0xffff) as i16 as f32 / OVERLAY_PPP;
	let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / OVERLAY_PPP;
	egui::pos2(x, y)
}

/// stdin reader (same protocol as linux.rs/desktop.rs): `open`/`close` toggle the overlay,
/// `stat …` feeds the HUD, `pace 0|1` toggles frame pacing, `chat`/`fsjson`/`kin` feed the
/// overlay's Chat + Files views (the app sends these platform-agnostically).
fn stdin_control() {
	use std::io::BufRead;
	let stdin = std::io::stdin();
	for line in stdin.lock().lines() {
		let Ok(l) = line else { break };
		let l = l.trim();
		if l == "open" {
			OPEN.store(true, Ordering::SeqCst);
		} else if l == "close" {
			OPEN.store(false, Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("stat ") {
			let v: Vec<f32> = rest
				.split_whitespace()
				.filter_map(|x| x.parse().ok())
				.collect();
			if v.len() >= 4 {
				*STATS.lock().unwrap() = [v[0], v[1], v[2], v[3]];
			}
		} else if let Some(rest) = l.strip_prefix("pace ") {
			PACE.store(rest.trim() != "0", Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("cursor ") {
			// Side-channel cursor position (normalized 0..1 over the host frame).
			let v: Vec<f32> = rest
				.split_whitespace()
				.filter_map(|x| x.parse().ok())
				.collect();
			if v.len() >= 2 {
				*CURSOR_POS.lock().unwrap() = Some((v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0)));
			}
		} else if let Some(rest) = l.strip_prefix("cursorimg ") {
			let mut it = rest.split_whitespace();
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
		} else if l == "cursorhide" {
			*CURSOR_POS.lock().unwrap() = None;
		} else if let Some(rest) = l.strip_prefix("engaged ") {
			// Live engage state (cursor visibility) — app-side edges, like linux.rs.
			let v = rest.trim();
			ENGAGED_R.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("audio ") {
			// Session audio truth from the app (`audio tx=1 mute=0 mic=0`) so the
			// overlay's Ses toggles highlight the real state — same key=value parse
			// as linux.rs (the app sends this platform-agnostically + on respawn).
			for kv in rest.split_whitespace() {
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
		} else if let Some(rest) = l.strip_prefix("hint ") {
			arm_hint(rest.trim());
		} else if let Some(rest) = l.strip_prefix("toast ") {
			let rest = rest.trim();
			if !rest.is_empty() {
				*HINT.lock().unwrap() =
					Some((rest.to_string(), std::time::Instant::now(), TOAST_SECS));
			}
		} else if let Some(rest) = l.strip_prefix("fit ") {
			// View-fit mode pushed by the frontend (persisted value / respawn re-seed).
			present::set_fit(rest.trim());
		} else if let Some(rest) = l.strip_prefix("hostenc ") {
			*HOST_ENC.lock().unwrap() = rest.trim().to_string();
		} else if let Some(rest) = l.strip_prefix("statshud ") {
			let v = rest.trim();
			STATS_HUD.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("ovbtn ") {
			let v = rest.trim();
			OVERLAY_BTN.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("ovbtnpos ") {
			let v: Vec<f32> = rest
				.split_whitespace()
				.filter_map(|x| x.parse().ok())
				.collect();
			if v.len() >= 2 {
				*OVBTN_POS.lock().unwrap() = (v[0], v[1]);
			}
		} else if let Some(rest) = l.strip_prefix("caps ") {
			// Host caps + active request + persisted chrome seeds — same fields as
			// linux.rs: codecs/encoders filter the overlay menu rows, codec/encoder
			// preselect, conn labels the transport, statshud/ovbtn/btnpos re-seed
			// the closed-state chrome after a respawn.
			let (mut codecs, mut encoders) = (Vec::new(), Vec::new());
			let (mut codec, mut encoder, mut conn) = (String::new(), String::new(), String::new());
			for kv in rest.split_whitespace() {
				if let Some((k, v)) = kv.split_once('=') {
					let on = v == "1" || v == "on" || v == "true";
					match k {
						"codecs" => codecs = v.split(',').map(str::to_string).collect(),
						"encoders" => encoders = v.split(',').map(str::to_string).collect(),
						"codec" => codec = v.to_string(),
						"encoder" => encoder = v.to_string(),
						"conn" => conn = v.to_string(),
						"statshud" => STATS_HUD.store(on, Ordering::SeqCst),
						"ovbtn" => OVERLAY_BTN.store(on, Ordering::SeqCst),
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
			*CAPS_SEED.lock().unwrap() = Some((codecs, encoders, codec, encoder, conn));
		} else if let Some(rest) = l.strip_prefix("rtt ") {
			// Keepalive RTT from the app — overrides the latency tile (linux.rs parity).
			if let Ok(ms) = rest.trim().parse::<f32>() {
				RTT_DMS.store((ms * 10.0) as u32, Ordering::Relaxed);
			}
		} else if let Some(rest) = l.strip_prefix("viewrect ") {
			// In-app embed rect (physical px) — see VIEW_RECT above.
			let v: Vec<i32> = rest
				.split_whitespace()
				.filter_map(|x| x.parse().ok())
				.collect();
			if v.len() >= 4 {
				*VIEW_RECT.lock().unwrap() = Some((v[0], v[1], v[2], v[3]));
			}
		} else if let Some(rest) = l.strip_prefix("chat ") {
			// `chat in <text>` (host) / `chat out <text>` (our own, echoed by the app).
			let mut it = rest.splitn(2, ' ');
			let dir = it.next().unwrap_or("in");
			let text = it.next().unwrap_or("").trim().to_string();
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
		} else if let Some(json) = l.strip_prefix("fsjson ") {
			// Remote file listing for the Files view (same one-line JSON as linux.rs):
			// {"path":"…","entries":[{"name":…,"dir":…,"size":…},…]}.
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
		} else if let Some(rest) = l.strip_prefix("kin ") {
			// Relayed keyboard for the Chat composer (`kin t <text>` / `kin k <name>`,
			// same protocol as linux.rs) — wndproc translates no WM_CHAR/WM_KEYDOWN.
			let mut it = rest.splitn(2, ' ');
			match it.next() {
				Some("t") => {
					let text = it.next().unwrap_or("").to_string();
					if !text.is_empty() {
						EGUI_EVENTS.lock().unwrap().push(egui::Event::Text(text));
					}
				}
				Some("k") => {
					let key = match it.next().map(str::trim) {
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
						let mut q = EGUI_EVENTS.lock().unwrap();
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
			}
		// Stream-selection respawn seeds (C14 — mirrors linux.rs): the app pushes these
		// after a codec/monitor-switch respawn so the fresh renderer's Stream/Display views
		// show the user's last picks instead of falling back to built-in defaults.
		} else if let Some(rest) = l.strip_prefix("res ") {
			*RES_SEED.lock().unwrap() = Some(rest.trim().to_string());
		} else if let Some(rest) = l.strip_prefix("fps ") {
			*FPS_SEL_SEED.lock().unwrap() = Some(rest.trim().to_string());
		} else if let Some(rest) = l.strip_prefix("bitrate ") {
			*BITRATE_SEED.lock().unwrap() = Some(rest.trim().to_string());
		} else if let Some(rest) = l.strip_prefix("quality ") {
			*QUALITY_SEED.lock().unwrap() = Some(rest.trim().to_string());
		} else if let Some(rest) = l.strip_prefix("display ") {
			if let Ok(idx) = rest.trim().parse::<u32>() {
				DISPLAY_IDX_SEED.store(idx, Ordering::SeqCst);
			}
		} else if let Some(rest) = l.strip_prefix("ctrls ") {
			// Connected controller list (game mode only):
			// `ctrls slot:kind:name:uuid:target:rumble,...`. 6-field (per-pad vibration);
			// 5/3-field legacy forms default the missing tail (rumble="medium", etc).
			// Underscores for spaces in kind/name; uuid/target/rumble are never underscored.
			// Mirrors linux.rs's CONTROLLERS static + parse (same protocol).
			let rest = rest.trim();
			let mut locked: Vec<String> = Vec::new();
			let list: Vec<(u8, String, String, String, String, String, bool)> = if rest.is_empty() {
				Vec::new()
			} else {
				rest.split(',')
					.filter_map(|e| {
						let mut p = e.splitn(8, ':');
						let slot: u8 = p.next()?.parse().ok()?;
						let kind = p.next()?.replace('_', " ");
						let name = p.next()?.replace('_', " ");
						let uuid = p.next().unwrap_or("").to_string();
						let target = p.next().unwrap_or("auto").to_string();
						let rumble = p.next().unwrap_or("medium").to_string();
						let disabled = p.next() == Some("1");
						// SPLIT MODE: 8th field = locked-to-this-session (1). Absent on
						// 7-field (split-off) lines → defaults unlocked.
						if p.next() == Some("1") {
							locked.push(uuid.clone());
						}
						Some((slot, kind, name, uuid, target, rumble, disabled))
					})
					.collect()
			};
			*CONTROLLERS.lock().unwrap() = list;
			*CONTROLLERS_LOCKED.lock().unwrap() = locked;
		}
		// `toast <text>` (and `hint …`) are drawn by the closed-state paint pass
		// (`paint_closed` via `draw_hint`), alongside the mini stats HUD and the
		// overlay-open button — see paint_closed below.
	}
}

/// Draw the side-channel cursor with egui's painter (own foreground layer, over the
/// video and under nothing) — same shape as linux.rs's paint_cursor closure.
fn paint_side_cursor(ctx: &egui::Context, draw: Option<(egui::Pos2, egui::TextureId, egui::Vec2)>) {
	if let Some((pos, id, size)) = draw {
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
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
	use windows::Win32::UI::WindowsAndMessaging::{
		WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
		WM_RBUTTONDOWN, WM_RBUTTONUP,
	};
	// ---- Capture-leak guard (races the OPEN flag) ----------------------------------
	// If OPEN flips false→true between WM_LBUTTONDOWN (SetCapture in closed state) and
	// the matching WM_LBUTTONUP, the up-edge handler below is skipped (it lives inside
	// the `!OPEN` block), so the mouse capture leaks and BTN_DRAG stays armed.
	// Fix: unconditional handlers placed BEFORE the OPEN gate.
	//
	// WM_CAPTURECHANGED — system or another window stole capture; just disarm.
	// (Do NOT call ReleaseCapture here — we no longer hold capture.)
	if msg == WM_CAPTURECHANGED {
		// Use try_lock so that if the lock is already held (e.g. ReleaseCapture was called
		// from inside the WM_LBUTTONUP handler while the guard was still live) this
		// re-entrant call skips instead of deadlocking the render thread.
		if let Ok(mut g) = BTN_DRAG.try_lock() {
			*g = None;
		}
		return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
	}
	// WM_LBUTTONUP while OPEN is true — release a capture that was set while CLOSED
	// but not yet released (the OPEN gate flipped between DOWN and UP).
	// Drop the event so it is not injected into egui as a phantom click.
	if msg == WM_LBUTTONUP && OPEN.load(Ordering::SeqCst) {
		if BTN_DRAG.lock().unwrap().take().is_some() {
			use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
			unsafe {
				let _ = ReleaseCapture();
			}
			return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
		}
	}
	// ---- end capture-leak guard ----------------------------------------------------

	// Overlay CLOSED: hide the cursor while input is captured (engage state from the
	// app), and treat a left-click as either the overlay-open button (hit-test — the
	// webview hotspot is buried under this child on Windows) or click-to-engage.
	if !OPEN.load(Ordering::SeqCst) {
		use windows::Win32::UI::WindowsAndMessaging::{SetCursor, WM_SETCURSOR};
		if msg == WM_SETCURSOR && ENGAGED_R.load(Ordering::SeqCst) {
			unsafe { SetCursor(None) };
			return LRESULT(1);
		}
		if msg == WM_LBUTTONDOWN {
			use std::io::Write as _;
			// Button hit-test in egui POINTS (same clamp as overlay::draw_open_button).
			let p = lparam_xy(lp);
			let mut rc = RECT::default();
			let _ = unsafe { GetClientRect(hwnd, &mut rc) };
			let (sw, sh) = (
				(rc.right - rc.left) as f32 / OVERLAY_PPP,
				(rc.bottom - rc.top) as f32 / OVERLAY_PPP,
			);
			let mut hit_btn = false;
			let (mut bx, mut by) = *OVBTN_POS.lock().unwrap();
			if OVERLAY_BTN.load(Ordering::SeqCst) {
				let bs = crate::overlay::BTN_SIZE;
				bx = bx.clamp(0.0, (sw - bs).max(0.0));
				by = by.clamp(0.0, (sh - bs).max(0.0));
				hit_btn = p.x >= bx && p.x <= bx + bs && p.y >= by && p.y <= by + bs;
			}
			if hit_btn {
				// Click vs drag is decided on the UP edge (≤3 pt = click → toggle).
				use windows::Win32::UI::Input::KeyboardAndMouse::SetCapture;
				unsafe { SetCapture(hwnd) };
				*BTN_DRAG.lock().unwrap() = Some((p.x, p.y, bx, by, false));
			} else {
				// Click-to-engage; the engaging click is not forwarded to the host.
				println!("ov engage");
				let _ = std::io::stdout().flush();
			}
			return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
		}
		if msg == WM_MOUSEMOVE {
			let mut g = BTN_DRAG.lock().unwrap();
			if let Some((dx0, dy0, bx0, by0, moved)) = g.as_mut() {
				let p = lparam_xy(lp);
				let (dx, dy) = (p.x - *dx0, p.y - *dy0);
				if dx.abs() + dy.abs() > 3.0 {
					*moved = true;
				}
				if *moved {
					let mut rc = RECT::default();
					let _ = unsafe { GetClientRect(hwnd, &mut rc) };
					let (sw, sh) = (
						(rc.right - rc.left) as f32 / OVERLAY_PPP,
						(rc.bottom - rc.top) as f32 / OVERLAY_PPP,
					);
					let bs = crate::overlay::BTN_SIZE;
					let nx = (*bx0 + dx).clamp(0.0, (sw - bs).max(0.0));
					let ny = (*by0 + dy).clamp(0.0, (sh - bs).max(0.0));
					*OVBTN_POS.lock().unwrap() = (nx, ny); // live visual follow
				}
				return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
			}
		}
		if msg == WM_LBUTTONUP {
			// DEADLOCK FIX: bind-and-drop the MutexGuard in a let-statement so it is
			// released at the trailing ';' BEFORE ReleaseCapture() fires.  If we kept
			// the guard alive inside an `if let` scrutinee the Mutex would still be
			// locked when ReleaseCapture() synchronously sends WM_CAPTURECHANGED back
			// into this same thread, hitting the handler above and trying to re-lock
			// the non-reentrant Mutex → deadlock (render thread frozen permanently).
			let taken = BTN_DRAG.lock().unwrap().take(); // guard dropped here at ';'
			if let Some((_, _, _, _, moved)) = taken {
				use std::io::Write as _;
				use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
				unsafe {
					let _ = ReleaseCapture();
				}
				if moved {
					let (x, y) = *OVBTN_POS.lock().unwrap();
					// Persist through the app (frontend mirrors + re-seeds on respawn).
					println!("ov set btnpos {x:.0},{y:.0}");
				} else {
					println!("ov toggle");
				}
				let _ = std::io::stdout().flush();
				return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
			}
		}
	}
	// Only collect input while the overlay is open (we hold mouse capture then).
	if OPEN.load(Ordering::SeqCst) {
		let mut ev = EGUI_EVENTS.lock().unwrap();
		let m = egui::Modifiers::default();
		match msg {
			WM_MOUSEMOVE => {
				let p = lparam_xy(lp);
				*POINTER.lock().unwrap() = (p.x, p.y);
				ev.push(egui::Event::PointerMoved(p));
			}
			WM_LBUTTONDOWN | WM_LBUTTONUP => {
				let p = lparam_xy(lp);
				ev.push(egui::Event::PointerButton {
					pos: p,
					button: egui::PointerButton::Primary,
					pressed: msg == WM_LBUTTONDOWN,
					modifiers: m,
				});
			}
			WM_RBUTTONDOWN | WM_RBUTTONUP => {
				let p = lparam_xy(lp);
				ev.push(egui::Event::PointerButton {
					pos: p,
					button: egui::PointerButton::Secondary,
					pressed: msg == WM_RBUTTONDOWN,
					modifiers: m,
				});
			}
			WM_MOUSEWHEEL => {
				let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
				ev.push(egui::Event::MouseWheel {
					unit: egui::MouseWheelUnit::Line,
					delta: egui::vec2(0.0, delta),
					modifiers: m,
				});
			}
			_ => {}
		}
	}
	unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The D3D11 device + swapchain + child window + the decode→present chain.
struct Renderer {
	hwnd: HWND,
	parent: HWND,
	device: ID3D11Device,
	context: ID3D11DeviceContext,
	swap: IDXGISwapChain1,
	rtv: Option<ID3D11RenderTargetView>,
	width: u32,
	height: u32,
	// Child position within the parent client area (frontend `viewrect`; 0,0 = full fill).
	x: i32,
	y: i32,
	// Video chain (built lazily): MF decoder + NV12→RGB VideoProcessor.
	decoder: Option<decode::Decoder>,
	present: Option<present::Present>,
	codec: Codec,
	// Decoded source size (from the NV12 texture); drives the letterbox + processor rebuild.
	src_w: u32,
	src_h: u32,
	// Last decoded NV12 frame (an owned, single-slice texture from sample_to_texture — NOT a
	// recycled MFT pool surface, so holding it is safe). Re-blt during a video STALL so the
	// overlay/HUD/cursor keep painting over the last frame instead of freezing (Linux-parity:
	// linux.rs re-presents `last` every vsync). Cleared on resolution change so a stale frame
	// is never re-presented through a mismatched VideoProcessor.
	last_nv12: Option<ID3D11Texture2D>,
	// fps counter for the `vidsink-fps` HUD line + self-measured stream metrics
	// (bytes → mbps, decode wall time → ms) so the overlay shows real numbers
	// without an app-side echo (the stdin `stat` line never arrives on Windows).
	frames: u32,
	last_fps_at: std::time::Instant,
	bytes: u64,
	dec_ms_acc: f32,
	dec_n: u32,
	// Side-channel cursor: uploaded egui texture + hotspot/dims (points come from the
	// statics fed over stdin; re-uploaded when CURSOR_IMG_GEN moves).
	cursor_tex: Option<egui::TextureHandle>,
	cursor_gen: u64,
	cursor_hot: (f32, f32),
	cursor_dims: (f32, f32),
	// egui overlay (same overlay.rs UI as Linux), painted on the swapchain after the video.
	egui_ctx: egui::Context,
	painter: Option<egui_paint::EguiPaint>,
	ostate: crate::overlay::OverlayState,
	// Frame-persistent overlay UI state (current page, chat composer, local file pane).
	ov_ui: crate::overlay::UiState,
	was_open: bool,
}

impl Renderer {
	/// Create the D3D11 device (BGRA + VIDEO support for the later VideoProcessor) and a
	/// flip-model swapchain bound to the child window.
	unsafe fn new(
		hwnd: HWND,
		parent: HWND,
		w: u32,
		h: u32,
		codec: Codec,
		mode: crate::overlay::Mode,
	) -> Result<Self> {
		let mut device: Option<ID3D11Device> = None;
		let mut context: Option<ID3D11DeviceContext> = None;
		D3D11CreateDevice(
			None,
			D3D_DRIVER_TYPE_HARDWARE,
			None,
			D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
			Some(&[D3D_FEATURE_LEVEL_11_0]),
			D3D11_SDK_VERSION,
			Some(&mut device),
			None,
			Some(&mut context),
		)?;
		let device = device.unwrap();
		let context = context.unwrap();

		// DXGI factory from the device → CreateSwapChainForHwnd (flip-model, low-latency).
		let dxgi_device: IDXGIDevice = device.cast()?;
		let adapter = dxgi_device.GetAdapter()?;
		let factory: IDXGIFactory2 = adapter.GetParent()?;
		let desc = DXGI_SWAP_CHAIN_DESC1 {
			Width: w.max(1),
			Height: h.max(1),
			Format: DXGI_FORMAT_B8G8R8A8_UNORM,
			SampleDesc: DXGI_SAMPLE_DESC {
				Count: 1,
				Quality: 0,
			},
			BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
			BufferCount: 2,
			SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
			..Default::default()
		};
		let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;

		// egui overlay: same UI as Linux. The painter is created up front (device available);
		// if it fails the overlay is simply unavailable (video unaffected).
		let egui_ctx = egui::Context::default();
		crate::overlay::apply_theme(&egui_ctx);
		// Same ppp as the Linux backend → identical overlay size/look across platforms.
		egui_ctx.set_pixels_per_point(OVERLAY_PPP);
		let painter = egui_paint::EguiPaint::new(&device).ok();
		let ostate = crate::overlay::OverlayState {
			mode,
			conn_label: "P2P".into(),
			..Default::default()
		};

		let mut r = Self {
			hwnd,
			parent,
			device,
			context,
			swap,
			rtv: None,
			width: w.max(1),
			height: h.max(1),
			x: 0,
			y: 0,
			decoder: None,
			present: None,
			codec,
			src_w: 0,
			src_h: 0,
			last_nv12: None,
			frames: 0,
			last_fps_at: std::time::Instant::now(),
			bytes: 0,
			dec_ms_acc: 0.0,
			dec_n: 0,
			cursor_tex: None,
			cursor_gen: 0,
			cursor_hot: (0.0, 0.0),
			cursor_dims: (0.0, 0.0),
			egui_ctx,
			painter,
			ostate,
			ov_ui: Default::default(),
			was_open: false,
		};
		r.make_rtv()?;
		Ok(r)
	}

	/// Run egui for one frame and paint the overlay onto the swapchain back buffer (over the
	/// video). Only when OPEN. Selector changes flow OUT as `ov …` stdout lines (same as Linux).
	unsafe fn paint_overlay(&mut self) {
		let open = OPEN.load(Ordering::SeqCst);
		// Open↔close edge bookkeeping. NO SetCapture while open: the child receives its
		// own clicks anyway (it is the window under the cursor), and holding the mouse
		// capture made the FIRST click on another application vanish into this window
		// (the user had to click twice to switch apps).
		if open != self.was_open {
			if open {
				// The Files view re-requests the remote listing once per overlay visit.
				self.ov_ui.remote_requested = false;
			} else {
				EGUI_EVENTS.lock().unwrap().clear();
				ENTER_IN.store(false, Ordering::SeqCst);
			}
			self.was_open = open;
		}
		if !open {
			self.paint_closed();
			return;
		}
		self.apply_caps_seed();
		self.sync_cursor_tex();
		let cursor_draw = self.cursor_draw();
		let Some(painter) = self.painter.as_mut() else {
			return;
		};
		let Some(rtv) = self.rtv.clone() else { return };

		// Live stats: self-measured by tick_fps (fps/decode/mbps) — or stdin `stat …`
		// if the app ever sends one. The latency tile prefers the keepalive RTT.
		let s = *STATS.lock().unwrap();
		if s[0] > 0.0 {
			self.ostate.fps = s[0];
			self.ostate.latency_ms = s[1];
			self.ostate.decode_ms = s[2];
			self.ostate.mbps = s[3];
		}
		let rtt = RTT_DMS.load(Ordering::Relaxed) as f32 / 10.0;
		if rtt > 0.0 {
			self.ostate.latency_ms = rtt;
		}
		self.ostate.host_active = HOST_ENC.lock().unwrap().clone();
		self.ostate.open = true;
		self.ostate.pace = PACE.load(Ordering::SeqCst);
		// Mirror the closed-state chrome toggles every frame (like linux.rs) so the
		// overlay rows show + apply the live values (stdin echoes land here too).
		self.ostate.stats_hud = STATS_HUD.load(Ordering::SeqCst);
		self.ostate.overlay_btn = OVERLAY_BTN.load(Ordering::SeqCst);
		self.ostate.btn_pos = *OVBTN_POS.lock().unwrap();
		// Session audio truth (linux.rs parity) → the Ses section's live highlight.
		self.ostate.audio_tx = AUDIO_TX.load(Ordering::SeqCst);
		self.ostate.audio_mute = AUDIO_MUTE.load(Ordering::SeqCst);
		self.ostate.mic_on = MIC_ON.load(Ordering::SeqCst);
		// Live fit-mode truth (the Display section's highlight follows whatever set
		// it last — the overlay click or a `fit` stdin line). Linux-parity.
		self.ostate.fit = present::fit_label().to_string();

		// Build RawInput (ppp = 1.0, so points == physical px). Drain the wndproc events.
		let events = std::mem::take(&mut *EGUI_EVENTS.lock().unwrap());
		// screen_rect is in POINTS (physical ÷ ppp), matching linux.rs.
		let raw = egui::RawInput {
			screen_rect: Some(egui::Rect::from_min_size(
				egui::pos2(0.0, 0.0),
				egui::vec2(
					self.width as f32 / OVERLAY_PPP,
					self.height as f32 / OVERLAY_PPP,
				),
			)),
			events,
			..Default::default()
		};

		// Run the shared overlay UI; collect interaction commands.
		let ostate = &mut self.ostate;
		ostate.chat = CHAT_LOG.lock().unwrap().clone();
		// Reading the chat clears the unread badge; new arrivals while the Chat view
		// is on screen count as read immediately.
		if self.ov_ui.view == crate::overlay::View::Chat {
			CHAT_UNREAD.store(0, Ordering::SeqCst);
		}
		ostate.chat_unread = CHAT_UNREAD.load(Ordering::SeqCst);
		{
			let (p, rows) = &*FS_REMOTE.lock().unwrap();
			ostate.fs_remote_path = p.clone();
			ostate.fs_remote = rows.clone();
		}
		ostate.chat_enter = ENTER_IN.swap(false, Ordering::SeqCst);
		// Connected controllers (game mode only — sent by play.rs gilrs reader via `ctrls` line).
		// The per-pad vibration level rides in each entry's 6th field.
		ostate.controllers = CONTROLLERS.lock().unwrap().clone();
		ostate.controllers_locked = CONTROLLERS_LOCKED.lock().unwrap().iter().cloned().collect();
		let ov_ui = &mut self.ov_ui;
		let mut cmds = Vec::new();
		let out = self.egui_ctx.run(raw, |ctx| {
			cmds = crate::overlay::draw(ctx, ostate, ov_ui);
			// Show the "stream stopped" indicator even while the overlay is open so the
			// user sees the state change regardless of whether the overlay is up.
			// (Windows respawns the renderer on a codec switch, so there is no local
			// SWITCHING state here — only STALLED applies.)
			if STALLED.load(Ordering::Relaxed) {
				crate::overlay::draw_stalled(ctx);
			}
			paint_side_cursor(ctx, cursor_draw);
		});
		// Apply + emit commands (mirrors desktop.rs / the Linux backend).
		for c in cmds {
			match c {
				crate::overlay::OverlayCmd::Set(field, val) => {
					match field {
						"codec" => self.ostate.codec = val.clone(),
						"encoder" => self.ostate.encoder = val.clone(),
						"decoder" => self.ostate.decoder = val.clone(),
						"res" => self.ostate.res = val.clone(),
						"fps" => self.ostate.fps_sel = val.clone(),
						"bitrate" => self.ostate.bitrate = val.clone(),
						"quality" => self.ostate.quality = val.clone(),
						"pace" => self.ostate.pace = val == "on",
						// Local echo for the closed-state chrome toggles: flip the statics
						// NOW so the row + HUD react instantly; the frontend's stdin echo
						// (`statshud`/`ovbtn`) just confirms the same value later.
						"statshud" => STATS_HUD.store(val == "on", Ordering::SeqCst),
						"ovbtn" => OVERLAY_BTN.store(val == "on", Ordering::SeqCst),
						// Audio toggles apply optimistically (linux.rs parity); the app's
						// `audio …` line re-syncs the truth after the host acknowledges.
						"atx" => AUDIO_TX.store(val == "on", Ordering::SeqCst),
						"amute" => AUDIO_MUTE.store(val == "on", Ordering::SeqCst),
						"mic" => MIC_ON.store(val == "on", Ordering::SeqCst),
						// Voice call = mic + host audio together (paired optimistic update).
						// ON enables BOTH; OFF drops ONLY the mic and leaves host audio
						// as-is (it has its own `atx` row). The overlay highlight derives
						// from MIC_ON alone (overlay::draw_audio) so it stays in sync.
						"call" => {
							let on = val == "on";
							MIC_ON.store(on, Ordering::SeqCst);
							if on {
								AUDIO_TX.store(true, Ordering::SeqCst);
							}
						}
						// View fit is renderer-local (instant Blt rect change) + forwarded
						// so the frontend mirrors/persists it — Linux-parity (linux.rs).
						"fit" => present::set_fit(&val),
						// Optimistic controller swap: the overlay row ▲/▼ was clicked.
						// Swap the two entries in CONTROLLERS immediately so the list
						// re-renders at the next frame without waiting for the gilrs
						// reader's next `ctrls` line (~16 ms later). The frontend will
						// re-emit the canonical ctrls line via set_controller_order, which
						// confirms the same state.
						"ctrlswap" => {
							if let Some((ai, bi)) = val.split_once(',') {
								if let (Ok(ai), Ok(bi)) = (ai.parse::<usize>(), bi.parse::<usize>()) {
									let mut ctrls = CONTROLLERS.lock().unwrap();
									let len = ctrls.len();
									if ai < len && bi < len {
										ctrls.swap(ai, bi);
									}
								}
							}
						}
						// Optimistic emulation-target update: the overlay picker was clicked.
						// Update the matching row's target in CONTROLLERS by uuid so the
						// row re-renders immediately without waiting for the next ctrls line.
						"ctrlemu" => {
							if let Some((uuid, target)) = val.split_once(',') {
								let mut ctrls = CONTROLLERS.lock().unwrap();
								for row in ctrls.iter_mut() {
									if row.3 == uuid {
										row.4 = target.to_string();
										break;
									}
								}
							}
						}
						// Optimistic PER-PAD vibration update: `val` = "uuid,level". Reflect it
						// locally (the matching pad row's 6th field) so the highlight moves
						// immediately; the frontend confirms via set_controller_rumble → ctrls.
						"ctrlrumble" => {
							if let Some((uuid, level)) = val.split_once(',') {
								let mut ctrls = CONTROLLERS.lock().unwrap();
								for row in ctrls.iter_mut() {
									if row.3 == uuid {
										row.5 = level.to_string();
										break;
									}
								}
							}
						}
						// Optimistic enable/disable SET: `val` = "uuid,state" (1 = disabled,
						// 0 = enabled). Apply to the matching pad so the row updates immediately.
						"ctrldisable" => {
							if let Some((uuid, state)) = val.split_once(',') {
								let mut ctrls = CONTROLLERS.lock().unwrap();
								for row in ctrls.iter_mut() {
									if row.3 == uuid {
										row.6 = state == "1";
										break;
									}
								}
							}
						}
						// Optimistic SPLIT-MODE lock SET: `val` = "uuid,state" (1 = lock to this
						// session, 0 = unlock). Reflect locally so the row's Kilitli/Serbest
						// updates immediately; play.rs confirms via the ctrls line's 8th field.
						// The lock is applied app-side (set_controller_lock, via `ov set ctrllock`).
						"ctrllock" => {
							if let Some((uuid, state)) = val.split_once(',') {
								let mut locked = CONTROLLERS_LOCKED.lock().unwrap();
								locked.retain(|u| u != uuid);
								if state == "1" {
									locked.push(uuid.to_string());
								}
							}
						}
						_ => {}
					}
					println!("ov set {field} {val}");
				}
				crate::overlay::OverlayCmd::End => println!("ov end"),
				crate::overlay::OverlayCmd::Close => {
					OPEN.store(false, Ordering::SeqCst);
					println!("ov close");
				}
				// Same wire lines as the Linux backend (render_stats.rs parses them).
				crate::overlay::OverlayCmd::Chat(t) => println!("ov chat {t}"),
				crate::overlay::OverlayCmd::FsLs(p) => println!("ov fsls {p}"),
				crate::overlay::OverlayCmd::FsGet(p) => println!("ov fsget {p}"),
				crate::overlay::OverlayCmd::FsSend(p) => println!("ov fssend {p}"),
				crate::overlay::OverlayCmd::OpenFiles => println!("ov files"),
			}
		}
		use std::io::Write;
		let _ = std::io::stdout().flush();

		// Upload texture deltas, then paint the tessellated primitives over the video.
		let _ = painter.update_textures(&self.device, &self.context, &out.textures_delta);
		let prims = self
			.egui_ctx
			.tessellate(out.shapes, self.egui_ctx.pixels_per_point());
		let _ = painter.paint(
			&self.device,
			&self.context,
			&rtv,
			[self.width, self.height],
			self.egui_ctx.pixels_per_point(),
			&prims,
		);
	}

	/// (Re)upload the side-channel cursor texture when a new `cursorimg` arrived.
	fn sync_cursor_tex(&mut self) {
		let gen = CURSOR_IMG_GEN.load(Ordering::SeqCst);
		if gen == self.cursor_gen {
			return;
		}
		self.cursor_gen = gen;
		let img = CURSOR_IMG.lock().unwrap().clone();
		if let Some(c) = img {
			let color = egui::ColorImage::from_rgba_unmultiplied([c.w, c.h], &c.rgba);
			self.cursor_tex = Some(self.egui_ctx.load_texture(
				"pulsar-side-cursor",
				color,
				egui::TextureOptions::NEAREST,
			));
			self.cursor_hot = (c.hot_x, c.hot_y);
			self.cursor_dims = (c.w as f32, c.h as f32);
		}
	}

	/// Side-channel cursor draw data: position in egui POINTS over the video's
	/// aspect-fit letterbox rect (same math as present.rs::dest_rect), or `None`
	/// when there is no side cursor / no video yet.
	fn cursor_draw(&self) -> Option<(egui::Pos2, egui::TextureId, egui::Vec2)> {
		let (nx, ny) = (*CURSOR_POS.lock().unwrap())?;
		let tex = self.cursor_tex.as_ref()?;
		if self.src_w == 0 || self.src_h == 0 {
			return None;
		}
		let (iw, ih) = (self.src_w as f32, self.src_h as f32);
		let (ow, oh) = (self.width as f32, self.height as f32);
		// Same fit-mode rects the video Blt uses — the cursor must track the video
		// through fit/stretch/original (original = a center CROP, so the source
		// position maps through the visible source window, not the full frame).
		let ((sx, sy, sw, sh), (dx, dy, dw, dh)) = present::fit_rects(iw, ih, ow, oh);
		// Host-pixel → displayed-pixel scale (the same factor the position uses). The
		// side-channel cursor bitmap/hotspot arrive in raw host pixels, so its drawn
		// size AND hotspot must scale by this too or the tip mis-aligns / shape is the
		// wrong size whenever the video isn't shown at 1:1 host resolution.
		let (kx, ky) = (dw / sw.max(1.0), dh / sh.max(1.0));
		let px = dx + (nx * iw - sx) * kx;
		let py = dy + (ny * ih - sy) * ky;
		let pos = egui::pos2(
			(px - self.cursor_hot.0 * kx) / OVERLAY_PPP,
			(py - self.cursor_hot.1 * ky) / OVERLAY_PPP,
		);
		let size = egui::vec2(
			self.cursor_dims.0 * kx / OVERLAY_PPP,
			self.cursor_dims.1 * ky / OVERLAY_PPP,
		);
		Some((pos, tex.id(), size))
	}

	/// Apply a pending caps line (host codec/encoder lists + active request + conn
	/// label) to the overlay state — one-shot per received line.
	fn apply_caps_seed(&mut self) {
		if let Some((codecs, encoders, codec, encoder, conn)) = CAPS_SEED.lock().unwrap().take() {
			self.ostate.host_codecs = codecs;
			self.ostate.host_encoders = encoders;
			if !codec.is_empty() {
				self.ostate.codec = codec;
			}
			if !encoder.is_empty() {
				self.ostate.encoder = encoder;
			}
			if !conn.is_empty() {
				self.ostate.conn_label = conn;
			}
		}
		// Stream-selection respawn seeds (C14): apply once per respawn seed, then clear,
		// so the overlay can update them live via emit_cmd thereafter.
		if let Some(v) = RES_SEED.lock().unwrap().take() {
			self.ostate.res = v;
		}
		if let Some(v) = FPS_SEL_SEED.lock().unwrap().take() {
			self.ostate.fps_sel = v;
		}
		if let Some(v) = BITRATE_SEED.lock().unwrap().take() {
			self.ostate.bitrate = v;
		}
		if let Some(v) = QUALITY_SEED.lock().unwrap().take() {
			self.ostate.quality = v;
		}
		{
			let idx = DISPLAY_IDX_SEED.load(Ordering::SeqCst);
			if idx != u32::MAX {
				self.ostate.display_idx = idx;
				DISPLAY_IDX_SEED.store(u32::MAX, Ordering::SeqCst);
			}
		}
	}

	/// Closed-state chrome (Linux-parity with linux.rs's `else` paint branch): the mini
	/// stats HUD, the Parsec-style overlay-open button and the transient helper tooltip,
	/// drawn over the live video while the overlay is CLOSED. Display chrome only — the
	/// open-button CLICK is hit-tested in `wndproc` (this child owns the pointer here).
	unsafe fn paint_closed(&mut self) {
		self.sync_cursor_tex();
		let cursor_draw = self.cursor_draw();
		// Expire + fade the tooltip (alpha 1→0 over the last HINT_FADE seconds).
		let hint_text: Option<(String, f32)> = {
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
		let stats_hud = STATS_HUD.load(Ordering::SeqCst);
		let ovbtn = OVERLAY_BTN.load(Ordering::SeqCst);
		let stalled = STALLED.load(Ordering::Relaxed);
		if !stats_hud && !ovbtn && hint_text.is_none() && cursor_draw.is_none() && !stalled {
			return;
		}
		// Sync the display state (same sources as the open path).
		let s = *STATS.lock().unwrap();
		if s[0] > 0.0 {
			self.ostate.fps = s[0];
			self.ostate.latency_ms = s[1];
			self.ostate.decode_ms = s[2];
			self.ostate.mbps = s[3];
		}
		let rtt = RTT_DMS.load(Ordering::Relaxed) as f32 / 10.0;
		if rtt > 0.0 {
			self.ostate.latency_ms = rtt;
		}
		self.ostate.host_active = HOST_ENC.lock().unwrap().clone();
		self.apply_caps_seed();
		self.ostate.open = false;
		self.ostate.stats_hud = stats_hud;
		self.ostate.overlay_btn = ovbtn;
		self.ostate.btn_pos = *OVBTN_POS.lock().unwrap();
		let Some(painter) = self.painter.as_mut() else {
			return;
		};
		let Some(rtv) = self.rtv.clone() else { return };
		let raw = egui::RawInput {
			screen_rect: Some(egui::Rect::from_min_size(
				egui::pos2(0.0, 0.0),
				egui::vec2(
					self.width as f32 / OVERLAY_PPP,
					self.height as f32 / OVERLAY_PPP,
				),
			)),
			events: Vec::new(),
			..Default::default()
		};
		self.ostate.chat_unread = CHAT_UNREAD.load(Ordering::SeqCst);
		let ostate = &self.ostate;
		let out = self.egui_ctx.run(raw, |ctx| {
			// Stream-stopped indicator: surfaces the stall state that the webview's .stall
			// div cannot show because the native renderer window occludes it.
			if stalled {
				crate::overlay::draw_stalled(ctx);
			}
			if stats_hud {
				crate::overlay::draw_hud(ctx, ostate);
			}
			if ovbtn {
				let _ = crate::overlay::draw_open_button(ctx, ostate);
			}
			if let Some((text, alpha)) = &hint_text {
				crate::overlay::draw_hint(ctx, text, *alpha);
			}
			paint_side_cursor(ctx, cursor_draw);
		});
		let _ = painter.update_textures(&self.device, &self.context, &out.textures_delta);
		let prims = self
			.egui_ctx
			.tessellate(out.shapes, self.egui_ctx.pixels_per_point());
		let _ = painter.paint(
			&self.device,
			&self.context,
			&rtv,
			[self.width, self.height],
			self.egui_ctx.pixels_per_point(),
			&prims,
		);
	}

	/// Decode one access unit and present its newest NV12 frame (zero-copy: decoder texture →
	/// VideoProcessor → swapchain). Builds the decoder lazily on the first AU. Per-AU errors are
	/// non-fatal (a bad packet must not kill the renderer).
	unsafe fn on_access_unit(&mut self, au: &AccessUnit) {
		if self.decoder.is_none() {
			// Nominal size; the MFT emits MF_E_TRANSFORM_STREAM_CHANGE → real size, and the
			// present path resizes to the decoded texture's actual dims.
			match decode::Decoder::new(&self.device, self.codec, 1920, 1080, 60) {
				Ok(d) => {
					// Report the active decoder to the app (read-only UI display; "na" =
					// HW/SW split not surfaced by the MFT enum path).
					println!("vidsink-dec mediafoundation na");
					{
						use std::io::Write;
						let _ = std::io::stdout().flush();
					}
					self.decoder = Some(d)
				}
				Err(e) => {
					eprintln!("pulsar-render(win): decoder init failed: {e}");
					return;
				}
			}
		}
		self.bytes += au.data.len() as u64;
		let dec_t0 = std::time::Instant::now();
		let texs = match self.decoder.as_mut().unwrap().decode(au) {
			Ok(t) => t,
			Err(e) => {
				eprintln!("pulsar-render(win): decode error: {e}");
				return;
			}
		};
		self.dec_ms_acc += dec_t0.elapsed().as_secs_f32() * 1000.0;
		self.dec_n += 1;
		// Present EVERY decoded frame in order: a single decode() drain can yield more than one
		// NV12 texture (multi-frame batch), and dropping all but the last silently loses frames
		// (micro-stutter). In steady state this is exactly one frame, so the loop is a no-op there.
		for nv12 in texs {
			self.show_frame(&nv12);
		}
	}

	/// Present one decoded NV12 texture onto the swapchain via the VideoProcessor.
	unsafe fn show_frame(&mut self, nv12: &ID3D11Texture2D) {
		// Source dims from the decoded texture.
		let mut td = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
		nv12.GetDesc(&mut td);
		let (sw, sh) = (td.Width, td.Height);
		if self.present.is_none() || sw != self.src_w || sh != self.src_h {
			// Resolution changed (or first frame): the held frame is now stale for the
			// resized processor — drop it so repaint_idle can't re-Blt a mismatched texture
			// (it is replaced with this fresh frame at the end of show_frame anyway).
			if sw != self.src_w || sh != self.src_h {
				self.last_nv12 = None;
			}
			self.src_w = sw;
			self.src_h = sh;
			// Report the STREAM pixel size on stdout (first frame + live resolution
			// switch) so the app sizes the windowed session to the host's aspect —
			// render_stats.rs parses this into `play-dims` (linux.rs/video.rs parity).
			if sw > 0 && sh > 0 {
				println!("vidsink-dims {sw}x{sh}");
				use std::io::Write as _;
				let _ = std::io::stdout().flush();
			}
			match self.present.as_mut() {
				Some(p) => {
					let _ = p.resize(sw, sh, self.width, self.height);
				}
				None => {
					match present::Present::new(&self.device, sw, sh, self.width, self.height) {
						Ok(p) => self.present = Some(p),
						Err(e) => {
							eprintln!("pulsar-render(win): present init failed: {e}");
							return;
						}
					}
				}
			}
		}
		let Some(back) = self.swap.GetBuffer::<ID3D11Texture2D>(0).ok() else {
			return;
		};
		// Scoped borrow: video Blt onto the back buffer, then drop the `present` borrow so
		// `paint_overlay` can take `&mut self`.
		let drawn = match self.present.as_ref() {
			Some(p) => p.blt(nv12, &back).is_ok(),
			None => false,
		};
		if drawn {
			// Retain this frame so a later video STALL can re-present it under the overlay
			// instead of freezing (see repaint_idle). Cheap COM refcount bump on an owned
			// single-slice texture.
			self.last_nv12 = Some(nv12.clone());
			self.paint_overlay(); // egui over the video (no-op unless OPEN)
			let _ = self.swap.Present(0, Default::default());
			self.tick_fps();
		}
	}

	/// Idle repaint during a video STALL (no fresh AU this tick): re-Blt the LAST decoded
	/// frame onto the back buffer, paint the overlay/HUD/cursor over it, and Present — so the
	/// egui overlay (and the side-channel cursor / closed-state chrome) stays live and
	/// responsive while the host video is paused/frozen, instead of being painted only on a
	/// freshly-decoded frame. Linux-parity: linux.rs re-presents `last` + overlay every vsync.
	/// The caller rate-caps this (~60 Hz). With FLIP_DISCARD the back buffer is undefined after
	/// Present, so we MUST re-Blt the last frame (not rely on stale content); if there is no
	/// last frame yet, dark-clear so the overlay still has a surface.
	unsafe fn repaint_idle(&mut self) {
		let back = match self.swap.GetBuffer::<ID3D11Texture2D>(0).ok() {
			Some(b) => b,
			None => return,
		};
		// Re-present the last frame if we have one + a live processor; otherwise dark-clear so
		// the overlay still paints (matches clear()'s pre-first-frame background).
		let drawn = match (self.last_nv12.clone(), self.present.as_ref()) {
			(Some(nv12), Some(p)) => p.blt(&nv12, &back).is_ok(),
			_ => false,
		};
		if !drawn {
			if let Some(rtv) = self.rtv.as_ref() {
				let c = [0.02f32, 0.02, 0.03, 1.0];
				self.context.ClearRenderTargetView(rtv, &c);
			}
		}
		self.paint_overlay(); // egui over the (re-presented) video — no-op unless OPEN or chrome
		let _ = self.swap.Present(0, Default::default());
	}

	/// Emit a `vidsink-fps <fps> <w>x<h>` HUD line once per second (same as the Linux backend).
	fn tick_fps(&mut self) {
		self.frames += 1;
		let dt = self.last_fps_at.elapsed();
		if dt.as_secs_f32() >= 1.0 {
			let fps = self.frames as f32 / dt.as_secs_f32();
			let mbps = (self.bytes as f32 * 8.0) / dt.as_secs_f32() / 1_000_000.0;
			let dec = if self.dec_n > 0 {
				self.dec_ms_acc / self.dec_n as f32
			} else {
				0.0
			};
			// 4-field line like the Linux backends (fps, dims, mbit, decode-ms) so the
			// app's render_stats reader gets real values for the webview HUD too.
			println!(
				"vidsink-fps {fps:.0} {}x{} {mbps:.1} {dec:.1}",
				self.src_w, self.src_h
			);
			use std::io::Write;
			let _ = std::io::stdout().flush();
			// Feed the overlay directly: no stdin `stat` echo exists on Windows; these
			// self-measured numbers ARE the HUD source (latency comes from `rtt`).
			*STATS.lock().unwrap() = [fps, 0.0, dec, mbps];
			self.frames = 0;
			self.bytes = 0;
			self.dec_ms_acc = 0.0;
			self.dec_n = 0;
			self.last_fps_at = std::time::Instant::now();
		}
	}

	/// (Re)create the render-target view of the swapchain's back buffer.
	unsafe fn make_rtv(&mut self) -> Result<()> {
		let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
		let mut rtv: Option<ID3D11RenderTargetView> = None;
		self.device
			.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
		self.rtv = rtv;
		Ok(())
	}

	/// Match the child window + swapchain to the parent client area (follows resize/fullscreen,
	/// like linux.rs's XGetGeometry/XResizeWindow loop).
	unsafe fn track_parent(&mut self) {
		// Frontend-driven embed rect (stdin `viewrect`) wins: position over the session
		// tab's CONTENT area so the app chrome/tabs stay visible (Linux-container parity).
		// Before the first report, fill the whole parent client area as before.
		let (x, y, w, h) = match *VIEW_RECT.lock().unwrap() {
			Some((_, _, vw, vh)) if vw <= 0 || vh <= 0 => {
				// Tab inactive / unmounted: park the child as 1×1 offscreen (cheap "hide" —
				// no extra show/hide state to track; the next nonzero rect restores it).
				(-1, -1, 1u32, 1u32)
			}
			Some((vx, vy, vw, vh)) => (vx, vy, vw as u32, vh as u32),
			// No viewrect yet: FILL the parent client area as a safe fallback. Parking the
			// child 1×1 offscreen here meant that if the frontend's first `viewrect` report
			// never arrived (lost/late on some session mounts), the video stayed permanently
			// invisible — the same "connected but nothing renders" failure. Linux fills the
			// parent by default in this situation; match it. A real `viewrect` still wins the
			// instant it lands (it switches this arm to the Some branch above), so the only
			// cost is that the tabs may be briefly covered before that first report — far
			// better than no video at all.
			None => {
				let mut rc = RECT::default();
				let _ = GetClientRect(self.parent, &mut rc);
				let w = (rc.right - rc.left).max(1) as u32;
				let h = (rc.bottom - rc.top).max(1) as u32;
				(0, 0, w, h)
			}
		};
		if x == self.x && y == self.y && w == self.width && h == self.height {
			return;
		}
		self.x = x;
		self.y = y;
		// HWND_TOP (not SWP_NOZORDER): the webview sibling (WRY_WEBVIEW/Chrome_WidgetWin)
		// otherwise sits ABOVE this child and the video is fully hidden behind the session
		// screen — the "connected but nothing renders" Windows bug. No-op when already top.
		let _ = SetWindowPos(
			self.hwnd,
			HWND_TOP,
			x,
			y,
			w as i32,
			h as i32,
			SWP_NOACTIVATE,
		);
		// Resize the swapchain: drop the RTV first, ResizeBuffers, rebuild.
		self.rtv = None;
		if self
			.swap
			.ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, Default::default())
			.is_ok()
		{
			self.width = w;
			self.height = h;
			let _ = self.make_rtv();
			// Keep the letterbox correct: rebuild the processor's output size.
			if let Some(p) = self.present.as_mut() {
				let _ = p.resize(self.src_w, self.src_h, w, h);
			}
		}
	}

	/// Idle present: dark clear (shown before the first decoded frame arrives).
	unsafe fn clear(&mut self) {
		if let Some(rtv) = self.rtv.as_ref() {
			let c = [0.02f32, 0.02, 0.03, 1.0];
			self.context.ClearRenderTargetView(rtv, &c);
		}
		self.paint_overlay(); // overlay still shows before the first frame (no-op unless OPEN)
		let _ = self.swap.Present(0, Default::default());
	}
}

pub fn run() {
	// ---- args: <sdp> --wid <hwnd> [--mode m] [--pace on|off] -------------------------------
	let args: Vec<String> = std::env::args().collect();
	let mut wid: u64 = 0;
	let mut sdp: Option<String> = None;
	let mut mode = crate::overlay::Mode::Game;
	let mut i = 1;
	while i < args.len() {
		match args[i].as_str() {
			"--wid" => {
				if let Some(s) = args.get(i + 1) {
					wid = parse_handle(s);
					i += 1;
				}
			}
			"--pace" => {
				if let Some(s) = args.get(i + 1) {
					PACE.store(s != "off", Ordering::SeqCst);
					i += 1;
				}
			}
			"--mode" => {
				if let Some(s) = args.get(i + 1) {
					mode = if s == "remote" {
						crate::overlay::Mode::Remote
					} else {
						crate::overlay::Mode::Game
					};
					i += 1;
				}
			}
			// First non-flag positional arg = the SDP path.
			a if !a.starts_with("--") && sdp.is_none() => sdp = Some(a.to_string()),
			_ => {}
		}
		i += 1;
	}
	if wid == 0 {
		eprintln!("pulsar-render(win): --wid <parent-hwnd> required");
		std::process::exit(2);
	}
	let Some(sdp) = sdp else {
		eprintln!("pulsar-render(win): <stream.sdp> required");
		std::process::exit(2);
	};
	let (port, codec) = match parse_sdp(&sdp) {
		Ok(v) => v,
		Err(e) => {
			eprintln!("pulsar-render(win): SDP parse failed: {e}");
			std::process::exit(2);
		}
	};
	PARENT.store(wid, Ordering::SeqCst);

	std::thread::spawn(stdin_control);

	if let Err(e) = unsafe { event_loop(HWND(wid as *mut _), port, codec, mode) } {
		eprintln!("pulsar-render(win): fatal: {e}");
		std::process::exit(1);
	}
}

/// Parse a window handle written as decimal or `0x`-hex.
fn parse_handle(s: &str) -> u64 {
	let s = s.trim();
	if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
		u64::from_str_radix(hex, 16).unwrap_or(0)
	} else {
		s.parse().unwrap_or(0)
	}
}

unsafe fn event_loop(
	parent: HWND,
	port: u16,
	codec: Codec,
	mode: crate::overlay::Mode,
) -> Result<()> {
	let hinstance = GetModuleHandleW(None)?;
	let class_name = w!("PulsarRenderChild");
	let wc = WNDCLASSW {
		lpfnWndProc: Some(wndproc),
		hInstance: hinstance.into(),
		lpszClassName: class_name,
		..Default::default()
	};
	RegisterClassW(&wc);

	// Initial size from the parent client area.
	let mut rc = RECT::default();
	let _ = GetClientRect(parent, &mut rc);
	let w0 = (rc.right - rc.left).max(1) as u32;
	let h0 = (rc.bottom - rc.top).max(1) as u32;

	let hwnd = CreateWindowExW(
		Default::default(),
		class_name,
		PCWSTR::null(),
		WS_CHILD | WS_VISIBLE,
		0,
		0,
		w0 as i32,
		h0 as i32,
		parent,
		HMENU::default(),
		hinstance,
		None,
	)?;
	let _ = CW_USEDEFAULT; // (kept for reference; child uses explicit geometry)
	// Raise above the webview sibling NOW — track_parent only re-asserts on resize, and a
	// child left below WRY_WEBVIEW renders invisibly behind the session screen.
	let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE);

	let mut r = Renderer::new(hwnd, parent, w0, h0, codec, mode)?;

	// RTP receive runs on its own thread (blocking UDP); completed access units land in a
	// shared bounded queue. The render thread drains + decodes + presents — keeping decode on
	// the render thread means one D3D11 device, zero-copy into the swapchain.
	let queue: std::sync::Arc<Mutex<std::collections::VecDeque<AccessUnit>>> =
		std::sync::Arc::new(Mutex::new(std::collections::VecDeque::new()));
	let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
	{
		let q = queue.clone();
		let stop2 = stop.clone();
		std::thread::spawn(move || {
			stream::rtp::recv_loop(
				port,
				codec,
				|au| {
					let mut g = q.lock().unwrap();
					// Bound the backlog (low latency): keep at most a few AUs; on overflow drop
					// the oldest non-key so we never play seconds-late video. Dropping a keyframe
					// would orphan every P-frame after it (green/mosaic until the next IDR), so
					// skip keys and only drop the oldest delta; if the backlog is somehow all
					// keyframes, drop the oldest of those to keep latency bounded.
					if g.len() >= 8 {
						let drop_idx = g.iter().position(|au| !au.key).unwrap_or(0);
						g.remove(drop_idx);
					}
					g.push_back(au);
				},
				&stop2,
			);
		});
	}

	// Pump Win32 messages + drain decode + present.
	let mut msg = MSG::default();
	let mut cursor_hidden = false;
	// Rate-cap the idle (video-stall) overlay repaint to ~60 Hz so a frozen/paused host
	// doesn't pin the CPU re-presenting the same frame every 2 ms loop tick.
	let mut last_idle_paint = std::time::Instant::now();
	// Stall detection: track when the last AU arrived so we can surface a "stream stopped"
	// indicator after STALL_SECS of silence (video was live = decoder exists).
	// This mirrors the webview media.svelte stall detector but lives inside the renderer
	// so it is visible on native paths where the webview is occluded.
	const STALL_SECS: u64 = 3;
	let mut last_au_at = std::time::Instant::now();
	loop {
		while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
			let _ = TranslateMessage(&msg);
			DispatchMessageW(&msg);
		}
		// Cursor visibility tracks the ENGAGE state (linux.rs parity). While engaged the
		// Interception capture suppresses mouse messages entirely, so WM_SETCURSOR never
		// fires — assert it from the render loop (this thread owns the window under the
		// frozen cursor; SetCursor from here is honored).
		{
			use windows::Win32::UI::WindowsAndMessaging::{LoadCursorW, SetCursor, IDC_ARROW};
			let hide = ENGAGED_R.load(Ordering::SeqCst) && !OPEN.load(Ordering::SeqCst);
			if hide {
				SetCursor(None);
				cursor_hidden = true;
			} else if cursor_hidden {
				if let Ok(arrow) = LoadCursorW(None, IDC_ARROW) {
					SetCursor(arrow);
				}
				cursor_hidden = false;
			}
		}
		r.track_parent();
		// Drain all queued AUs this tick (decode is fast; present shows the newest frame).
		let aus: Vec<AccessUnit> = {
			let mut g = queue.lock().unwrap();
			g.drain(..).collect()
		};
		if aus.is_empty() {
			if r.decoder.is_none() {
				r.clear(); // nothing decoded yet → dark background
			} else {
				// Video has stalled (host paused — e.g. for the overlay — minimized, or a
				// network/encode hiccup). Keep the overlay/HUD/side-cursor LIVE and
				// responsive instead of freezing on the last frame: drain the queued wndproc
				// input and re-present the last frame + overlay, capped at ~60 Hz. Only when
				// there is actually chrome to draw (overlay open OR any closed-state HUD /
				// open-button / hint / side-cursor) so a plain frozen frame stays idle.
				//
				// After STALL_SECS of silence (decoder exists → video was live) set STALLED
				// so paint_closed / repaint_idle surfaces the "stream stopped" indicator
				// that the webview's .stall div cannot show (it is occluded by this window).
				// Windows respawns the renderer on a codec switch, so there is no local
				// SWITCHING state to suppress here (a respawn resets last_au_at).
				if last_au_at.elapsed().as_secs() >= STALL_SECS {
					STALLED.store(true, Ordering::Relaxed);
				}
				let stalled = STALLED.load(Ordering::Relaxed);
				let chrome = OPEN.load(Ordering::SeqCst)
					|| STATS_HUD.load(Ordering::SeqCst)
					|| OVERLAY_BTN.load(Ordering::SeqCst)
					|| HINT.lock().unwrap().is_some()
					|| CURSOR_POS.lock().unwrap().is_some()
					|| stalled;
				if chrome && last_idle_paint.elapsed() >= std::time::Duration::from_millis(16) {
					r.repaint_idle();
					last_idle_paint = std::time::Instant::now();
				}
			}
			std::thread::sleep(std::time::Duration::from_millis(2));
		} else {
			// AUs are arriving — clear any stall indicator and reset the stall timer.
			STALLED.store(false, Ordering::Relaxed);
			last_au_at = std::time::Instant::now();
			for au in &aus {
				r.on_access_unit(au);
			}
		}
	}
}
