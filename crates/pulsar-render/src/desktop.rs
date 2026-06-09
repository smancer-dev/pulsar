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

/// Read control + stats from stdin (host → overlay): `open` / `close` toggle visibility,
/// `stat <fps> <lat> <dec> <mbps>` updates the HUD.
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
            let v: Vec<f32> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() >= 4 {
                *STATS.lock().unwrap() = [v[0], v[1], v[2], v[3]];
            }
        }
    }
}

struct Overlay {
    state: OverlayState,
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
            for c in overlay::draw(ctx, &self.state) {
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
                mode = if s == "remote" { Mode::Remote } else { Mode::Game };
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

    let state = OverlayState { mode, open: false, id: "—".into(), conn_label: "P2P".into(), ..Default::default() };
    let _ = eframe::run_native(
        "pulsar-render",
        options,
        Box::new(move |cc| {
            overlay::apply_theme(&cc.egui_ctx);
            Ok(Box::new(Overlay { state, was_open: false }))
        }),
    );
}
