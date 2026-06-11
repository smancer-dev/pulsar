//! Moonlight-style tiered decoder selection (Linux).
//!
//! Instead of a hardcoded name list, candidates are enumerated per codec and each is
//! VALIDATED by really decoding a tiny canned keyframe (committed under `testdata/`,
//! embedded via `include_bytes!`). First success wins:
//!
//! - **Tier 0 — zero-copy SoC decoders**: any ffmpeg decoder with `AV_CODEC_CAP_HARDWARE`
//!   whose `pix_fmts` include `DRM_PRIME` (rkmpp, v4l2m2m-drm, future SoCs — found by
//!   capability, not by name). Output feeds the existing dmabuf→EGLImage zero-copy path.
//! - **Tier 1 — generic hwaccels**: the stock decoder + a hw device context
//!   (VAAPI → CUDA → Vulkan → DRM). Frames are either `av_hwframe_map`ped to DRM_PRIME
//!   (zero-copy) or `av_hwframe_transfer_data` read back (NV12 upload) by the caller.
//! - **Tier 2 — software**: the stock decoder, planar/NV12 GL upload. Always present —
//!   the absolute fallback on every platform.
//!
//! `--probe` mode reuses the same machinery headless and prints JSON for the app's
//! startup capability detection.

use crate::video::is_displayable_sw;
use ffmpeg_sys_next as ff;
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;
use std::sync::atomic::{AtomicI32, Ordering};

// Canned single-keyframe bitstreams (320×180 testsrc2; regenerate with
// `ffmpeg -f lavfi -i testsrc2=size=320x180:rate=30 -frames:v 1 -c:v <enc> -f <mux> …`).
const TEST_H264: &[u8] = include_bytes!("../testdata/test.h264");
const TEST_H265: &[u8] = include_bytes!("../testdata/test.h265");
const TEST_AV1: &[u8] = include_bytes!("../testdata/test.av1.ivf");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
	/// Named/SoC hardware decoder emitting DRM_PRIME directly (rkmpp class).
	ZeroCopyHw,
	/// Stock decoder + hwaccel device context (VAAPI/CUDA/Vulkan/DRM).
	HwAccel,
	/// Plain software decode.
	Software,
}

impl Tier {
	pub fn as_str(self) -> &'static str {
		match self {
			Tier::ZeroCopyHw => "hw-zerocopy",
			Tier::HwAccel => "hwaccel",
			Tier::Software => "software",
		}
	}
}

/// A validated decoder choice for one codec. The raw pointers are 'static (ffmpeg
/// registry data), so this is safe to carry across the decode thread spawn.
#[derive(Clone, Debug)]
pub struct Selected {
	pub name: String,
	pub tier: Tier,
	/// Hwaccel device type for Tier::HwAccel (open a device ctx before decode).
	pub hwdev: Option<ff::AVHWDeviceType>,
	/// The hwaccel pixel format frames will arrive in for Tier::HwAccel.
	pub hw_fmt: ff::AVPixelFormat,
}

struct Candidate {
	dec: *const ff::AVCodec,
	name: String,
	tier: Tier,
	hwdev: Option<ff::AVHWDeviceType>,
	hw_fmt: ff::AVPixelFormat,
}

/// The hwaccel pixel format produced under a device type.
fn hw_fmt_for(dev: ff::AVHWDeviceType) -> ff::AVPixelFormat {
	use ff::AVHWDeviceType::*;
	match dev {
		AV_HWDEVICE_TYPE_VAAPI => ff::AVPixelFormat::AV_PIX_FMT_VAAPI,
		AV_HWDEVICE_TYPE_CUDA => ff::AVPixelFormat::AV_PIX_FMT_CUDA,
		AV_HWDEVICE_TYPE_VULKAN => ff::AVPixelFormat::AV_PIX_FMT_VULKAN,
		AV_HWDEVICE_TYPE_DRM => ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
		_ => ff::AVPixelFormat::AV_PIX_FMT_NONE,
	}
}

unsafe fn decoder_name(dec: *const ff::AVCodec) -> String {
	std::ffi::CStr::from_ptr((*dec).name)
		.to_string_lossy()
		.into_owned()
}

