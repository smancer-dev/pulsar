//! Windows / macOS overlay backend — eframe (winit + wgpu: D3D12 on Windows, Metal on macOS).
//! Renders the SAME `overlay.rs` egui UI as the Linux egui_glow path, so the overlay looks
//! identical across all platforms. A transparent, borderless, always-on-top window; shown while
//! the overlay is open, click-through while closed.
//!
//! Phase: brings up the identical overlay on these platforms. Still TODO for full parity with
//! Linux: native video decode behind it (replacing the webview WebCodecs path) + tracking the
//! Tauri window's exact geometry + the toggle wired from the OS keyboard hook. Today it toggles
//! on stdin `open`/`close` and reads `stat …` lines (same protocol the host already speaks).

use crate::overlay::{self, Mode, OverlayCmd, OverlayState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static OPEN: AtomicBool = AtomicBool::new(false);
static STATS: Mutex<[f32; 4]> = Mutex::new([0.0; 4]); // fps, latency_ms, decode_ms, mbps
/// Chat log fed over stdin (`chat in|out <text>`, same protocol as linux.rs) —
/// shown in the overlay's Chat view while it is open.
static CHAT_LOG: Mutex<Vec<(bool, String)>> = Mutex::new(Vec::new());
/// Host messages not yet seen in the overlay Chat view — badge on the open button.
static CHAT_UNREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Files view, REMOTE pane: (HOME-relative path, rows) from `fsjson …` (same
/// one-line JSON as linux.rs) — copied into the overlay state each frame.
static FS_REMOTE: Mutex<(String, Vec<overlay::FsRow>)> = Mutex::new((String::new(), Vec::new()));

/// Read control + stats from stdin (host → overlay): `open` / `close` toggle visibility,
/// `stat <fps> <lat> <dec> <mbps>` updates the HUD, `chat`/`fsjson` feed the
/// overlay's Chat + Files views.
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
		// `toast <text>` is tolerated but not drawn yet: this backend only paints
		// while the overlay is OPEN — closed-state toast/HUD is the follow-up.
	}
}

struct Overlay {
	state: OverlayState,
	// Frame-persistent overlay UI state (current page, chat composer, local file pane).
	ov_ui: overlay::UiState,
	was_open: bool,
}

impl eframe::App for Overlay {
	fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
		[0.0, 0.0, 0.0, 0.0] // transparent so the video/desktop below shows through
	}

	fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
		let open = OPEN.load(Ordering::SeqCst);
		self.state.open = open;
		// Show/hide + click-through follow the open state.
		if open != self.was_open {
			ctx.send_viewport_cmd(egui::ViewportCommand::Visible(open));
			ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!open));
			if open {
				// The Files view re-requests the remote listing once per overlay visit.
				self.ov_ui.remote_requested = false;
			}
			self.was_open = open;
		}
		if open {
			let s = *STATS.lock().unwrap();
			if s[0] > 0.0 {
				self.state.fps = s[0];
				self.state.latency_ms = s[1];
				self.state.decode_ms = s[2];
				self.state.mbps = s[3];
			}
			self.state.chat = CHAT_LOG.lock().unwrap().clone();
			// Reading the chat clears the unread badge (this backend only paints while
			// the overlay is open — the badge itself shows on Windows/Linux paths).
			if self.ov_ui.view == overlay::View::Chat {
				CHAT_UNREAD.store(0, Ordering::SeqCst);
			}
			self.state.chat_unread = CHAT_UNREAD.load(Ordering::SeqCst);
			{
				let (p, rows) = &*FS_REMOTE.lock().unwrap();
				self.state.fs_remote_path = p.clone();
				self.state.fs_remote = rows.clone();
			}
			// This backend has REAL keyboard focus (its own eframe window), so Enter
			// in the chat composer arrives as normal egui input — no `kin` relay.
			self.state.chat_enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
			for c in overlay::draw(ctx, &self.state, &mut self.ov_ui) {
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
							_ => {}
						}
						println!("ov set {field} {val}");
					}
					OverlayCmd::End => println!("ov end"),
					OverlayCmd::Close => {
						OPEN.store(false, Ordering::SeqCst);
						println!("ov close");
					}
					// Same wire lines as the Linux backend (render_stats.rs parses them).
					OverlayCmd::Chat(t) => println!("ov chat {t}"),
					OverlayCmd::FsLs(p) => println!("ov fsls {p}"),
					OverlayCmd::FsGet(p) => println!("ov fsget {p}"),
					OverlayCmd::FsSend(p) => println!("ov fssend {p}"),
					OverlayCmd::OpenFiles => println!("ov files"),
				}
			}
			use std::io::Write;
			let _ = std::io::stdout().flush();
		}
		ctx.request_repaint();
	}
}

pub fn run() {
	let args: Vec<String> = std::env::args().collect();
	let mut mode = Mode::Game;
	let mut i = 1;
	while i < args.len() {
		if args[i] == "--mode" {
			if let Some(s) = args.get(i + 1) {
				mode = if s == "remote" {
					Mode::Remote
				} else {
					Mode::Game
				};
				i += 1;
			}
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
			.with_visible(false)
			.with_maximized(true),
		..Default::default()
	};

	let state = OverlayState {
		mode,
		open: false,
		id: "—".into(),
		conn_label: "P2P".into(),
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
			}))
		}),
	);
}
