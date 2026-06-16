//! The in-session overlay — pure egui, backend-agnostic, so every platform paints
//! the IDENTICAL UI. Mirrors the Svelte game overlay (`Session.svelte`): header +
//! stat tiles + selector rows + quality segment + End + footer. Game mode shows the
//! slim set; Remote mode adds the full remote controls (file/clipboard/monitor) later.

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
	Game,
	Remote,
}

/// Live state the host process feeds in each frame (stats) + the current selector
/// values (the overlay owns these once open; changes flow OUT as `OverlayCmd`).
pub struct OverlayState {
	pub mode: Mode,
	pub open: bool,
	pub id: String,
	pub conn_label: String,
	// live stats
	pub fps: f32,
	pub latency_ms: f32,
	pub decode_ms: f32,
	pub mbps: f32,
	// selector current values (string keys matching the frontend setters)
	pub codec: String,
	pub encoder: String,
	/// The ACTUAL decoder in use (read-only display — selection is automatic).
	pub decoder: String,
	/// Host's validated caps (empty = unknown → no filtering): codecs the host can
	/// emit and encoder backends it has. Synced from the app via the stdin `caps` line.
	pub host_codecs: Vec<String>,
	pub host_encoders: Vec<String>,
	/// Host's streamable monitors as `(idx, label)`, primary first — the Display
	/// section's screen picker. Empty / single = no picker. Synced over the `caps` line.
	pub displays: Vec<(u32, String)>,
	/// Currently-streamed host monitor index (0 = primary). Owned by the overlay once
	/// open; a change flows out as `OverlayCmd::Set("display", "<idx>")`.
	pub display_idx: u32,
	/// Host's ACTIVE encode summary (split per-field under the selectors).
	pub host_active: String,
	/// Always-on mini stats HUD while the overlay is CLOSED (user toggle, persisted
	/// by the frontend; synced over stdin `statshud 0|1`).
	pub stats_hud: bool,
	/// Parsec-style always-visible overlay-open button (Pulsar mark, top-center)
	/// while the overlay is CLOSED. Toggleable from the overlay + Settings.
	pub overlay_btn: bool,
	/// Overlay-open button position: top-left offset in egui POINTS. Drag-movable
	/// from the webview hotspot; synced over stdin (`ovbtnpos <x> <y>`, re-seeded
	/// after a respawn via the caps line's `btnpos=x,y`).
	pub btn_pos: (f32, f32),
	pub res: String,
	pub fps_sel: String,
	pub bitrate: String, // Mbit, "0" = auto
	pub quality: String, // "latency" | "quality"
	pub pace: bool,      // frame pacing (Moonlight-style smoothing) on/off
	/// View-fit mode label ("fit"/"stretch"/"original") — mirrors video::fit_label.
	pub fit: String,
	/// Session audio state (transmit from host / host muted / mic to host).
	pub audio_tx: bool,
	pub audio_mute: bool,
	pub mic_on: bool,
	/// Chat log (me, text) — fed by the app over stdin (`chat in|out …`).
	pub chat: Vec<(bool, String)>,
	/// Host messages received while the Chat view wasn't open — drawn as a count
	/// badge on the overlay-open button; the backend zeroes it on Chat entry.
	pub chat_unread: usize,
	/// Whether the relayed Enter key fired this frame (sends the chat composer).
	pub chat_enter: bool,
	/// Remote file pane: current HOME-relative path + its entries (`fsjson` stdin).
	pub fs_remote_path: String,
	pub fs_remote: Vec<FsRow>,
	/// Connected controllers (slot, kind_label, device_name) — synced from the app
	/// over stdin (`ctrllist <json>` or similar). Game mode only.
	pub controllers: Vec<(u8, String, String)>,
}

/// Which overlay page is showing: the compact ROOT (category boxes) or a section.
/// The windowed app is often ~720p — the old single tall panel didn't fit, so the
/// overlay is now a small hub whose boxes open per-topic sub-views (AnyDesk-style).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
	#[default]
	Root,
	Stream,      // codec/encoder/decoder/res/fps/bandwidth/quality/pacing
	Display,     // view-fit modes
	Audio,       // transmit / host-mute / mic / call
	Tools,       // clipboard / reverse / fullscreen / file-pick
	Chat,        // NATIVE chat (the webview menu is gone on Linux)
	Files,       // NATIVE two-pane file manager
	Gauges,      // stats HUD + overlay button toggles
	Controllers, // player-slot list + swap order (game mode only)
}

/// One row of a file listing (local pane = the renderer's own std::fs; remote pane
/// = the host's `FsEntries`, relayed by the app over stdin as one-line JSON).
#[derive(Clone)]
pub struct FsRow {
	pub name: String,
	pub dir: bool,
	pub size: u64,
}

/// Frame-persistent UI state the render loop owns (the overlay itself is immediate
/// mode): current page, the chat composer buffer, and the LOCAL file pane.
#[derive(Default)]
pub struct UiState {
	pub view: View,
	/// Mirror of `view` from the previous frame + a bump counter: a mismatch marks
	/// the frame a section was entered, (re)starting the fade/slide-in animation
	/// (the counter keys a fresh egui animation id per transition).
	pub anim_view: View,
	pub anim_gen: u64,
	/// Overlay open-state from the previous draw, so the enter animation can be
	/// (re)triggered on the closed→open edge even when the view is unchanged (e.g.
	/// reopening while the last view was Root — without this the fade never plays
	/// because anim_view already equals view). See `draw`.
	pub was_open: bool,
	pub chat_input: String,
	pub local_path: String, // HOME-relative ("" = home)
	pub local_rows: Vec<FsRow>,
	pub local_loaded: bool,
	/// Whether the Files view already asked the host for its initial listing —
	/// once per overlay open (backends reset it on the open edge), NOT per frame:
	/// an empty/failed reply must not loop `ov fsls` at frame rate.
	pub remote_requested: bool,
}

impl Default for OverlayState {
	fn default() -> Self {
		Self {
			mode: Mode::Game,
			open: false,
			id: String::new(),
			conn_label: String::new(),
			fps: 0.0,
			latency_ms: 0.0,
			decode_ms: 0.0,
			mbps: 0.0,
			codec: "auto".into(),
			encoder: "auto".into(),
			host_codecs: Vec::new(),
			displays: Vec::new(),
			display_idx: 0,
			host_active: String::new(),
			stats_hud: false,
			overlay_btn: true,
			btn_pos: BTN_POS_DEFAULT,
			host_encoders: Vec::new(),
			decoder: "auto".into(),
			res: "auto".into(),
			fps_sel: "auto".into(),
			bitrate: "0".into(),
			quality: "latency".into(),
			pace: false,
			fit: "fit".into(),
			audio_tx: true,
			audio_mute: false,
			mic_on: false,
			chat: Vec::new(),
			chat_unread: 0,
			chat_enter: false,
			fs_remote_path: String::new(),
			fs_remote: Vec::new(),
			controllers: Vec::new(),
		}
	}
}

