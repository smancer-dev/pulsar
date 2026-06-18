//! Encoder/codec/capture types and the encoder detection + resolution helpers.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HwEncoder {
	/// Pick the best available at runtime.
	Auto,
	/// NVIDIA NVENC hardware encoder.
	Nvenc,
	/// AMD AMF hardware encoder (Windows; AMD GPUs/iGPUs).
	Amf,
	/// VA-API (Intel iGPU / AMD on Linux).
	Vaapi,
	/// Intel QuickSync.
	Qsv,
	/// Apple VideoToolbox (macOS).
	VideoToolbox,
	/// Vulkan video encode (Linux; vendor-agnostic — any Vulkan-encode-capable GPU).
	Vulkan,
	/// Windows Media Foundation (Qualcomm/ARM Windows; the only HW encoder on Snapdragon).
	MediaFoundation,
	/// Rockchip MPP (RK3588-class SBCs). Reachable two ways: ffmpeg `h264/hevc_rkmpp`
	/// encoders when an ffmpeg-rockchip build is installed, or the GStreamer
	/// `mpph264enc/mpph265enc` elements (see `pipeline::gst`) — both share this id.
	Rkmpp,
	/// libx264/libx265/libsvtav1 on the CPU.
	Software,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
	/// Windows GDI desktop grab (legacy fallback; can be black on multiseat / virtual displays).
	Gdigrab,
	/// Windows DXGI Desktop Duplication (GPU, low-latency — the Parsec-style path). Default on Windows.
	Ddagrab,
	/// macOS AVFoundation screen capture.
	AvFoundation,
}

impl CaptureMethod {
	/// The right default for the platform we're built for.
	pub fn default_for_os() -> Self {
		if cfg!(target_os = "windows") {
			Self::Ddagrab
		} else if cfg!(target_os = "macos") {
			Self::AvFoundation
		} else {
			Self::X11grab
		}
	}

