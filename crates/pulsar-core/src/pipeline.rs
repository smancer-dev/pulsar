//! Real-time video pipeline driven by the system **ffmpeg/ffplay**.
//!
//! This is the concrete encode/decode the design's "Donanımsal kodlama
//! (NVENC / QuickSync / VideoToolbox)" refers to. It selects a hardware encoder
//! — **NVENC via `prime-run`** on hybrid-GPU Linux, **VAAPI** for an iGPU/AMD,
//! **QuickSync**, **VideoToolbox** on macOS, or a software fallback — captures
//! the screen, encodes low-latency, and ships MPEG-TS to the peer; the client
//! decodes + displays with ffplay.
//!
//! Everything here is pure (builds argument vectors / parses `ffmpeg -encoders`)
//! so it's unit-tested; the actual process spawning lives in the Tauri layer.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HwEncoder {
	/// Pick the best available at runtime.
	Auto,
	/// NVIDIA NVENC (needs `prime-run` on PRIME/Optimus laptops).
	Nvenc,
	/// VA-API (Intel iGPU / AMD on Linux).
	Vaapi,
	/// Intel QuickSync.
	Qsv,
	/// Apple VideoToolbox (macOS).
	VideoToolbox,
	/// libx264/libx265/libsvtav1 on the CPU.
	Software,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VCodec {
	H264,
	H265,
	Av1,
}

/// Per-platform screen capture backend for ffmpeg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMethod {
	/// Linux X11 / XWayland.
	X11grab,
	/// Linux DRM/KMS (native Wayland; needs cap_sys_admin on ffmpeg).
	Kmsgrab,
	/// Windows GDI desktop grab.
	Gdigrab,
	/// macOS AVFoundation screen capture.
	AvFoundation,
}

impl CaptureMethod {
	/// The right default for the platform we're built for.
	pub fn default_for_os() -> Self {
		if cfg!(target_os = "windows") {
			Self::Gdigrab
		} else if cfg!(target_os = "macos") {
			Self::AvFoundation
		} else {
			Self::X11grab
		}
	}

	/// ffmpeg input args for this capture method.
	fn input_args(self, plan: &StreamPlan) -> Vec<String> {
		let s = |x: &str| x.to_string();
		let fps = plan.fps.to_string();
		let size = format!("{}x{}", plan.width, plan.height);
		match self {
			Self::X11grab => vec![
				s("-f"), s("x11grab"), s("-framerate"), fps, s("-video_size"), size, s("-i"),
				plan.display.clone(),
			],
			Self::Kmsgrab => vec![s("-f"), s("kmsgrab"), s("-framerate"), fps, s("-i"), s("-")],
			Self::Gdigrab => vec![
				s("-f"), s("gdigrab"), s("-framerate"), fps, s("-video_size"), size, s("-i"),
				s("desktop"),
			],
			Self::AvFoundation => vec![
				s("-f"), s("avfoundation"), s("-framerate"), fps, s("-capture_cursor"), s("1"),
				s("-i"), format!("{}:none", plan.display),
			],
		}
	}
}

impl HwEncoder {
	/// The ffmpeg encoder name for a codec, or `None` if unsupported by this hw.
	pub fn ffmpeg_name(self, codec: VCodec) -> Option<&'static str> {
		Some(match (self, codec) {
			(Self::Nvenc, VCodec::H264) => "h264_nvenc",
			(Self::Nvenc, VCodec::H265) => "hevc_nvenc",
			(Self::Nvenc, VCodec::Av1) => "av1_nvenc",
			(Self::Vaapi, VCodec::H264) => "h264_vaapi",
			(Self::Vaapi, VCodec::H265) => "hevc_vaapi",
			(Self::Vaapi, VCodec::Av1) => "av1_vaapi",
			(Self::Qsv, VCodec::H264) => "h264_qsv",
			(Self::Qsv, VCodec::H265) => "hevc_qsv",
			(Self::Qsv, VCodec::Av1) => "av1_qsv",
			(Self::VideoToolbox, VCodec::H264) => "h264_videotoolbox",
			(Self::VideoToolbox, VCodec::H265) => "hevc_videotoolbox",
			(Self::Software, VCodec::H264) => "libx264",
			(Self::Software, VCodec::H265) => "libx265",
			(Self::Software, VCodec::Av1) => "libsvtav1",
			(Self::VideoToolbox, VCodec::Av1) | (Self::Auto, _) => return None,
		})
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Auto => "Otomatik",
			Self::Nvenc => "NVIDIA NVENC",
			Self::Vaapi => "VA-API",
			Self::Qsv => "Intel QuickSync",
			Self::VideoToolbox => "Apple VideoToolbox",
			Self::Software => "Yazılım (CPU)",
		}
	}

	/// NVENC on a hybrid-GPU Linux box needs the NVIDIA offload wrapper.
	pub fn needs_prime(self) -> bool {
		matches!(self, Self::Nvenc)
	}
}