unsafe fn pix_fmts_contain(dec: *const ff::AVCodec, want: ff::AVPixelFormat) -> bool {
	let mut p = (*dec).pix_fmts;
	if p.is_null() {
		return false;
	}
	while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
		if *p == want {
			return true;
		}
		p = p.add(1);
	}
	false
}

/// Enumerate candidates for `codec_id`, tier order. No names anywhere — Tier 0 is
/// "hardware decoder that outputs DRM_PRIME", which is what makes a NEW SoC work
/// without Pulsar changes (Moonlight's format-not-device lesson).
unsafe fn candidates(codec_id: ff::AVCodecID) -> Vec<Candidate> {
	let mut out = Vec::new();

	// Tier 0: AV_CODEC_CAP_HARDWARE decoders with DRM_PRIME output.
	let mut it: *mut std::ffi::c_void = ptr::null_mut();
	loop {
		let dec = ff::av_codec_iterate(&mut it);
		if dec.is_null() {
			break;
		}
		if ff::av_codec_is_decoder(dec) == 0 || (*dec).id != codec_id {
			continue;
		}
		if (*dec).capabilities & (ff::AV_CODEC_CAP_HARDWARE as c_int) == 0 {
			continue;
		}
		if pix_fmts_contain(dec, ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME) {
			out.push(Candidate {
				dec,
				name: decoder_name(dec),
				tier: Tier::ZeroCopyHw,
				hwdev: None,
				hw_fmt: ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
			});
		}
	}

	// Tier 1: the stock decoder's hwaccel configs, preferred device order.
	let stock = ff::avcodec_find_decoder(codec_id);
	if !stock.is_null() {
		use ff::AVHWDeviceType::*;
		for want in [
			AV_HWDEVICE_TYPE_VAAPI,
			AV_HWDEVICE_TYPE_CUDA,
			AV_HWDEVICE_TYPE_VULKAN,
			AV_HWDEVICE_TYPE_DRM,
		] {
			let mut i = 0;
			loop {
				let cfg = ff::avcodec_get_hw_config(stock, i);
				if cfg.is_null() {
					break;
				}
				let methods = (*cfg).methods;
				if (*cfg).device_type == want
					&& methods & (ff::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int) != 0
				{
					out.push(Candidate {
						dec: stock,
						name: format!("{}+{}", decoder_name(stock), hwdev_name(want)),
						tier: Tier::HwAccel,
						hwdev: Some(want),
						hw_fmt: hw_fmt_for(want),
					});
					break;
				}
				i += 1;
			}
		}
		// Tier 2: software (the stock decoder without hwaccel) — always last, always there.
		if (*stock).capabilities & (ff::AV_CODEC_CAP_HARDWARE as c_int) == 0 {
			out.push(Candidate {
				dec: stock,
				name: decoder_name(stock),
				tier: Tier::Software,
				hwdev: None,
				hw_fmt: ff::AVPixelFormat::AV_PIX_FMT_NONE,
			});
		}
	}

	out
}

fn hwdev_name(dev: ff::AVHWDeviceType) -> &'static str {
	use ff::AVHWDeviceType::*;
	match dev {
		AV_HWDEVICE_TYPE_VAAPI => "vaapi",
		AV_HWDEVICE_TYPE_CUDA => "cuda",
		AV_HWDEVICE_TYPE_VULKAN => "vulkan",
		AV_HWDEVICE_TYPE_DRM => "drm",
		_ => "hw",
	}
}

/// The hw format the validate/get_format callback should accept for the candidate
/// being tried (callbacks can't capture, single-threaded selection → a global is fine).
static WANT_HW_FMT: AtomicI32 = AtomicI32::new(ff::AVPixelFormat::AV_PIX_FMT_NONE as i32);

pub(crate) fn set_wanted_hw_fmt(fmt: ff::AVPixelFormat) {
	WANT_HW_FMT.store(fmt as i32, Ordering::SeqCst);
}