/// Emitted on interaction → serialized to stdout (`ov <field> <value>`) for the host.
pub enum OverlayCmd {
	Set(&'static str, String),
	End,
	Close,
	/// Send a chat line to the host (`ov chat <text>` — rest-of-line payload).
	Chat(String),
	/// Remote file-pane ops, HOME-relative paths (rest-of-line):
	/// `ov fsls <path>` list, `ov fsget <path>` download, `ov fssend <abs>` upload.
	FsLs(String),
	FsGet(String),
	FsSend(String),
	/// Open the app-side per-session file-manager WINDOW (`ov files`) — the overlay's
	/// Files box routes here instead of the cramped in-overlay two-pane view.
	OpenFiles,
}

fn codecs_opts() -> [(&'static str, &'static str); 4] {
	[
		("auto", t("auto")),
		("h264", "H.264"),
		("h265", "H.265"),
		("av1", "AV1"),
	]
}
fn encoders_opts() -> [(&'static str, &'static str); 8] {
	[
		("auto", t("auto")),
		("nvenc", "NVIDIA NVENC"),
		("qsv", "Intel QuickSync"),
		("amf", "AMD AMF"),
		("videotoolbox", "Apple VideoToolbox"),
		("vaapi", "VA-API"),
		("rkmpp", "Rockchip MPP"),
		("software", t("software")),
	]
}
fn res_opts() -> [(&'static str, &'static str); 4] {
	[
		("auto", t("auto")),
		("1080p", "1080p"),
		("1440p", "1440p"),
		("4K", "4K"),
	]
}
fn fps_opts() -> [(&'static str, &'static str); 4] {
	[("auto", t("auto")), ("30", "30"), ("60", "60"), ("120", "120")]
}
fn bitrate_opts() -> [(&'static str, &'static str); 6] {
	[
		("0", t("auto")),
		("10", "10 Mbit"),
		("20", "20 Mbit"),
		("30", "30 Mbit"),
		("50", "50 Mbit"),
		("100", "100 Mbit"),
	]
}

const ACCENT: egui::Color32 = egui::Color32::from_rgb(124, 110, 245); // electric indigo
const CYAN: egui::Color32 = egui::Color32::from_rgb(120, 200, 240);

/// All overlay strings come from the central language catalogs (`lang/*.json`)
/// through the keyed lookup — see `i18n.rs`.
use crate::i18n::t;

/// Apply the Pulsar dark theme to an egui context (call once at startup).
/// Centered "switching screen…" indicator (spinner + label) drawn over the held last
/// frame while a monitor/codec switch waits for the new stream's first keyframe. Keeps
/// the user informed that the change is loading, not hung.
pub fn draw_switching(ctx: &egui::Context) {
	egui::Area::new(egui::Id::new("switching"))
		.anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
		.show(ctx, |ui| {
			egui::Frame::none()
				.fill(egui::Color32::from_rgba_unmultiplied(18, 20, 26, 230))
				.rounding(12.0)
				.inner_margin(egui::Margin::symmetric(22.0, 18.0))
				.show(ui, |ui| {
					ui.vertical_centered(|ui| {
						ui.add(egui::Spinner::new().size(28.0));
						ui.add_space(8.0);
						ui.label(
							egui::RichText::new(t("switching"))
								.size(14.0)
								.color(egui::Color32::WHITE),
						);
					});
				});
		});
}

/// Centered "stream stopped" indicator drawn over the frozen last frame when the stall
/// detector has tripped (no fresh frame for ≥ 3 s while video was live). Mirrors the
/// webview's `.stall` div that is permanently occluded by the native renderer window;
/// this surfaces the same message inside the renderer where the user can see it.
pub fn draw_stalled(ctx: &egui::Context) {
	egui::Area::new(egui::Id::new("stalled"))
		.anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
		.show(ctx, |ui| {
			egui::Frame::none()
				.fill(egui::Color32::from_rgba_unmultiplied(44, 18, 18, 230))
				.stroke(egui::Stroke::new(
					1.0,
					egui::Color32::from_rgba_unmultiplied(200, 60, 60, 180),
				))
				.rounding(12.0)
				.inner_margin(egui::Margin::symmetric(22.0, 18.0))
				.show(ui, |ui| {
					ui.set_max_width(320.0);
					ui.vertical_centered(|ui| {
						ui.label(
							egui::RichText::new("⚠")
								.size(28.0)
								.color(egui::Color32::from_rgb(255, 160, 100)),
						);
						ui.add_space(6.0);
						ui.label(
							egui::RichText::new(t("stalled"))
								.size(13.5)
								.color(egui::Color32::from_rgb(255, 210, 200)),
						);
					});
				});
		});
}

/// Parse the `caps` line's `displays=idx:name:w:h:primary,…` field into the overlay's
/// `(idx, label)` monitor list (primary marked). Tolerant: skips malformed entries.
pub fn parse_displays(s: &str) -> Vec<(u32, String)> {
	if s.is_empty() {
		return Vec::new();
	}
	s.split(',')
		.filter_map(|e| {
			let mut p = e.split(':');
			let idx: u32 = p.next()?.parse().ok()?;
			let name = p.next()?;
			let w = p.next().unwrap_or("");
			let h = p.next().unwrap_or("");
			let primary = p.next() == Some("1");
			let mut label = name.to_string();
			if !w.is_empty() && !h.is_empty() {
				label.push_str(&format!(" {w}×{h}"));
			}
			if primary {
				label.push_str(&format!(" ({})", t("monitor.primary")));
			}
			Some((idx, label))
		})
		.collect()
}

pub fn apply_theme(ctx: &egui::Context) {
	let mut v = egui::Visuals::dark();
	v.panel_fill = egui::Color32::from_rgba_premultiplied(13, 14, 20, 235);
	v.window_fill = egui::Color32::from_rgb(18, 20, 28);
	v.widgets.inactive.bg_fill = egui::Color32::from_rgb(28, 30, 42);
	v.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 42, 58);
	v.selection.bg_fill = ACCENT;
	v.override_text_color = Some(egui::Color32::from_rgb(228, 230, 240));
	ctx.set_visuals(v);
	let mut style = (*ctx.style()).clone();
	style.spacing.item_spacing = egui::vec2(10.0, 10.0);
	style.spacing.button_padding = egui::vec2(10.0, 6.0);
	ctx.set_style(style);
}

