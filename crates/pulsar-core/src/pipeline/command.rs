//! ffmpeg/ffplay command builders for the host encode + client decode paths.

use super::{CaptureMethod, HwEncoder, StreamPlan, VCodec};

/// Build the host capture+encode command: `(program, args)`. Program is always
/// `ffmpeg` (the bundled binary is substituted by the caller); the encoder is
/// selected via ffmpeg's `-c:v` flag, no external wrapper.
pub fn encode_command(plan: &StreamPlan) -> (String, Vec<String>) {
	let enc = plan.encoder.ffmpeg_name(plan.codec).unwrap_or("libx264");
	let s = |x: &str| x.to_string();
	let mut a: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error")];

	// VA-API needs the device declared before the input.
	if plan.encoder == HwEncoder::Vaapi {
		a.push(s("-vaapi_device"));
		a.push(plan.vaapi_device.clone());
	}

	// Vulkan encode needs a vulkan hwdevice declared up front (frames are uploaded to it
	// below). `vk` is the device alias used by `hwupload`.
	if plan.encoder == HwEncoder::Vulkan {
		a.extend([
			s("-init_hw_device"),
			s("vulkan=vk"),
			s("-filter_hw_device"),
			s("vk"),
		]);
	}

	// QSV on Windows/ddagrab: ddagrab creates and uses its OWN internal D3D11 device,
	// so the filter device must be that D3D11 device (`dx`), NOT the global QSV device
	// (`qs`). The downstream QSV device is derived from ddagrab's own frames by
	// `hwmap=derive_device=qsv` in the filter (see CaptureMethod::input_args), which
	// attaches a valid QSV device to the output frames for `scale_qsv`/`h264_qsv`. So
	// capture→scale→encode all stay on the Intel GPU.
	if plan.encoder == HwEncoder::Qsv && plan.capture == CaptureMethod::Ddagrab {
		a.extend([
			s("-init_hw_device"),
			s("d3d11va=dx"),
			s("-init_hw_device"),
			s("qsv=qs@dx"),
			s("-filter_hw_device"),
			s("dx"),
		]);
	}

	// Screen capture (platform-specific).
	a.extend(plan.capture.input_args(plan));

	// VA-API / Vulkan upload CPU frames into a GPU hwframe before the encoder. The upload
	// format matches the requested depth/chroma (p010 for HDR, yuv444 for 4:4:4).
	if plan.encoder == HwEncoder::Vaapi || plan.encoder == HwEncoder::Vulkan {
		let up = match (plan.yuv444, plan.hdr) {
			(true, true) => "yuv444p10le",
			(true, false) => "yuv444p",
			(false, true) => "p010le",
			(false, false) => "nv12",
		};
		a.extend([s("-vf"), format!("format={up},hwupload")]);
	}

	// GOP: game mode uses a short GOP (~0.25 s) so the picture self-heals fast after
	// loss (the UDP relay has no retransmit); desktop/quality mode uses a longer GOP
	// (~2 s) — desktop is low-motion, so fewer keyframes spends the bitrate on a
	// sharper picture instead.
	let gop = if plan.low_latency {
		(plan.fps / 4).max(1)
	} else {
		(plan.fps * 2).max(1)
	};
	a.extend([
		s("-c:v"),
		s(enc),
		s("-b:v"),
		format!("{}k", plan.bitrate_kbps),
		s("-g"),
		gop.to_string(),
	]);
	match (plan.encoder, plan.low_latency) {
		// NVENC, game: absolute lowest latency (ultra-low-latency tune, CBR, no lookahead).
		(HwEncoder::Nvenc, true) => a.extend([
			s("-preset"),
			s("p1"),
			s("-tune"),
			s("ull"),
			s("-delay"),
			s("0"),
			// CBR low-delay: spread bits evenly so an IDR doesn't burst far above the
			// target rate and overflow the receive buffers right after each keyframe.
			s("-rc"),
			s("cbr"),
			s("-rc-lookahead"),
			s("0"),
		]),
		// NVENC, desktop: lean on quality — higher preset, low-latency-high-quality
		// tune, spatial AQ for sharper text/edges.
		(HwEncoder::Nvenc, false) => a.extend([
			s("-preset"),
			s("p5"),
			s("-tune"),
			s("ll"),
			s("-rc"),
			s("vbr"),
			s("-spatial-aq"),
			s("1"),
		]),
		// Software: zerolatency both ways; ultrafast for games, veryfast (sharper) for
		// the desktop.  libsvtav1 (AV1) takes -preset as an INTEGER (0-13) and has NO
		// -tune flag — libx264/libx265 string presets cause "Unable to parse preset
		// option value ultrafast" / exit 127 → dead video.  Branch on the codec so each
		// encoder gets its own vocabulary.
		(HwEncoder::Software, true) => match plan.codec {
			VCodec::Av1 => {
				// libsvtav1 low-latency game: preset 8 (fast) + low-power thread params.
				a.extend([
					s("-preset"),
					s("8"),
					s("-svtav1-params"),
					s("lp=0:fast-decode=1"),
				])
			}
			_ => {
				// libx264 / libx265: string preset + zerolatency tune.
				a.extend([s("-preset"), s("ultrafast"), s("-tune"), s("zerolatency")])
			}
		},
		(HwEncoder::Software, false) => match plan.codec {
			VCodec::Av1 => {
				// libsvtav1 quality mode: preset 6 (balanced quality/speed), no tune.
				a.extend([s("-preset"), s("6")])
			}
			_ => {
				// libx264 / libx265: string preset + zerolatency tune.
				a.extend([s("-preset"), s("veryfast"), s("-tune"), s("zerolatency")])
			}
		},
		(HwEncoder::Qsv, true) => a.extend([s("-preset"), s("veryfast"), s("-low_power"), s("1")]),
		(HwEncoder::Qsv, false) => a.extend([s("-preset"), s("medium")]),
		// AMD AMF: low-latency CBR for games, quality "transcoding" for desktop.
		(HwEncoder::Amf, true) => a.extend([s("-usage"), s("lowlatency"), s("-rc"), s("cbr")]),
		(HwEncoder::Amf, false) => a.extend([s("-usage"), s("transcoding")]),
		// VA-API (Intel/AMD on Linux): CBR + low-delay-B disabled for games (no B-frames →
		// no reorder latency); desktop leans on the default rate control for a sharper frame.
		(HwEncoder::Vaapi, true) => a.extend([
			s("-rc_mode"),
			s("CBR"),
			s("-bf"),
			s("0"),
			s("-async_depth"),
			s("1"),
		]),
		(HwEncoder::Vaapi, false) => a.extend([s("-rc_mode"), s("VBR"), s("-bf"), s("0")]),
		// Apple VideoToolbox: realtime + no frame reordering for low latency; desktop allows
		// the encoder its default quality path. `-realtime 1` is the VT low-latency switch.
		(HwEncoder::VideoToolbox, true) => {
			a.extend([s("-realtime"), s("1"), s("-prio_speed"), s("1")])
		}
		(HwEncoder::VideoToolbox, false) => a.extend([s("-realtime"), s("1")]),
		// Vulkan video encode: low-power-ish defaults; no B-frames for low latency. The
		// frames must already be on a Vulkan hwframe (uploaded below).
		(HwEncoder::Vulkan, true) => a.extend([s("-bf"), s("0"), s("-async_depth"), s("1")]),
		(HwEncoder::Vulkan, false) => a.extend([s("-bf"), s("0")]),
		// Windows Media Foundation (Qualcomm/ARM): low-latency hardware mode for games.
		(HwEncoder::MediaFoundation, true) => {
			a.extend([s("-hw_encoding"), s("1"), s("-rate_control"), s("cbr")])
		}
		(HwEncoder::MediaFoundation, false) => a.extend([s("-hw_encoding"), s("1")]),
		_ => {}
	}
	// HDR (10-bit) / YUV444: pick the encode profile, pixel format, and HDR colorspace.
	// For CPU-fed encoders (software / x11grab / the ddagrab CPU-bounce branches) swscale
	// honors `-pix_fmt` directly; the ddagrab filter also emits the matching format so the
	// frame reaches the encoder in the requested depth/chroma (see CaptureMethod::input_args).
	//
	// Use effective_hdr_yuv444() — NOT plan.hdr/plan.yuv444 directly — so that GPU-filter
	// paths (NVENC/QSV ddagrab) that are clamped to SDR 4:2:0 in the filter are also clamped
	// here.  The filter and -pix_fmt MUST derive from the same source; this call is that source.
	let (eff_hdr, eff_yuv444) = plan.effective_hdr_yuv444();
	if eff_hdr || eff_yuv444 {
		// Profile bumps required for 10-bit / 4:4:4. AV1 `main` already spans 8/10-bit 4:2:0.
		let profile = match (plan.codec, eff_hdr, eff_yuv444) {
			(VCodec::H265, _, true) => Some("rext"), // HEVC Range Extensions = 4:4:4
			(VCodec::H265, true, false) => Some("main10"),
			(VCodec::H264, _, true) => Some("high444p"),
			(VCodec::H264, true, false) => Some("high10"),
			_ => None,
		};
		if let Some(p) = profile {
			a.extend([s("-profile:v"), s(p)]);
		}
		let pix = match (eff_yuv444, eff_hdr) {
			(true, true) => "yuv444p10le",
			(true, false) => "yuv444p",
			(false, true) => "p010le",
			(false, false) => "nv12",
		};
		a.extend([s("-pix_fmt"), s(pix)]);
		// HDR10 signaling: BT.2020 primaries + SMPTE-2084 (PQ) transfer + non-constant
		// luminance matrix. The client carries these through to its display.
		if eff_hdr {
			a.extend([
				s("-color_primaries"),
				s("bt2020"),
				s("-color_trc"),
				s("smpte2084"),
				s("-colorspace"),
				s("bt2020nc"),
			]);
		}
	} else if matches!(plan.encoder, HwEncoder::Software) {
		// SDR software encode: PIN 4:2:0. Without it ffmpeg matches the capture's BGR frames
		// to the encoder's "closest" format — libx264 then silently produces High 4:4:4
		// (yuv444p), which hardware decoders (rkmpp) and 4:2:0-only client paths can't play
		// (the "stream runs, screen stays black" failure). HW encoders keep their native
		// formats (nv12/hwframes) via their own format= chains.
		a.extend([s("-pix_fmt"), s("yuv420p")]);
	}

	// RTP/H.264 so the client can depacketize and feed WebCodecs in the webview.
	// `dump_extra` re-inserts SPS/PPS so a mid-stream join still gets a keyframe.
	a.extend([
		s("-bsf:v"),
		s("dump_extra"),
		s("-f"),
		s("rtp"),
		s("-payload_type"),
		s("96"),
		plan.dest.clone(),
	]);

	// Always invoke ffmpeg directly. NVENC works without any wrapper; if a user
	// on a hybrid-GPU Linux box needs PRIME/Optimus offload, that's their call —
	// they launch Pulsar itself under `prime-run` and ffmpeg inherits it.
	("ffmpeg".to_string(), a)
}