/// Shared get_format: DRM_PRIME (tier 0) → the candidate's hwaccel format (tier 1,
/// only when a device ctx is armed) → a displayable software format. NEVER an
/// unrequested hwaccel entry (that's the "vaapi 'succeeds' → opaque frames → black
/// screen" bug class).
pub(crate) unsafe extern "C" fn get_format(
	_c: *mut ff::AVCodecContext,
	fmts: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
	let want_hw = WANT_HW_FMT.load(Ordering::SeqCst);
	let mut p = fmts;
	while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
		if *p == ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME {
			return *p;
		}
		p = p.add(1);
	}
	if want_hw != ff::AVPixelFormat::AV_PIX_FMT_NONE as i32 {
		let mut p = fmts;
		while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
			if *p as i32 == want_hw {
				return *p;
			}
			p = p.add(1);
		}
	}
	let mut p = fmts;
	while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
		if is_displayable_sw(*p as c_int) {
			return *p;
		}
		p = p.add(1);
	}
	// Last resort: any NON-hwaccel entry (e.g. yuv420p10le for a 10-bit stream — the
	// caller skips non-displayable sw frames instead of erroring). Returning *fmts
	// blind could hand back a hwaccel format with no device ctx armed → ff_get_format
	// rejects it → every frame errors (the bug class described above).
	let mut p = fmts;
	let mut sw = ff::AVPixelFormat::AV_PIX_FMT_NONE;
	while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
		let d = ff::av_pix_fmt_desc_get(*p);
		if !d.is_null() && (*d).flags & (ff::AV_PIX_FMT_FLAG_HWACCEL as u64) == 0 {
			sw = *p; // keep the LAST sw entry (the decoder's native format)
		}
		p = p.add(1);
	}
	if sw != ff::AVPixelFormat::AV_PIX_FMT_NONE {
		return sw;
	}
	*fmts
}

/// Write the canned bitstream for `codec_id` to a temp file (avformat wants a path)
/// and return it. None for codecs we have no fixture for.
fn fixture_path(codec_id: ff::AVCodecID) -> Option<std::path::PathBuf> {
	let (bytes, name) = match codec_id {
		ff::AVCodecID::AV_CODEC_ID_H264 => (TEST_H264, "pulsar-test.h264"),
		ff::AVCodecID::AV_CODEC_ID_HEVC => (TEST_H265, "pulsar-test.h265"),
		ff::AVCodecID::AV_CODEC_ID_AV1 => (TEST_AV1, "pulsar-test.av1.ivf"),
		_ => return None,
	};
	let path = std::env::temp_dir().join(name);
	std::fs::write(&path, bytes).ok()?;
	Some(path)
}

/// Really decode the canned keyframe with this candidate. True ⇒ ≥1 frame came out
/// in an output class the presenter can actually show for the tier.
unsafe fn validate(cand: &Candidate, fixture: &std::path::Path) -> bool {
	let cpath = match CString::new(fixture.to_string_lossy().as_bytes()) {
		Ok(c) => c,
		Err(_) => return false,
	};
	let mut fmt: *mut ff::AVFormatContext = ptr::null_mut();
	if ff::avformat_open_input(&mut fmt, cpath.as_ptr(), ptr::null_mut(), ptr::null_mut()) < 0 {
		return false;
	}
	let mut ok = false;
	'done: {
		if ff::avformat_find_stream_info(fmt, ptr::null_mut()) < 0 {
			break 'done;
		}
		let vs = ff::av_find_best_stream(
			fmt,
			ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
			-1,
			-1,
			ptr::null_mut(),
			0,
		);
		if vs < 0 {
			break 'done;
		}
		let st = *(*fmt).streams.add(vs as usize);

		let dc = ff::avcodec_alloc_context3(cand.dec);
		if dc.is_null() {
			break 'done;
		}
		ff::avcodec_parameters_to_context(dc, (*st).codecpar);
		set_wanted_hw_fmt(if cand.tier == Tier::HwAccel {
			cand.hw_fmt
		} else {
			ff::AVPixelFormat::AV_PIX_FMT_NONE
		});
		(*dc).get_format = Some(get_format);
		let mut hwdev_ok = true;
		if let Some(dev) = cand.hwdev {
			let mut hwctx: *mut ff::AVBufferRef = ptr::null_mut();
			if ff::av_hwdevice_ctx_create(&mut hwctx, dev, ptr::null(), ptr::null_mut(), 0) < 0 {
				hwdev_ok = false;
			} else {
				(*dc).hw_device_ctx = hwctx;
			}
		}
		let mut dc_owned = dc;
		if hwdev_ok && ff::avcodec_open2(dc, cand.dec, ptr::null_mut()) >= 0 {
			let pkt = ff::av_packet_alloc();
			let frame = ff::av_frame_alloc();
			// Push every fixture packet, then flush; some HW decoders only emit
			// after the flush (single-keyframe input).
			while ff::av_read_frame(fmt, pkt) >= 0 {
				if (*pkt).stream_index == vs {
					let _ = ff::avcodec_send_packet(dc, pkt);
				}
				ff::av_packet_unref(pkt);
			}
			let _ = ff::avcodec_send_packet(dc, ptr::null()); // flush
													 // Drain with a few retries (async HW decoders return EAGAIN briefly).
			for _ in 0..50 {
				let r = ff::avcodec_receive_frame(dc, frame);
				if r == 0 {
					let f = (*frame).format;
					ok = match cand.tier {
						Tier::ZeroCopyHw => f == ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int,
						Tier::HwAccel => f == cand.hw_fmt as c_int || is_displayable_sw(f),
						Tier::Software => is_displayable_sw(f),
					};
					ff::av_frame_unref(frame);
					if ok {
						break;
					}
				} else if r == ff::AVERROR(libc::EAGAIN) {
					std::thread::sleep(std::time::Duration::from_millis(10));
				} else {
					break;
				}
			}
			let mut pkt = pkt;
			let mut frame = frame;
			ff::av_packet_free(&mut pkt);
			ff::av_frame_free(&mut frame);
		}
		ff::avcodec_free_context(&mut dc_owned);
	}
	ff::avformat_close_input(&mut fmt);
	ok
}