/// Draw the overlay for this frame; returns the commands the user triggered.
/// `ui_state` (page, chat composer, local file pane) is owned by the render loop
/// so it survives frames; the page resets when the overlay (re)opens.
pub fn draw(ctx: &egui::Context, st: &OverlayState, ui_state: &mut UiState) -> Vec<OverlayCmd> {
	let mut cmds = Vec::new();
	if !st.open {
		// Remember we're closed so the next open is detected as an edge below.
		ui_state.was_open = false;
		return cmds;
	}
	// Closed→open edge: force the enter animation to restart even if the view didn't
	// change since last open (the backends reset `view` to Root on open but the fade
	// is keyed off an anim_view != view mismatch — reopening on Root would otherwise
	// skip the fade). Resetting anim_view to a sentinel guarantees the mismatch fires.
	let open_edge = !ui_state.was_open;
	ui_state.was_open = true;
	// Dim scrim behind the panel; clicking it closes the overlay.
	egui::Area::new("scrim".into())
		.order(egui::Order::Background)
		.fixed_pos(egui::pos2(0.0, 0.0))
		.show(ctx, |ui| {
			let r = ctx.screen_rect();
			ui.painter()
				.rect_filled(r, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 120));
			if ui
				.interact(r, egui::Id::new("scrim_click"), egui::Sense::click())
				.clicked()
			{
				cmds.push(OverlayCmd::Close);
			}
		});

	// The panel must FIT a windowed app (~720p): compact fixed width and per-topic
	// sub-views keep every page short (tallest ≈ 340 pt — under a 576 pt 720p
	// surface), so no scroll container is needed (an auto-height egui Window
	// collapses a ScrollArea to its first row).
	egui::Window::new("pulsar_overlay")
		.title_bar(false)
		.resizable(false)
		.collapsible(false)
		.anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
		.fixed_size(egui::vec2(360.0, 0.0))
		.frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(14.0)))
		.show(ctx, |ui| {
			// Header: brand + mode + back arrow when inside a section.
			ui.horizontal(|ui| {
				if ui_state.view != View::Root {
					// PAINTED back arrow — the "←" glyph is missing from egui's bundled
					// fonts (it rendered as a tofu box, i.e. "no icon" to the user).
					let (rect, resp) =
						ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
					let col = if resp.hovered() {
						egui::Color32::WHITE
					} else {
						egui::Color32::from_rgb(150, 155, 170)
					};
					let c = rect.center();
					let s = egui::Stroke::new(2.0, col);
					let p = ui.painter();
					p.line_segment([c + egui::vec2(-6.0, 0.0), c + egui::vec2(6.0, 0.0)], s);
					p.line_segment([c + egui::vec2(-6.0, 0.0), c + egui::vec2(-1.0, -5.0)], s);
					p.line_segment([c + egui::vec2(-6.0, 0.0), c + egui::vec2(-1.0, 5.0)], s);
					if resp.clicked() {
						ui_state.view = View::Root;
					}
					resp.on_hover_cursor(egui::CursorIcon::PointingHand);
				}
				ui.heading(egui::RichText::new("Pulsar").color(CYAN).strong());
				let (icon, key) = match (ui_state.view, st.mode) {
					(View::Stream, _) => ("📡", "view.stream"),
					(View::Display, _) => ("🖥", "view.display"),
					(View::Audio, _) => ("🔊", "view.audio"),
					(View::Tools, _) => ("🛠", "view.tools"),
					(View::Chat, _) => ("💬", "view.chat"),
					(View::Files, _) => ("📁", "view.files"),
					(View::Gauges, _) => ("📊", "view.gauges"),
					(View::Controllers, _) => ("🎮", "view.controllers"),
					(View::Root, Mode::Game) => ("🎮", "mode.game"),
					(View::Root, Mode::Remote) => ("🖥", "mode.remote"),
				};
				ui.label(
					egui::RichText::new(format!("{icon} {}", t(key))).color(egui::Color32::GRAY),
				);
				ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
					ui.label(
						egui::RichText::new(format!("🔗 {}", st.conn_label))
							.monospace()
							.color(egui::Color32::from_rgb(150, 155, 170)),
					);
				});
			});
			ui.add_space(6.0);

			// Section enter/leave animation: a quick fade + downward slide-in instead of
			// the old instant swap. The transition restarts on every view change (the
			// bumped counter keys a fresh animation id, primed at 0 the first frame).
			if ui_state.anim_view != ui_state.view || open_edge {
				ui_state.anim_view = ui_state.view;
				ui_state.anim_gen = ui_state.anim_gen.wrapping_add(1);
				ui.ctx().animate_value_with_time(
					egui::Id::new(("ov-view-fade", ui_state.anim_gen)),
					0.0,
					0.0,
				);
			}
			let fade = ui.ctx().animate_value_with_time(
				egui::Id::new(("ov-view-fade", ui_state.anim_gen)),
				1.0,
				0.16,
			);
			ui.scope(|ui| {
				ui.set_opacity(fade);
				ui.add_space((1.0 - fade) * 8.0);
				match ui_state.view {
					View::Root => draw_root(ui, st, &mut ui_state.view, &mut cmds),
					View::Stream => draw_stream(ui, st, &mut cmds),
					View::Display => draw_display(ui, st, st.mode, &mut cmds),
					View::Audio => draw_audio(ui, st, &mut cmds),
					View::Tools => draw_tools(ui, &mut ui_state.view, &mut cmds),
					View::Chat => draw_chat(ui, st, &mut ui_state.chat_input, &mut cmds),
					View::Files => draw_files(ui, st, ui_state, &mut cmds),
					View::Gauges => draw_gauges(ui, st, &mut cmds),
					View::Controllers => draw_controllers(ui, st, &mut cmds),
				}
			});
			ui.add_space(6.0);
			ui.label(
				egui::RichText::new(t("shortcuts"))
				.monospace()
				.small()
				.color(egui::Color32::from_rgb(120, 125, 140)),
			);
		});
	cmds
}

/// Root hub: compact stat tiles + the category boxes + end-session.
fn draw_root(ui: &mut egui::Ui, st: &OverlayState, view: &mut View, cmds: &mut Vec<OverlayCmd>) {
	let (enc_ms, target_mbit) = host_parts(st);
	let bw = fmt_mbps(st.mbps);
	// Two compact tile rows: RTT · fps · encode / decode · BITRATE target · bandwidth.
	ui.horizontal(|ui| {
		stat_tile(ui, &format!("{:.0}", st.latency_ms), "📡", t("stat.latency"));
		stat_tile(ui, &format!("{:.0}", st.fps), "🎬", "FPS");
		stat_tile(
			ui,
			&enc_ms.map_or("—".into(), |v: f32| format!("{v:.1}")),
			"🖥",
			t("stat.encode"),
		);
	});
	ui.horizontal(|ui| {
		stat_tile(ui, &format!("{:.1}", st.decode_ms), "⚡", t("stat.decode"));
		stat_tile(
			ui,
			&target_mbit.map_or("—".into(), |v: f32| format!("{v:.0}")),
			"🎯",
			"Bitrate Mbps",
		);
		stat_tile(ui, &bw, "📶", t("stat.band"));
	});
	ui.add_space(8.0);

	// Category boxes, 2 per row (AnyDesk-style hub). Chat + Files are NATIVE views
	// now — the webview session menu is gone on Linux (it lived under the video).
	// Game mode shows the slim set (Stream + Display view-fit + Gauges only);
	// Remote mode gets the full seven boxes incl. file transfer, chat, mic, monitor.
	let game_boxes: &[(&str, &str, View)] = &[
		("📡", t("view.stream"), View::Stream),
		("🖥", t("view.display"), View::Display),
		("📊", t("view.gauges"), View::Gauges),
		("🎮", t("view.controllers"), View::Controllers),
	];
	let remote_boxes: &[(&str, &str, View)] = &[
		("📡", t("view.stream"), View::Stream),
		("🖥", t("view.display"), View::Display),
		("🔊", t("view.audio"), View::Audio),
		("💬", t("view.chat"), View::Chat),
		("📁", t("view.files"), View::Files),
		("🛠", t("view.tools"), View::Tools),
		("📊", t("view.gauges"), View::Gauges),
	];
	let boxes: &[(&str, &str, View)] = match st.mode {
		Mode::Game => game_boxes,
		Mode::Remote => remote_boxes,
	};
	egui::Grid::new("ov_boxes")
		.num_columns(2)
		.spacing(egui::vec2(8.0, 8.0))
		.show(ui, |ui| {
			for (i, (icon, label, v)) in boxes.iter().enumerate() {
				if cat_box(ui, icon, label) {
					// Files opens the app's dedicated per-session window — the in-overlay
					// two-pane view was too cramped (kept as code for a potential fallback).
					if *v == View::Files {
						cmds.push(OverlayCmd::OpenFiles);
					} else {
						*view = *v;
					}
				}
				if i % 2 == 1 {
					ui.end_row();
				}
			}
		});
	ui.add_space(10.0);
	if ui
		.add(
			egui::Button::new(
				egui::RichText::new(format!("✖  {}", t("end"))).color(egui::Color32::WHITE),
			)
				.fill(egui::Color32::from_rgb(200, 60, 70))
				.min_size(egui::vec2(ui.available_width(), 32.0)),
		)
		.clicked()
	{
		cmds.push(OverlayCmd::End);
	}
}

