//! GStreamer encode backend: pure string builders for the host's gst-launch pipelines.
//!
//! Why gst next to ffmpeg: some hardware encoders are reachable ONLY through GStreamer
//! plugins — the canonical case is Rockchip MPP on RK3588 boards (`mpph264enc` /
//! `mpph265enc`), where the distro ffmpeg has rkmpp DECODE but no encode. The Wayland
//! host path already runs gst (PipeWire portal capture); this module generalizes its
//! previously hardcoded `x264enc` into a selectable encoder fragment, and adds an
//! `ximagesrc` pipeline so X11 hosts (e.g. an Orange Pi 5) can use gst HW encode too.
//!
//! Everything here is a pure `String` builder (no gst linkage), unit-tested like
//! `encode_command`. Runtime availability is probed by the app (`gst-inspect-1.0
//! --exists` + a one-frame `gst-launch` validation — see `process::validated_gst_encoders`).

use super::types::VCodec;

/// A GStreamer encoder element family. Priority-ordered hardware first; X264 is the
/// software terminal fallback (H.264 only — realtime software HEVC stays off, same
/// policy as the ffmpeg path).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GstEncoder {
	/// Rockchip MPP (RK3588-class SBCs): `mpph264enc` / `mpph265enc`.
	Mpp,
	/// VA-API plugins (Intel/AMD on Linux): `vaapih264enc` / `vaapih265enc`.
	Vaapi,
	/// NVIDIA NVENC plugins: `nvh264enc` / `nvh265enc`.
	Nv,
	/// Software x264 (always buildable; H.264 only).
	X264,
}

impl GstEncoder {
	/// Hardware-first probe/selection order, terminal software last.
	pub const PRIORITY: [GstEncoder; 4] = [Self::Mpp, Self::Vaapi, Self::Nv, Self::X264];

	/// The gst element name for a codec, or `None` if this family can't emit it.
	pub fn element(self, codec: VCodec) -> Option<&'static str> {
		Some(match (self, codec) {
			(Self::Mpp, VCodec::H264) => "mpph264enc",
			(Self::Mpp, VCodec::H265) => "mpph265enc",
			(Self::Vaapi, VCodec::H264) => "vaapih264enc",
			(Self::Vaapi, VCodec::H265) => "vaapih265enc",
			(Self::Nv, VCodec::H264) => "nvh264enc",
			(Self::Nv, VCodec::H265) => "nvh265enc",
			(Self::X264, VCodec::H264) => "x264enc",
			_ => return None, // no AV1 elements modeled; no software HEVC
		})
	}

	/// The codecs this family can emit (static set; runtime presence is probed).
	pub fn codecs(self) -> &'static [VCodec] {
		match self {
			Self::X264 => &[VCodec::H264],
			_ => &[VCodec::H264, VCodec::H265],
		}
	}

	/// The shared wire/UI id (same vocabulary as `HwEncoder` wire ids, so the UI shows
	/// ONE entry per family regardless of the ffmpeg-vs-gst backend that serves it).
	pub fn wire_id(self) -> &'static str {
		match self {
			Self::Mpp => "rkmpp",
			Self::Vaapi => "vaapi",
			Self::Nv => "nvenc",
			Self::X264 => "software",
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Mpp => "Rockchip MPP",
			Self::Vaapi => "VA-API",
			Self::Nv => "NVIDIA NVENC",
			Self::X264 => "Yazılım (CPU)",
		}
	}
}

/// Map a wire/UI encoder id to the gst family that serves it (None = no gst analog).
pub fn from_wire_id(id: &str) -> Option<GstEncoder> {
	match id {
		"rkmpp" => Some(GstEncoder::Mpp),
		"vaapi" => Some(GstEncoder::Vaapi),
		"nvenc" => Some(GstEncoder::Nv),
		"software" => Some(GstEncoder::X264),
		_ => None,
	}
}