/// Pick the first candidate that REALLY decodes the canned keyframe. Tier order =
/// zero-copy SoC → hwaccel (vaapi/cuda/vulkan/drm) → software.
pub fn select(codec_id: ff::AVCodecID) -> Option<Selected> {
	let fixture = fixture_path(codec_id)?;
	unsafe {
		for cand in candidates(codec_id) {
			if validate(&cand, &fixture) {
				eprintln!(
					"pulsar-render: selected decoder {} ({})",
					cand.name,
					cand.tier.as_str()
				);
				return Some(Selected {
					name: cand.name,
					tier: cand.tier,
					hwdev: cand.hwdev,
					hw_fmt: cand.hw_fmt,
				});
			}
			eprintln!("pulsar-render: decoder {} failed validation", cand.name);
		}
	}
	None
}

/// Resolve a Selected back to its AVCodec for opening the real stream context.
/// (Pointers aren't carried in `Selected` so it stays Send/Sync-trivial.)
pub(crate) unsafe fn find_decoder(sel: &Selected, codec_id: ff::AVCodecID) -> *const ff::AVCodec {
	match sel.tier {
		Tier::ZeroCopyHw => {
			// Re-find by name (tier-0 candidates are specific named decoders).
			let base = sel.name.split('+').next().unwrap_or(&sel.name);
			let c = CString::new(base).unwrap();
			let dec = ff::avcodec_find_decoder_by_name(c.as_ptr());
			if dec.is_null() {
				ff::avcodec_find_decoder(codec_id)
			} else {
				dec
			}
		}
		_ => ff::avcodec_find_decoder(codec_id),
	}
}

/// `--probe`: run the chain per codec, headless, print one JSON array on stdout.
/// Consumed by the app's startup capability detection.
pub fn probe_json() -> String {
	let mut entries = Vec::new();
	for (label, id) in [
		("h264", ff::AVCodecID::AV_CODEC_ID_H264),
		("h265", ff::AVCodecID::AV_CODEC_ID_HEVC),
		("av1", ff::AVCodecID::AV_CODEC_ID_AV1),
	] {
		match select(id) {
			Some(sel) => entries.push(format!(
				r#"{{"codec":"{label}","ok":true,"decoder":"{}","tier":"{}","hw":{}}}"#,
				sel.name,
				sel.tier.as_str(),
				sel.tier != Tier::Software
			)),
			None => entries.push(format!(r#"{{"codec":"{label}","ok":false}}"#)),
		}
	}
	format!("[{}]", entries.join(","))
}