/// Stream section: codec/encoder/decoder + res/fps/bandwidth-limit + quality/pacing.
fn draw_stream(ui: &mut egui::Ui, st: &OverlayState, cmds: &mut Vec<OverlayCmd>) {
	let parts: Vec<&str> = st.host_active.split(" · ").collect();
	let act = |i: usize| parts.get(i).copied().unwrap_or("").to_string();
	let bw = fmt_mbps(st.mbps);
	let act_bitrate = if parts.len() > 4 {
		format!("{} {} · {} {bw} Mbps", t("limit"), act(4), t("used"))
	} else if st.mbps > 0.0 {
		format!("{} {bw} Mbps", t("used"))
	} else {
		String::new()
	};
	// Capability gating: only offer what the HOST really has (empty = unknown
	// host → show everything, it degrades gracefully server-side anyway).
	let codecs: Vec<(&str, &str)> = codecs_opts()
		.iter()
		.filter(|(v, _)| {
			*v == "auto" || st.host_codecs.is_empty() || st.host_codecs.iter().any(|c| c == v)
		})
		.copied()
		.collect();
	let encoders: Vec<(&str, &str)> = encoders_opts()
		.iter()
		.filter(|(v, _)| {
			*v == "auto" || st.host_encoders.is_empty() || st.host_encoders.iter().any(|c| c == v)
		})
		.copied()
		.collect();
	egui::Grid::new("ov_stream")
		.num_columns(2)
		.spacing(egui::vec2(12.0, 8.0))
		.show(ui, |ui| {
			if let Some(v) = combo(ui, "Codec", "codec", &st.codec, &codecs, &act(0)) {
				cmds.push(OverlayCmd::Set("codec", v));
			}
			if let Some(v) = combo(ui, "Encoder", "encoder", &st.encoder, &encoders, &act(1)) {
				cmds.push(OverlayCmd::Set("encoder", v));
			}
			ui.end_row();
			// The decoder is auto-selected by this renderer — display only.
			ui.label(
				egui::RichText::new("Decoder")
					.size(12.0)
					.color(egui::Color32::from_gray(160)),
			);
			ui.label(
				egui::RichText::new(if st.decoder.is_empty() {
					t("auto").to_string()
				} else {
					st.decoder.clone()
				})
				.size(12.0),
			);
			ui.end_row();
			if let Some(v) = combo(ui, t("res"), "res", &st.res, &res_opts(), &act(2)) {
				cmds.push(OverlayCmd::Set("res", v));
			}
			if let Some(v) = combo(ui, "FPS", "fps", &st.fps_sel, &fps_opts(), &act(3)) {
				cmds.push(OverlayCmd::Set("fps", v));
			}
			ui.end_row();
			if let Some(v) = combo(
				ui,
				t("bwlimit"),
				"bitrate",
				&st.bitrate,
				&bitrate_opts(),
				&act_bitrate,
			) {
				cmds.push(OverlayCmd::Set("bitrate", v));
			}
			ui.end_row();
		});
	ui.add_space(8.0);
	ui.horizontal(|ui| {
		ui.label(t("quality"));
		info(ui, t("info.quality"));
		if seg(ui, st.quality == "latency", t("lowlat")) {
			cmds.push(OverlayCmd::Set("quality", "latency".into()));
		}
		if seg(ui, st.quality == "quality", t("quality")) {
			cmds.push(OverlayCmd::Set("quality", "quality".into()));
		}
	});
	ui.horizontal(|ui| {
		ui.label(t("pacing"));
		info(ui, t("info.pacing"));
		if seg(ui, st.pace, t("on")) {
			cmds.push(OverlayCmd::Set("pace", "on".into()));
		}
		if seg(ui, !st.pace, t("off")) {
			cmds.push(OverlayCmd::Set("pace", "off".into()));
		}
	});
}

/// Display section: host monitor picker (when the host has >1, remote mode only) +
/// AnyDesk-style view-fit modes (renderer-local, instant).
fn draw_display(ui: &mut egui::Ui, st: &OverlayState, mode: Mode, cmds: &mut Vec<OverlayCmd>) {
	// Host monitor picker — remote mode only (multi-monitor is irrelevant in-game)
	// and only meaningful when the host exposes more than one display.
	if mode == Mode::Remote && st.displays.len() > 1 {
		ui.label(
			egui::RichText::new(format!("🖵 {}", t("monitor.title")))
				.small()
				.color(egui::Color32::GRAY),
		);
		ui.add_space(4.0);
		ui.horizontal_wrapped(|ui| {
			for (idx, label) in &st.displays {
				let selected = *idx == st.display_idx;
				let fill = if selected {
					ACCENT
				} else {
					egui::Color32::from_rgb(40, 44, 54)
				};
				// A monitor glyph over the screen's label (name + WxH) — a square-ish tile.
				let text = egui::RichText::new(format!("🖥\n{label}"))
					.size(12.0)
					.color(egui::Color32::WHITE);
				if ui
					.add(
						egui::Button::new(text)
							.fill(fill)
							.min_size(egui::vec2(120.0, 56.0)),
					)
					.clicked()
					&& !selected
				{
					cmds.push(OverlayCmd::Set("display", idx.to_string()));
				}
			}
		});
		ui.add_space(8.0);
	}
	ui.label(
		egui::RichText::new(format!("🖥 {}", t("fit.title")))
			.small()
			.color(egui::Color32::GRAY),
	);
	ui.horizontal(|ui| {
		if seg(ui, st.fit == "fit", t("fit.fit")) {
			cmds.push(OverlayCmd::Set("fit", "fit".into()));
		}
		if seg(ui, st.fit == "stretch", t("fit.stretch")) {
			cmds.push(OverlayCmd::Set("fit", "stretch".into()));
		}
		if seg(ui, st.fit == "original", t("fit.original")) {
			cmds.push(OverlayCmd::Set("fit", "original".into()));
		}
	});
	ui.add_space(4.0);
	ui.label(
		egui::RichText::new(t("fit.help"))
			.size(10.0)
			.color(egui::Color32::from_gray(110)),
	);
}

