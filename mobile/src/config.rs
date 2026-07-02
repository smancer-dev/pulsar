//! Mobile config commands: `get_config` / `set_config`.
//!
//! Persists to `<app_data_dir>/config.json` via [`pulsar_core::Config::load`] /
//! [`pulsar_core::Config::save`]. The JS store (`js/store/config.js`) calls these
//! on boot and after every settings change; other commands (`connect_host`,
//! `go_online`) read the stored config when their args are omitted/empty.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use pulsar_core::{Config, Language, NetworkMode};

// ── Path helper ──────────────────────────────────────────────────────────────

/// Where the mobile config lives: `<app_data_dir>/config.json`.
pub(crate) fn config_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("config.json")
}

/// Convenience: load the persisted `Config` (or default if absent/corrupt).
pub(crate) fn load_config<R: Runtime>(app: &AppHandle<R>) -> Config {
    Config::load(config_path(app))
}

// ── Wire DTO ─────────────────────────────────────────────────────────────────

/// Subset of [`pulsar_core::Config`] fields that the mobile UI cares about.
/// Serialises using the same string vocabulary as core (kebab-case net mode,
/// lowercase language) so the JS store can store and send it back as-is.
///
/// We derive `Deserialize` so `set_config` can accept a partial object from JS
/// (unrecognised fields are silently ignored via `#[serde(deny_unknown_fields)]`
/// being absent — serde's default is to ignore them).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileConfig {
    /// `host:port` of the relay / rendezvous server.
    pub relay: String,
    /// `"auto"` / `"p2p-only"` / `"relay-only"`.
    pub network_mode: String,
    /// Friendly name advertised to peers.
    pub device_name: String,
    /// UI language code: `"tr"` | `"en"` | `"ru"` | `"kk"`.
    pub language: String,
    /// Allow unattended (gözetimsiz) access to this host.
    pub unattended_access: bool,
    /// Optional standing connect password (empty = OTP-only).
    pub connect_password: String,
    /// Preferred codec: `"auto"` / `"h265"` / `"h264"`.
    #[serde(default = "default_codec")]
    pub codec_pref: String,
    /// Preferred quality: `"latency"` / `"balanced"` / `"quality"`.
    #[serde(default = "default_quality")]
    pub quality_pref: String,
    /// Local UDP port for this device's relay node (0 = random/ephemeral). Lets a
    /// user pin a port for firewall / port-forward rules.
    #[serde(default)]
    pub node_port: u16,
    /// Identity image presented to peers. On mobile only `"wallpaper"` (the
    /// home-screen wallpaper, resolved via the pulsar-video plugin) and
    /// `"anonymous"` (none) are meaningful; the desktop `"user"` (account photo)
    /// has no Android source and resolves to the wallpaper. Mirrors core
    /// `Config.avatar_mode`.
    #[serde(default = "default_avatar")]
    pub avatar_mode: String,
}

fn default_avatar() -> String {
    "wallpaper".to_string()
}

fn default_codec() -> String {
    "auto".to_string()
}

fn default_quality() -> String {
    "balanced".to_string()
}

impl From<&Config> for MobileConfig {
    fn from(c: &Config) -> Self {
        let network_mode = match c.network_mode {
            NetworkMode::P2pOnly => "p2p-only",
            NetworkMode::RelayOnly => "relay-only",
            NetworkMode::Auto => "auto",
        }
        .to_string();

        let language = match c.language {
            Language::En => "en",
            Language::Tr => "tr",
        }
        .to_string();

        Self {
            relay: c.relay.clone(),
            network_mode,
            device_name: c.device_name.clone(),
            language,
            unattended_access: c.unattended_access,
            connect_password: c.connect_password.clone(),
            // Codec and quality are mobile-only fields not stored in core Config;
            // we default them here. W3-quality will persist them once that wave
            // adds a per-mobile config layer.
            codec_pref: default_codec(),
            quality_pref: default_quality(),
            node_port: c.node_port,
            avatar_mode: c.avatar_mode.clone(),
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Read the persisted config (or defaults). Returns a `MobileConfig` JSON object.
///
/// JS: `invoke('get_config', {})` → `{relay, networkMode, deviceName, language,
/// unattendedAccess, connectPassword, codecPref, qualityPref}`.
#[tauri::command]
pub async fn get_config<R: Runtime>(app: AppHandle<R>) -> Result<MobileConfig, String> {
    let cfg = load_config(&app);
    Ok(MobileConfig::from(&cfg))
}

/// Merge the given fields into the persisted config and save. Returns the full
/// merged config. Any field left empty / at its zero value is NOT overwritten
/// (the JS store passes only the changed fields).
///
/// JS: `invoke('set_config', { relay, networkMode, ... })` → `MobileConfig`.
#[tauri::command]
pub async fn set_config<R: Runtime>(
    app: AppHandle<R>,
    relay: Option<String>,
    network_mode: Option<String>,
    device_name: Option<String>,
    language: Option<String>,
    unattended_access: Option<bool>,
    connect_password: Option<String>,
    codec_pref: Option<String>,
    quality_pref: Option<String>,
    node_port: Option<u16>,
    avatar_mode: Option<String>,
) -> Result<MobileConfig, String> {
    let path = config_path(&app);
    let mut cfg = Config::load(&path);

    if let Some(v) = relay {
        if !v.is_empty() {
            cfg.relay = v;
        }
    }
    if let Some(v) = network_mode {
        cfg.network_mode = match v.as_str() {
            "p2p-only" => NetworkMode::P2pOnly,
            "relay-only" => NetworkMode::RelayOnly,
            _ => NetworkMode::Auto,
        };
    }
    if let Some(v) = device_name {
        if !v.is_empty() {
            cfg.device_name = v;
        }
    }
    if let Some(v) = language {
        cfg.language = match v.as_str() {
            "en" => Language::En,
            _ => Language::Tr,
        };
    }
    if let Some(v) = unattended_access {
        cfg.unattended_access = v;
    }
    if let Some(v) = connect_password {
        cfg.connect_password = v;
    }
    if let Some(v) = node_port {
        cfg.node_port = v;
    }
    if let Some(v) = avatar_mode {
        if !v.is_empty() {
            cfg.avatar_mode = v;
        }
    }
    // codec_pref / quality_pref are mobile-only; not mirrored into core Config.

    cfg.save(&path).map_err(|e| format!("config save failed: {e}"))?;

    let mut mc = MobileConfig::from(&cfg);
    // Preserve caller's codec/quality preferences in the returned object even
    // though they are not (yet) in the core Config.
    if let Some(v) = codec_pref {
        mc.codec_pref = v;
    }
    if let Some(v) = quality_pref {
        mc.quality_pref = v;
    }
    Ok(mc)
}
