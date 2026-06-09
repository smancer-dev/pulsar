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
    pub decoder: String,
    pub res: String,
    pub fps_sel: String,
    pub bitrate: String, // Mbit, "0" = auto
    pub quality: String, // "latency" | "quality"
    pub pace: bool,      // frame pacing (Moonlight-style smoothing) on/off
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
            decoder: "auto".into(),
            res: "auto".into(),
            fps_sel: "auto".into(),
            bitrate: "0".into(),
            quality: "latency".into(),
            pace: false,
        }
    }
}

/// Emitted on interaction → serialized to stdout (`ov <field> <value>`) for the host.
pub enum OverlayCmd {
    Set(&'static str, String),
    End,
    Close,
}

const CODECS: &[(&str, &str)] = &[("auto", "Otomatik"), ("h264", "H.264"), ("h265", "H.265"), ("av1", "AV1")];
const ENCODERS: &[(&str, &str)] = &[
    ("auto", "Otomatik"),
    ("nvenc", "NVIDIA NVENC"),
    ("quicksync", "Intel QuickSync"),
    ("amf", "AMD AMF"),
    ("videotoolbox", "Apple VideoToolbox"),
    ("vaapi", "VA-API"),
    ("software", "Yazılım (CPU)"),
];
const RES: &[(&str, &str)] = &[("auto", "Otomatik"), ("1080p", "1080p"), ("1440p", "1440p"), ("4K", "4K")];
const FPS: &[(&str, &str)] = &[("auto", "Otomatik"), ("30", "30"), ("60", "60"), ("120", "120")];
const BITRATE: &[(&str, &str)] =
    &[("0", "Otomatik"), ("10", "10 Mbit"), ("20", "20 Mbit"), ("30", "30 Mbit"), ("50", "50 Mbit"), ("100", "100 Mbit")];

const ACCENT: egui::Color32 = egui::Color32::from_rgb(124, 110, 245); // electric indigo
const CYAN: egui::Color32 = egui::Color32::from_rgb(120, 200, 240);

/// Apply the Pulsar dark theme to an egui context (call once at startup).
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
pub fn draw(ctx: &egui::Context, st: &OverlayState) -> Vec<OverlayCmd> {
    let mut cmds = Vec::new();
    if !st.open {
        return cmds;
    }
    // Dim scrim behind the panel; clicking it closes the overlay.
    egui::Area::new("scrim".into())
        .order(egui::Order::Background)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let r = ctx.screen_rect();
            ui.painter().rect_filled(r, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 120));
            if ui.interact(r, egui::Id::new("scrim_click"), egui::Sense::click()).clicked() {
                cmds.push(OverlayCmd::Close);
            }
        });

    egui::Window::new("pulsar_overlay")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .fixed_size(egui::vec2(520.0, 0.0))
        .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(20.0)))
        .show(ctx, |ui| {
            // Header
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("Pulsar").color(CYAN).strong());
                let label = if st.mode == Mode::Game { "Oyun" } else { "Uzak Masaüstü" };
                ui.label(egui::RichText::new(label).color(egui::Color32::GRAY));
            });
            ui.label(
                egui::RichText::new(format!("{} · {} · {:.0} fps", st.id, st.conn_label, st.fps))
                    .monospace()
                    .color(egui::Color32::from_rgb(150, 155, 170)),
            );
            ui.add_space(8.0);

            // Stat tiles
            ui.horizontal(|ui| {
                stat_tile(ui, &format!("{:.0}", st.latency_ms), "Gecikme ms");
                stat_tile(ui, &format!("{:.0}", st.fps), "FPS");
                stat_tile(ui, &format!("{:.1}", st.decode_ms), "Çözme ms");
                stat_tile(ui, &format!("{:.1}", st.mbps), "Mbps");
            });
            ui.add_space(12.0);

            // Selectors (game-relevant; remote will extend later)
            egui::Grid::new("ov_fields").num_columns(2).spacing(egui::vec2(16.0, 10.0)).show(ui, |ui| {
                if let Some(v) = combo(ui, "Codec", "codec", &st.codec, CODECS) { cmds.push(OverlayCmd::Set("codec", v)); }
                if let Some(v) = combo(ui, "Encoder", "encoder", &st.encoder, ENCODERS) { cmds.push(OverlayCmd::Set("encoder", v)); }
                ui.end_row();
                if let Some(v) = combo(ui, "Decoder", "decoder", &st.decoder, ENCODERS) { cmds.push(OverlayCmd::Set("decoder", v)); }
                if let Some(v) = combo(ui, "Çözünürlük", "res", &st.res, RES) { cmds.push(OverlayCmd::Set("res", v)); }
                ui.end_row();
                if let Some(v) = combo(ui, "FPS", "fps", &st.fps_sel, FPS) { cmds.push(OverlayCmd::Set("fps", v)); }
                if let Some(v) = combo(ui, "Bitrate", "bitrate", &st.bitrate, BITRATE) { cmds.push(OverlayCmd::Set("bitrate", v)); }
                ui.end_row();
            });
            ui.add_space(12.0);

            // Quality segment
            ui.horizontal(|ui| {
                ui.label("Kalite");
                if seg(ui, st.quality == "latency", "Düşük gecikme") { cmds.push(OverlayCmd::Set("quality", "latency".into())); }
                if seg(ui, st.quality == "quality", "Kalite") { cmds.push(OverlayCmd::Set("quality", "quality".into())); }
            });
            ui.add_space(10.0);

            // Frame pacing segment (Moonlight-style: smooth vs lowest-latency)
            ui.horizontal(|ui| {
                ui.label("Kare eşitleme");
                if seg(ui, st.pace, "Açık") { cmds.push(OverlayCmd::Set("pace", "on".into())); }
                if seg(ui, !st.pace, "Kapalı") { cmds.push(OverlayCmd::Set("pace", "off".into())); }
            });
            ui.add_space(16.0);

            // End + footer
            if ui.add(egui::Button::new(egui::RichText::new("Oturumu Bitir").color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(200, 60, 70)).min_size(egui::vec2(ui.available_width(), 36.0))).clicked() {
                cmds.push(OverlayCmd::End);
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Ctrl+Shift+M kapat · Ctrl+Shift+Q çık")
                .monospace().small().color(egui::Color32::from_rgb(120, 125, 140)));
        });
    cmds
}