/// Audio section: host transmit / host mute / mic to host + the one-switch call.
fn draw_audio(ui: &mut egui::Ui, st: &OverlayState, cmds: &mut Vec<OverlayCmd>) {
	// "Sesli görüşme" = both directions at once (host audio here + our mic there);
	// the host already plays inbound mic audio, so this is just a paired toggle.
	// SEMANTIC (must match the backends' `call` handler): turning the call ON enables
	// BOTH mic and host audio; turning it OFF drops ONLY the mic and leaves host audio
	// as the user had it (host audio has its own row). So the highlight must derive from
	// `mic_on` ALONE — keying it off `mic_on && audio_tx` made "call off" (which clears
	// only mic) still read as ON whenever host audio stayed on, so the highlight and the
	// real state disagreed.
	let call_on = st.mic_on;
	ui.horizontal(|ui| {
		ui.label(format!("📞 {}", t("audio.call")));
		info(ui, t("info.call"));
		if seg(ui, call_on, t("on")) {
			cmds.push(OverlayCmd::Set("call", "on".into()));
		}
		if seg(ui, !call_on, t("off")) {
			cmds.push(OverlayCmd::Set("call", "off".into()));
		}
	});
	ui.add_space(4.0);
	ui.horizontal(|ui| {
		ui.label(format!("🎵 {}", t("audio.host")));
		if seg(ui, st.audio_tx, t("on")) {
			cmds.push(OverlayCmd::Set("atx", "on".into()));
		}
		if seg(ui, !st.audio_tx, t("off")) {
			cmds.push(OverlayCmd::Set("atx", "off".into()));
		}
	});
	ui.horizontal(|ui| {
		ui.label(format!("🔊 {}", t("audio.speakers")));
		if seg(ui, !st.audio_mute, t("on")) {
			cmds.push(OverlayCmd::Set("amute", "off".into()));
		}
		if seg(ui, st.audio_mute, t("muted")) {
			cmds.push(OverlayCmd::Set("amute", "on".into()));
		}
	});
	ui.horizontal(|ui| {
		ui.label(format!("🎤 {}", t("audio.mic")));
		if seg(ui, st.mic_on, t("on")) {
			cmds.push(OverlayCmd::Set("mic", "on".into()));
		}
		if seg(ui, !st.mic_on, t("off")) {
			cmds.push(OverlayCmd::Set("mic", "off".into()));
		}
	});
}

/// Tools section: clipboard push, quick file pick (OS dialog), reverse direction,
/// fullscreen toggle. Chat + the file manager have their own native views now.
fn draw_tools(ui: &mut egui::Ui, view: &mut View, cmds: &mut Vec<OverlayCmd>) {
	let w = ui.available_width();
	if ui
		.add(egui::Button::new(format!("📋  {}", t("tools.clip"))).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		cmds.push(OverlayCmd::Set("sendclip", "1".into()));
	}
	if ui
		.add(egui::Button::new(format!("📁  {}", t("tools.file"))).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		// Handled Rust-side (rfd) in render_stats.rs — no webview activation needed.
		cmds.push(OverlayCmd::Set("pickfile", "1".into()));
	}
	if ui
		.add(egui::Button::new(format!("🔁  {}", t("tools.reverse"))).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		cmds.push(OverlayCmd::Set("reverse", "1".into()));
	}
	if ui
		.add(
			egui::Button::new(format!("🖥  {}", t("tools.fullscreen")))
				.min_size(egui::vec2(w, 30.0)),
		)
		.clicked()
	{
		cmds.push(OverlayCmd::Set("fullscreen", "1".into()));
	}
	let _ = view;
}

/// NATIVE chat: the session conversation + a composer. Typing arrives as relayed
/// key events (the app's webview captures keydowns while the overlay is open and
/// pipes them over stdin — this child window can't take X focus without killing
/// the focus-gated combos).
fn draw_chat(ui: &mut egui::Ui, st: &OverlayState, input: &mut String, cmds: &mut Vec<OverlayCmd>) {
	egui::ScrollArea::vertical()
		.max_height(230.0)
		.auto_shrink([false, false])
		.stick_to_bottom(true)
		.show(ui, |ui| {
			if st.chat.is_empty() {
				ui.add_space(90.0);
				ui.vertical_centered(|ui| {
					ui.label(
						egui::RichText::new(t("chat.empty")).color(egui::Color32::from_gray(120)),
					);
				});
				return;
			}
			for (me, text) in &st.chat {
				let layout = if *me {
					egui::Layout::right_to_left(egui::Align::Min)
				} else {
					egui::Layout::left_to_right(egui::Align::Min)
				};
				ui.with_layout(layout, |ui| {
					let fill = if *me {
						ACCENT
					} else {
						egui::Color32::from_rgb(34, 36, 50)
					};
					egui::Frame::none()
						.fill(fill)
						.rounding(9.0)
						.inner_margin(egui::Margin::symmetric(10.0, 6.0))
						.show(ui, |ui| {
							ui.set_max_width(250.0);
							ui.label(
								egui::RichText::new(text)
									.size(13.0)
									.color(egui::Color32::WHITE),
							);
						});
				});
				ui.add_space(2.0);
			}
		});
	ui.add_space(6.0);
	let send_now = st.chat_enter && !input.trim().is_empty();
	ui.horizontal(|ui| {
		let resp = ui.add_sized(
			egui::vec2(ui.available_width() - 76.0, 28.0),
			egui::TextEdit::singleline(input).hint_text(t("chat.placeholder")),
		);
		// Keep the composer focused while this view is up so relayed Text events land.
		resp.request_focus();
		let clicked = ui
			.add(egui::Button::new(t("chat.send")).min_size(egui::vec2(68.0, 28.0)))
			.clicked();
		if (clicked || send_now) && !input.trim().is_empty() {
			cmds.push(OverlayCmd::Chat(input.trim().to_string()));
			input.clear();
		}
	});
}

