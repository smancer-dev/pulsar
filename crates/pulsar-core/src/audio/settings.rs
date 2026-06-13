//! Audio toggles, the game-mode policy override, and the per-platform capture
//! source. The `AudioSettings::policy` rule is the single source of truth for
//! how game mode moves audio to the player.

use serde::{Deserialize, Serialize};

/// PCM/stream format constants (also the Opus encode target).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// The host audio channel layout to capture and encode, mirroring Sunshine's
/// `stream_config_e` (we keep only the channel *count* dimension here — the
/// high/low-bitrate split is a separate `-b:a` concern handled in `command.rs`).
///
/// Each layout maps to a valid **Opus multistream** description (RFC 7845 / the
/// libopus multistream API): a channel count, the number of Opus *streams*, the
/// number of those streams that are *coupled* (stereo) vs. uncoupled (mono), and a
/// channel **mapping** that reorders the interleaved PCM channels into Opus/Vorbis
/// channel order. ffmpeg's libopus encoder derives `streams`/`coupled` and the
/// mapping itself from `mapping_family 1` + the channel layout, so on the encode
/// side we only need to *name* the layout correctly — but the stream/coupled counts
/// are exposed here so the values match Sunshine exactly and are unit-testable, and
/// so a future native (non-ffmpeg) `opus_multistream_encoder_create` path has them.
///
/// The numeric values are the channel counts, matching Sunshine's `channelCount`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelLayout {
	/// 2.0 — front L/R. The default; every endpoint supports it.
	Stereo = 2,
	/// 5.1 — FL, FR, FC, LFE, BL, BR.
	Surround51 = 6,
	/// 7.1 — FL, FR, FC, LFE, BL, BR, SL, SR.
	Surround71 = 8,
}

impl Default for ChannelLayout {
	/// Stereo: the universally-decodable default (matches `CHANNELS`).
	fn default() -> Self {
		Self::Stereo
	}
}

impl ChannelLayout {
	/// Number of PCM/Opus channels in this layout (also the enum's discriminant).
	pub fn channels(self) -> u16 {
		match self {
			Self::Stereo => 2,
			Self::Surround51 => 6,
			Self::Surround71 => 8,
		}
	}

	/// The Opus **multistream** description for this layout: `(streams, coupled_streams)`.
	///
	/// `streams` is the total number of Opus streams; `coupled` is how many of those are
	/// stereo-coupled (carry 2 channels each). The remaining `streams - coupled` are mono.
	/// So `channels == coupled * 2 + (streams - coupled)`. Values copied verbatim from
	/// Sunshine's low-bitrate (non-`HIGH_QUALITY`) `stream_configs` entries, which are the
	/// canonical Moonlight/Opus layouts:
	///
	/// * Stereo (2ch): 1 stream, 1 coupled  → `(1, 1)`
	/// * 5.1   (6ch): 4 streams, 2 coupled → `(4, 2)`  (FL/FR + BL/BR coupled; FC, LFE mono)
	/// * 7.1   (8ch): 5 streams, 3 coupled → `(5, 3)`  (FL/FR + BL/BR + SL/SR coupled; FC, LFE mono)
	pub fn opus_streams(self) -> (u8, u8) {
		match self {
			Self::Stereo => (1, 1),
			Self::Surround51 => (4, 2),
			Self::Surround71 => (5, 3),
		}
	}

	/// The Opus channel **mapping** for this layout: for each Opus output channel index,
	/// which input PCM channel it draws from. This reorders interleaved PCM into Opus/Vorbis
	/// channel order. Sunshine's `platf::speaker::map_*` arrays are already in
	/// {FL, FR, FC, LFE, BL, BR, SL, SR} order — the natural WASAPI/PCM order — so the
	/// mapping is the identity `[0, 1, …, channels-1]`. Returned as an owned `Vec<u8>`
	/// sized to the channel count.
	///
	/// (Mapping family 1 with these counts is what makes a valid multistream Opus the
	/// client's `opus_multistream_decoder_create` / WebCodecs/`libopus` decoder can decode,
	/// provided it is configured with the *same* streams/coupled/mapping — see the SDP note
	/// in `command.rs`.)
	pub fn opus_mapping(self) -> Vec<u8> {
		(0..self.channels() as u8).collect()
	}

