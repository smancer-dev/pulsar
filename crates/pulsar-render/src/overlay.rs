//! The in-session overlay — pure egui, backend-agnostic, so every platform paints
//! the IDENTICAL UI. Mirrors the Svelte game overlay (`Session.svelte`): header +
//! stat tiles + selector rows + quality segment + End + footer. Game mode shows the
//! slim set; Remote mode adds the full remote controls (file/clipboard/monitor) later.

/// Phosphor icon glyphs (the `regular` variant — installed into egui's atlas in `apply_theme`).
/// Used for the category boxes and stat tiles so each shows a DISTINCT icon (the old emoji
/// strings rendered as tofu in the bundled egui font).
use egui_phosphor::regular as icon;

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
	/// Connected controllers (slot, kind_label, device_name, uuid, target, rumble, disabled) —
	/// synced from the app over stdin (`ctrls slot:kind:name:uuid:target:rumble:disabled,...`).
	/// Game mode only. `target` "auto"/"xbox360"/"ds4"; `rumble` the per-pad level
	/// "off"/"weak"/"medium"/"strong"; `disabled` = the pad is toggled off. Legacy short lines
	/// default the missing tail.
	pub controllers: Vec<(u8, String, String, String, String, String, bool)>,
	/// SPLIT MODE: the set of pad uuids LOCKED to THIS session (this renderer process == one
	/// pane/session), parsed from the `ctrls` line's NEW 8th per-pad field (`…:disabled:locked`,
	/// `locked` = 1 when locked to this session). Drives the per-pad "Bu oturuma kilitle" toggle's
	/// checked state in `draw_controllers`. Kept SEPARATE from the `controllers` tuple so the
	/// platform `ctrls` parsers (linux.rs / win/mod.rs) can fill it without changing the long-lived
	/// 7-tuple's arity. Empty when split mode is off / before any lock.
	pub controllers_locked: std::collections::HashSet<String>,
	/// Available display MODES per host monitor (`idx → Vec<(w,h)>`, largest-first), parsed from
	/// the caps line's `displays=` field (Windows host only — empty elsewhere). Feeds the Stream
	/// view's resolution list (the captured monitor's REAL resolutions, not just fixed presets)
	/// and pairs with the Display view's "screen adaptation" toggle.
	pub display_modes: Vec<(u32, Vec<(u32, u32)>)>,
	/// SCREEN ADAPTATION (Parsec-style) toggle for THIS pane: when on, the host switches the
	/// captured monitor to the display mode that best fits the pane and reverts it on exit. Owned
	/// by the overlay once open; a change flows out as `OverlayCmd::Set("adapt", "<w>x<h>"|"off")`.
	pub adapt: bool,
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
	/// Root-menu selection index for controller/keyboard nav. egui's own Tab-focus would
	/// not stick in this override-redirect overlay (focused() stayed None every frame), so
	/// the menu is driven by an explicit index instead: the pad's up/down move it, the
	/// highlighted box is `sel`, and A/Enter activates it.
	pub sel: usize,
	/// Selection index WITHIN a sub-view (Stream/Display/Controllers rows), driven by the pad
	/// up/down; left/right change the selected row's value. Reset to 0 whenever back on Root.
	pub sub_sel: usize,
	/// STAGED (un-applied) value for the selected value-row: `(sub_sel, value)`. A controller
	/// left/right stages it (visual only); A/activate confirms → it's applied
	/// (`OverlayCmd::Set`) and cleared; moving rows or changing view discards it. So a pad
	/// cycle never disrupts the live stream until the user confirms (mouse clicks on a value's
	/// seg button still apply immediately, per the "instant for mouse" rule).
	pub pending: Option<(usize, String)>,
	/// Whether the user has STARTED navigating with the keyboard/controller this overlay open.
	/// The Root-menu selection highlight stays HIDDEN until the FIRST directional/activate input,
	/// so a freshly-opened overlay shows nothing pre-selected (mouse users never see a stray
	/// cursor). Reset on every open edge; the first input reveals the cursor at box 0.
	pub nav_active: bool,
}

