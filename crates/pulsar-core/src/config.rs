//! Persistent app configuration.
//!
//! The relay endpoint is **user-changeable** (the app can point at any relay,
//! including a self-hosted or local one), and the network mode controls the
//! P2P/relay strategy — matching the design's Ayarlar → Ağ section.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default relay endpoint. Points at a local relay so the app works out of the
/// box with `cargo run -p pulsar-relay`; users override it in Settings → Ağ to
/// point at a public / self-hosted relay.
pub const DEFAULT_RELAY: &str = "127.0.0.1:21116";

/// How Pulsar establishes a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
	/// Try direct P2P first, fall back to the relay automatically. (Recommended.)
	#[default]
	Auto,
	/// Only ever connect directly (no relay fallback).
	P2pOnly,
	/// Always go through the relay (skip hole punching).
	RelayOnly,
}

/// UI language. The app ships Turkish (default) and English; the core stays
/// language-agnostic and just stores the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
	#[default]
	Tr,
	En,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
	/// `host:port` of the relay / rendezvous server. Changeable by the user.
	pub relay: String,
	/// Connection strategy.
	pub network_mode: NetworkMode,
	/// Friendly name advertised to peers.
	pub device_name: String,
	/// UI language.
	pub language: Language,
	/// Allow unattended (gözetimsiz) access to this host.
	pub unattended_access: bool,
	/// Optional PERSISTENT connect password (empty = none). Accepted alongside the
	/// rotating one-time password — a client presenting either is let in without
	/// the Allow/Deny prompt. Wrong attempts are rate-limited host-side (the
	/// password is a standing secret, so it must not be brute-forceable).
	#[serde(default)]
	pub connect_password: String,
	/// Stream this host's audio to the client (host → client). When off, the
	/// session is video-only. See [`crate::audio`] for how game mode overrides this.
	/// `#[serde(default)]` so configs written before this field still load.
	#[serde(default = "default_true")]
	pub transmit_audio: bool,
	/// Silence this host's *local* speakers while streaming (the sound then plays
	/// only on the client). Independent of [`Self::transmit_audio`]; game mode
	/// forces both on so audio moves entirely to the player.
	#[serde(default)]
	pub mute_host_audio: bool,
	/// Audio capture source override (empty = platform default). Windows: a
	/// DirectShow device name (a loopback / "Stereo Mix" / virtual cable); Linux: a
	/// PulseAudio/PipeWire source (typically a sink `.monitor`); macOS: an
	/// AVFoundation device index. Configurable because the right loopback device is
	/// machine-specific.
	#[serde(default)]
	pub audio_input: String,
	/// Local node listen port for direct/P2P connections (`0` = pick automatically).
	/// Set a fixed port to make port-forwarding to this host predictable.
	#[serde(default)]
	pub node_port: u16,
	/// What identity to present to a peer when connecting: the OS account photo
	/// (`user`), the desktop wallpaper (`wallpaper`), or nothing (`anonymous`).
	/// The display name shown alongside is [`Self::device_name`].
	#[serde(default = "default_avatar_mode")]
	pub avatar_mode: String,
	/// Use the **native renderer** (a bundled ffplay window, hardware-decoded) for
	/// incoming video instead of the in-webview WebCodecs canvas. Far lighter on
	/// CPU/GPU; Windows-only, opt-in, falls back to the webview if ffplay won't run.
	#[serde(default)]
	pub native_player: bool,
	/// Host audio channel layout to capture + encode (stereo / 5.1 / 7.1). Threads
	/// into [`crate::audio::AudioSettings::layout`]. `#[serde(default)]` (stereo) so
	/// configs written before surround support still load and stay stereo.
	#[serde(default)]
	pub audio_layout: crate::audio::ChannelLayout,
}

fn default_avatar_mode() -> String {
	"user".to_string()
}

fn default_true() -> bool {
	true
}

impl Default for Config {
	fn default() -> Self {
		Self {
			relay: DEFAULT_RELAY.to_string(),
			network_mode: NetworkMode::Auto,
			device_name: default_device_name(),
			language: Language::Tr,
			unattended_access: false,
			connect_password: String::new(),
			transmit_audio: true,
			mute_host_audio: false,
			audio_input: String::new(),
			node_port: 0,
			avatar_mode: default_avatar_mode(),
			native_player: false,
			audio_layout: crate::audio::ChannelLayout::Stereo,
		}
	}
}

impl Config {
	/// Load from a JSON file, or return defaults if it doesn't exist / is invalid.
	pub fn load(path: impl AsRef<Path>) -> Self {
		std::fs::read_to_string(path)
			.ok()
			.and_then(|s| serde_json::from_str(&s).ok())
			.unwrap_or_default()
	}

