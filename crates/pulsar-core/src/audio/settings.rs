//! Audio toggles, the game-mode policy override, and the per-platform capture
//! source. The `AudioSettings::policy` rule is the single source of truth for
//! how game mode moves audio to the player.

use serde::{Deserialize, Serialize};

/// PCM/stream format constants (also the Opus encode target).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// The user's two audio toggles. Mirrors the matching [`crate::config::Config`]
/// fields; kept as its own type so the policy logic is testable in isolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
	/// Send host audio to the client (host → client).
	pub transmit: bool,
	/// Mute the host's local speakers while streaming.
	pub mute_host: bool,
}

impl Default for AudioSettings {
	/// Sensible default: stream audio, but don't touch the host's speakers.
	fn default() -> Self {
		Self {
			transmit: true,
			mute_host: false,
		}
	}
}

/// What actually happens for a given session, after the game-mode override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPolicy {
	/// Capture + send host audio to the client.
	pub transmit: bool,
	/// Mute the host's local output for the duration of the session.
	pub mute_host: bool,
}

impl AudioSettings {
	/// Resolve the effective policy. In **game mode** audio belongs to the player:
	/// transmit is forced ON and the host is muted, regardless of the toggles. In
	/// desktop mode the toggles are honored as-is.
	pub fn policy(self, game_mode: bool) -> AudioPolicy {
		if game_mode {
			AudioPolicy {
				transmit: true,
				mute_host: true,
			}
		} else {
			AudioPolicy {
				transmit: self.transmit,
				mute_host: self.mute_host,
			}
		}
	}
}

/// Per-platform audio capture source for ffmpeg. Loopback (capturing what the host
/// is *playing*, not a mic) is the goal: a virtual-cable/Stereo-Mix DirectShow
/// device on Windows, a sink `.monitor` on PulseAudio/PipeWire, an AVFoundation
/// device index on macOS. The exact device name is machine-specific, so it's
/// configurable; these are the defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "device", rename_all = "kebab-case")]
pub enum AudioInput {
	/// Windows DirectShow audio device (a loopback / "Stereo Mix" / virtual cable).
	Dshow(String),
	/// PulseAudio/PipeWire source — typically `<sink>.monitor` to capture playback.
	Pulse(String),
	/// macOS AVFoundation audio device index.
	AvFoundation(u32),
}

impl AudioInput {
	/// The best-guess default capture source for the build platform.
	pub fn default_for_os() -> Self {
		if cfg!(windows) {
			// Shipped by many loopback tools; users override in Settings if absent.
			Self::Dshow("virtual-audio-capturer".to_string())
		} else if cfg!(target_os = "macos") {
			Self::AvFoundation(0)
		} else {
			// PulseAudio resolves the default sink's monitor from this token.
			Self::Pulse("@DEFAULT_MONITOR@".to_string())
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn game_mode_forces_transmit_and_mute() {
		// Desktop mode honors the toggles…
		let off = AudioSettings {
			transmit: false,
			mute_host: false,
		};
		assert_eq!(
			off.policy(false),
			AudioPolicy {
				transmit: false,
				mute_host: false
			}
		);
		// …but game mode moves audio entirely to the client regardless.
		assert_eq!(
			off.policy(true),
			AudioPolicy {
				transmit: true,
				mute_host: true
			}
		);
	}

	#[test]
	fn desktop_mode_passes_settings_through() {
		let s = AudioSettings {
			transmit: true,
			mute_host: false,
		};
		assert_eq!(
			s.policy(false),
			AudioPolicy {
				transmit: true,
				mute_host: false
			}
		);
		let s2 = AudioSettings {
			transmit: true,
			mute_host: true,
		};
		assert_eq!(
			s2.policy(false),
			AudioPolicy {
				transmit: true,
				mute_host: true
			}
		);
	}

	#[test]
	fn default_settings_stream_without_muting() {
		assert_eq!(
			AudioSettings::default(),
			AudioSettings {
				transmit: true,
				mute_host: false
			}
		);
	}

	#[test]
	fn default_input_matches_platform() {
		let inp = AudioInput::default_for_os();
		if cfg!(windows) {
			assert!(matches!(inp, AudioInput::Dshow(_)));
		} else if cfg!(target_os = "macos") {
			assert!(matches!(inp, AudioInput::AvFoundation(_)));
		} else {
			assert!(matches!(inp, AudioInput::Pulse(_)));
		}
	}

	#[test]
	fn audio_input_serde_round_trips() {
		for inp in [
			AudioInput::Dshow("d".into()),
			AudioInput::Pulse("p".into()),
			AudioInput::AvFoundation(1),
		] {
			let j = serde_json::to_string(&inp).unwrap();
			assert_eq!(serde_json::from_str::<AudioInput>(&j).unwrap(), inp);
		}
	}
}