fn stat_tile(ui: &mut egui::Ui, value: &str, label: &str) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(22, 24, 34))
        .rounding(8.0)
        .inner_margin(egui::Margin::symmetric(14.0, 8.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(value).size(22.0).strong().color(CYAN));
                ui.label(egui::RichText::new(label).small().color(egui::Color32::GRAY));
            });
        });
}

/// A labelled ComboBox; returns Some(value) when the user picks a new value.
fn combo(ui: &mut egui::Ui, label: &str, id: &str, cur: &str, opts: &[(&str, &str)]) -> Option<String> {
    let mut picked = None;
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(label).small().color(egui::Color32::GRAY));
        let cur_label = opts.iter().find(|(v, _)| *v == cur).map(|(_, l)| *l).unwrap_or(cur);
        egui::ComboBox::from_id_salt(id).selected_text(cur_label).width(180.0).show_ui(ui, |ui| {
            for (v, l) in opts {
                if ui.selectable_label(*v == cur, *l).clicked() && *v != cur {
                    picked = Some((*v).to_string());
                }
            }
        });
    });
    picked
}

fn seg(ui: &mut egui::Ui, on: bool, label: &str) -> bool {
    let fill = if on { ACCENT } else { egui::Color32::from_rgb(28, 30, 42) };
    ui.add(egui::Button::new(label).fill(fill)).clicked() && !on
}
