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
}

impl Default for Config {
	fn default() -> Self {
		Self {
			relay: DEFAULT_RELAY.to_string(),
			network_mode: NetworkMode::Auto,
			device_name: default_device_name(),
			language: Language::Tr,
			unattended_access: false,
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

	/// Returns true if the relay endpoint looks like a usable `host:port`.
	pub fn relay_is_valid(&self) -> bool {
		match self.relay.rsplit_once(':') {
			Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
			None => false,
		}
	}
}

fn default_device_name() -> String {
	std::env::var("HOSTNAME")
		.ok()
		.filter(|h| !h.is_empty())
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