/// One frame's controller/keyboard nav input, read once in `draw` and handed to the active
/// view. Directions are plain egui arrow keys (the renderer relays the pad d-pad/stick as
/// arrows — Tab is avoided because egui's focus pass swallows it before a view can read it).
#[derive(Clone, Copy, Default)]
struct Nav {
	up: bool,
	down: bool,
	left: bool,
	right: bool,
	activate: bool,
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
			controllers: Vec::new(), // Vec<(slot, kind_label, name, uuid, target, rumble)>
			controllers_locked: std::collections::HashSet::new(),
			display_modes: Vec::new(),
			adapt: false,
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
						// Warning glyph as a PAINTED triangle + "!" (the ⚠ emoji is tofu in the
						// bundled font).
						let (wr, _) = ui.allocate_exact_size(egui::vec2(34.0, 30.0), egui::Sense::hover());
						let amber = egui::Color32::from_rgb(255, 160, 100);
						let c = wr.center();
						let h = 13.0;
						ui.painter().add(egui::Shape::convex_polygon(
							vec![
								egui::pos2(c.x, c.y - h),
								egui::pos2(c.x - h, c.y + h * 0.85),
								egui::pos2(c.x + h, c.y + h * 0.85),
							],
							egui::Color32::TRANSPARENT,
							egui::Stroke::new(2.2, amber),
						));
						ui.painter().text(
							egui::pos2(c.x, c.y + 2.0),
							egui::Align2::CENTER_CENTER,
							"!",
							egui::FontId::proportional(15.0),
							amber,
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
	// Install the Phosphor icon font into egui's fonts BEFORE any frame tessellates, so its
	// glyphs land in the font atlas. This is the single shared font-setup point — every backend
	// (Linux egui_glow, the Windows custom D3D11 painter `win/egui_paint.rs`, macOS eframe) calls
	// `apply_theme` on its fresh `egui::Context`, so all three get the icons. `add_to_fonts`
	// inserts "phosphor" at index 1 of the Proportional family, i.e. as a fallback for our
	// proportional text — so `PHOSPHOR_*` glyph constants render wherever we draw normal text.
	let mut fonts = egui::FontDefinitions::default();
	egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
	ctx.set_fonts(fonts);

	let mut v = egui::Visuals::dark();
	v.panel_fill = egui::Color32::from_rgba_premultiplied(13, 14, 20, 235);
	v.window_fill = egui::Color32::from_rgb(18, 20, 28);
	v.widgets.inactive.bg_fill = egui::Color32::from_rgb(28, 30, 42);
	v.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 42, 58);
	// Controller/keyboard focus indicator: egui paints a focused widget with the `active`
	// visuals AND a focus ring keyed off `selection.stroke`. The defaults were nearly
	// invisible on the dark panel, so the Tab-focused control inside sub-views looked dead.
	// A bold cyan stroke (matching the Root menu's selection border) makes it unmistakable.
	let focus_stroke = egui::Stroke::new(2.0, CYAN);
	v.selection.stroke = focus_stroke;
	v.widgets.active.bg_stroke = focus_stroke;
	v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(34, 64, 82);
	v.widgets.active.bg_fill = egui::Color32::from_rgb(34, 64, 82);
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
	// Fresh open starts the menu cursor on the first box — but HIDDEN until the user navigates
	// (nav_active gates the highlight, reset here so each open is ring-less until the first input).
	if open_edge {
		ui_state.sel = 0;
		ui_state.nav_active = false;
	}
	// B / Escape steps back one level: a sub-view returns to the Root menu; from Root it
	// closes the overlay. (The pad's B button is relayed as Escape — see play.rs.)
	if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
		if ui_state.view != View::Root {
			ui_state.view = View::Root;
		} else {
			cmds.push(OverlayCmd::Close);
		}
	}
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
				let on_root = ui_state.view == View::Root;
				// "Pulsar" brand only on the Root hub — inside a sub-view the back arrow + the
				// section title are enough (the extra brand line was redundant clutter).
				if on_root {
					ui.heading(egui::RichText::new("Pulsar").color(CYAN).strong());
				}
				let key = match (ui_state.view, st.mode) {
					(View::Stream, _) => "view.stream",
					(View::Display, _) => "view.display",
					(View::Audio, _) => "view.audio",
					(View::Tools, _) => "view.tools",
					(View::Chat, _) => "view.chat",
					(View::Files, _) => "view.files",
					(View::Gauges, _) => "view.gauges",
					(View::Controllers, _) => "view.controllers",
					(View::Root, Mode::Game) => "mode.game",
					(View::Root, Mode::Remote) => "mode.remote",
				};
				ui.label(egui::RichText::new(t(key)).color(egui::Color32::GRAY));
				// Connection label (Relay/Direct) only on Root — irrelevant clutter in sub-views.
				if on_root {
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
						ui.label(
							egui::RichText::new(st.conn_label.clone())
								.monospace()
								.color(egui::Color32::from_rgb(150, 155, 170)),
						);
					});
				}
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
			// One read of the controller/keyboard nav for this frame, handed to the active view.
			let mut nav = ui.input(|i| Nav {
				up: i.key_pressed(egui::Key::ArrowUp),
				down: i.key_pressed(egui::Key::ArrowDown),
				left: i.key_pressed(egui::Key::ArrowLeft),
				right: i.key_pressed(egui::Key::ArrowRight),
				activate: i.key_pressed(egui::Key::Enter),
			});
			// #5: the FIRST nav input only WAKES the cursor (reveals it at box 0) without also
			// moving or activating — so the overlay opens with nothing selected and a single
			// d-pad tap just shows the cursor. Consume that input so it doesn't also step a row.
			let any_nav = nav.up || nav.down || nav.left || nav.right || nav.activate;
			if !ui_state.nav_active && any_nav {
				ui_state.nav_active = true;
				ui_state.sel = 0;
				ui_state.sub_sel = 0;
				nav = Nav::default();
			}
			// Entering any sub-view always starts its row cursor at the top.
			if ui_state.view == View::Root {
				ui_state.sub_sel = 0;
				ui_state.pending = None;
			}
			// Moving the row cursor DISCARDS any staged-but-unconfirmed value (revert) —
			// "başka yere geçerse onaylamadan geri alınmalı". Confirm is A/activate (handled
			// inside choice_row), which is not up/down so it survives.
			if nav.up || nav.down {
				ui_state.pending = None;
			}
			ui.scope(|ui| {
				ui.set_opacity(fade);
				ui.add_space((1.0 - fade) * 8.0);
				match ui_state.view {
					View::Root => {
						draw_root(ui, st, &mut ui_state.view, &mut ui_state.sel, nav, ui_state.nav_active, &mut cmds)
					}
					View::Stream => {
						draw_stream(ui, st, &mut ui_state.sub_sel, nav, &mut ui_state.pending, &mut cmds)
					}
					View::Display => {
						draw_display(ui, st, st.mode, &mut ui_state.sub_sel, nav, &mut ui_state.pending, &mut cmds)
					}
					View::Audio => {
						draw_audio(ui, st, &mut ui_state.sub_sel, nav, &mut ui_state.pending, &mut cmds)
					}
					View::Tools => {
						draw_tools(ui, &mut ui_state.view, st.mode, &mut ui_state.sub_sel, nav, &mut cmds)
					}
					View::Chat => draw_chat(ui, st, &mut ui_state.chat_input, &mut cmds),
					View::Files => draw_files(ui, st, ui_state, &mut cmds),
					View::Gauges => {
						draw_gauges(ui, st, &mut ui_state.sub_sel, nav, &mut ui_state.pending, &mut cmds)
					}
					View::Controllers => {
						draw_controllers(ui, st, &mut ui_state.sub_sel, nav, &mut ui_state.pending, &mut cmds)
					}
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

/// Move the Root-menu selection over a 2-column grid of `n` boxes plus an End-session button
/// at index `n` (its own full-width row below the grid). left/right step within a grid row;
/// up/down step between rows, with the bottom grid row going to End and End going back up.
fn move_grid_sel(sel: usize, n: usize, nav: Nav) -> usize {
	const COLS: usize = 2;
	let end = n;
	let mut s = sel.min(end);
	if nav.right && s < n && s % COLS < COLS - 1 && s + 1 < n {
		s += 1;
	}
	if nav.left && s < n && s % COLS > 0 {
		s -= 1;
	}
	if nav.down {
		if s < n {
			let nx = s + COLS;
			s = if nx < n { nx } else { end };
		}
		// From End there is nowhere lower — stay.
	}
	if nav.up {
		if s == end {
			// Jump back to the first cell of the last grid row.
			s = if n == 0 { 0 } else { ((n - 1) / COLS) * COLS };
		} else if s >= COLS {
			s -= COLS;
		}
	}
	s
}

/// Root hub: compact stat tiles + the category boxes + end-session.
fn draw_root(
	ui: &mut egui::Ui,
	st: &OverlayState,
	view: &mut View,
	sel: &mut usize,
	nav: Nav,
	nav_active: bool,
	cmds: &mut Vec<OverlayCmd>,
) {
	let (enc_ms, target_mbit) = host_parts(st);
	let bw = fmt_mbps(st.mbps);
	// Two compact tile rows: RTT · fps · encode / decode · BITRATE target · bandwidth.
	ui.horizontal(|ui| {
		stat_tile(ui, &format!("{:.0}", st.latency_ms), icon::PULSE, t("stat.latency"));
		stat_tile(ui, &format!("{:.0}", st.fps), icon::FILM_STRIP, "FPS");
		stat_tile(
			ui,
			&enc_ms.map_or("—".into(), |v: f32| format!("{v:.1}")),
			icon::CPU,
			t("stat.encode"),
		);
	});
	ui.horizontal(|ui| {
		stat_tile(ui, &format!("{:.1}", st.decode_ms), icon::LIGHTNING, t("stat.decode"));
		stat_tile(
			ui,
			&target_mbit.map_or("—".into(), |v: f32| format!("{v:.0}")),
			icon::TARGET,
			"Bitrate Mbps",
		);
		stat_tile(ui, &bw, icon::CELL_SIGNAL_HIGH, t("stat.band"));
	});
	ui.add_space(8.0);

	// Category boxes, 2 per row (AnyDesk-style hub). Chat + Files are NATIVE views
	// now — the webview session menu is gone on Linux (it lived under the video).
	// Game mode shows the slim set; Audio + Tools were added on the maintainer's request —
	// in-game audio control (host audio / speakers / mic) is essential, and Tools carries the
	// fullscreen toggle (game mode trims Tools to JUST fullscreen, see draw_tools). Remote mode
	// gets the full eight boxes incl. file transfer, chat, mic, monitor.
	let game_boxes: &[(&str, &str, View)] = &[
		(icon::BROADCAST, t("view.stream"), View::Stream),
		(icon::MONITOR, t("view.display"), View::Display),
		(icon::SPEAKER_HIGH, t("view.audio"), View::Audio),
		(icon::CHART_BAR, t("view.gauges"), View::Gauges),
		(icon::GAME_CONTROLLER, t("view.controllers"), View::Controllers),
		(icon::WRENCH, t("view.tools"), View::Tools),
	];
	let remote_boxes: &[(&str, &str, View)] = &[
		(icon::BROADCAST, t("view.stream"), View::Stream),
		(icon::MONITOR, t("view.display"), View::Display),
		(icon::SPEAKER_HIGH, t("view.audio"), View::Audio),
		(icon::CHAT_CIRCLE, t("view.chat"), View::Chat),
		(icon::FOLDER, t("view.files"), View::Files),
		(icon::WRENCH, t("view.tools"), View::Tools),
		(icon::GAME_CONTROLLER, t("view.controllers"), View::Controllers),
		(icon::CHART_BAR, t("view.gauges"), View::Gauges),
	];
	let boxes: &[(&str, &str, View)] = match st.mode {
		Mode::Game => game_boxes,
		Mode::Remote => remote_boxes,
	};
	let n = boxes.len();
	// Selection cycles over the n boxes PLUS the End-session button at index n, so the pad
	// reaches End too.
	let total = n + 1;
	// 2D grid nav (matches the visible 2-column layout): left/right move within a row, up/down
	// move between rows; the End-session button (index n) sits in its own row below the grid.
	let activate = nav.activate;
	if *sel >= total {
		*sel = 0;
	}
	*sel = move_grid_sel(*sel, n, nav);
	egui::Grid::new("ov_boxes")
		.num_columns(2)
		.spacing(egui::vec2(8.0, 8.0))
		.show(ui, |ui| {
			for (i, (icon, label, v)) in boxes.iter().enumerate() {
				let resp = cat_box(ui, icon, label, nav_active && i == *sel);
				if resp.clicked() || (activate && i == *sel) {
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
	// End-session button = the last nav item (index n). Cyan stroke when the pad cursor is on
	// it; A/Enter (activate) ends the session, same as a click. Highlight only once nav woke.
	let end_selected = nav_active && *sel == n;
	let mut end_btn = egui::Button::new(
		egui::RichText::new(t("end")).color(egui::Color32::WHITE),
	)
	.fill(egui::Color32::from_rgb(200, 60, 70))
	.min_size(egui::vec2(ui.available_width(), 32.0));
	if end_selected {
		end_btn = end_btn.stroke(egui::Stroke::new(2.5, CYAN));
	}
	if ui.add(end_btn).clicked() || (activate && end_selected) {
		cmds.push(OverlayCmd::End);
	}
}

/// Stream section: codec/encoder/res/fps/bandwidth-limit + quality/pacing, as pad-navigable
/// rows. up/down move the row cursor; left/right cycle the selected row's value.
fn draw_stream(
	ui: &mut egui::Ui,
	st: &OverlayState,
	sub_sel: &mut usize,
	nav: Nav,
	pending: &mut Option<(usize, String)>,
	cmds: &mut Vec<OverlayCmd>,
) {
	// Capability gating: only offer what the HOST really has (empty = unknown host → show
	// everything; it degrades gracefully server-side anyway).
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
	// Resolution options: when the host advertised the CAPTURED monitor's real display modes
	// (Windows), list THOSE — so the picker reflects the actual screen and pairs with the Display
	// view's "screen adaptation" feature — otherwise the fixed presets. Real modes use a "WxH"
	// value (`controls.setRes` parses it into explicit width/height); presets keep their tokens.
	let mode_strs: Vec<(String, String)> = st
		.display_modes
		.iter()
		.find(|(idx, _)| *idx == st.display_idx)
		.map(|(_, modes)| {
			modes
				.iter()
				.map(|(w, h)| (format!("{w}x{h}"), format!("{w}×{h}")))
				.collect()
		})
		.unwrap_or_default();
	let res_owned: Vec<(&str, &str)> = if mode_strs.is_empty() {
		res_opts().to_vec()
	} else {
		let mut v = vec![("auto", t("auto"))];
		v.extend(mode_strs.iter().map(|(val, disp)| (val.as_str(), disp.as_str())));
		v
	};
	let res: &[(&str, &str)] = &res_owned;
	let fps = fps_opts();
	let br = bitrate_opts();
	let quality = [("latency", t("lowlat")), ("quality", t("quality"))];
	let pace = [("on", t("on")), ("off", t("off"))];
	let pace_cur = if st.pace { "on" } else { "off" };

	const ROWS: usize = 7;
	if nav.down {
		*sub_sel = (*sub_sel + 1).min(ROWS - 1);
	}
	if nav.up {
		*sub_sel = sub_sel.saturating_sub(1);
	}
	let s = (*sub_sel).min(ROWS - 1);
	ui.spacing_mut().item_spacing.y = 6.0;
	if let Some(v) = choice_row(ui, s == 0, nav, "Codec", &st.codec, &codecs, 0, pending) {
		cmds.push(OverlayCmd::Set("codec", v));
	}
	if let Some(v) = choice_row(ui, s == 1, nav, "Encoder", &st.encoder, &encoders, 1, pending) {
		cmds.push(OverlayCmd::Set("encoder", v));
	}
	if let Some(v) = choice_row(ui, s == 2, nav, t("res"), &st.res, res, 2, pending) {
		cmds.push(OverlayCmd::Set("res", v));
	}
	if let Some(v) = choice_row(ui, s == 3, nav, "FPS", &st.fps_sel, &fps, 3, pending) {
		cmds.push(OverlayCmd::Set("fps", v));
	}
	if let Some(v) = choice_row(ui, s == 4, nav, t("bwlimit"), &st.bitrate, &br, 4, pending) {
		cmds.push(OverlayCmd::Set("bitrate", v));
	}
	if let Some(v) = choice_row(ui, s == 5, nav, t("quality"), &st.quality, &quality, 5, pending) {
		cmds.push(OverlayCmd::Set("quality", v));
	}
	if let Some(v) =
		choice_row_i(ui, s == 6, nav, t("pacing"), pace_cur, &pace, 6, pending, Some(t("info.pacing")))
	{
		cmds.push(OverlayCmd::Set("pace", v));
	}
	// Decoder is auto-selected by this renderer — display only.
	ui.add_space(4.0);
	ui.label(
		egui::RichText::new(format!(
			"Decoder · {}",
			if st.decoder.is_empty() {
				t("auto")
			} else {
				st.decoder.as_str()
			}
		))
		.size(11.0)
		.color(egui::Color32::from_gray(150)),
	);
}

/// Display section: host monitor picker (when the host has >1 display, BOTH modes) +
/// AnyDesk-style view-fit modes (renderer-local, instant).
fn draw_display(
	ui: &mut egui::Ui,
	st: &OverlayState,
	_mode: Mode,
	sub_sel: &mut usize,
	nav: Nav,
	pending: &mut Option<(usize, String)>,
	cmds: &mut Vec<OverlayCmd>,
) {
	// Rows: the host monitor picker (only when the host exposes >1 display), the view-fit mode,
	// then the "screen adaptation" toggle. up/down move the cursor; left/right change the row.
	let multi = st.displays.len() > 1;
	let rows = if multi { 3 } else { 2 };
	if nav.down {
		*sub_sel = (*sub_sel + 1).min(rows - 1);
	}
	if nav.up {
		*sub_sel = sub_sel.saturating_sub(1);
	}
	let s = (*sub_sel).min(rows - 1);
	ui.spacing_mut().item_spacing.y = 6.0;
	let mut row = 0;
	if multi {
		let idx_strs: Vec<String> = st.displays.iter().map(|(i, _)| i.to_string()).collect();
		let opts: Vec<(&str, &str)> = st
			.displays
			.iter()
			.enumerate()
			.map(|(k, (_, label))| (idx_strs[k].as_str(), label.as_str()))
			.collect();
		let cur = st.display_idx.to_string();
		if let Some(v) = choice_row(ui, s == row, nav, t("monitor.title"), &cur, &opts, row, pending) {
			cmds.push(OverlayCmd::Set("display", v));
		}
		row += 1;
	}
	let fit = [
		("fit", t("fit.fit")),
		("stretch", t("fit.stretch")),
		("original", t("fit.original")),
	];
	if let Some(v) = choice_row(ui, s == row, nav, t("fit.title"), &st.fit, &fit, row, pending) {
		cmds.push(OverlayCmd::Set("fit", v));
	}
	row += 1;
	// SCREEN ADAPTATION (Parsec-style): when ON, the host switches the captured monitor to the
	// display mode that best fits THIS pane and reverts on exit — unlike "stretch" (which only
	// scales the frame), this changes the host's real resolution so the geometry is native to the
	// pane. The pane size = this renderer's own window (egui screen rect × pixels-per-point), sent
	// so the host can pick the closest mode.
	let onoff = [("on", t("on")), ("off", t("off"))];
	let adapt_cur = if st.adapt { "on" } else { "off" };
	if let Some(v) =
		choice_row_i(ui, s == row, nav, t("display.adapt"), adapt_cur, &onoff, row, pending, Some(t("info.adapt")))
	{
		if v == "on" {
			let sr = ui.ctx().screen_rect();
			let ppp = ui.ctx().pixels_per_point();
			let w = (sr.width() * ppp).round() as u32;
			let h = (sr.height() * ppp).round() as u32;
			cmds.push(OverlayCmd::Set("adapt", format!("{w}x{h}")));
		} else {
			cmds.push(OverlayCmd::Set("adapt", "off".into()));
		}
	}
	ui.add_space(6.0);
	ui.label(
		egui::RichText::new(t("fit.help"))
			.size(10.0)
			.color(egui::Color32::from_gray(110)),
	);
}

/// Audio section: voice call / host transmit / host speakers / mic to host — as pad-navigable
/// rows (up/down move the cursor, ◂/▸ stage + A confirms), so the controller can drive audio in
/// game mode (the old seg-button layout was mouse-only). Mirrors the other sub-views.
///
/// "Sesli görüşme" = both directions at once (host audio here + our mic there); the host already
/// plays inbound mic audio, so it's a paired toggle. SEMANTIC (must match the backends' `call`
/// handler): turning the call ON enables BOTH mic and host audio; OFF drops ONLY the mic and
/// leaves host audio as the user had it — so its on/off derives from `mic_on` ALONE.
fn draw_audio(
	ui: &mut egui::Ui,
	st: &OverlayState,
	sub_sel: &mut usize,
	nav: Nav,
	pending: &mut Option<(usize, String)>,
	cmds: &mut Vec<OverlayCmd>,
) {
	let onoff = [("on", t("on")), ("off", t("off"))];
	const ROWS: usize = 4;
	if nav.down {
		*sub_sel = (*sub_sel + 1).min(ROWS - 1);
	}
	if nav.up {
		*sub_sel = sub_sel.saturating_sub(1);
	}
	let s = (*sub_sel).min(ROWS - 1);
	ui.spacing_mut().item_spacing.y = 6.0;
	let call = if st.mic_on { "on" } else { "off" };
	if let Some(v) =
		choice_row_i(ui, s == 0, nav, t("audio.call"), call, &onoff, 0, pending, Some(t("info.call")))
	{
		cmds.push(OverlayCmd::Set("call", v));
	}
	let host = if st.audio_tx { "on" } else { "off" };
	if let Some(v) = choice_row(ui, s == 1, nav, t("audio.host"), host, &onoff, 1, pending) {
		cmds.push(OverlayCmd::Set("atx", v));
	}
	// Speakers ON = not muted; the emitted `amute` is the inverse.
	let spk = if st.audio_mute { "off" } else { "on" };
	if let Some(v) = choice_row(ui, s == 2, nav, t("audio.speakers"), spk, &onoff, 2, pending) {
		cmds.push(OverlayCmd::Set("amute", if v == "on" { "off".into() } else { "on".into() }));
	}
	let mic = if st.mic_on { "on" } else { "off" };
	if let Some(v) = choice_row(ui, s == 3, nav, t("audio.mic"), mic, &onoff, 3, pending) {
		cmds.push(OverlayCmd::Set("mic", v));
	}
}

/// Tools section. GAME mode trims this to JUST the fullscreen toggle (clipboard / file pick /
/// reverse are remote-desktop concerns — irrelevant in-game; maintainer's request) and renders
/// it as a pad-navigable action row. REMOTE mode keeps the full set of mouse buttons (clipboard
/// push, quick file pick, reverse direction, fullscreen). Chat + the file manager have their own
/// native views now.
fn draw_tools(
	ui: &mut egui::Ui,
	view: &mut View,
	mode: Mode,
	sub_sel: &mut usize,
	nav: Nav,
	cmds: &mut Vec<OverlayCmd>,
) {
	if mode == Mode::Game {
		// One row → always the selection target; A/Enter or a click toggles fullscreen.
		*sub_sel = 0;
		if action_row(ui, true, nav, t("tools.fullscreen"), CYAN) {
			cmds.push(OverlayCmd::Set("fullscreen", "1".into()));
		}
		let _ = view;
		return;
	}
	let w = ui.available_width();
	// Label-only buttons — the leading emoji icons render as tofu in the bundled egui font.
	if ui
		.add(egui::Button::new(t("tools.clip")).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		cmds.push(OverlayCmd::Set("sendclip", "1".into()));
	}
	if ui
		.add(egui::Button::new(t("tools.file")).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		// Handled Rust-side (rfd) in render_stats.rs — no webview activation needed.
		cmds.push(OverlayCmd::Set("pickfile", "1".into()));
	}
	if ui
		.add(egui::Button::new(t("tools.reverse")).min_size(egui::vec2(w, 30.0)))
		.clicked()
	{
		cmds.push(OverlayCmd::Set("reverse", "1".into()));
	}
	if ui
		.add(egui::Button::new(t("tools.fullscreen")).min_size(egui::vec2(w, 30.0)))
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
						&& fs_row(ui, &format!(".. {}", t("files.up")), true, 0, None)
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
						let action = (!row.dir).then_some(">");
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
						&& fs_row(ui, &format!(".. {}", t("files.up")), true, 0, None)
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
						let action = (!row.dir).then_some("<");
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

/// One file row; returns true when clicked. `action` renders a trailing marker
/// (">" upload / "<" download) for files. Dirs get a trailing "/" — all ASCII so
/// nothing tofus in the bundled egui font.
fn fs_row(ui: &mut egui::Ui, name: &str, dir: bool, size: u64, action: Option<&str>) -> bool {
	let label = if dir {
		format!("{name}/")
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
fn draw_gauges(
	ui: &mut egui::Ui,
	st: &OverlayState,
	sub_sel: &mut usize,
	nav: Nav,
	pending: &mut Option<(usize, String)>,
	cmds: &mut Vec<OverlayCmd>,
) {
	let onoff = [("on", t("on")), ("off", t("off"))];
	const ROWS: usize = 2;
	if nav.down {
		*sub_sel = (*sub_sel + 1).min(ROWS - 1);
	}
	if nav.up {
		*sub_sel = sub_sel.saturating_sub(1);
	}
	let s = (*sub_sel).min(ROWS - 1);
	ui.spacing_mut().item_spacing.y = 6.0;
	let hud = if st.stats_hud { "on" } else { "off" };
	if let Some(v) = choice_row(ui, s == 0, nav, t("gauges.statshud"), hud, &onoff, 0, pending) {
		cmds.push(OverlayCmd::Set("statshud", v));
	}
	let ovb = if st.overlay_btn { "on" } else { "off" };
	if let Some(v) = choice_row(ui, s == 1, nav, t("gauges.ovbtn"), ovb, &onoff, 1, pending) {
		cmds.push(OverlayCmd::Set("ovbtn", v));
	}
}

/// Controllers section: ordered player-slot list with ▲/▼ swap buttons and a
/// per-row emulation-target picker (Otomatik / Xbox 360 / DualShock 4).
/// Game mode only — the box does not appear in remote_boxes.
/// ▲ click on row i pushes `OverlayCmd::Set("ctrlswap", "{i},{i-1}")`.
/// ▼ click on row i pushes `OverlayCmd::Set("ctrlswap", "{i},{i+1}")`.
/// Emulation picker pushes `OverlayCmd::Set("ctrlemu", "{uuid},{target}")`.
/// The bundled egui font lacks ▲/▼ glyphs — both arrows are painted with
/// `line_segment`, matching the back-arrow style used in the header.
fn draw_controllers(
	ui: &mut egui::Ui,
	st: &OverlayState,
	sub_sel: &mut usize,
	nav: Nav,
	pending: &mut Option<(usize, String)>,
	cmds: &mut Vec<OverlayCmd>,
) {
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
	// Each pad = 5 controller-navigable rows: [header/reorder, emulation, vibration, test, status].
	// up/down move the cursor across ALL pads' rows; ◂/▸ change the focused row's value (or
	// swap, on the header row); A confirms a staged value (or fires the test pulse). Every row is
	// mouse-clickable too — one setting per row, so nothing overflows the narrow panel.
	const PER: usize = 6;
	let rows = n * PER;
	if nav.down {
		*sub_sel = (*sub_sel + 1).min(rows - 1);
	}
	if nav.up {
		*sub_sel = sub_sel.saturating_sub(1);
	}
	let s = (*sub_sel).min(rows - 1);

	let emu_opts: &[(&str, &str)] = &[
		("auto", t("controllers.emuAuto")),
		("xbox360", t("controllers.emuXbox")),
		("ds4", t("controllers.emuDs4")),
	];
	let rumble_opts: &[(&str, &str)] = &[
		("off", t("controllers.rumbleOff")),
		("weak", t("controllers.rumbleWeak")),
		("medium", t("controllers.rumbleMedium")),
		("strong", t("controllers.rumbleStrong")),
	];
	let status_opts: &[(&str, &str)] = &[
		("on", t("controllers.enabled")),
		("off", t("controllers.disabled")),
	];
	// SPLIT MODE: per-pad "lock to THIS session" toggle options. "Kilitli" = forward only to this
	// pane regardless of focus; "Serbest" = follow the focused pane (default).
	let lock_opts: &[(&str, &str)] = &[
		("on", t("controllers.lockOn")),
		("off", t("controllers.lockOff")),
	];

	ui.spacing_mut().item_spacing.y = 5.0;
	for (i, (slot, kind, name, uuid, target, rumble, disabled)) in st.controllers.iter().enumerate() {
		let base = i * PER;
		// --- Row 0: header + reorder. Label = "Oyuncu N · kind · name"; ◂/▸ swap with the
		// neighbouring pad (immediate; the cursor follows the moved pad). ---
		let hdr = format!("{} {} · {} · {}", t("controllers.slot"), slot + 1, kind, name);
		let hsel = s == base;
		ui.add_space(if i == 0 { 0.0 } else { 4.0 });
		let (hrect, hresp) =
			ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::click());
		if hsel {
			ui.painter().rect_filled(hrect.expand(2.0), 7.0, CYAN);
			ui.painter().rect_filled(hrect, 6.0, egui::Color32::from_rgb(20, 54, 70));
		}
		let hcol = if *disabled {
			egui::Color32::from_gray(110)
		} else if hsel {
			CYAN
		} else {
			egui::Color32::from_rgb(228, 230, 240)
		};
		ui.painter().text(
			egui::pos2(hrect.left() + 10.0, hrect.center().y),
			egui::Align2::LEFT_CENTER,
			&hdr,
			egui::FontId::proportional(12.5),
			hcol,
		);
		let hovered_h = hresp.hovered();
		if hsel || hovered_h {
			let cy = hrect.center().y;
			let p = ui.painter();
			paint_tri(&p, egui::pos2(hrect.right() - 12.0, cy), 4.5, true, CYAN);
			paint_tri(&p, egui::pos2(hrect.right() - 28.0, cy), 4.5, false, CYAN);
		}
		// Pad reorder via ◂/▸ (when selected) or a mouse click on the row's right/left half.
		let swap = |to: usize, cmds: &mut Vec<OverlayCmd>| {
			cmds.push(OverlayCmd::Set("ctrlswap", format!("{i},{to}")));
		};
		if hsel && nav.left && i > 0 {
			swap(i - 1, cmds);
			*sub_sel = (i - 1) * PER;
		} else if hsel && nav.right && i + 1 < n {
			swap(i + 1, cmds);
			*sub_sel = (i + 1) * PER;
		} else if hresp.clicked() {
			let px = hresp.interact_pointer_pos().map(|p| p.x).unwrap_or(hrect.center().x);
			if px >= hrect.center().x && i + 1 < n {
				swap(i + 1, cmds);
				*sub_sel = (i + 1) * PER;
			} else if px < hrect.center().x && i > 0 {
				swap(i - 1, cmds);
				*sub_sel = (i - 1) * PER;
			}
		}

		// --- Rows 1-3: emulation, vibration, status — standard pad/mouse choice rows. ---
		if let Some(v) =
			choice_row(ui, s == base + 1, nav, t("controllers.emulation"), target, emu_opts, base + 1, pending)
		{
			if !uuid.is_empty() {
				cmds.push(OverlayCmd::Set("ctrlemu", format!("{uuid},{v}")));
			}
		}
		if let Some(v) =
			choice_row(ui, s == base + 2, nav, t("controllers.rumble"), rumble, rumble_opts, base + 2, pending)
		{
			if !uuid.is_empty() {
				cmds.push(OverlayCmd::Set("ctrlrumble", format!("{uuid},{v}")));
			}
		}
		// Test row — fire a one-shot rumble pulse at the pad's CURRENT level so the player can feel
		// it. Disabled when vibration is off (would do nothing) or the pad has no uuid.
		let test_on = !uuid.is_empty() && rumble != "off";
		let test_label = if test_on { t("controllers.test") } else { t("controllers.testOff") };
		let test_col = if test_on { CYAN } else { egui::Color32::from_gray(110) };
		if action_row(ui, s == base + 3, nav, test_label, test_col) && test_on {
			cmds.push(OverlayCmd::Set("ctrltest", uuid.clone()));
		}
		let status_cur = if *disabled { "off" } else { "on" };
		if let Some(v) =
			choice_row(ui, s == base + 4, nav, t("controllers.status"), status_cur, status_opts, base + 4, pending)
		{
			if !uuid.is_empty() {
				let dis = if v == "off" { 1 } else { 0 };
				cmds.push(OverlayCmd::Set("ctrldisable", format!("{uuid},{dis}")));
			}
		}
		// --- Row 5 (SPLIT MODE): "Bu oturuma kilitle" — lock this pad to THIS session/pane so it
		// forwards only here regardless of which pane has input focus. Checked state comes from the
		// `ctrls` line's per-pad lock field (st.controllers_locked, filled by the platform parser).
		// Emits `ctrllock` "<uuid>,1|0" — mirroring `ctrldisable` exactly. ---
		let lock_cur = if st.controllers_locked.contains(uuid) { "on" } else { "off" };
		if let Some(v) =
			choice_row(ui, s == base + 5, nav, t("controllers.lock"), lock_cur, lock_opts, base + 5, pending)
		{
			if !uuid.is_empty() {
				let locked = if v == "on" { 1 } else { 0 };
				cmds.push(OverlayCmd::Set("ctrllock", format!("{uuid},{locked}")));
			}
		}
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

/// A clickable category box (the root hub's grid cells). `icon` is a Phosphor glyph
/// (e.g. `egui_phosphor::regular::BROADCAST`) painted at the left — the Phosphor font is
/// installed into egui's atlas in `apply_theme`, so the glyph renders on every backend.
fn cat_box(ui: &mut egui::Ui, icon: &str, label: &str, selected: bool) -> egui::Response {
	let size = egui::vec2((ui.available_width() / 2.0 - 6.0).max(140.0), 40.0);
	let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
	// `selected` = the controller-nav cursor (UiState.sel, moved by the pad up/down) is on
	// this box. Draw a bright cyan BORDER + brighter fill so it's unmistakable; A/Enter
	// activates it. Driven purely by sel (not egui has_focus) so egui's invisible Tab-focus
	// traversal can't paint a second highlight on a different box.
	let focused = selected;
	if focused {
		ui.painter().rect_filled(rect.expand(2.5), 9.0, CYAN);
	}
	let fill = if focused {
		egui::Color32::from_rgb(18, 70, 92)
	} else if resp.hovered() {
		egui::Color32::from_rgb(40, 42, 58)
	} else {
		egui::Color32::from_rgb(28, 30, 42)
	};
	ui.painter().rect_filled(rect, 8.0, fill);
	// Section icon: the per-category Phosphor glyph (distinct per box). Cyan so it reads as the
	// accent; it renders because `apply_theme` installed the Phosphor font into egui's atlas.
	ui.painter().text(
		egui::pos2(rect.left() + 16.0, rect.center().y),
		egui::Align2::CENTER_CENTER,
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
	resp
}

/// One pad-navigable settings row inside a sub-view: `label` left, the current option's display
/// right (framed by ◂ ▸ when selected). STAGED-CONFIRM model: when selected, the pad's
/// left/right cycle a PENDING value (shown amber, NOT applied); pressing A/activate confirms it
/// and the value is returned (else None). Moving rows / changing view discards the pending value
/// (cleared by the caller), so a controller cycle never disrupts the live stream until confirmed.
/// `row` is this row's index within the view; `pending` is `(row, value)` held in UiState.
fn choice_row(
	ui: &mut egui::Ui,
	selected: bool,
	nav: Nav,
	label: &str,
	current: &str,
	options: &[(&str, &str)],
	row: usize,
	pending: &mut Option<(usize, String)>,
) -> Option<String> {
	choice_row_i(ui, selected, nav, label, current, options, row, pending, None)
}

/// Like [`choice_row`] but with an optional inline ⓘ explainer right after the label (a painted
/// info glyph with a click/hover popup — same affordance as the standalone [`info`] helper, used
/// for rows whose name alone doesn't explain them, e.g. frame pacing / voice call). A click on the
/// ⓘ opens the popup WITHOUT cycling the row value.
#[allow(clippy::too_many_arguments)]
fn choice_row_i(
	ui: &mut egui::Ui,
	selected: bool,
	nav: Nav,
	label: &str,
	current: &str,
	options: &[(&str, &str)],
	row: usize,
	pending: &mut Option<(usize, String)>,
	info: Option<&str>,
) -> Option<String> {
	// Effective value = the staged value for THIS row if one is pending, else the live value.
	let effective = match pending.as_ref() {
		Some((r, v)) if *r == row => v.clone(),
		_ => current.to_string(),
	};
	let mut confirmed = None;
	if selected && !options.is_empty() {
		let cur = options.iter().position(|(v, _)| *v == effective).unwrap_or(0);
		if nav.right {
			*pending = Some((row, options[(cur + 1) % options.len()].0.to_string()));
		} else if nav.left {
			*pending = Some((row, options[(cur + options.len() - 1) % options.len()].0.to_string()));
		} else if nav.activate {
			if let Some((r, v)) = pending.clone() {
				if r == row {
					confirmed = Some(v);
					*pending = None;
				}
			}
		}
	}
	// Re-read after this frame's staging for display.
	let staged = selected && matches!(pending.as_ref(), Some((r, _)) if *r == row);
	let value = match pending.as_ref() {
		Some((r, v)) if *r == row => v.clone(),
		_ => current.to_string(),
	};
	let cur = options.iter().position(|(v, _)| *v == value).unwrap_or(0);
	let disp = options.get(cur).map(|(_, d)| *d).unwrap_or(value.as_str());
	let (rect, resp) =
		ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
	// Inline ⓘ explainer hit-rect (computed before the row-click handling so a click ON the icon
	// opens its popup WITHOUT also cycling the row value). Positioned just right of the label.
	let info_icon = info.map(|text| {
		let lw = ui
			.painter()
			.layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), egui::Color32::from_gray(205))
			.size()
			.x;
		let ic = egui::pos2(rect.left() + 12.0 + lw + 12.0, rect.center().y);
		(text, ic, egui::Rect::from_center_size(ic, egui::vec2(16.0, 16.0)))
	});
	let over_info = info_icon
		.map(|(_, _, ir)| ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| ir.contains(p)))
		.unwrap_or(false);
	// MOUSE: clicking the row changes the value IMMEDIATELY — right half = next option, left
	// half = previous (the ◂ ▸ are clickable). Mouse applies directly (instant), discarding
	// any pad-staged value; the pad path stages + confirms with A.
	let hovered = resp.hovered();
	if confirmed.is_none() && resp.clicked() && !over_info && !options.is_empty() {
		let live = options.iter().position(|(v, _)| *v == current).unwrap_or(0);
		let px = resp.interact_pointer_pos().map(|p| p.x).unwrap_or(rect.center().x);
		let ni = if px >= rect.center().x {
			(live + 1) % options.len()
		} else {
			(live + options.len() - 1) % options.len()
		};
		confirmed = Some(options[ni].0.to_string());
		*pending = None;
	}
	if hovered {
		resp.on_hover_cursor(egui::CursorIcon::PointingHand);
	}
	// Show the value arrows + framing when pad-selected OR mouse-hovered (so it reads as
	// clickable under the mouse too).
	let active = selected || hovered;
	if selected {
		ui.painter().rect_filled(rect.expand(2.0), 8.0, CYAN);
		ui.painter().rect_filled(rect, 7.0, egui::Color32::from_rgb(20, 54, 70));
	} else {
		ui.painter()
			.rect_filled(rect, 7.0, egui::Color32::from_rgb(26, 28, 38));
	}
	ui.painter().text(
		egui::pos2(rect.left() + 12.0, rect.center().y),
		egui::Align2::LEFT_CENTER,
		label,
		egui::FontId::proportional(13.0),
		egui::Color32::from_gray(205),
	);
	// Value (right-aligned), flanked by PAINTED ◂ ▸ triangles when selected. AMBER while a
	// change is staged (not yet applied — press A to confirm); cyan when selected & committed.
	let amber = egui::Color32::from_rgb(240, 185, 70);
	let color = if staged {
		amber
	} else if active {
		CYAN
	} else {
		egui::Color32::from_gray(225)
	};
	let font = egui::FontId::proportional(13.0);
	let vw = ui
		.painter()
		.layout_no_wrap(disp.to_string(), font.clone(), color)
		.size()
		.x;
	let cy = rect.center().y;
	let tri = 4.5;
	let gap = 8.0;
	let val_right = if active {
		rect.right() - 12.0 - tri * 2.0 - gap
	} else {
		rect.right() - 12.0
	};
	ui.painter().text(
		egui::pos2(val_right, cy),
		egui::Align2::RIGHT_CENTER,
		disp,
		font,
		color,
	);
	if active {
		let arrow = if staged { amber } else { CYAN };
		let p = ui.painter();
		paint_tri(&p, egui::pos2(rect.right() - 12.0 - tri, cy), tri, true, arrow);
		paint_tri(&p, egui::pos2(val_right - vw - gap - tri, cy), tri, false, arrow);
	}
	// Inline ⓘ explainer: paint the glyph + a click/hover popup (the hit-rect was reserved above).
	if let Some((text, ic, irect)) = info_icon {
		let popup_id = ui.make_persistent_id(("crow-info-popup", row));
		let iresp = ui.interact(irect, ui.make_persistent_id(("crow-info-hit", row)), egui::Sense::click());
		let open = ui.memory(|m| m.is_popup_open(popup_id));
		let col = if iresp.hovered() || open {
			CYAN
		} else {
			egui::Color32::from_rgb(120, 125, 140)
		};
		{
			let p = ui.painter();
			p.circle_stroke(ic, 6.0, egui::Stroke::new(1.3, col));
			p.circle_filled(ic + egui::vec2(0.0, -2.8), 0.9, col);
			p.line_segment(
				[ic + egui::vec2(0.0, -0.4), ic + egui::vec2(0.0, 3.0)],
				egui::Stroke::new(1.5, col),
			);
		}
		if iresp.clicked() {
			ui.memory_mut(|m| m.toggle_popup(popup_id));
		}
		iresp.clone().on_hover_text(egui::RichText::new(text).size(12.0));
		egui::popup_below_widget(
			ui,
			popup_id,
			&iresp,
			egui::PopupCloseBehavior::CloseOnClickOutside,
			|ui| {
				ui.set_max_width(240.0);
				ui.label(egui::RichText::new(text).size(12.0));
			},
		);
	}
	confirmed
}

/// A button-style row (no value, no ◂▸): pad-select + A, or a mouse click, fires the action.
/// Returns true when activated this frame. `accent` colours the label when active (e.g. cyan).
fn action_row(ui: &mut egui::Ui, selected: bool, nav: Nav, label: &str, accent: egui::Color32) -> bool {
	let (rect, resp) =
		ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
	let hovered = resp.hovered();
	let clicked = resp.clicked();
	if hovered {
		resp.on_hover_cursor(egui::CursorIcon::PointingHand);
	}
	let active = selected || hovered;
	if selected {
		ui.painter().rect_filled(rect.expand(2.0), 8.0, accent);
		ui.painter().rect_filled(rect, 7.0, egui::Color32::from_rgb(20, 54, 70));
	} else {
		ui.painter()
			.rect_filled(rect, 7.0, egui::Color32::from_rgb(26, 28, 38));
	}
	let col = if active { accent } else { egui::Color32::from_gray(205) };
	ui.painter().text(
		rect.center(),
		egui::Align2::CENTER_CENTER,
		label,
		egui::FontId::proportional(12.5),
		col,
	);
	(selected && nav.activate) || clicked
}

/// Paint a small filled triangle marker (the ◂/▸ value arrows — the unicode glyphs are missing
/// from the bundled egui font). `right` = pointing right, otherwise pointing left.
fn paint_tri(painter: &egui::Painter, center: egui::Pos2, half: f32, right: bool, color: egui::Color32) {
	let dx = if right { half } else { -half };
	let pts = vec![
		egui::pos2(center.x - dx, center.y - half),
		egui::pos2(center.x - dx, center.y + half),
		egui::pos2(center.x + dx, center.y),
	];
	painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

fn stat_tile(ui: &mut egui::Ui, value: &str, icon: &str, label: &str) {
	egui::Frame::none()
		.fill(egui::Color32::from_rgb(22, 24, 34))
		.rounding(8.0)
		.inner_margin(egui::Margin::symmetric(10.0, 8.0))
		.show(ui, |ui| {
			ui.vertical(|ui| {
				ui.label(egui::RichText::new(value).size(18.0).strong().color(CYAN));
				// Phosphor icon glyph + label. The icon is a real font glyph now (installed in
				// `apply_theme`), so it paints instead of a tofu box.
				ui.horizontal(|ui| {
					ui.spacing_mut().item_spacing.x = 4.0;
					ui.label(
						egui::RichText::new(icon)
							.size(12.0)
							.color(egui::Color32::from_rgb(120, 200, 230)),
					);
					ui.label(
						egui::RichText::new(label)
							.small()
							.color(egui::Color32::GRAY),
					);
				});
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

// Retained for the mouse/remote-mode menus and as a fallback; the game-mode sub-views now use
// pad-navigable `choice_row`s instead.
#[allow(dead_code)]
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

// Retained for the remote-mode seg menus / as a fallback — the game sub-views moved to
// pad-navigable choice_rows (which carry the inline ⓘ explainer now), leaving this unused.
#[allow(dead_code)]
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
/// fades in via egui's built-in popup animation. (Superseded inline in `choice_row_i`'s
/// `info` arg, but kept as the standalone helper.)
#[allow(dead_code)]
fn info(ui: &mut egui::Ui, text: &str) {
	// Clickable (not just hover): a tap toggles a popup with the explanation — on a controller-
	// /touch-driven overlay hover tooltips never appear, so clicking did nothing before.
	let (rect, resp) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::click());
	let popup_id = ui.make_persistent_id(("info-popup", rect.left() as i32, rect.top() as i32));
	let open = ui.memory(|m| m.is_popup_open(popup_id));
	let col = if resp.hovered() || open {
		CYAN
	} else {
		egui::Color32::from_rgb(110, 115, 130)
	};
	let c = rect.center();
	{
		let p = ui.painter();
		p.circle_stroke(c, 6.0, egui::Stroke::new(1.3, col));
		p.circle_filled(c + egui::vec2(0.0, -2.8), 0.9, col);
		p.line_segment(
			[c + egui::vec2(0.0, -0.4), c + egui::vec2(0.0, 3.0)],
			egui::Stroke::new(1.5, col),
		);
	}
	if resp.clicked() {
		ui.memory_mut(|m| m.toggle_popup(popup_id));
	}
	resp.clone().on_hover_text(egui::RichText::new(text).size(12.0));
	egui::popup_below_widget(
		ui,
		popup_id,
		&resp,
		egui::PopupCloseBehavior::CloseOnClickOutside,
		|ui| {
			ui.set_max_width(240.0);
			ui.label(egui::RichText::new(text).size(12.0));
		},
	);
}

#[cfg(test)]
mod focus_tests {
	use super::*;

	// The pad-nav overlay (G6) moves focus with egui Tab/Shift-Tab events fed from the
	// controller reader (up→shifttab, down→tab). This proves the menu boxes are actually
	// keyboard-focusable and that Tab walks them — the necessary condition for pad nav to
	// work. If this fails, no amount of pad input will move the overlay selection.
	#[test]
	fn tab_walks_the_menu_boxes() {
		let ctx = egui::Context::default();
		let run = |events: Vec<egui::Event>| {
			ctx.run(
				egui::RawInput {
					events,
					screen_rect: Some(egui::Rect::from_min_size(
						egui::pos2(0.0, 0.0),
						egui::vec2(400.0, 600.0),
					)),
					..Default::default()
				},
				|ctx| {
					egui::CentralPanel::default().show(ctx, |ui| {
						cat_box(ui, icon::BROADCAST, "one", false);
						cat_box(ui, icon::MONITOR, "two", false);
						cat_box(ui, icon::CHART_BAR, "three", false);
					});
				},
			);
		};
		let tab = || egui::Event::Key {
			key: egui::Key::Tab,
			physical_key: None,
			pressed: true,
			repeat: false,
			modifiers: egui::Modifiers::default(),
		};
		run(vec![]); // first frame: register the widgets
		run(vec![tab()]);
		let f1 = ctx.memory(|m| m.focused());
		run(vec![tab()]);
		let f2 = ctx.memory(|m| m.focused());
		assert!(f1.is_some(), "Tab focused nothing — cat_box is not keyboard-focusable");
		assert_ne!(f1, f2, "second Tab did not advance focus to the next box");
	}
}