	/// ffmpeg input args for this capture method.
	pub(super) fn input_args(self, plan: &StreamPlan) -> Vec<String> {
		let s = |x: &str| x.to_string();
		let fps = plan.fps.to_string();
		let size = format!("{}x{}", plan.width, plan.height);
		match self {
			Self::X11grab => vec![
				s("-f"),
				s("x11grab"),
				s("-framerate"),
				fps,
				s("-video_size"),
				size,
				s("-i"),
				plan.display.clone(),
			],
			Self::Kmsgrab => vec![s("-f"), s("kmsgrab"), s("-framerate"), fps, s("-i"), s("-")],
			Self::Gdigrab => vec![
				s("-f"),
				s("gdigrab"),
				s("-framerate"),
				fps,
				s("-video_size"),
				size,
				s("-i"),
				s("desktop"),
			],
			// DXGI Desktop Duplication (GPU capture). The filter tail is ENCODER-AWARE so
			// the frame stays on the GPU wherever possible — the old "hwdownload → CPU
			// scale → yuv420p → re-upload" round trip pinned a CPU core + both GPUs.
			Self::Ddagrab => {
				let (fps, w, h) = (plan.fps, plan.width, plan.height);
				let oi = plan.output_idx;
				// CPU-bounce target format honors HDR (10-bit) / YUV444 — the encoder's
				// `-pix_fmt` (set in encode_command) must match what the filter produces.
				// NOTE: uses plan.effective_hdr() / plan.effective_yuv444() (not the raw
				// fields) so GPU-path branches that cannot produce 10-bit/444 are already
				// clamped to SDR 4:2:0 here, and encode_command sees the same clamped
				// values when it appends -pix_fmt (via the same helpers).
				let (eff_hdr, eff_yuv444) = plan.effective_hdr_yuv444();
				let cpu_fmt = match (eff_yuv444, eff_hdr) {
					(true, true) => "yuv444p10le",
					(true, false) => "yuv444p",
					(false, true) => "p010le",
					(false, false) => "yuv420p",
				};
				let filter = match plan.encoder {
					// NVENC + display on the NVIDIA GPU: fully zero-copy (D3D11→CUDA→NVENC).
					// HDR/YUV444 on the CUDA path would require scale_cuda to produce
					// p010le/yuv444p10le — risky driver/format compat; degraded to SDR 4:2:0
					// (eff_hdr/eff_yuv444 are false for this branch, matching encode_command).
					HwEncoder::Nvenc if plan.gpu_zerocopy => format!(
						"ddagrab=output_idx={oi}:framerate={fps}:draw_mouse=1,hwmap=derive_device=cuda,scale_cuda={w}:{h}:format=nv12"
					),
					// NVENC where the zero-copy CUDA map isn't available (hybrid laptop: the
					// display is on the iGPU, so ffmpeg's `hwmap=derive_device=cuda` fails with
					// -40). Feed ddagrab's D3D11 frame STRAIGHT into h264_nvenc — NVENC ingests the
					// D3D11 texture and does the cross-GPU copy itself on the GPU. This is what
					// Sunshine/Parsec do; it avoids the old hwdownload→CPU-convert→hwupload round
					// trip (which pinned ~3 CPU cores and capped ~51 fps). No on-GPU scaler is
					// available without the CUDA map, so we stream the capture's native resolution
					// and let the client scale (`{w}`/`{h}` intentionally unused here).
					// HDR/YUV444 degraded to SDR 4:2:0 for the same reason as zero-copy above.
					HwEncoder::Nvenc => {
						let _ = (w, h);
						// `fps={fps}` forces CONSTANT-frame-rate output (even-interval timestamps).
						// ddagrab/DXGI only delivers a frame when the screen CHANGES, so on a desktop
						// it emits ~65 fps IRREGULARLY → the client receives jittery frame intervals →
						// cursor/typing feel hitchy even though input is instant. The fps filter
						// duplicates the last frame to pace delivery to a steady {fps} (Sunshine does
						// the same), so the client presents smoothly.
						format!("ddagrab=output_idx={oi}:framerate={fps}:draw_mouse=1,fps={fps}")
					}
					// AMD AMF: on-GPU scale is unreliable on iGPUs; do the minimal CPU work
					// (download + BGRA→NV12 + scale) — far lighter than the old yuv420p path.
					// AMF goes through cpu_fmt (eff_hdr/eff_yuv444 honored; AMF supports 10-bit).
					HwEncoder::Amf => format!(
						"ddagrab=output_idx={oi}:framerate={fps}:draw_mouse=1,hwdownload,format=bgra,scale={w}:{h},format={f}",
						f = if eff_hdr || eff_yuv444 { cpu_fmt } else { "nv12" }
					),
					// Intel QSV: fully on-GPU. scale_qsv only supports nv12 and p010 (QSV
					// internal 10-bit); yuv444 via QSV on ddagrab is not reliably supported.
					// HDR/YUV444 degraded to SDR 4:2:0 (eff_hdr/eff_yuv444 false for QSV
					// ddagrab, matching encode_command).
					HwEncoder::Qsv => format!(
						"ddagrab=output_idx={oi}:framerate={fps}:draw_mouse=1,hwmap=derive_device=qsv,format=qsv,scale_qsv=w={w}:h={h}:format=nv12"
					),
					// Software / VAAPI / VideoToolbox / Auto: CPU frame (yuv420p, or the HDR/444
					// format when requested) as before.
					_ => format!(
						"ddagrab=output_idx={oi}:framerate={fps}:draw_mouse=1,hwdownload,format=bgra,scale={w}:{h},format={cpu_fmt}"
					),
				};
				vec![s("-filter_complex"), filter]
			}
			Self::AvFoundation => vec![
				s("-f"),
				s("avfoundation"),
				s("-framerate"),
				fps,
				s("-capture_cursor"),
				s("1"),
				s("-i"),
				format!("{}:none", plan.display),
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
			(Self::Amf, VCodec::H264) => "h264_amf",
			(Self::Amf, VCodec::H265) => "hevc_amf",
			(Self::Amf, VCodec::Av1) => "av1_amf",
			(Self::Vaapi, VCodec::H264) => "h264_vaapi",
			(Self::Vaapi, VCodec::H265) => "hevc_vaapi",
			(Self::Vaapi, VCodec::Av1) => "av1_vaapi",
			(Self::Qsv, VCodec::H264) => "h264_qsv",
			(Self::Qsv, VCodec::H265) => "hevc_qsv",
			(Self::Qsv, VCodec::Av1) => "av1_qsv",
			(Self::VideoToolbox, VCodec::H264) => "h264_videotoolbox",
			(Self::VideoToolbox, VCodec::H265) => "hevc_videotoolbox",
			(Self::Vulkan, VCodec::H264) => "h264_vulkan",
			(Self::Vulkan, VCodec::H265) => "hevc_vulkan",
			(Self::Vulkan, VCodec::Av1) => "av1_vulkan",
			(Self::MediaFoundation, VCodec::H264) => "h264_mf",
			(Self::MediaFoundation, VCodec::H265) => "hevc_mf",
			(Self::MediaFoundation, VCodec::Av1) => "av1_mf",
			(Self::Rkmpp, VCodec::H264) => "h264_rkmpp",
			(Self::Rkmpp, VCodec::H265) => "hevc_rkmpp",
			(Self::Software, VCodec::H264) => "libx264",
			(Self::Software, VCodec::H265) => "libx265",
			(Self::Software, VCodec::Av1) => "libsvtav1",
			(Self::VideoToolbox, VCodec::Av1) | (Self::Rkmpp, VCodec::Av1) | (Self::Auto, _) => {
				return None
			}
		})
	}

