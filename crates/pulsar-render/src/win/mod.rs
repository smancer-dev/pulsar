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
	SetWindowPos, TranslateMessage, CW_USEDEFAULT, HMENU, MSG, PM_REMOVE, SWP_NOACTIVATE,
	SWP_NOZORDER, WNDCLASSW, WS_CHILD, WS_VISIBLE,
};

// ---- shared control state (host → renderer over stdin) ---------------------------------
static OPEN: AtomicBool = AtomicBool::new(false);
static PACE: AtomicBool = AtomicBool::new(true);
static STATS: Mutex<[f32; 4]> = Mutex::new([0.0; 4]); // fps, latency_ms, decode_ms, mbps
/// Parent (Tauri) HWND passed via `--wid`; the child window is created under it.
static PARENT: AtomicU64 = AtomicU64::new(0);

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
	// Video chain (built lazily): MF decoder + NV12→RGB VideoProcessor.
	decoder: Option<decode::Decoder>,
	present: Option<present::Present>,
	codec: Codec,
	// Decoded source size (from the NV12 texture); drives the letterbox + processor rebuild.
	src_w: u32,
	src_h: u32,
	// fps counter for the `vidsink-fps` HUD line.
	frames: u32,
	last_fps_at: std::time::Instant,
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
			decoder: None,
			present: None,
			codec,
			src_w: 0,
			src_h: 0,
			frames: 0,
			last_fps_at: std::time::Instant::now(),
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
		// Capture/release the mouse on the open↔close edge so clicks reach the child window.
		if open != self.was_open {
			use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
			if open {
				let _ = SetCapture(self.hwnd);
				// The Files view re-requests the remote listing once per overlay visit.
				self.ov_ui.remote_requested = false;
			} else {
				let _ = ReleaseCapture();
				EGUI_EVENTS.lock().unwrap().clear();
				ENTER_IN.store(false, Ordering::SeqCst);
			}
			self.was_open = open;
		}
		if !open {
			return;
		}
		let Some(painter) = self.painter.as_mut() else {
			return;
		};
		let Some(rtv) = self.rtv.clone() else { return };

		// Live stats from the host (stdin `stat …`).
		let s = *STATS.lock().unwrap();
		if s[0] > 0.0 {
			self.ostate.fps = s[0];
			self.ostate.latency_ms = s[1];
			self.ostate.decode_ms = s[2];
			self.ostate.mbps = s[3];
		}
		self.ostate.open = true;
		self.ostate.pace = PACE.load(Ordering::SeqCst);

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
		let texs = match self.decoder.as_mut().unwrap().decode(au) {
			Ok(t) => t,
			Err(e) => {
				eprintln!("pulsar-render(win): decode error: {e}");
				return;
			}
		};
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
			println!("vidsink-fps {fps:.0} {}x{}", self.src_w, self.src_h);
			use std::io::Write;
			let _ = std::io::stdout().flush();
			self.frames = 0;
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
		let mut rc = RECT::default();
		if GetClientRect(self.parent, &mut rc).is_err() {
			return;
		}
		let w = (rc.right - rc.left).max(1) as u32;
		let h = (rc.bottom - rc.top).max(1) as u32;
		if w == self.width && h == self.height {
			return;
		}
		let _ = SetWindowPos(
			self.hwnd,
			None,
			0,
			0,
			w as i32,
			h as i32,
			SWP_NOZORDER | SWP_NOACTIVATE,
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
	loop {
		while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
			let _ = TranslateMessage(&msg);
			DispatchMessageW(&msg);
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