/// NATIVE two-pane file manager: LEFT = this machine (renderer-local std::fs,
/// starting at HOME), RIGHT = the host (FsEntries relayed over stdin). Download
/// from the right, upload from the left — both go through the session's existing
/// chunked file channel.
fn draw_files(ui: &mut egui::Ui, st: &OverlayState, us: &mut UiState, cmds: &mut Vec<OverlayCmd>) {
	if !us.local_loaded {
		load_local(us);
	}
	// First visit: ask the host for its HOME listing — exactly once per overlay open
	// (an empty or lost reply must not re-emit `ov fsls` every frame).
	if !us.remote_requested && st.fs_remote_path.is_empty() && st.fs_remote.is_empty() {
		us.remote_requested = true;
		cmds.push(OverlayCmd::FsLs(String::new()));
	}
	let pane_w = (ui.available_width() - 8.0) / 2.0;
	ui.horizontal_top(|ui| {
		// LOCAL pane.
		ui.vertical(|ui| {
			ui.set_width(pane_w);
			ui.label(
				egui::RichText::new(format!("{} · ~/{}", t("files.local"), us.local_path))
					.small()
					.color(egui::Color32::GRAY),
			);
			let mut nav: Option<String> = None;
			let mut send: Option<String> = None;
			egui::ScrollArea::vertical()
				.id_salt("fs_local")
				.max_height(220.0)
				.auto_shrink([false, false])
				.show(ui, |ui| {
					if !us.local_path.is_empty()
						&& fs_row(ui, &format!("⬆ {}", t("files.up")), true, 0, None)
					{
						let mut p = us.local_path.clone();
						if let Some(i) = p.rfind('/') {
							p.truncate(i);
						} else {
							p.clear();
						}
						nav = Some(p);
					}
					for row in &us.local_rows {
						let action = (!row.dir).then_some("→");
						if fs_row(ui, &row.name, row.dir, row.size, action) {
							if row.dir {
								nav = Some(join_rel(&us.local_path, &row.name));
							} else {
								send = Some(join_rel(&us.local_path, &row.name));
							}
						}
					}
				});
			if let Some(p) = nav {
				us.local_path = p;
				load_local(us);
			}
			if let Some(rel) = send {
				if let Some(home) = home() {
					let abs = std::path::Path::new(&home).join(&rel);
					cmds.push(OverlayCmd::FsSend(abs.to_string_lossy().into_owned()));
				}
			}
		});
		ui.add_space(8.0);
		// REMOTE pane.
		ui.vertical(|ui| {
			ui.set_width(pane_w);
			ui.label(
				egui::RichText::new(format!("{} · ~/{}", t("files.remote"), st.fs_remote_path))
				.small()
				.color(egui::Color32::GRAY),
			);
			egui::ScrollArea::vertical()
				.id_salt("fs_remote")
				.max_height(220.0)
				.auto_shrink([false, false])
				.show(ui, |ui| {
					if !st.fs_remote_path.is_empty()
						&& fs_row(ui, &format!("⬆ {}", t("files.up")), true, 0, None)
					{
						let mut p = st.fs_remote_path.clone();
						if let Some(i) = p.rfind('/') {
							p.truncate(i);
						} else {
							p.clear();
						}
						cmds.push(OverlayCmd::FsLs(p));
					}
					for row in &st.fs_remote {
						let action = (!row.dir).then_some("↓");
						if fs_row(ui, &row.name, row.dir, row.size, action) {
							let p = join_rel(&st.fs_remote_path, &row.name);
							if row.dir {
								cmds.push(OverlayCmd::FsLs(p));
							} else {
								cmds.push(OverlayCmd::FsGet(p));
							}
						}
					}
				});
		});
	});
	ui.add_space(4.0);
	ui.label(
		egui::RichText::new(t("files.help"))
		.size(10.0)
		.color(egui::Color32::from_gray(110)),
	);
}

/// One file row; returns true when clicked. `action` renders a trailing glyph
/// (→ upload / ↓ download) for files.
fn fs_row(ui: &mut egui::Ui, name: &str, dir: bool, size: u64, action: Option<&str>) -> bool {
	let label = if dir {
		format!("▸ {name}")
	} else {
		format!("{name}  ·  {}", fmt_size(size))
	};
	let text = egui::RichText::new(if let Some(a) = action {
		format!("{label}   {a}")
	} else {
		label
	})
	.size(12.0);
	ui.add(
		egui::Button::new(text)
			.frame(false)
			.min_size(egui::vec2(ui.available_width(), 20.0)),
	)
	.clicked()
}

fn fmt_size(b: u64) -> String {
	if b >= 1_048_576 {
		format!("{:.1} MB", b as f64 / 1_048_576.0)
	} else if b >= 1024 {
		format!("{:.0} KB", b as f64 / 1024.0)
	} else {
		format!("{b} B")
	}
}

fn join_rel(base: &str, name: &str) -> String {
	if base.is_empty() {
		name.to_string()
	} else {
		format!("{base}/{name}")
	}
}

/// The user's home dir for the LOCAL pane: HOME on unix, USERPROFILE on Windows
/// (where HOME is normally unset — without the fallback the pane stays empty).
fn home() -> Option<std::ffi::OsString> {
	std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
}

/// (Re)load the LOCAL pane from the renderer's own filesystem (HOME-relative).
fn load_local(us: &mut UiState) {
	us.local_loaded = true;
	us.local_rows.clear();
	let Some(home) = home() else { return };
	let dir = std::path::Path::new(&home).join(&us.local_path);
	let Ok(rd) = std::fs::read_dir(dir) else {
		return;
	};
	for e in rd.flatten() {
		let name = e.file_name().to_string_lossy().into_owned();
		if name.starts_with('.') {
			continue; // dotfiles add noise in a 220 pt pane
		}
		let Ok(meta) = e.metadata() else { continue };
		us.local_rows.push(FsRow {
			name,
			dir: meta.is_dir(),
			size: meta.len(),
		});
	}
	us.local_rows.sort_by(|a, b| {
		b.dir
			.cmp(&a.dir)
			.then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
	});
}

/// Gauges section: the always-on HUD + the Parsec-style open button toggles.
fn draw_gauges(ui: &mut egui::Ui, st: &OverlayState, cmds: &mut Vec<OverlayCmd>) {
	ui.horizontal(|ui| {
		ui.label(t("gauges.statshud"));
		info(ui, t("info.statshud"));
		if seg(ui, st.stats_hud, t("on")) {
			cmds.push(OverlayCmd::Set("statshud", "on".into()));
		}
		if seg(ui, !st.stats_hud, t("off")) {
			cmds.push(OverlayCmd::Set("statshud", "off".into()));
		}
	});
	ui.horizontal(|ui| {
		ui.label(t("gauges.ovbtn"));
		info(ui, t("info.ovbtn"));
		if seg(ui, st.overlay_btn, t("on")) {
			cmds.push(OverlayCmd::Set("ovbtn", "on".into()));
		}
		if seg(ui, !st.overlay_btn, t("off")) {
			cmds.push(OverlayCmd::Set("ovbtn", "off".into()));
		}
	});
}

