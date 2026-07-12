//! Pulsar Setup — Discord-style branded bootstrapper (see Cargo.toml header).
//!
//! Modes:
//!   (none)          branded UI install
//!   /S, --silent    headless install/update into the registered (or default) dir
//!   --uninstall     remove the install (branded confirm → progressless removal)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, Color32, RichText};

// Brand palette (design tokens.css, converted to sRGB): light-first, electric indigo.
const ACCENT: Color32 = Color32::from_rgb(0x58, 0x51, 0xdb); // oklch(0.555 0.205 272)
const BG: Color32 = Color32::from_rgb(0xfb, 0xfb, 0xfc);
const TEXT: Color32 = Color32::from_rgb(0x21, 0x21, 0x2b);
const FAINT: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x99);
const SURFACE: Color32 = Color32::from_rgb(0xf1, 0xf1, 0xf5);

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));

	if has("/S") || has("--silent") {
		// Headless: updater / scripted install. Exit code carries the outcome.
		std::process::exit(match install::silent_install() {
			Ok(()) => 0,
			Err(e) => {
				eprintln!("pulsar-setup: {e}");
				1
			}
		});
	}

	let uninstall = has("--uninstall");
	let opts = eframe::NativeOptions {
		viewport: egui::ViewportBuilder::default()
			.with_inner_size([460.0, 340.0])
			.with_resizable(false)
			.with_maximize_button(false),
		centered: true,
		..Default::default()
	};
	let _ = eframe::run_native(
		if uninstall { "Pulsar Kaldır" } else { "Pulsar Kurulumu" },
		opts,
		Box::new(move |cc| {
			cc.egui_ctx.set_visuals(visuals());
			Ok(Box::new(SetupApp::new(uninstall)))
		}),
	);
}

fn visuals() -> egui::Visuals {
	let mut v = egui::Visuals::light();
	v.panel_fill = BG;
	v.override_text_color = Some(TEXT);
	v.selection.bg_fill = ACCENT;
	v.widgets.hovered.bg_fill = SURFACE;
	v
}

enum Phase {
	Welcome,
	Installing,
	Done,
	Failed(String),
	ConfirmUninstall,
	Uninstalled,
}

struct SetupApp {
	phase: Phase,
	desktop_shortcut: bool,
	dir: PathBuf,
	total: usize,
	done_entries: usize,
	current: String,
	finishing: bool,
	rx: Option<Receiver<install::Msg>>,
	webview2_missing: bool,
	/// Animation clock for the pulse rings.
	t0: std::time::Instant,
}

impl SetupApp {
	fn new(uninstall: bool) -> Self {
		Self {
			phase: if uninstall { Phase::ConfirmUninstall } else { Phase::Welcome },
			desktop_shortcut: true,
			dir: install::registered_install_dir().unwrap_or_else(install::default_install_dir),
			total: 0,
			done_entries: 0,
			current: String::new(),
			finishing: false,
			rx: None,
			webview2_missing: false,
			t0: std::time::Instant::now(),
		}
	}

	fn start_install(&mut self) {
		let (tx, rx): (Sender<install::Msg>, Receiver<install::Msg>) = std::sync::mpsc::channel();
		self.rx = Some(rx);
		self.phase = Phase::Installing;
		let dir = self.dir.clone();
		let desktop = self.desktop_shortcut;
		std::thread::spawn(move || install::install(&dir, desktop, &tx));
	}

	fn pump(&mut self) {
		let Some(rx) = &self.rx else { return };
		while let Ok(m) = rx.try_recv() {
			match m {
				install::Msg::Total(n) => self.total = n,
				install::Msg::Entry(i, name) => {
					self.done_entries = i + 1;
					self.current = name;
				}
				install::Msg::Finishing => self.finishing = true,
				install::Msg::Done => {
					self.webview2_missing = !install::webview2_present();
					self.phase = Phase::Done;
				}
				install::Msg::Err(e) => self.phase = Phase::Failed(e),
			}
		}
	}

	/// The concentric pulse-rings logo, animated (the brand mark).
	fn logo(&self, ui: &mut egui::Ui) {
		let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::hover());
		let c = rect.center();
		let t = self.t0.elapsed().as_secs_f32();
		let p = ui.painter();
		p.circle_filled(c, 9.0, ACCENT);
		for i in 0..3 {
			let phase = ((t * 0.9) + i as f32 * 0.33) % 1.0;
			let r = 12.0 + phase * 24.0;
			let alpha = ((1.0 - phase) * 130.0) as u8;
			p.circle_stroke(
				c,
				r,
				egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(0x58, 0x51, 0xdb, alpha)),
			);
		}
	}
}

impl eframe::App for SetupApp {
	fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
		self.pump();
		ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ring anim + progress