	/// All codecs this backend can emit (the full static capability set). Used by
	/// `available_codecs` to intersect against what the bundled ffmpeg actually advertises.
	pub fn all_codecs(self) -> &'static [VCodec] {
		match self {
			// VideoToolbox has no AV1 ENCODE on Apple silicon (decode only).
			Self::VideoToolbox => &[VCodec::H264, VCodec::H265],
			// RK3588 VEPU does H.264 + HEVC (no AV1 encode).
			Self::Rkmpp => &[VCodec::H264, VCodec::H265],
			Self::Auto => &[],
			// Every other backend (NVENC/AMF/QSV/VAAPI/Software) covers all three.
			_ => &[VCodec::H264, VCodec::H265, VCodec::Av1],
		}
	}

	/// Which codecs are ACTUALLY usable for this backend in the running ffmpeg build —
	/// the static capability set (`all_codecs`) intersected with the encoder names present
	/// in `ffmpeg -encoders`. This is how HEVC/AV1 become selectable (the old code gated a
	/// whole vendor on its H.264 name alone, so HEVC/AV1 were dead even when present).
	pub fn available_codecs(self, ffmpeg_encoders_output: &str) -> Vec<VCodec> {
		self.all_codecs()
			.iter()
			.copied()
			.filter(|&c| {
				self.ffmpeg_name(c)
					.is_some_and(|n| ffmpeg_encoders_output.contains(n))
			})
			.collect()
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Auto => "Otomatik",
			Self::Nvenc => "NVIDIA NVENC",
			Self::Amf => "AMD AMF",
			Self::Vaapi => "VA-API",
			Self::Qsv => "Intel QuickSync",
			Self::VideoToolbox => "Apple VideoToolbox",
			Self::Vulkan => "Vulkan",
			Self::MediaFoundation => "Media Foundation",
			Self::Rkmpp => "Rockchip MPP",
			Self::Software => "Yazılım (CPU)",
		}
	}
}

/// Parse `ffmpeg -hide_banner -encoders` output into the hw encoders available. A backend
/// is available if ANY of its codec encoders (H.264 / HEVC / AV1) is present — not just its
/// H.264 name, so an HEVC/AV1-only build still registers the vendor. Per-codec availability
/// is then `HwEncoder::available_codecs`.
pub fn detect(ffmpeg_encoders_output: &str) -> Vec<HwEncoder> {
	[
		HwEncoder::Nvenc,
		HwEncoder::Amf,
		HwEncoder::Vaapi,
		HwEncoder::Qsv,
		HwEncoder::Vulkan,
		HwEncoder::VideoToolbox,
		HwEncoder::MediaFoundation,
		HwEncoder::Rkmpp,
		HwEncoder::Software,
	]
	.into_iter()
	.filter(|e| !e.available_codecs(ffmpeg_encoders_output).is_empty())
	.collect()
}