/// Controllers section: ordered player-slot list with painted ▲/▼ swap buttons.
/// Game mode only — the box does not appear in remote_boxes.
/// ▲ click on row i pushes `OverlayCmd::Set("ctrlswap", "{i},{i-1}")`.
/// ▼ click on row i pushes `OverlayCmd::Set("ctrlswap", "{i},{i+1}")`.
/// The bundled egui font lacks ▲/▼ glyphs — both arrows are painted with
/// `line_segment`, matching the back-arrow style used in the header (lines 380-397).
fn draw_controllers(ui: &mut egui::Ui, st: &OverlayState, cmds: &mut Vec<OverlayCmd>) {
	if st.controllers.is_empty() {
		ui.add_space(20.0);
		ui.vertical_centered(|ui| {
			ui.label(
				egui::RichText::new(t("controllers.empty"))
					.color(egui::Color32::from_gray(120)),
			);
		});
		return;
	}
	let n = st.controllers.len();
	for (i, (slot, kind, name)) in st.controllers.iter().enumerate() {
		ui.horizontal(|ui| {
			// Row label: "Oyuncu 1 · Xbox · Controller name"
			let label = format!(
				"{} {} · {} · {}",
				t("controllers.slot"),
				slot + 1,
				kind,
				name,
			);
			ui.label(
				egui::RichText::new(label)
					.size(12.5)
					.color(egui::Color32::from_rgb(228, 230, 240)),
			);
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				// ▼ button (swap with next) — only if not the last row.
				if i + 1 < n {
					let (rect, resp) =
						ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
					let col = if resp.hovered() {
						egui::Color32::WHITE
					} else {
						egui::Color32::from_rgb(150, 155, 170)
					};
					// Paint ▼: a downward-pointing chevron (two line_segments).
					let c = rect.center();
					let s = egui::Stroke::new(2.0, col);
					let p = ui.painter();
					p.line_segment([c + egui::vec2(-5.0, -3.0), c + egui::vec2(0.0, 3.0)], s);
					p.line_segment([c + egui::vec2(5.0, -3.0), c + egui::vec2(0.0, 3.0)], s);
					if resp.clicked() {
						cmds.push(OverlayCmd::Set("ctrlswap", format!("{i},{}", i + 1)));
					}
					resp.on_hover_cursor(egui::CursorIcon::PointingHand);
				}
				// ▲ button (swap with previous) — only if not the first row.
				if i > 0 {
					let (rect, resp) =
						ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
					let col = if resp.hovered() {
						egui::Color32::WHITE
					} else {
						egui::Color32::from_rgb(150, 155, 170)
					};
					// Paint ▲: an upward-pointing chevron (two line_segments).
					let c = rect.center();
					let s = egui::Stroke::new(2.0, col);
					let p = ui.painter();
					p.line_segment([c + egui::vec2(-5.0, 3.0), c + egui::vec2(0.0, -3.0)], s);
					p.line_segment([c + egui::vec2(5.0, 3.0), c + egui::vec2(0.0, -3.0)], s);
					if resp.clicked() {
						cmds.push(OverlayCmd::Set("ctrlswap", format!("{i},{}", i - 1)));
					}
					resp.on_hover_cursor(egui::CursorIcon::PointingHand);
				}
			});
		});
		ui.add_space(2.0);
	}
}

/// Parse the host's Stats label parts we surface as tiles: encode pace (optional
/// 6th part) and the TARGET bitrate ("15 Mbit hedef" → 15.0).
fn host_parts(st: &OverlayState) -> (Option<f32>, Option<f32>) {
	let parts: Vec<&str> = st.host_active.split(" · ").collect();
	let enc_ms = parts
		.get(5)
		.and_then(|p| p.split_whitespace().next())
		.and_then(|v| v.parse().ok());
	let target = parts
		.get(4)
		.and_then(|p| p.split_whitespace().next())
		.and_then(|v| v.parse().ok());
	(enc_ms, target)
}

/// Measured bandwidth, ALWAYS in Mbps with a 0.1 floor (maintainer rule: never
/// kbps, never "0.0" — an active stream reads at least 0.1).
fn fmt_mbps(mbps: f32) -> String {
	format!("{:.1}", mbps.max(0.1))
}

/// A clickable category box (the root hub's grid cells).
fn cat_box(ui: &mut egui::Ui, icon: &str, label: &str) -> bool {
	let size = egui::vec2((ui.available_width() / 2.0 - 6.0).max(140.0), 40.0);
	let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
	let fill = if resp.hovered() {
		egui::Color32::from_rgb(40, 42, 58)
	} else {
		egui::Color32::from_rgb(28, 30, 42)
	};
	ui.painter().rect_filled(rect, 8.0, fill);
	// Icon (cyan, left) + label — left-aligned with a fixed icon column so the
	// labels line up across boxes instead of wandering with the icon width.
	ui.painter().text(
		egui::pos2(rect.left() + 14.0, rect.center().y),
		egui::Align2::LEFT_CENTER,
		icon,
		egui::FontId::proportional(16.0),
		CYAN,
	);
	ui.painter().text(
		egui::pos2(rect.left() + 42.0, rect.center().y),
		egui::Align2::LEFT_CENTER,
		label,
		egui::FontId::proportional(13.0),
		egui::Color32::from_rgb(228, 230, 240),
	);
	resp.clicked()
}

fn stat_tile(ui: &mut egui::Ui, value: &str, icon: &str, label: &str) {
	egui::Frame::none()
		.fill(egui::Color32::from_rgb(22, 24, 34))
		.rounding(8.0)
		.inner_margin(egui::Margin::symmetric(10.0, 8.0))
		.show(ui, |ui| {
			ui.vertical(|ui| {
				ui.label(egui::RichText::new(value).size(18.0).strong().color(CYAN));
				ui.label(
					egui::RichText::new(format!("{icon} {label}"))
						.small()
						.color(egui::Color32::GRAY),
				);
			});
		});
}

/// A labelled ComboBox; returns Some(value) when the user picks a new value.
/// Mini always-on stats HUD (top-right) drawn while the overlay is CLOSED and the
/// user enabled it. Pointer events are NOT selected in that state (the container is
/// pass-through), so this is display-only by construction.
pub fn draw_hud(ctx: &egui::Context, st: &OverlayState) {
	let bw = fmt_mbps(st.mbps);
	egui::Area::new("pulsar_hud".into())
		.order(egui::Order::Foreground)
		.anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
		.show(ctx, |ui| {
			egui::Frame::none()
				.fill(egui::Color32::from_rgba_premultiplied(10, 11, 16, 200))
				.rounding(6.0)
				.inner_margin(egui::Margin::symmetric(10.0, 6.0))
				.show(ui, |ui| {
					ui.add(
						egui::Label::new(
							egui::RichText::new(format!(
								"{:.0} fps · {:.0} ms · {} {:.1} ms · {bw} Mbps",
								st.fps,
								st.latency_ms,
								t("hud.decode"),
								st.decode_ms
							))
							.monospace()
							.size(12.0)
							.color(egui::Color32::from_rgb(180, 220, 240)),
						)
						.wrap_mode(egui::TextWrapMode::Extend),
					);
				});
		});
}