/// Parse `ffmpeg -hide_banner -encoders` output into the hw encoders available.
pub fn detect(ffmpeg_encoders_output: &str) -> Vec<HwEncoder> {
	let has = |n: &str| ffmpeg_encoders_output.contains(n);
	let mut out = Vec::new();
	if has("h264_nvenc") || has("hevc_nvenc") {
		out.push(HwEncoder::Nvenc);
	}
	if has("h264_vaapi") {
		out.push(HwEncoder::Vaapi);
	}
	if has("h264_qsv") {
		out.push(HwEncoder::Qsv);
	}
	if has("h264_videotoolbox") {
		out.push(HwEncoder::VideoToolbox);
	}
	if has("libx264") {
		out.push(HwEncoder::Software);
	}
	out
}

/// Resolve a (possibly `Auto`/unavailable) choice to a concrete encoder, in
/// quality order, falling back to software.
pub fn resolve(choice: HwEncoder, available: &[HwEncoder]) -> HwEncoder {
	if choice != HwEncoder::Auto && available.contains(&choice) {
		return choice;
	}
	for c in [
		HwEncoder::Nvenc,
		HwEncoder::Vaapi,
		HwEncoder::Qsv,
		HwEncoder::VideoToolbox,
		HwEncoder::Software,
	] {
		if available.contains(&c) {
			return c;
		}
	}
	HwEncoder::Software
}

/// What/where to capture and encode.
#[derive(Clone, Debug)]
pub struct StreamPlan {
	pub encoder: HwEncoder,
	pub codec: VCodec,
	pub width: u32,
	pub height: u32,
	pub fps: u32,
	pub bitrate_kbps: u32,
	/// Screen capture backend (platform-dependent).
	pub capture: CaptureMethod,
	/// X11 display / macOS capture device index, e.g. `:0.0` or `1`.
	pub display: String,
	/// VA-API render node, e.g. `/dev/dri/renderD128`.
	pub vaapi_device: String,
	/// Destination URL, e.g. `rtp://1.2.3.4:9000` (RTP/H.264 for the WebCodecs client).
	pub dest: String,
}