/// Resolve a (possibly `Auto`/unavailable) choice to a concrete encoder, in
/// quality order, falling back to software.
pub fn resolve(choice: HwEncoder, available: &[HwEncoder]) -> HwEncoder {
	if choice != HwEncoder::Auto && available.contains(&choice) {
		return choice;
	}
	for c in [
		HwEncoder::Nvenc,
		HwEncoder::Amf,
		HwEncoder::Qsv,
		HwEncoder::Vaapi,
		HwEncoder::Vulkan,
		HwEncoder::VideoToolbox,
		HwEncoder::MediaFoundation,
		HwEncoder::Rkmpp,
		HwEncoder::Software,
	] {
		if available.contains(&c) {
			return c;
		}
	}
	HwEncoder::Software
}

/// Resolve the requested codec against what the chosen encoder can actually emit in this
/// ffmpeg build. Honors the request if available; else prefers H.264 (universally decodable
/// by the webview client); else the first available codec. Mirrors Sunshine's codec fallback.
pub fn resolve_codec(
	encoder: HwEncoder,
	requested: VCodec,
	ffmpeg_encoders_output: &str,
) -> VCodec {
	let avail = encoder.available_codecs(ffmpeg_encoders_output);
	if avail.contains(&requested) {
		requested
	} else if avail.contains(&VCodec::H264) {
		VCodec::H264
	} else {
		avail.first().copied().unwrap_or(VCodec::H264)
	}
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
	/// Tune for **lowest latency** (game mode) vs **quality** (remote desktop,
	/// AnyDesk-style). Game streaming makes latency paramount; desktop favors a
	/// sharper picture. Drives preset/tune/GOP in [`encode_command`].
	pub low_latency: bool,
	/// (Windows/ddagrab/NVENC) the display adapter IS the NVIDIA GPU, so a fully
	/// zero-copy D3D11→CUDA→NVENC path works (no CPU round-trip). Set from a one-time
	/// probe; false on hybrid boxes (iGPU display + dGPU encode), where we use the
	/// GPU-scale-with-CPU-bounce path instead. Ignored by non-NVENC encoders.
	pub gpu_zerocopy: bool,
	/// (Windows/ddagrab) DXGI output index to capture. 0 = primary monitor (default).
	/// Passed directly to `ddagrab=output_idx=N` in the ffmpeg filter graph so the
	/// ffmpeg fallback path captures the same monitor the client selected.
	pub output_idx: u32,
	/// Encode 10-bit **HDR** (P010 + BT2020 primaries / SMPTE2084 PQ transfer). Requires an
	/// HDR-capable encoder+codec (HEVC main10 / AV1 main / H.264 high10); SDR otherwise.
	pub hdr: bool,
	/// Encode **4:4:4** chroma (no subsampling — sharper text/lines for remote desktop).
	/// Only a few encoders support it (NVENC, QSV, software); ignored elsewhere.
	pub yuv444: bool,
}

impl StreamPlan {
	/// Returns the `(hdr, yuv444)` values that are **actually achievable** for the
	/// current encoder + capture combination, after clamping paths that cannot produce
	/// 10-bit or 4:4:4 frames in the GPU filter.
	///
	/// NVENC (both zero-copy and hybrid) and QSV with `Ddagrab` use GPU-side filters
	/// (`scale_cuda` / `scale_qsv`) that only reliably output `nv12` (8-bit 4:2:0) on
	/// current ffmpeg builds.  Allowing `hdr` or `yuv444` for those paths would cause
	/// the filter to emit `nv12` while `-pix_fmt` requests `p010le`/`yuv444p…` →
	/// ffmpeg errors or silently encodes SDR while signaling HDR.  We degrade both
	/// flags to `false` for those paths so the filter format and `-pix_fmt` always
	/// agree.  All other encoders (AMF, Software, VAAPI, catch-all) use a CPU-bounce
	/// path whose `format=` step can produce any pixel format, so they respect the
	/// user's request.
	///
	/// **Both `CaptureMethod::input_args` (filter) and `encode_command` (-pix_fmt /
	/// -profile:v) call this method**, so the two can never disagree.
	pub fn effective_hdr_yuv444(&self) -> (bool, bool) {
		// NVENC/QSV Ddagrab GPU paths: clamp to SDR 4:2:0.
		let is_gpu_ddagrab = self.capture == CaptureMethod::Ddagrab
			&& matches!(self.encoder, HwEncoder::Nvenc | HwEncoder::Qsv);
		if is_gpu_ddagrab {
			(false, false)
		} else {
			(self.hdr, self.yuv444)
		}
	}
}
