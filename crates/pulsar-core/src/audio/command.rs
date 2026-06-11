//! Pure ffmpeg command builders (argument vectors) for the host audio
//! capture+encode flow: the shared Opus/RTP output stage and the direct-capture
//! (dshow/pulse/avfoundation) command. Process spawning lives in the Tauri layer.

use super::{AudioInput, CHANNELS, SAMPLE_RATE};

/// The shared ffmpeg **output** stage: encode low-latency **Opus** and packetize as
/// **RTP** to `dest` (e.g. `rtp://1.2.3.4:9100`). Used by both the direct-capture
/// [`audio_command`] (dshow/pulse/avfoundation input) and the Windows WASAPI-loopback
/// path (raw PCM piped in on stdin), so the encode settings stay identical.
pub fn opus_rtp_output(dest: &str) -> Vec<String> {
	let s = |x: &str| x.to_string();
	vec![
		s("-ac"),
		CHANNELS.to_string(),
		s("-ar"),
		SAMPLE_RATE.to_string(),
		s("-c:a"),
		s("libopus"),
		s("-b:a"),
		s("128k"),
		// 10 ms frames + low expected packet loss keeps latency down on a LAN/relay.
		s("-application"),
		s("lowdelay"),
		s("-frame_duration"),
		s("10"),
		s("-f"),
		s("rtp"),
		s("-payload_type"),
		s("97"),
		dest.to_string(),
	]
}

/// Build the host audio capture+encode command: `(program, args)`. Captures the
/// chosen source, encodes low-latency **Opus**, and sends **RTP** to `dest`
/// (e.g. `rtp://1.2.3.4:9100`) — a second flow next to the H.264 video. Program is
/// always `ffmpeg` (the caller substitutes the bundled binary). On Windows the
/// preferred path is WASAPI loopback instead (see [`run_loopback_capture`]); this
/// dshow path is the fallback for an explicitly-named capture device.
///
/// [`run_loopback_capture`]: super::run_loopback_capture
pub fn audio_command(input: &AudioInput, dest: &str) -> (String, Vec<String>) {
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

	// Encode Opus, low-latency, and packetize as RTP for the client.
	a.extend(opus_rtp_output(dest));
	("ffmpeg".to_string(), a)
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