/// Build the host capture+encode command: `(program, args)`. Program is
/// `prime-run` (with `ffmpeg` as first arg) for NVENC, otherwise `ffmpeg`.
pub fn encode_command(plan: &StreamPlan) -> (String, Vec<String>) {
	let enc = plan.encoder.ffmpeg_name(plan.codec).unwrap_or("libx264");
	let s = |x: &str| x.to_string();
	let mut a: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error")];

	// VA-API needs the device declared before the input.
	if plan.encoder == HwEncoder::Vaapi {
		a.push(s("-vaapi_device"));
		a.push(plan.vaapi_device.clone());
	}

	// Screen capture (platform-specific).
	a.extend(plan.capture.input_args(plan));

	// VA-API uploads frames to the GPU before encoding.
	if plan.encoder == HwEncoder::Vaapi {
		a.extend([s("-vf"), s("format=nv12,hwupload")]);
	}

	// Encode, low-latency.
	a.extend([
		s("-c:v"),
		s(enc),
		s("-b:v"),
		format!("{}k", plan.bitrate_kbps),
		s("-g"),
		(plan.fps * 2).to_string(),
	]);
	match plan.encoder {
		HwEncoder::Nvenc => a.extend([s("-preset"), s("p1"), s("-tune"), s("ull"), s("-delay"), s("0")]),
		HwEncoder::Software => a.extend([s("-preset"), s("ultrafast"), s("-tune"), s("zerolatency")]),
		HwEncoder::Qsv => a.extend([s("-preset"), s("veryfast"), s("-low_power"), s("1")]),
		_ => {}
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

	if plan.encoder.needs_prime() {
		let mut args = vec![s("ffmpeg")];
		args.extend(a);
		("prime-run".to_string(), args)
	} else {
		("ffmpeg".to_string(), a)
	}
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

	fn plan(enc: HwEncoder) -> StreamPlan {
		StreamPlan {
			encoder: enc,
			codec: VCodec::H264,
			width: 1920,
			height: 1080,
			fps: 60,
			bitrate_kbps: 30_000,
			capture: CaptureMethod::X11grab,
			display: ":0.0".into(),
			vaapi_device: "/dev/dri/renderD128".into(),
			dest: "rtp://10.0.0.5:9000".into(),
		}
	}

	#[test]
	fn capture_backends_emit_the_right_input() {
		let mut p = plan(HwEncoder::Software);
		p.capture = CaptureMethod::X11grab;
		assert!(encode_command(&p).1.iter().any(|a| a == "x11grab"));
		p.capture = CaptureMethod::Gdigrab;
		let (_, args) = encode_command(&p);
		assert!(args.iter().any(|a| a == "gdigrab"));
		assert!(args.iter().any(|a| a == "desktop"));
		p.capture = CaptureMethod::AvFoundation;
		assert!(encode_command(&p).1.iter().any(|a| a == "avfoundation"));
		p.capture = CaptureMethod::Kmsgrab;
		assert!(encode_command(&p).1.iter().any(|a| a == "kmsgrab"));
	}

	#[test]
	fn encoder_names_per_codec() {
		assert_eq!(HwEncoder::Nvenc.ffmpeg_name(VCodec::H265), Some("hevc_nvenc"));
		assert_eq!(HwEncoder::Vaapi.ffmpeg_name(VCodec::H264), Some("h264_vaapi"));
		assert_eq!(HwEncoder::Software.ffmpeg_name(VCodec::Av1), Some("libsvtav1"));
		assert_eq!(HwEncoder::Auto.ffmpeg_name(VCodec::H264), None);
	}

	#[test]
	fn detect_parses_encoder_list() {
		let out = " V..... h264_nvenc\n V..... h264_vaapi\n V..... libx264\n";
		let got = detect(out);
		assert!(got.contains(&HwEncoder::Nvenc));
		assert!(got.contains(&HwEncoder::Vaapi));
		assert!(got.contains(&HwEncoder::Software));
		assert!(!got.contains(&HwEncoder::Qsv));
	}

	#[test]
	fn resolve_prefers_best_then_falls_back() {
		let avail = [HwEncoder::Vaapi, HwEncoder::Software];
		// manual choice honored when available
		assert_eq!(resolve(HwEncoder::Vaapi, &avail), HwEncoder::Vaapi);
		// unavailable manual choice → best available
		assert_eq!(resolve(HwEncoder::Nvenc, &avail), HwEncoder::Vaapi);
		// auto → best available
		assert_eq!(resolve(HwEncoder::Auto, &avail), HwEncoder::Vaapi);
		// nothing → software
		assert_eq!(resolve(HwEncoder::Auto, &[]), HwEncoder::Software);
	}

	#[test]
	fn nvenc_command_uses_prime_run_and_encoder() {
		let (program, args) = encode_command(&plan(HwEncoder::Nvenc));
		assert_eq!(program, "prime-run");
		assert_eq!(args[0], "ffmpeg");
		assert!(args.iter().any(|a| a == "h264_nvenc"));
		assert!(args.iter().any(|a| a == "x11grab"));
		assert!(args.iter().any(|a| a == "ull")); // low-latency tune
		assert!(args.iter().any(|a| a == "rtp")); // RTP output for WebCodecs
		assert!(args.last().unwrap().starts_with("rtp://"));
	}

	#[test]
	fn vaapi_command_sets_device_and_upload() {
		let (program, args) = encode_command(&plan(HwEncoder::Vaapi));
		assert_eq!(program, "ffmpeg");
		assert!(args.iter().any(|a| a == "-vaapi_device"));
		assert!(args.iter().any(|a| a == "/dev/dri/renderD128"));
		assert!(args.iter().any(|a| a == "format=nv12,hwupload"));
		assert!(args.iter().any(|a| a == "h264_vaapi"));
	}

	#[test]
	fn software_command_is_zerolatency() {
		let (program, args) = encode_command(&plan(HwEncoder::Software));
		assert_eq!(program, "ffmpeg");
		assert!(args.iter().any(|a| a == "libx264"));
		assert!(args.iter().any(|a| a == "zerolatency"));
	}

	#[test]
	fn decode_command_is_low_latency_ffplay() {
		let (program, args) = decode_command("udp://@:9000");
		assert_eq!(program, "ffplay");
		assert!(args.iter().any(|a| a == "nobuffer"));
		assert!(args.iter().any(|a| a == "low_delay"));
		assert_eq!(args.last().unwrap(), "udp://@:9000");
	}
}