	/// Persist to a JSON file (creating parent dirs).
	pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
		let path = path.as_ref();
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		let json = serde_json::to_string_pretty(self).expect("config serializes");
		std::fs::write(path, json)
	}

	/// The audio toggles as an [`crate::audio::AudioSettings`] for policy resolution.
	pub fn audio_settings(&self) -> crate::audio::AudioSettings {
		crate::audio::AudioSettings {
			transmit: self.transmit_audio,
			mute_host: self.mute_host_audio,
			layout: self.audio_layout,
		}
	}

	/// The configured capture source as an [`crate::audio::AudioInput`]; an empty
	/// override resolves to the platform default.
	pub fn audio_input(&self) -> crate::audio::AudioInput {
		let dev = self.audio_input.trim();
		if dev.is_empty() {
			crate::audio::AudioInput::default_for_os()
		} else if cfg!(windows) {
			crate::audio::AudioInput::Dshow(dev.to_string())
		} else if cfg!(target_os = "macos") {
			crate::audio::AudioInput::AvFoundation(dev.parse().unwrap_or(0))
		} else {
			crate::audio::AudioInput::Pulse(dev.to_string())
		}
	}

	/// Windows only: capture system audio via **WASAPI loopback** (the default render
	/// endpoint) rather than an ffmpeg dshow device. True when no explicit device is set
	/// or it's the `loopback`/`wasapi` sentinel — so audio streams out of the box without
	/// a `virtual-audio-capturer` / Stereo Mix device installed. A named device opts back
	/// into the dshow path ([`Self::audio_input`]).
	pub fn audio_loopback(&self) -> bool {
		if !cfg!(windows) {
			return false;
		}
		let d = self.audio_input.trim();
		d.is_empty() || d.eq_ignore_ascii_case("loopback") || d.eq_ignore_ascii_case("wasapi")
	}

	/// Returns true if the relay endpoint looks like a usable `host:port`.
	pub fn relay_is_valid(&self) -> bool {
		match self.relay.rsplit_once(':') {
			Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
			None => false,
		}
	}
}

fn default_device_name() -> String {
	// Use the real OS hostname cross-platform (whoami handles Windows/Linux/macOS).
	// `$HOSTNAME` is normally unset on Windows (the name lives in COMPUTERNAME) and
	// often unexported to GUI sessions on Linux, so reading it gave the generic
	// placeholder on most fresh installs.
	whoami::fallible::hostname()
		.ok()
		.filter(|h| !h.trim().is_empty())
		.unwrap_or_else(|| "Pulsar Cihazı".to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_are_auto_mode_and_turkish() {
		let c = Config::default();
		assert_eq!(c.network_mode, NetworkMode::Auto);
		assert_eq!(c.language, Language::Tr);
		assert!(
			c.relay_is_valid(),
			"default relay should be valid host:port"
		);
	}

	#[test]
	fn network_mode_serializes_kebab_case() {
		assert_eq!(
			serde_json::to_string(&NetworkMode::P2pOnly).unwrap(),
			"\"p2p-only\""
		);
		assert_eq!(
			serde_json::from_str::<NetworkMode>("\"relay-only\"").unwrap(),
			NetworkMode::RelayOnly
		);
	}

	#[test]
	fn relay_validation_catches_garbage() {
		let mut c = Config::default();
		c.relay = "no-port".into();
		assert!(!c.relay_is_valid());
		c.relay = "host:notaport".into();
		assert!(!c.relay_is_valid());
		c.relay = "127.0.0.1:21116".into();
		assert!(c.relay_is_valid());
	}

	#[test]
	fn audio_defaults_transmit_without_muting() {
		let c = Config::default();
		assert!(c.transmit_audio);
		assert!(!c.mute_host_audio);
		let s = c.audio_settings();
		assert!(s.transmit && !s.mute_host);
	}

	#[cfg(windows)]
	#[test]
	fn audio_loopback_is_windows_default_until_a_device_is_named() {
		let mut c = Config::default();
		// Empty (the default) → WASAPI loopback, so audio works with no capture device installed.
		assert!(c.audio_loopback());
		c.audio_input = "loopback".into();
		assert!(c.audio_loopback());
		// A named dshow device opts back out of loopback.
		c.audio_input = "Stereo Mix".into();
		assert!(!c.audio_loopback());
	}

	#[test]
	fn old_config_without_audio_fields_still_loads() {
		// A config written before the audio fields existed must still deserialize
		// (serde defaults fill them) rather than resetting every other setting.
		let json = r#"{"relay":"1.2.3.4:21116","network_mode":"relay-only",
			"device_name":"Eski PC","language":"en","unattended_access":true}"#;
		let c: Config = serde_json::from_str(json).expect("loads with serde defaults");
		assert_eq!(c.device_name, "Eski PC");
		assert!(c.unattended_access);
		assert!(c.transmit_audio); // default-true
		assert!(!c.mute_host_audio); // default-false
	}

	#[test]
	fn load_missing_file_yields_defaults() {
		let c = Config::load("/nonexistent/pulsar/config.json");
		assert_eq!(c, Config::default());
	}

	#[test]
	fn save_then_load_round_trips() {
		let dir = std::env::temp_dir().join(format!("pulsar-cfg-test-{}", std::process::id()));
		let path = dir.join("config.json");
		let mut cfg = Config::default();
		cfg.relay = "127.0.0.1:21116".into();
		cfg.network_mode = NetworkMode::RelayOnly;
		cfg.device_name = "Ev PC’si".into();
		cfg.save(&path).unwrap();

		let loaded = Config::load(&path);
		assert_eq!(loaded, cfg);
		let _ = std::fs::remove_dir_all(&dir);
	}
}
