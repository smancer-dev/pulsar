//! Pulsar mobile (Android + iOS) — client-only Tauri shell.
//!
//! Deliberately separate from `pulsar-tauri` (the desktop app), which pulls in
//! desktop-only, non-portable deps (SDL3, ffmpeg, the relay, gtk/x11, arboard,
//! rfd, libmpv) that don't cross-compile to Android/iOS. This crate stays thin:
//! the webview draws the UI, and (from M2 on) a native plugin draws HW-decoded
//! video on a surface BEHIND a transparent webview. `pulsar-core` is added at M4.

mod client;
mod config;
mod datachan;
mod host;
mod identity;
mod input_cmds;
mod net;
mod relay_cmds;
mod rtp;
mod session_cmds;

/// Tauri entry point. On mobile the `mobile_entry_point` macro exports the
/// symbol the generated Android/iOS shell calls; on desktop `main.rs` calls it
/// directly for quick sanity runs.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Route `log` records (client read-loop counters) to logcat on Android.
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("PulsarMobile"),
    );

    tauri::Builder::default()
        // Native video surface under the (transparent) webview — Android/iOS.
        .plugin(tauri_plugin_pulsar_video::init())
        // Per-slot input senders (existing, owned by client.rs).
        .manage(client::InputSenders::default())
        // Per-slot cancel/restream handles (W2-teardown, owned by session_cmds.rs).
        .manage(session_cmds::SessionRegistry::default())
        // Per-slot password-prompt oneshots (W3-client-authprompt).
        .manage(session_cmds::PwPending::default())
        // Per-slot file reassembly state (W4-rust-data, owned by datachan.rs).
        .manage(datachan::XferMap::default())
        // W5: which slot is the active foreground pane (audio routing).
        .manage(session_cmds::ActivePane::default())
        // W5: claimed-display map (freeDisplayFor logic for split panes).
        .manage(session_cmds::ClaimedDisplays::default())
        // Host lifecycle state (W3-host): go_online stores a fresh Arc<HostState>
        // here each call. Without this manage(), go_online panics with
        // "state() called before manage() for Mutex<Arc<HostState>>".
        .manage(std::sync::Mutex::new(std::sync::Arc::new(
            host::HostState::default(),
        )))
        // Host encoder/codec capability cache (W3/W5), read by go_online's
        // StreamCaps closure.
        .manage(host::HostCaps::default())
        // ONE shared relay Node for both client (connect_host) and host (go_online).
        // Separate per-role nodes registered twice under the same identity and made
        // the relay route inbound connections to the wrong node (no accept loop) →
        // no incoming-request popup. See `net.rs`.
        .manage(net::SharedNode::default())
        .manage(net::SharedDiscovery::default())
        // Local relay server task (Settings → Ağ → "Yerel relay").
        .manage(relay_cmds::LocalRelay::default())
        .invoke_handler(tauri::generate_handler![
            // ── Input / session (client.rs) ───────────────────────────────────
            client::connect_host,
            client::relay_health,
            client::local_ip,
            client::node_port,
            client::app_build_info,
            client::send_pointer,
            client::send_button,
            client::set_video_transform,
            // ── W4-rust-client: reverse direction + mic ───────────────────────
            client::reverse_play,
            client::mic_start,
            client::mic_stop,
            // ── Extended input commands (W2-inputcmds, input_cmds.rs) ─────────
            input_cmds::send_scroll,
            input_cmds::send_key,
            input_cmds::send_char,
            input_cmds::send_pointer_rel,
            // Gamepad commands (W4-rust-input): defined in input_cmds.rs but were
            // missing here, so every JS invoke failed "command not found" (swallowed
            // by gamepad.js's .catch) → the whole mobile gamepad feature was dead.
            input_cmds::send_gamepad,
            input_cmds::send_gamepad_disconnect,
            // ── Session lifecycle (W2-teardown, session_cmds.rs) ──────────────
            session_cmds::end_session,
            // ── Auth race (W3-client-authprompt, session_cmds.rs) ─────────────
            session_cmds::submit_password,
            // ── Live restream quality controls (W3-quality, session_cmds.rs) ──
            session_cmds::set_play_codec,
            session_cmds::set_play_bitrate,
            session_cmds::set_play_fps,
            session_cmds::set_play_resolution,
            session_cmds::set_play_quality,
            session_cmds::set_play_encoder,
            // ── W4-rust-client: monitor switch (session_cmds.rs) ─────────────
            session_cmds::set_play_monitor,
            // ── W5-rust-session: multi-session active routing + keyframe ──────
            session_cmds::set_active_pane,
            session_cmds::request_keyframe,
            // ── W5-rust-session: LAN device discovery (best-effort stub) ──────
            client::lan_devices,
            // ── Host (host.rs) ────────────────────────────────────────────────
            host::go_online,
            // These four were defined but NEVER registered, so every JS invoke of
            // them threw "command not found" — silently swallowed by the host UI's
            // catch blocks. Most visibly: tapping "Allow" on an incoming-connection
            // request did nothing (respond_request never resolved the approval
            // oneshot → the host auto-denied after 30 s).
            host::go_offline,
            host::new_password,
            host::respond_request,
            host::disconnect_session,
            host::host_codecs,
            host::open_a11y_settings,
            host::a11y_enabled,
            // ── Config (config.rs) ────────────────────────────────────────────
            config::get_config,
            config::set_config,
            // ── Local relay server (relay_cmds.rs) ────────────────────────────
            relay_cmds::start_local_relay,
            relay_cmds::stop_local_relay,
            relay_cmds::local_relay_status,
            // ── Side-channel data (W4-rust-data, datachan.rs) ─────────────────
            datachan::send_clipboard,
            datachan::send_chat,
            datachan::fs_list,
            datachan::fs_get,
            datachan::send_file,
            datachan::self_avatar,
            datachan::set_avatar_image,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulsar mobile");
}
