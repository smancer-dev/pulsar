//! Pure ffmpeg command builders (argument vectors) for the host audio
//! capture+encode flow: the shared Opus/RTP output stage and the direct-capture
//! (dshow/pulse/avfoundation) command. Process spawning lives in the Tauri layer.

use super::{AudioInput, CHANNELS, SAMPLE_RATE};
// `ChannelLayout` is referenced via the sibling `settings` module directly: the
// `audio` module's re-export list (in audio.rs) is owned by the integration phase,
// so we don't depend on it re-exporting `ChannelLayout` to compile here.
use super::settings::ChannelLayout;

/// The shared ffmpeg **output** stage: encode low-latency **Opus** and packetize as
/// **RTP** to `dest` (e.g. `rtp://1.2.3.4:9100`). Used by both the direct-capture
/// [`audio_command`] (dshow/pulse/avfoundation input) and the Windows WASAPI-loopback
/// path (raw PCM piped in on stdin), so the encode settings stay identical.
///
/// This is the **stereo** convenience wrapper, preserved for the existing
/// (stereo-only) call sites. New, layout-aware call sites should use
/// [`opus_rtp_output_layout`]; this is exactly `opus_rtp_output_layout(dest,
/// ChannelLayout::Stereo)`.
pub fn opus_rtp_output(dest: &str) -> Vec<String> {
	opus_rtp_output_layout(dest, ChannelLayout::Stereo)
}

/// The shared ffmpeg **output** stage, for an arbitrary [`ChannelLayout`]: encode
/// low-latency **Opus** at the layout's channel count and packetize as **RTP** to
/// `dest`. For more than 2 channels this emits a valid **Opus multistream** (Vorbis
/// channel order) that a correctly-configured client decoder can decode.
///
/// libopus channel handling:
/// * `-ac N` sets the channel count (2 / 6 / 8).
/// * `-channel_layout <name>` tags the speaker positions (`stereo` / `5.1` / `7.1`)
///   so libopus uses the right Vorbis-order mapping.
/// * `-mapping_family 1` is **required past stereo** — it switches libopus from a
///   single-stream (family 0) to the multistream Vorbis layout. We pass it
///   explicitly for 5.1/7.1 (family 0 for stereo, the implicit default, is left
///   alone to keep the stereo bytes byte-for-byte identical to before).
///
/// The encoder derives the concrete stream/coupled counts and per-channel mapping
/// from the channel layout + family; those counts are mirrored in
/// [`ChannelLayout::opus_streams`] / [`ChannelLayout::opus_mapping`] so a future
/// native (non-ffmpeg) `opus_multistream_encoder_create` path stays in lock-step.
pub fn opus_rtp_output_layout(dest: &str, layout: ChannelLayout) -> Vec<String> {
	let s = |x: &str| x.to_string();
	let mut a: Vec<String> = vec![
		s("-ac"),
		layout.channels().to_string(),
	];
	// Past stereo, libopus needs the multistream mapping family + an explicit channel
	// layout to lay channels out in Opus/Vorbis order. Stereo keeps the implicit
	// family-0 single stream (no extra flags) so its output is unchanged.
	if layout != ChannelLayout::Stereo {
		a.extend([
			s("-mapping_family"),
			layout.mapping_family().to_string(),
			s("-channel_layout"),
			layout.ffmpeg_layout().to_string(),
		]);
	}
	a.extend([
		s("-ar"),
		SAMPLE_RATE.to_string(),
		s("-c:a"),
		s("libopus"),
		s("-b:a"),
		opus_bitrate(layout).to_string(),
		s("-application"),
		s("lowdelay"),
		// 10 ms Opus frames. 5 ms halves per-packet latency but doubled the packet rate and
		// audibly crackled here (more packets to lose/jitter, tighter timing); 10 ms is the
		// clear, stable choice and the latency win was dwarfed by the RTP reorder buffer
		// (fixed on the player input with -max_delay 0, see spawn.rs).
		s("-frame_duration"),
		s("10"),
		// Push each RTP packet out the instant it is encoded (no muxer batching) and keep
		// the muxer reorder/look-ahead at zero — audio must not sit buffered behind the
		// ultra-low-latency video. Without these ffmpeg's rtp muxer adds a fixed delay.
		s("-flush_packets"),
		s("1"),
		s("-max_delay"),
		s("0"),
		s("-f"),
		s("rtp"),
		s("-payload_type"),
		s("97"),
		dest.to_string(),
	]);
	a
}