		egui::CentralPanel::default().show(ctx, |ui| {
			ui.add_space(18.0);
			ui.vertical_centered(|ui| {
				self.logo(ui);
				ui.add_space(2.0);
				ui.label(RichText::new("Pulsar").size(24.0).strong());
				match &self.phase {
					Phase::Welcome => ui.label(RichText::new("Uzak masaüstü · oyun akışı").color(FAINT)),
					Phase::Installing => ui.label(RichText::new("Kuruluyor…").color(FAINT)),
					Phase::Done => ui.label(RichText::new("Kurulum tamamlandı").color(FAINT)),
					Phase::Failed(_) => ui.label(RichText::new("Kurulum başarısız").color(FAINT)),
					Phase::ConfirmUninstall => ui.label(RichText::new("Kaldırma").color(FAINT)),
					Phase::Uninstalled => ui.label(RichText::new("Kaldırıldı").color(FAINT)),
				}
			});
			ui.add_space(16.0);

			match &self.phase {
				Phase::Welcome => {
					ui.vertical_centered(|ui| {
						ui.label(
							RichText::new(format!("Kurulum yeri: {}", self.dir.display()))
								.color(FAINT)
								.size(11.5),
						);
						ui.add_space(6.0);
						ui.checkbox(&mut self.desktop_shortcut, "Masaüstü kısayolu oluştur");
						ui.add_space(14.0);
						if !install::have_payload() {
							ui.colored_label(
								Color32::from_rgb(0xc0, 0x3a, 0x2e),
								"Bu derlemede gömülü uygulama paketi yok (geliştirme sürümü).",
							);
						} else {
							let btn = egui::Button::new(
								RichText::new(format!("Kur  ·  v{}", install::version()))
									.size(15.0)
									.color(Color32::WHITE),
							)
							.fill(ACCENT)
							.min_size(egui::vec2(220.0, 40.0))
							.rounding(8.0);
							if ui.add(btn).clicked() {
								self.start_install();
							}
						}
					});
				}
				Phase::Installing => {
					ui.vertical_centered(|ui| {
						let frac = if self.total > 0 {
							self.done_entries as f32 / self.total as f32
						} else {
							0.0
						};
						ui.add(
							egui::ProgressBar::new(frac)
								.desired_width(320.0)
								.fill(ACCENT)
								.text(if self.finishing {
									"Kısayollar ve kayıt…".to_string()
								} else {
									format!("{}/{}", self.done_entries, self.total.max(1))
								}),
						);
						ui.add_space(6.0);
						ui.label(RichText::new(&self.current).color(FAINT).size(11.0));
					});
				}
				Phase::Done => {
					ui.vertical_centered(|ui| {
						if self.webview2_missing {
							ui.label(
								RichText::new(
									"WebView2 çalışma zamanı bulunamadı — arayüz için gerekli.\n\
									 İndirip kurmak için aşağıdaki düğmeyi kullan (internet gerekir).",
								)
								.color(FAINT)
								.size(12.0),
							);
							ui.add_space(6.0);
							if ui.button("WebView2'yi indir ve kur").clicked() {
								install::install_webview2();
								self.webview2_missing = !install::webview2_present();
							}
							ui.add_space(10.0);
						}
						let btn = egui::Button::new(
							RichText::new("Pulsar'ı Başlat").size(15.0).color(Color32::WHITE),
						)
						.fill(ACCENT)
						.min_size(egui::vec2(220.0, 40.0))
						.rounding(8.0);
						if ui.add(btn).clicked() {
							install::launch_app(&self.dir);
							ctx.send_viewport_cmd(egui::ViewportCommand::Close);
						}
					});
				}
				Phase::Failed(e) => {
					let e = e.clone();
					ui.vertical_centered(|ui| {
						ui.colored_label(Color32::from_rgb(0xc0, 0x3a, 0x2e), &e);
						ui.add_space(10.0);
						if ui.button("Tekrar dene").clicked() {
							self.start_install();
						}
					});
				}
				Phase::ConfirmUninstall => {
					ui.vertical_centered(|ui| {
						ui.label("Pulsar bu bilgisayardan kaldırılsın mı?");
						ui.add_space(14.0);
						ui.horizontal(|ui| {
							ui.add_space(ui.available_width() / 2.0 - 110.0);
							if ui
								.add(egui::Button::new("Vazgeç").min_size(egui::vec2(100.0, 34.0)))
								.clicked()
							{
								ctx.send_viewport_cmd(egui::ViewportCommand::Close);
							}
							let btn = egui::Button::new(RichText::new("Kaldır").color(Color32::WHITE))
								.fill(Color32::from_rgb(0xc0, 0x3a, 0x2e))
								.min_size(egui::vec2(100.0, 34.0));
							if ui.add(btn).clicked() {
								install::uninstall();
								self.phase = Phase::Uninstalled;
							}
						});
					});
				}
				Phase::Uninstalled => {
					ui.vertical_centered(|ui| {
						ui.label("Pulsar kaldırıldı. Bu pencereyi kapatabilirsin.");
						ui.add_space(10.0);
						if ui.button("Kapat").clicked() {
							ctx.send_viewport_cmd(egui::ViewportCommand::Close);
						}
					});
				}
			}
		});
	}
}