/// Build the encoder→parser→RTP-payloader fragment, or `None` for an impossible
/// (family, codec) pair. Low-latency knobs mirror the ffmpeg path: no B-frames,
/// ~1 s GOP, CBR-ish target bitrate, headers re-sent so a mid-stream join recovers
/// (`header-mode=each-idr` on MPP, `config-interval=1` on the payloader).
pub fn encoder_fragment(
	enc: GstEncoder,
	codec: VCodec,
	bitrate_kbps: u32,
	fps: u32,
) -> Option<String> {
	let element = enc.element(codec)?;
	let key_int = fps.max(1);
	let enc_props = match enc {
		// MPP wants absolute bits/s; gop=-1 means "fps", but pin it to the key interval.
		// Verified live on an Orange Pi 5 (gst 1.20): bps / gop / header-mode all apply.
		GstEncoder::Mpp => format!(
			"{element} bps={bps} gop={key_int} header-mode=each-idr",
			bps = (bitrate_kbps.max(1) as u64 * 1000).min(u32::MAX as u64) as u32
		),
		// vaapih26Xenc: bitrate in kbit/s; CBR; keyframe-period analog of key-int.
		GstEncoder::Vaapi => format!(
			"{element} rate-control=cbr bitrate={bitrate_kbps} keyframe-period={key_int}"
		),
		// nvh26Xenc: bitrate in kbit/s; zerolatency preset + CBR; gop-size.
		GstEncoder::Nv => format!(
			"{element} preset=low-latency-hq rc-mode=cbr bitrate={bitrate_kbps} gop-size={key_int} zerolatency=true"
		),
		GstEncoder::X264 => format!(
			"{element} tune=zerolatency speed-preset=ultrafast bitrate={bitrate_kbps} key-int-max={key_int} bframes=0"
		),
	};
	let (parse, pay) = match codec {
		VCodec::H264 => ("h264parse", "rtph264pay"),
		VCodec::H265 => ("h265parse", "rtph265pay"),
		VCodec::Av1 => return None,
	};
	Some(format!(
		"{enc_props} ! {parse} ! {pay} config-interval=1 pt=96 mtu=1200"
	))
}

/// Full Wayland (portal/PipeWire) pipeline: capture → bounded leaky queue (drop stale
/// frames instead of growing latency) → NV12 convert → encoder fragment → UDP RTP.
///
/// NV12 (not I420) is the universal encoder input: `nvh264enc`/`vaapih264enc` accept
/// `video/x-raw` only in {NV12, Y444, RGBA, …} — NOT I420 — so forcing I420 made the
/// NVENC/VAAPI link fail and the host silently fell back to **software x264**. `x264enc`
/// accepts NV12 too (verified), so NV12 works for every gst encoder on this path.
pub fn wayland_pipeline(fd: i32, node_id: u32, fragment: &str, ip: &str, port: u16) -> String {
	format!(
		"pipewiresrc fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 \
		 ! queue leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 \
		 ! videoconvert ! video/x-raw,format=NV12 \
		 ! {fragment} \
		 ! udpsink host={ip} port={port} sync=false"
	)
}

/// Full X11 pipeline (`ximagesrc`): for X11 hosts whose HW encoder lives in gst
/// (Orange Pi 5 MPP). `use-damage=0` = full frames at a steady rate (damage events
/// would starve the encoder on a static desktop).
///
/// `direct_bgrx` (MPP only): hand the capture's native BGRx STRAIGHT to the encoder
/// — Rockchip's mpp plugin converts it on the RGA blitter, so the CPU `videoconvert`
/// (the measured ~20 ms/frame bottleneck that capped streams at ~49 fps) drops out
/// entirely. Measured on the Orange Pi 5: 600 frames 20.5 s → 11.9 s, CPU halved.
/// Other encoders (vaapi/nv/x264) keep the I420 convert — their gst elements don't
/// take BGRx everywhere.
pub fn x11_pipeline(
	display: &str,
	fps: u32,
	fragment: &str,
	ip: &str,
	port: u16,
	direct_bgrx: bool,
	region: Option<(i32, i32, u32, u32)>,
) -> String {
	let fps = fps.max(1);
	// Multi-monitor: ximagesrc captures the whole X root by default; constrain it to the
	// selected monitor's rectangle with startx/starty/endx/endy (inclusive end coords).
	let region = match region {
		Some((x, y, w, h)) if w > 0 && h > 0 => format!(
			" startx={x} starty={y} endx={} endy={}",
			x + w as i32 - 1,
			y + h as i32 - 1
		),
		_ => String::new(),
	};
	// Both variants keep the leaky queue right after capture, so stale frames drop
	// BEFORE any further work when the encoder can't keep up (bounded latency).
	let convert = if direct_bgrx {
		format!(
			"! video/x-raw,format=BGRx,framerate={fps}/1 \
			 ! queue leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 "
		)
	} else {
		format!(
			"! video/x-raw,framerate={fps}/1 \
			 ! queue leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 \
			 ! videoconvert ! video/x-raw,format=I420 "
		)
	};
	// `PULSAR_DAMAGE=1` (opt-in, DEFAULT OFF): switch ximagesrc to damage-event capture
	// (`use-damage=1`) and re-pace with `videorate` so the encoder still sees a steady
	// `framerate` even though ximagesrc only emits on screen changes. The idea: a mostly
	// static remote desktop stops spending the X-server full-frame copy on every vblank
	// (the ~84 fps ximagesrc ceiling is that copy), so host encode CPU drops when little
	// moves. Trade-off (why it's gated, to be measured on the Pi): `videorate` duplicates
	// the last frame to hold the rate, which can ADD a frame of latency on motion. Default
	// stays `use-damage=0` (full frames, steady rate) — the proven path.
	let (damage, rate) = if std::env::var("PULSAR_DAMAGE").as_deref() == Ok("1") {
		(1, "! videorate ")
	} else {
		(0, "")
	};
	format!(
		"ximagesrc display-name={display}{region} use-damage={damage} show-pointer=true \
		 {rate}{convert}\
		 ! {fragment} \
		 ! udpsink host={ip} port={port} sync=false"
	)
}

