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
		low_latency: true,
		gpu_zerocopy: false,
		hdr: false,
		yuv444: false,
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
	// DXGI Desktop Duplication is a filter source, not a `-f`/`-i` input.
	p.capture = CaptureMethod::Ddagrab;
	let (_, args) = encode_command(&p);
	assert!(args.iter().any(|a| a == "-filter_complex"));
	assert!(args.iter().any(|a| a.contains("ddagrab")));
	assert!(!args.iter().any(|a| a == "-i")); // no input file/device for ddagrab
}

#[test]
fn windows_default_capture_is_dxgi() {
	// Windows hosts default to DXGI Desktop Duplication, not legacy gdigrab.
	if cfg!(target_os = "windows") {
		assert_eq!(CaptureMethod::default_for_os(), CaptureMethod::Ddagrab);
	}
}

#[test]
fn encoder_names_per_codec() {
	assert_eq!(
		HwEncoder::Nvenc.ffmpeg_name(VCodec::H265),
		Some("hevc_nvenc")
	);
	assert_eq!(
		HwEncoder::Vaapi.ffmpeg_name(VCodec::H264),
		Some("h264_vaapi")
	);
	assert_eq!(
		HwEncoder::Software.ffmpeg_name(VCodec::Av1),
		Some("libsvtav1")
	);
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
fn detect_registers_vendor_on_any_codec_not_just_h264() {
	// An HEVC/AV1-only build (no h264_amf) must still register AMF.
	let out = " V..... hevc_amf\n V..... av1_qsv\n V..... libx265\n";
	let got = detect(out);
	assert!(got.contains(&HwEncoder::Amf), "hevc_amf alone must register AMF");
	assert!(got.contains(&HwEncoder::Qsv), "av1_qsv alone must register QSV");
	assert!(got.contains(&HwEncoder::Software), "libx265 alone must register Software");
}

#[test]
fn available_codecs_intersects_with_ffmpeg() {
	let out = " V..... h264_nvenc\n V..... av1_nvenc\n"; // no hevc_nvenc
	let c = HwEncoder::Nvenc.available_codecs(out);
	assert!(c.contains(&VCodec::H264) && c.contains(&VCodec::Av1));
	assert!(!c.contains(&VCodec::H265), "missing hevc_nvenc must not be advertised");
	// VideoToolbox never advertises AV1 even if the name somehow appears.
	assert!(!HwEncoder::VideoToolbox.all_codecs().contains(&VCodec::Av1));
}

#[test]
fn resolve_codec_honors_then_falls_back() {
	let full = " h264_nvenc hevc_nvenc av1_nvenc ";
	// requested available → honored
	assert_eq!(resolve_codec(HwEncoder::Nvenc, VCodec::Av1, full), VCodec::Av1);
	// requested missing → prefer H.264 (webview-decodable)
	let only_h264 = " h264_nvenc ";
	assert_eq!(resolve_codec(HwEncoder::Nvenc, VCodec::H265, only_h264), VCodec::H264);
	// no H.264 but HEVC present → first available
	let only_hevc = " hevc_nvenc ";
	assert_eq!(resolve_codec(HwEncoder::Nvenc, VCodec::Av1, only_hevc), VCodec::H265);
}

#[test]
fn hdr_and_yuv444_set_pixfmt_profile_colorspace() {
	// HDR HEVC: p010 + main10 + BT2020/PQ.
	let mut p = plan(HwEncoder::Nvenc);
	p.codec = VCodec::H265;
	p.hdr = true;
	let j = encode_command(&p).1.join(" ");
	assert!(j.contains("-pix_fmt p010le"));
	assert!(j.contains("-profile:v main10"));
	assert!(j.contains("smpte2084") && j.contains("bt2020"));
	// YUV444 H.264: high444p + yuv444p, no HDR colorspace.
	let mut q = plan(HwEncoder::Software);
	q.codec = VCodec::H264;
	q.yuv444 = true;
	let j = encode_command(&q).1.join(" ");
	assert!(j.contains("-profile:v high444p"));
	assert!(j.contains("-pix_fmt yuv444p"));
	assert!(!j.contains("smpte2084"));
	// SDR 4:2:0 path unchanged (no pix_fmt override).
	let j = encode_command(&plan(HwEncoder::Nvenc)).1.join(" ");
	assert!(!j.contains("-pix_fmt"));
}

#[test]
fn probe_command_is_one_frame_to_null() {
	let (prog, args) = probe_command(HwEncoder::Nvenc, VCodec::Av1, "/dev/dri/renderD128").unwrap();
	assert_eq!(prog, "ffmpeg");
	let j = args.join(" ");
	assert!(j.contains("testsrc"), "uses a synthetic source");
	assert!(j.contains("-frames:v 1"), "exactly one frame");
	assert!(j.contains("av1_nvenc"));
	assert!(j.ends_with("null -"), "discards output");
	// VA-API probe declares the device + uploads.
	let (_, va) = probe_command(HwEncoder::Vaapi, VCodec::H265, "/dev/dri/renderD128").unwrap();
	let j = va.join(" ");
	assert!(j.contains("-vaapi_device /dev/dri/renderD128"));
	assert!(j.contains("hwupload"));
	// Unsupported combo → None.
	assert!(probe_command(HwEncoder::VideoToolbox, VCodec::Av1, "").is_none());
}

#[test]
fn new_backends_have_names_and_labels() {
	assert_eq!(HwEncoder::Vulkan.ffmpeg_name(VCodec::Av1), Some("av1_vulkan"));
	assert_eq!(HwEncoder::MediaFoundation.ffmpeg_name(VCodec::H265), Some("hevc_mf"));
	assert_eq!(HwEncoder::Vulkan.label(), "Vulkan");
	// Vulkan declares all three codecs.
	assert_eq!(HwEncoder::Vulkan.all_codecs().len(), 3);
	// detect registers them from their listed names.
	let got = detect(" h264_vulkan hevc_mf libx264 ");
	assert!(got.contains(&HwEncoder::Vulkan));
	assert!(got.contains(&HwEncoder::MediaFoundation));
}

#[test]
fn vaapi_and_videotoolbox_have_tune_args() {
	let mut v = plan(HwEncoder::Vaapi);
	v.low_latency = true;
	let args = encode_command(&v).1.join(" ");
	assert!(args.contains("-rc_mode CBR"), "vaapi game mode = CBR");
	let mut t = plan(HwEncoder::VideoToolbox);
	t.low_latency = true;
	assert!(encode_command(&t).1.iter().any(|a| a == "-realtime"));
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
fn nvenc_command_invokes_ffmpeg_directly_with_encoder() {
	let (program, args) = encode_command(&plan(HwEncoder::Nvenc));
	assert_eq!(program, "ffmpeg"); // no prime-run / wrapper — ffmpeg directly
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
fn profile_picks_latency_vs_quality_params() {
	// Game (low-latency): ultrafast + short GOP.
	let mut p = plan(HwEncoder::Software);
	p.low_latency = true;
	p.fps = 60;
	let (_, game) = encode_command(&p);
	assert!(game.iter().any(|a| a == "ultrafast"));
	// GOP ~ fps/4 = 15 follows the -g flag.
	let gi = game.iter().position(|a| a == "-g").unwrap();
	assert_eq!(game[gi + 1], "15");

	// Desktop (quality): veryfast + a longer GOP (fps*2 = 120).
	p.low_latency = false;
	let (_, desk) = encode_command(&p);
	assert!(desk.iter().any(|a| a == "veryfast"));
	let gi = desk.iter().position(|a| a == "-g").unwrap();
	assert_eq!(desk[gi + 1], "120");

	// NVENC: ull tune for games, a higher preset for the desktop.
	let mut n = plan(HwEncoder::Nvenc);
	n.low_latency = true;
	assert!(encode_command(&n).1.iter().any(|a| a == "ull"));
	n.low_latency = false;
	let (_, nq) = encode_command(&n);
	assert!(nq.iter().any(|a| a == "p5"));
	assert!(nq.iter().any(|a| a == "-spatial-aq"));
}

#[test]
fn ddagrab_is_fully_gpu_per_encoder() {
	// NVENC + display on the NVIDIA GPU: zero-copy D3D11→CUDA→NVENC, no hwdownload.
	let mut p = plan(HwEncoder::Nvenc);
	p.capture = CaptureMethod::Ddagrab;
	p.gpu_zerocopy = true;
	let f = encode_command(&p).1.join(" ");
	assert!(f.contains("hwmap=derive_device=cuda"));
	assert!(f.contains("scale_cuda"));
	assert!(!f.contains("hwdownload"), "zero-copy must not round-trip to CPU");

	// NVENC hybrid (display on iGPU, CUDA map unavailable): feed ddagrab's D3D11 frame
	// STRAIGHT into NVENC — no CPU round-trip at all (the hwdownload/hwupload path capped
	// ~51 fps on a 3080 laptop). NVENC does the cross-GPU copy itself.
	p.gpu_zerocopy = false;
	let f = encode_command(&p).1.join(" ");
	assert!(!f.contains("hwdownload"), "hybrid NVENC must not round-trip to CPU");
	assert!(!f.contains("scale_cuda"), "no on-GPU scaler without the CUDA map; native res");

	// AMD AMF: NV12, no yuv420p swscale.
	let mut a = plan(HwEncoder::Amf);
	a.capture = CaptureMethod::Ddagrab;
	let f = encode_command(&a).1.join(" ");
	assert!(f.contains("h264_amf") && f.contains("format=nv12"));

	// Software still uses the CPU yuv420p path (libx264 needs it).
	let mut s = plan(HwEncoder::Software);
	s.capture = CaptureMethod::Ddagrab;
	assert!(encode_command(&s).1.join(" ").contains("format=yuv420p"));
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