/// Default overlay-open button offset from the top-left, in egui points (the webview
/// hotspot mirrors it as CSS px ×1.25 — keep the two in sync).
pub const BTN_POS_DEFAULT: (f32, f32) = (90.0, 70.0);
/// Overlay-open button size in egui points (the 36 pt disc).
pub const BTN_SIZE: f32 = 36.0;

/// The open button's effective rect: `pos` validated (non-finite → default) and clamped
/// into `screen` so a position persisted from a larger window can't land the button
/// off-window (invisible AND unclickable). Used by `draw_open_button` and by backends
/// that hit-test closed-state clicks — visual and hotspot stay aligned by construction.
pub fn btn_rect(pos: (f32, f32), screen: egui::Rect) -> egui::Rect {
	let (mut x, mut y) = pos;
	if !x.is_finite() || !y.is_finite() {
		(x, y) = BTN_POS_DEFAULT;
	}
	x = x.clamp(0.0, (screen.width() - BTN_SIZE).max(0.0));
	y = y.clamp(0.0, (screen.height() - BTN_SIZE).max(0.0));
	egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(BTN_SIZE, BTN_SIZE))
}

/// Parsec-style overlay-open button: the Pulsar mark (concentric pulse rings) in a
/// translucent dark disc, at `st.btn_pos` (top-LEFT default, user-draggable via the
/// webview hotspot), drawn while the overlay is CLOSED. Returns true when clicked
/// (platforms whose renderer receives pointer events in the closed state — Windows;
/// on Linux the closed container is input pass-through, so the webview provides the
/// matching invisible hotspot instead).
pub fn draw_open_button(ctx: &egui::Context, st: &OverlayState) -> bool {
	let mut clicked = false;
	egui::Area::new("pulsar_ovbtn".into())
		.order(egui::Order::Foreground)
		.fixed_pos(btn_rect(st.btn_pos, ctx.screen_rect()).min)
		.show(ctx, |ui| {
			let size = egui::vec2(BTN_SIZE, BTN_SIZE);
			let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
			let hovered = resp.hovered();
			let p = ui.painter();
			let c = rect.center();
			let bg_a = if hovered { 235 } else { 150 };
			p.circle_filled(
				c,
				18.0,
				egui::Color32::from_rgba_premultiplied(13, 14, 20, bg_a),
			);
			// The brand mark: pure concentric circles (pulse rings) + core dot.
			let ring = if hovered {
				ACCENT
			} else {
				egui::Color32::from_rgb(150, 140, 245)
			};
			p.circle_stroke(c, 12.0, egui::Stroke::new(1.6, ring.gamma_multiply(0.55)));
			p.circle_stroke(c, 7.5, egui::Stroke::new(1.8, ring.gamma_multiply(0.8)));
			p.circle_filled(c, 3.2, ring);
			// Unread host-chat badge (top-right): a red disc with the count, like an
			// app-icon notification. Cleared by the backend when the Chat view opens.
			if st.chat_unread > 0 {
				let n = if st.chat_unread > 9 {
					"9+".to_string()
				} else {
					st.chat_unread.to_string()
				};
				let bc = egui::pos2(rect.right() - 5.0, rect.top() + 5.0);
				p.circle_filled(bc, 7.5, egui::Color32::from_rgb(225, 60, 70));
				p.text(
					bc,
					egui::Align2::CENTER_CENTER,
					n,
					egui::FontId::proportional(9.5),
					egui::Color32::WHITE,
				);
			}
			clicked = resp.clicked();
		});
	clicked
}

/// Transient helper tooltip, bottom-center over the video (drawn while the overlay is
/// CLOSED): "click to take control" after the overlay closes, "how to release" right
/// after control is engaged. The caller owns the ~3 s lifetime and passes `alpha`
/// (1→0 over the final stretch) for a smooth fade-out. One wide line — the label is
/// explicitly non-wrapping so the text never folds into a narrow column.
pub fn draw_hint(ctx: &egui::Context, text: &str, alpha: f32) {
	egui::Area::new("pulsar_hint".into())
		.order(egui::Order::Foreground)
		.anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
		.show(ctx, |ui| {
			ui.set_opacity(alpha.clamp(0.0, 1.0));
			egui::Frame::none()
				.fill(egui::Color32::from_rgba_premultiplied(10, 11, 16, 215))
				.rounding(8.0)
				.inner_margin(egui::Margin::symmetric(14.0, 8.0))
				.show(ui, |ui| {
					ui.add(
						egui::Label::new(
							egui::RichText::new(text)
								.size(13.0)
								.color(egui::Color32::from_rgb(210, 215, 230)),
						)
						.wrap_mode(egui::TextWrapMode::Extend),
					);
				});
		});
}

fn combo(
	ui: &mut egui::Ui,
	label: &str,
	id: &str,
	cur: &str,
	opts: &[(&str, &str)],
	active: &str,
) -> Option<String> {
	let mut picked = None;
	ui.vertical(|ui| {
		ui.label(
			egui::RichText::new(label)
				.small()
				.color(egui::Color32::GRAY),
		);
		let cur_label = opts
			.iter()
			.find(|(v, _)| *v == cur)
			.map(|(_, l)| *l)
			.unwrap_or(cur);
		egui::ComboBox::from_id_salt(id)
			.selected_text(cur_label)
			.width(180.0)
			.show_ui(ui, |ui| {
				for (v, l) in opts {
					if ui.selectable_label(*v == cur, *l).clicked() && *v != cur {
						picked = Some((*v).to_string());
					}
				}
			});
		// What's REALLY in use right now (faint truth line; the combo is the request).
		if !active.is_empty() {
			ui.label(
				egui::RichText::new(format!("{} {active}", t("active")))
					.size(10.0)
					.color(egui::Color32::from_gray(110)),
			);
		}
	});
	picked
}

fn seg(ui: &mut egui::Ui, on: bool, label: &str) -> bool {
	let fill = if on {
		ACCENT
	} else {
		egui::Color32::from_rgb(28, 30, 42)
	};
	ui.add(egui::Button::new(label).fill(fill)).clicked() && !on
}

/// Small painted ⓘ marker with a hover tooltip — for options whose name alone
/// doesn't explain them (e.g. frame pacing). Painted (circle + dot + stem)
/// because egui's bundled fonts lack a clean info glyph; the tooltip itself
/// fades in via egui's built-in popup animation.
fn info(ui: &mut egui::Ui, text: &str) {
	let (rect, resp) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::hover());
	let col = if resp.hovered() {
		CYAN
	} else {
		egui::Color32::from_rgb(110, 115, 130)
	};
	let c = rect.center();
	let p = ui.painter();
	p.circle_stroke(c, 6.0, egui::Stroke::new(1.3, col));
	p.circle_filled(c + egui::vec2(0.0, -2.8), 0.9, col);
	p.line_segment(
		[c + egui::vec2(0.0, -0.4), c + egui::vec2(0.0, 3.0)],
		egui::Stroke::new(1.5, col),
	);
	resp.on_hover_text(egui::RichText::new(text).size(12.0));
}