/// Full KMS (DRM scanout) pipeline: ZERO-COPY capture for game streaming on
/// RK3588-class hosts. `kmssrc` imports the CRTC's framebuffer as a DMABuf every
/// vblank and `mpph26Xenc` imports that fd straight into the encoder — no CPU
/// touch at all, which is what lifts the ximagesrc ceiling (~84 fps spent on the
/// X server copy alone) to true 1080p120.
///
/// Runtime requirements — all probed by `kms_probe_pipeline`, never assumed:
/// - a gstreamer-rockchip build whose mppenc sink accepts `memory:DMABuf` caps
///   (the distro snapshot doesn't; a patched build in the user plugin dir does);
/// - a `CAP_SYS_ADMIN` gst-launch binary: DRM `GETFB2` hands out GEM handles only
///   to the DRM master or privileged callers (same rule as ffmpeg's `kmsgrab`),
///   otherwise kmssrc silently produces EMPTY buffers.
///
/// `sync-fb=false` is load-bearing: kmssrc then paces on the vblank alone
/// (120 Hz panel → 120 fps). The default waits for a NEW pageflip per frame,
/// which on an idle X desktop throttles to the compositor repaint rate
/// (~58 fps measured on the Orange Pi 5).
///
/// KNOWN limit: the X HW cursor lives on its own DRM plane, so it is NOT in the
/// captured frame. Game mode only (games draw their own pointer in-frame);
/// remote-desktop sessions keep the ximagesrc path, which composites the cursor.
pub fn kms_pipeline(fps: u32, fragment: &str, ip: &str, port: u16) -> String {
	let fps = fps.max(1);
	format!(
		"kmssrc driver-name=rockchip dma-feature=true sync-fb=false \
		 ! video/x-raw(memory:DMABuf),format=BGRx,framerate={fps}/1 \
		 ! queue leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 \
		 ! {fragment} \
		 ! udpsink host={ip} port={port} sync=false"
	)
}

/// Two-frame validation for the zero-copy KMS path: exit 0 ⇒ kmssrc can export
/// the scanout FB (privileged binary), the encoder negotiates DMABuf caps AND
/// encodes real frames. Any missing piece (stock plugin, no CAP_SYS_ADMIN, no
/// DRM access) fails fast and the caller falls back to `x11_pipeline`.
pub fn kms_probe_pipeline(fragment: &str) -> String {
	format!(
		"kmssrc driver-name=rockchip dma-feature=true sync-fb=false num-buffers=2 \
		 ! video/x-raw(memory:DMABuf),format=BGRx \
		 ! {fragment} \
		 ! fakesink sync=false"
	)
}

/// One-frame validation pipeline for an encoder fragment (the gst analog of the
/// ffmpeg `probe_command`): exit 0 ⇒ the elements exist AND initialize on this box.
pub fn probe_pipeline(fragment: &str) -> String {
	// NV12, matching wayland_pipeline: probing with I420 made nvh264enc/vaapih264enc fail
	// to link (they don't accept I420), so they were marked unsupported and the host used
	// software x264 even on machines with a working NVENC/VAAPI encoder.
	format!(
		"videotestsrc num-buffers=2 \
		 ! video/x-raw,width=640,height=360,framerate=30/1 \
		 ! videoconvert ! video/x-raw,format=NV12 \
		 ! {fragment} \
		 ! fakesink sync=false"
	)
}