/// Build a **validation-probe** command (Sunshine technique): encode ONE frame of a
/// synthetic `testsrc` with the given encoder/codec to `null`. Exit 0 ⇒ the encoder is not
/// just listed by ffmpeg but actually initializes on THIS machine's driver/GPU (catches the
/// "h264_qsv is listed but no Intel GPU present" and "av1_nvenc on an Ampere card" cases that
/// name-presence detection misses). Returns `(program, args)`; `None` if the codec is
/// unsupported by the encoder at all. `vaapi_device` is only used for VA-API.
pub fn probe_command(
	encoder: HwEncoder,
	codec: VCodec,
	vaapi_device: &str,
) -> Option<(String, Vec<String>)> {
	let name = encoder.ffmpeg_name(codec)?;
	let s = |x: &str| x.to_string();
	let mut a: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error")];

	// Per-encoder hw-device init (mirrors encode_command), so the probe exercises the real
	// upload+encode path, not a CPU shortcut that would falsely pass.
	match encoder {
		HwEncoder::Vaapi => a.extend([s("-vaapi_device"), s(vaapi_device)]),
		HwEncoder::Vulkan => a.extend([
			s("-init_hw_device"),
			s("vulkan=vk"),
			s("-filter_hw_device"),
			s("vk"),
		]),
		HwEncoder::Qsv => a.extend([
			s("-init_hw_device"),
			s("qsv=qs"),
			s("-filter_hw_device"),
			s("qs"),
		]),
		_ => {}
	}

	// One synthetic 320x240 frame.
	a.extend([
		s("-f"),
		s("lavfi"),
		s("-i"),
		s("testsrc=size=320x240:rate=30"),
		s("-frames:v"),
		s("1"),
	]);

	// Upload to the GPU where the encoder needs a hwframe.
	match encoder {
		HwEncoder::Vaapi | HwEncoder::Vulkan => a.extend([s("-vf"), s("format=nv12,hwupload")]),
		HwEncoder::Qsv => a.extend([s("-vf"), s("format=nv12,hwupload=extra_hw_frames=4")]),
		_ => {}
	}

	a.extend([s("-c:v"), s(name)]);

	// Mirror the encoder-specific options from encode_command so the probe exercises the
	// same init path the real spawn uses.  Without this, a Software+AV1 probe passes
	// (no options → libsvtav1 defaults are fine) while the real encode_command would have
	// previously failed with an x264-style string preset.  Now that encode_command emits
	// the correct integer preset for libsvtav1, the probe must match.
	if encoder == HwEncoder::Software && codec == VCodec::Av1 {
		// Use the low-latency preset for the probe (fastest init; same preset string the
		// real encode uses so the probe catches any preset-parse failure).
		a.extend([s("-preset"), s("8"), s("-svtav1-params"), s("lp=0:fast-decode=1")]);
	}

	// AV1: probe through the REAL RTP muxer, not just `-f null`. Current ffmpeg gates AV1
	// packetization behind `-strict experimental` (header write fails outright), and the
	// Software arms emit x264-style presets libsvtav1 rejects — so a null-mux probe passes
	// for a stream `encode_command` then can't start ("ffmpeg başlamadı", dead video). With
	// the muxer in the probe, validation fails on such builds and `resolve_codec_validated`
	// degrades AV1 to HEVC/H.264 instead; a future build where AV1-over-RTP works passes the
	// probe and re-enables it with no code change. One frame to the loopback discard port.
	if codec == VCodec::Av1 {
		a.extend([
			s("-f"),
			s("rtp"),
			s("rtp://127.0.0.1:9"),
			s("-frames:v"),
			s("1"),
		]);
	}

	a.extend([s("-f"), s("null"), s("-")]);
	Some(("ffmpeg".to_string(), a))
}