/// The total Opus bitrate (in bits/s, as an ffmpeg `-b:a` value) for a layout. More
/// channels need more bits to stay clean; these track Sunshine's *low-bitrate*
/// (non-`HIGH_QUALITY`) `stream_configs` tier so the defaults match Moonlight:
/// stereo 128 kbps (we keep our existing value), 5.1 256 kbps, 7.1 450 kbps.
pub fn opus_bitrate(layout: ChannelLayout) -> u32 {
	match layout {
		// Preserves the prior stereo default exactly (was hard-coded "128k").
		ChannelLayout::Stereo => 128_000,
		ChannelLayout::Surround51 => 256_000,
		ChannelLayout::Surround71 => 450_000,
	}
}

/// Build the host audio capture+encode command: `(program, args)`. Captures the
/// chosen source, encodes low-latency **Opus**, and sends **RTP** to `dest`
/// (e.g. `rtp://1.2.3.4:9100`) — a second flow next to the H.264 video. Program is
/// always `ffmpeg` (the caller substitutes the bundled binary). On Windows the
/// preferred path is WASAPI loopback instead (see [`run_loopback_capture`]); this
/// dshow path is the fallback for an explicitly-named capture device.
///
/// This is the **stereo** convenience wrapper, preserved for existing call sites; it
/// is exactly `audio_command_layout(input, dest, ChannelLayout::Stereo)`.
///
/// [`run_loopback_capture`]: super::run_loopback_capture
pub fn audio_command(input: &AudioInput, dest: &str) -> (String, Vec<String>) {
	audio_command_layout(input, dest, ChannelLayout::Stereo)
}

/// Build the host audio capture+encode command for a given [`ChannelLayout`]:
/// `(program, args)`. Same as [`audio_command`] but encodes `layout`'s channel
/// count as a (multistream, past stereo) Opus stream via [`opus_rtp_output_layout`].
///
/// Note the capture source itself must actually deliver that many channels (a
/// stereo loopback device can't produce real 5.1). The builder is layout-correct;
/// matching the source's channel count is the caller's job.
pub fn audio_command_layout(
	input: &AudioInput,
	dest: &str,
	layout: ChannelLayout,
) -> (String, Vec<String>) {
	let s = |x: &str| x.to_string();
	let mut a: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error")];

	// Capture (platform-specific input).
	match input {
		AudioInput::Dshow(dev) => a.extend([s("-f"), s("dshow"), s("-i"), format!("audio={dev}")]),
		AudioInput::Pulse(src) => a.extend([s("-f"), s("pulse"), s("-i"), src.clone()]),
		AudioInput::AvFoundation(idx) => {
			a.extend([s("-f"), s("avfoundation"), s("-i"), format!(":{idx}")])
		}
	}

	// Encode Opus (multistream past stereo), low-latency, and packetize as RTP.
	a.extend(opus_rtp_output_layout(dest, layout));
	("ffmpeg".to_string(), a)
}

// Compile-time assurance the stereo wrapper still emits the historical channel count.
const _: () = assert!(CHANNELS == 2);

#[cfg(test)]
mod tests {
	use super::*;