	/// The Opus **mapping family**: 0 for mono/stereo (RTP-style single stream), 1 for
	/// multichannel (Vorbis channel order, multistream). libopus/ffmpeg requires family 1
	/// for anything past stereo.
	pub fn mapping_family(self) -> u8 {
		match self {
			Self::Stereo => 0,
			Self::Surround51 | Self::Surround71 => 1,
		}
	}

	/// ffmpeg's `-channel_layout` token for this layout (used with `aformat`/`-ac` so
	/// libopus tags the stream with the right speaker positions). These are the standard
	/// ffmpeg channel-layout names.
	pub fn ffmpeg_layout(self) -> &'static str {
		match self {
			Self::Stereo => "stereo",
			Self::Surround51 => "5.1",
			Self::Surround71 => "7.1",
		}
	}
}

/// The user's two audio toggles. Mirrors the matching [`crate::config::Config`]
/// fields; kept as its own type so the policy logic is testable in isolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
	/// Send host audio to the client (host → client).
	pub transmit: bool,
	/// Mute the host's local speakers while streaming.
	pub mute_host: bool,
	/// The channel layout to capture + encode (stereo / 5.1 / 7.1).
	#[serde(default)]
	pub layout: ChannelLayout,
}

impl Default for AudioSettings {
	/// Sensible default: stream stereo audio, but don't touch the host's speakers.
	fn default() -> Self {
		Self {
			transmit: true,
			mute_host: false,
			layout: ChannelLayout::Stereo,
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
	/// The channel layout to capture + encode. Passed through from settings unchanged
	/// (game mode does not alter it); the spawn layer feeds it to the ffmpeg builders.
	pub layout: ChannelLayout,
}

impl AudioSettings {
	/// Resolve the effective policy. In **game mode** transmit is forced ON (the player
	/// must hear the game), but the host is **not** force-muted: silencing the host means
	/// muting the render endpoint whose WASAPI loopback we capture, and on common codecs
	/// that tap is post-mute, so a capture opened while muted streams pure silence. The
	/// `mute_host` toggle is honored as-is in both modes (the user can still mute, ideally
	/// mid-session once the capture is live). Desktop mode honors both toggles as-is.
	pub fn policy(self, game_mode: bool) -> AudioPolicy {
		AudioPolicy {
			transmit: self.transmit || game_mode,
			mute_host: self.mute_host,
			// The channel layout is independent of the game-mode override — pass it through.
			layout: self.layout,
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
	fn game_mode_forces_transmit_but_never_mutes() {
		// Desktop mode honors the toggles…
		let off = AudioSettings {
			transmit: false,
			mute_host: false,
			layout: ChannelLayout::Stereo,
		};
		assert_eq!(
			off.policy(false),
			AudioPolicy {
				transmit: false,
				mute_host: false,
				layout: ChannelLayout::Stereo,
			}
		);
		// …game mode forces transmit ON (the player must hear the game) but must NOT
		// force-mute the host: muting the captured render endpoint silences the loopback
		// stream on post-mute codecs. mute_host stays whatever the user set (here: off).
		assert_eq!(
			off.policy(true),
			AudioPolicy {
				transmit: true,
				mute_host: false,
				layout: ChannelLayout::Stereo,
			}
		);
		// A user who explicitly asked to mute keeps that wish in game mode too.
		let muted = AudioSettings {
			transmit: false,
			mute_host: true,
			layout: ChannelLayout::Stereo,
		};
		assert_eq!(
			muted.policy(true),
			AudioPolicy {
				transmit: true,
				mute_host: true,
				layout: ChannelLayout::Stereo,
			}
		);
	}

	#[test]
	fn desktop_mode_passes_settings_through() {
		let s = AudioSettings {
			transmit: true,
			mute_host: false,
			layout: ChannelLayout::Stereo,
		};
		assert_eq!(
			s.policy(false),
			AudioPolicy {
				transmit: true,
				mute_host: false,
				layout: ChannelLayout::Stereo,
			}
		);
		let s2 = AudioSettings {
			transmit: true,
			mute_host: true,
			layout: ChannelLayout::Stereo,
		};
		assert_eq!(
			s2.policy(false),
			AudioPolicy {
				transmit: true,
				mute_host: true,
				layout: ChannelLayout::Stereo,
			}
		);
	}

	#[test]
	fn default_settings_stream_without_muting() {
		assert_eq!(
			AudioSettings::default(),
			AudioSettings {
				transmit: true,
				mute_host: false,
				layout: ChannelLayout::Stereo,
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

	#[test]
	fn channel_layout_channel_counts() {
		assert_eq!(ChannelLayout::Stereo.channels(), 2);
		assert_eq!(ChannelLayout::Surround51.channels(), 6);
		assert_eq!(ChannelLayout::Surround71.channels(), 8);
		// The discriminant equals the channel count (so `as u16` is the channel count too).
		assert_eq!(ChannelLayout::Stereo as u16, 2);
		assert_eq!(ChannelLayout::Surround51 as u16, 6);
		assert_eq!(ChannelLayout::Surround71 as u16, 8);
	}

	#[test]
	fn channel_layout_opus_stream_counts_match_sunshine() {
		// Verbatim from Sunshine's low-bitrate stream_configs (audio.cpp): (streams, coupled).
		assert_eq!(ChannelLayout::Stereo.opus_streams(), (1, 1));
		assert_eq!(ChannelLayout::Surround51.opus_streams(), (4, 2));
		assert_eq!(ChannelLayout::Surround71.opus_streams(), (5, 3));
	}

	#[test]
	fn channel_layout_stream_coupled_invariant_holds() {
		// The defining Opus-multistream identity: channels == coupled*2 + (streams - coupled).
		// (Each coupled stream carries 2 channels; each remaining stream carries 1.)
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			let (streams, coupled) = layout.opus_streams();
			let uncoupled = streams - coupled;
			assert_eq!(
				coupled as u16 * 2 + uncoupled as u16,
				layout.channels(),
				"stream/coupled counts must sum to the channel count for {layout:?}"
			);
		}
	}

	#[test]
	fn channel_layout_mapping_is_identity_sized_to_channels() {
		// Sunshine's map_* arrays are already in WASAPI/PCM order → identity mapping.
		assert_eq!(ChannelLayout::Stereo.opus_mapping(), vec![0, 1]);
		assert_eq!(ChannelLayout::Surround51.opus_mapping(), vec![0, 1, 2, 3, 4, 5]);
		assert_eq!(
			ChannelLayout::Surround71.opus_mapping(),
			vec![0, 1, 2, 3, 4, 5, 6, 7]
		);
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			assert_eq!(layout.opus_mapping().len(), layout.channels() as usize);
		}
	}

	#[test]
	fn channel_layout_mapping_family_and_ffmpeg_name() {
		// Family 0 for stereo (single RTP-style stream), family 1 for multichannel.
		assert_eq!(ChannelLayout::Stereo.mapping_family(), 0);
		assert_eq!(ChannelLayout::Surround51.mapping_family(), 1);
		assert_eq!(ChannelLayout::Surround71.mapping_family(), 1);
		assert_eq!(ChannelLayout::Stereo.ffmpeg_layout(), "stereo");
		assert_eq!(ChannelLayout::Surround51.ffmpeg_layout(), "5.1");
		assert_eq!(ChannelLayout::Surround71.ffmpeg_layout(), "7.1");
	}

	#[test]
	fn channel_layout_defaults_to_stereo() {
		assert_eq!(ChannelLayout::default(), ChannelLayout::Stereo);
		assert_eq!(AudioSettings::default().layout, ChannelLayout::Stereo);
	}

	#[test]
	fn policy_passes_layout_through_in_both_modes() {
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			let s = AudioSettings {
				transmit: true,
				mute_host: false,
				layout,
			};
			// Layout is independent of the game-mode override.
			assert_eq!(s.policy(false).layout, layout);
			assert_eq!(s.policy(true).layout, layout);
		}
	}

	#[test]
	fn channel_layout_serde_round_trips_kebab_case() {
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			let j = serde_json::to_string(&layout).unwrap();
			assert_eq!(serde_json::from_str::<ChannelLayout>(&j).unwrap(), layout);
		}
		// Confirms the kebab-case wire form so the UI/config can rely on it.
		assert_eq!(
			serde_json::to_string(&ChannelLayout::Surround51).unwrap(),
			"\"surround51\""
		);
	}

	#[test]
	fn audio_settings_deserializes_without_layout_field() {
		// `#[serde(default)]` lets an older persisted config (no `layout` key) still load,
		// defaulting to stereo — so adding the field doesn't break existing user configs.
		let s: AudioSettings =
			serde_json::from_str(r#"{"transmit":true,"mute_host":false}"#).unwrap();
		assert_eq!(s.layout, ChannelLayout::Stereo);
		assert!(s.transmit);
		assert!(!s.mute_host);
	}
}
