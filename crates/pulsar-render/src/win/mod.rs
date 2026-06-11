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
		} else if let Some(rest) = l.strip_prefix("engaged ") {
			// Live engage state (cursor visibility) — app-side edges, like linux.rs.
			let v = rest.trim();
			ENGAGED_R.store(v == "1" || v == "on" || v == "true", Ordering::SeqCst);
		} else if let Some(rest) = l.strip_prefix("hint ") {
			arm_hint(rest.trim());
		} else if let Some(rest) = l.strip_prefix("toast ") {
			let rest = rest.trim();
			if !rest.is_empty() {
				*HINT.lock().unwrap() =
					Some((rest.to_string(), std::time::Instant::now(), TOAST_SECS));
			}
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
		}
		// `toast <text>` is tolerated but not drawn yet: this backend has no
		// closed-state paint pass (paint_overlay no-ops while the overlay is
		// closed) — wiring that is the documented follow-up.
	}
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
	use windows::Win32::UI::WindowsAndMessaging::{
		WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
	};
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
			if let Some((_, _, _, _, moved)) = BTN_DRAG.lock().unwrap().take() {
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
	// fps counter for the `vidsink-fps` HUD line + self-measured stream metrics
	// (bytes → mbps, decode wall time → ms) so the overlay shows real numbers
	// without an app-side echo (the stdin `stat` line never arrives on Windows).
	frames: u32,
	last_fps_at: std::time::Instant,
	bytes: u64,
	dec_ms_acc: f32,
	dec_n: u32,
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
			frames: 0,
			last_fps_at: std::time::Instant::now(),
			bytes: 0,
			dec_ms_acc: 0.0,
			dec_n: 0,
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
		{
			let (p, rows) = &*FS_REMOTE.lock().unwrap();
			ostate.fs_remote_path = p.clone();
			ostate.fs_remote = rows.clone();
		}
		ostate.chat_enter = ENTER_IN.swap(false, Ordering::SeqCst);
		let ov_ui = &mut self.ov_ui;
		let mut cmds = Vec::new();
		let out = self.egui_ctx.run(raw, |ctx| {
			cmds = crate::overlay::draw(ctx, ostate, ov_ui);
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
	}

	/// Closed-state chrome (Linux-parity with linux.rs's `else` paint branch): the mini
	/// stats HUD, the Parsec-style overlay-open button and the transient helper tooltip,
	/// drawn over the live video while the overlay is CLOSED. Display chrome only — the
	/// open-button CLICK is hit-tested in `wndproc` (this child owns the pointer here).
	unsafe fn paint_closed(&mut self) {
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
		if !stats_hud && !ovbtn && hint_text.is_none() {
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
		let ostate = &self.ostate;
		let out = self.egui_ctx.run(raw, |ctx| {
			if stats_hud {
				crate::overlay::draw_hud(ctx, ostate);
			}
			if ovbtn {
				let _ = crate::overlay::draw_open_button(ctx, ostate);
			}
			if let Some((text, alpha)) = &hint_text {
				crate::overlay::draw_hint(ctx, text, *alpha);
			}
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
		if let Some(nv12) = texs.into_iter().last() {
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
			self.src_w = sw;
			self.src_h = sh;
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
			self.paint_overlay(); // egui over the video (no-op unless OPEN)
			let _ = self.swap.Present(0, Default::default());
			self.tick_fps();
		}
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
			// No viewrect yet: stay PARKED instead of filling the parent — filling first
			// covered the session top bar until the frontend's first report landed, so
			// the tabs "appeared late". The frontend always reports on session mount.
			None => (-1, -1, 1, 1),
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
					// the oldest non-key so we never play seconds-late video.
					if g.len() >= 8 {
						g.pop_front();
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
			}
			std::thread::sleep(std::time::Duration::from_millis(2));
		} else {
			for au in &aus {
				r.on_access_unit(au);
			}
		}
	}
}