	/// Return the value following the first occurrence of `flag` in `args`.
	fn val_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
		args.iter()
			.position(|a| a == flag)
			.and_then(|i| args.get(i + 1))
			.map(|s| s.as_str())
	}

	#[test]
	fn audio_command_encodes_opus_rtp() {
		let (prog, args) = audio_command(
			&AudioInput::Pulse("x.monitor".into()),
			"rtp://10.0.0.5:9100",
		);
		assert_eq!(prog, "ffmpeg");
		assert!(args.iter().any(|a| a == "pulse"));
		assert!(args.iter().any(|a| a == "x.monitor"));
		assert!(args.iter().any(|a| a == "libopus"));
		assert!(args.iter().any(|a| a == "rtp"));
		assert_eq!(args.last().unwrap(), "rtp://10.0.0.5:9100");
	}

	#[test]
	fn opus_rtp_output_is_the_shared_encode_stage() {
		let out = opus_rtp_output("rtp://1.2.3.4:9100");
		assert!(out.iter().any(|a| a == "libopus"));
		assert!(out.iter().any(|a| a == "rtp"));
		// Both the dshow command and the loopback path end with the same RTP destination.
		assert_eq!(out.last().unwrap(), "rtp://1.2.3.4:9100");
	}

	#[test]
	fn dshow_and_avfoundation_inputs() {
		let (_, win) = audio_command(&AudioInput::Dshow("Stereo Mix".into()), "rtp://1.2.3.4:1");
		assert!(win.iter().any(|a| a == "dshow"));
		assert!(win.iter().any(|a| a == "audio=Stereo Mix"));
		let (_, mac) = audio_command(&AudioInput::AvFoundation(2), "rtp://1.2.3.4:1");
		assert!(mac.iter().any(|a| a == "avfoundation"));
		assert!(mac.iter().any(|a| a == ":2"));
	}

	#[test]
	fn stereo_wrapper_unchanged_no_mapping_family() {
		// The stereo path must stay byte-for-byte what it was before surround support:
		// 2 channels, NO -mapping_family / -channel_layout, 128k, payload 97.
		let out = opus_rtp_output("rtp://1.2.3.4:9100");
		assert_eq!(val_after(&out, "-ac"), Some("2"));
		assert!(
			!out.iter().any(|a| a == "-mapping_family"),
			"stereo must not emit -mapping_family (keeps family-0 single stream)"
		);
		assert!(!out.iter().any(|a| a == "-channel_layout"));
		assert_eq!(val_after(&out, "-b:a"), Some("128000"));
		assert_eq!(val_after(&out, "-payload_type"), Some("97"));
		assert_eq!(val_after(&out, "-frame_duration"), Some("10"));
		// And the layout-aware builder for Stereo produces the exact same args.
		assert_eq!(
			opus_rtp_output_layout("rtp://1.2.3.4:9100", ChannelLayout::Stereo),
			out
		);
	}

	#[test]
	fn surround51_emits_six_channels_family1_and_layout() {
		let out = opus_rtp_output_layout("rtp://1.2.3.4:9100", ChannelLayout::Surround51);
		assert_eq!(val_after(&out, "-ac"), Some("6"));
		assert_eq!(val_after(&out, "-mapping_family"), Some("1"));
		assert_eq!(val_after(&out, "-channel_layout"), Some("5.1"));
		assert_eq!(val_after(&out, "-b:a"), Some("256000"));
		assert!(out.iter().any(|a| a == "libopus"));
		assert_eq!(out.last().unwrap(), "rtp://1.2.3.4:9100");
	}

	#[test]
	fn surround71_emits_eight_channels_family1_and_layout() {
		let out = opus_rtp_output_layout("rtp://1.2.3.4:9100", ChannelLayout::Surround71);
		assert_eq!(val_after(&out, "-ac"), Some("8"));
		assert_eq!(val_after(&out, "-mapping_family"), Some("1"));
		assert_eq!(val_after(&out, "-channel_layout"), Some("7.1"));
		assert_eq!(val_after(&out, "-b:a"), Some("450000"));
	}

	#[test]
	fn audio_command_layout_carries_channel_count() {
		let (_, args) =
			audio_command_layout(&AudioInput::Pulse("x.monitor".into()), "rtp://1:1", ChannelLayout::Surround71);
		// Capture input still present…
		assert!(args.iter().any(|a| a == "x.monitor"));
		// …and the encode stage is the 8-channel multistream one.
		assert_eq!(val_after(&args, "-ac"), Some("8"));
		assert_eq!(val_after(&args, "-mapping_family"), Some("1"));
	}

	#[test]
	fn opus_bitrate_tiers_match_sunshine_low_quality() {
		assert_eq!(opus_bitrate(ChannelLayout::Stereo), 128_000);
		assert_eq!(opus_bitrate(ChannelLayout::Surround51), 256_000);
		assert_eq!(opus_bitrate(ChannelLayout::Surround71), 450_000);
	}

	#[test]
	fn ac_value_always_matches_layout_channels() {
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			let out = opus_rtp_output_layout("rtp://1:1", layout);
			assert_eq!(
				val_after(&out, "-ac"),
				Some(layout.channels().to_string().as_str()),
				"-ac must equal channel count for {layout:?}"
			);
		}
	}
}