/// Build the client decode+display command (`ffplay`) reading from `listen`,
/// e.g. `udp://@:9000`.
pub fn decode_command(listen: &str) -> (String, Vec<String>) {
	let s = |x: &str| x.to_string();
	(
		"ffplay".to_string(),
		vec![
			s("-hide_banner"),
			s("-loglevel"),
			s("error"),
			s("-fflags"),
			s("nobuffer"),
			s("-flags"),
			s("low_delay"),
			s("-framedrop"),
			s("-probesize"),
			s("32"),
			s("-analyzeduration"),
			s("0"),
			s("-i"),
			s(listen),
		],
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Minimal StreamPlan for unit tests.  Only the fields that encode_command reads
	/// need to be filled in; the rest are inert defaults.
	fn sw_av1_plan(low_latency: bool) -> StreamPlan {
		StreamPlan {
			encoder: HwEncoder::Software,
			codec: VCodec::Av1,
			width: 1920,
			height: 1080,
			fps: 60,
			bitrate_kbps: 8000,
			capture: CaptureMethod::X11grab,
			display: ":0.0".to_string(),
			vaapi_device: String::new(),
			dest: "rtp://127.0.0.1:9000".to_string(),
			low_latency,
			gpu_zerocopy: false,
			output_idx: 0,
			hdr: false,
			yuv444: false,
		}
	}

	/// [sw-av1-preset-probe-divergence] Software+AV1 must NOT emit libx264/libx265
	/// preset vocabulary — libsvtav1 rejects the string preset with exit 127 ("Unable
	/// to parse preset option value ultrafast").  It must also NOT emit "-tune" (which
	/// libsvtav1 does not accept), and MUST include an integer -preset.
	#[test]
	fn software_av1_low_latency_uses_integer_preset() {
		let (_prog, args) = encode_command(&sw_av1_plan(true));
		let args_str = args.join(" ");
		// Must not contain x264/x265 string presets.
		assert!(
			!args_str.contains("ultrafast"),
			"Software+AV1 low_latency must not emit 'ultrafast': {args_str}"
		);
		assert!(
			!args_str.contains("veryfast"),
			"Software+AV1 low_latency must not emit 'veryfast': {args_str}"
		);
		// Must not contain the zerolatency tune flag (not valid for libsvtav1).
		assert!(
			!args_str.contains("zerolatency"),
			"Software+AV1 low_latency must not emit 'zerolatency': {args_str}"
		);
		// Must contain an integer -preset value (specifically "8" for low_latency).
		let preset_idx = args.iter().position(|a| a == "-preset");
		assert!(
			preset_idx.is_some(),
			"Software+AV1 low_latency must emit -preset: {args_str}"
		);
		let preset_val = &args[preset_idx.unwrap() + 1];
		assert!(
			preset_val.parse::<u32>().is_ok(),
			"Software+AV1 low_latency -preset value must be an integer, got '{preset_val}'"
		);
		assert_eq!(preset_val, "8", "Software+AV1 low_latency preset should be 8");
		// Must include the svtav1 low-latency params.
		assert!(
			args_str.contains("svtav1-params"),
			"Software+AV1 low_latency must emit -svtav1-params: {args_str}"
		);
	}

	#[test]
	fn software_av1_quality_uses_integer_preset() {
		let (_prog, args) = encode_command(&sw_av1_plan(false));
		let args_str = args.join(" ");
		assert!(
			!args_str.contains("ultrafast"),
			"Software+AV1 quality must not emit 'ultrafast': {args_str}"
		);
		assert!(
			!args_str.contains("veryfast"),
			"Software+AV1 quality must not emit 'veryfast': {args_str}"
		);
		assert!(
			!args_str.contains("zerolatency"),
			"Software+AV1 quality must not emit 'zerolatency': {args_str}"
		);
		let preset_idx = args.iter().position(|a| a == "-preset");
		assert!(
			preset_idx.is_some(),
			"Software+AV1 quality must emit -preset: {args_str}"
		);
		let preset_val = &args[preset_idx.unwrap() + 1];
		assert!(
			preset_val.parse::<u32>().is_ok(),
			"Software+AV1 quality -preset value must be an integer, got '{preset_val}'"
		);
		assert_eq!(preset_val, "6", "Software+AV1 quality preset should be 6");
	}

	/// Ensure the H.264 software path is NOT broken by the AV1 branch — it must still
	/// emit the string preset and zerolatency tune.
	#[test]
	fn software_h264_still_uses_string_preset() {
		let plan = StreamPlan {
			codec: VCodec::H264,
			..sw_av1_plan(true)
		};
		let (_prog, args) = encode_command(&plan);
		let args_str = args.join(" ");
		assert!(
			args_str.contains("ultrafast"),
			"Software+H264 low_latency must still emit 'ultrafast': {args_str}"
		);
		assert!(
			args_str.contains("zerolatency"),
			"Software+H264 low_latency must still emit 'zerolatency': {args_str}"
		);
	}
}
