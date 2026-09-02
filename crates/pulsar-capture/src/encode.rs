//! Zero-copy H.264 encode of a captured D3D11 BGRA texture → RTP, using the
//! **NVENC SDK** directly (runtime-loaded `nvEncodeAPI64.dll` via `libloading`).
//! This REPLACES the host `ddagrab … h264_nvenc … rtp` ffmpeg CLI for the NVENC
//! fast path; ffmpeg stays as the fallback (see the crate `lib.rs` handshake +
//! the host branch). NO ffmpeg/libav anywhere in this module.
//!
//! ## HYBRID cross-adapter design (the reality on this laptop)
//! The display owner is the **AMD** iGPU (dxgi.rs builds the D3D11 capture device on
//! it). NVENC lives on the **NVIDIA** dGPU. `nvEncOpenEncodeSessionEx(device=AMD)`
//! returns `NV_ENC_ERR_UNSUPPORTED_DEVICE` — you can only open an NVENC session on an
//! NVIDIA D3D11 device. So we run the Sunshine cross-adapter model
//! (`display_vram.cpp`): capture stays on the AMD device; we create a SEPARATE NVIDIA
//! D3D11 device (with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`) and move the whole
//! NVENC + VideoProcessor + NV12 chain onto it. The two adapters cannot share a plain
//! texture — `CopyResource` is same-device only — so the ONLY legal bridge is a
//! **shared, keyed-mutex texture**: a BGRA intermediate created on the AMD device with
//! `SHARED_NTHANDLE | SHARED_KEYEDMUTEX`, opened by NT handle on the NVIDIA device.
//! Each frame the AMD context copies `frame.texture → amd_shared` under the keyed
//! mutex; the NVIDIA context copies `nv_view → nvidia_bgra` under the same mutex; then
//! the NVIDIA VideoProcessor does BGRA→NV12 and NVENC encodes — all on NVIDIA. Capture
//! never blocks on NVENC because the mutex is only held for the cross-adapter copy.
//!
//! ## Data flow (one paced tick, hybrid)
//! ```text
//!   dxgi.rs pool BGRA texture (frame.texture)            [AMD device]
//!        │  CopyResource under keyed-mutex Acquire(0)/Release(1)  (AMD context)
//!        ▼
//!   amd_shared BGRA (SHARED_NTHANDLE|SHARED_KEYEDMUTEX)  [AMD device]
//!        ┊  ← the ONLY cross-adapter bridge (NT shared handle) →
//!   nv_view  = OpenSharedResource1(handle)               [NVIDIA device]
//!        │  CopyResource under keyed-mutex Acquire(1)/Release(0)  (NVIDIA context)
//!        ▼
//!   nvidia_bgra BGRA (plain, MiscFlags:0)                [NVIDIA device]
//!        │  ID3D11VideoProcessorBlt  (BGRA → NV12, one GPU call; scales if needed)
//!        ▼
//!   nv12_tex NV12 (DEFAULT, BIND_RENDER_TARGET)          [NVIDIA device]
//!        │  nvEncMapInputResource → nvEncEncodePicture (SYNC, no B-frames)
//!        ▼
//!   NVENC H.264 (preset P1 / tune ULL / CBR, repeatSPSPPS) → bitstream buffer
//!        │  nvEncLockBitstream → Annex-B access unit (SPS/PPS in-band on each IDR)
//!        ▼
//!   RtpSender::send_access_unit  (rtp.rs: RFC 6184 single-NAL / FU-A, PT=96, 90 kHz)
//!        ▼
//!   rtp://<client-ip>:<client-port>   (identical wire to the ffmpeg path: PT=96,
//!                                       H.264, 90 kHz, in-band SPS/PPS — so
//!                                       `src/lib/h264.ts` needs NO change)
//! ```
//! When capture and encode are already on the SAME NVIDIA adapter (no AMD iGPU), the
//! shared-texture hop is wasted; `new()` detects a 0x10DE capture adapter and takes the
//! same-device fast path (no bridge, VideoProcessor/NV12/NVENC on the capture device).
//!
//! ## The load-bearing FFI contracts (read before touching anything)
//! 1. We AddRef the NVENC device/context (by *cloning* the windows-rs interface, which
//!    is an `AddRef`) and hand the raw device pointer to `nvEncOpenEncodeSessionEx`.
//!    NVENC does NOT AddRef it, so our clone MUST outlive the encoder and drop LAST.
//!    In the hybrid case that device is the NVIDIA clone (NOT the AMD capture device).
//! 2. NVENC needs an NV12 D3D11 input surface, NOT BGRA. So the BGRA→NV12 conversion
//!    is OUR job: an `ID3D11VideoProcessor` on the NVENC device, Blt'ing the (post-hop)
//!    BGRA into a single NV12 texture WE allocate (no ffmpeg hwframe pool exists here).
//! 3. The cross-adapter bridge is an NT-handle keyed-mutex texture: created with
//!    `CreateSharedHandle` (NOT `GetSharedHandle`) and opened with `OpenSharedResource1`
//!    (NOT `OpenSharedResource`). A plain shared handle would not interop here.
//! 4. NVENC struct `version` words are version-stamped magic. A wrong word ⇒
//!    `NV_ENC_ERR_INVALID_VERSION`. Every struct sets `version` via the researched
//!    12.x `_VER` table below; the big [out]-bearing structs carry the high bit.
//!
//! Everything here is `unsafe` FFI; the risky lines are commented inline. On ANY
//! init failure `new()` returns `Err(String)` so the host falls back to ffmpeg.
//!
//! ## Module layout
//! This module is split across submodules (all behaviour-preserving, same paths):
//! - [`encoder`] — the `Encoder` struct + teardown (`flush_and_close` / `Drop`).
//! - [`new`] — `Encoder::new` (build the whole NVENC chain).
//! - [`submit`] — `Encoder::submit` (one paced tick) + the BGRA→NV12 Blt.
//! - [`d3d`] — free D3D11/DXGI helpers + the cross-adapter bridge + the NVENC status check.
//! - [`nvenc`] — the hand-rolled NVENC FFI bindings.

#![allow(clippy::missing_safety_doc)]
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]
#![cfg(windows)]

use crate::Codec;

mod d3d;
mod encoder;
mod new;
pub(crate) mod nvenc;
mod submit;

// Re-export the public API at the original `crate::encode::*` paths. `lib.rs` uses
// `encode::Encoder` and `encode::EncParams`; `Encoder` lives in the `encoder` submodule.
pub use encoder::Encoder;

/// PCI vendor ID of an NVIDIA adapter (the only vendor whose D3D11 device NVENC will
/// accept in `nvEncOpenEncodeSessionEx`). 0x1002 = AMD, 0x8086 = Intel (display owners).
const VENDOR_NVIDIA: u32 = 0x10DE;

/// Which codecs THIS machine's NVENC can actually encode — asked of the NVENC SDK itself
/// (open a session, enumerate its encode GUIDs, close). Empty = no usable NVENC at all.
///
/// This is the only oracle that answers correctly on both classes name/ffmpeg detection
/// gets wrong: a GP108 (GeForce MX1xx–MX3xx) has NO NVENC silicon yet ffmpeg lists
/// `h264_nvenc`, while a hybrid laptop HAS NVENC yet an `ffmpeg -c:v h264_nvenc` one-frame
/// probe can fail because ffmpeg targets the display GPU. It is also per-CODEC, so a
/// Kepler/GM107 card (H.264-only NVENC) is never advertised as an HEVC encoder — claiming
/// HEVC there and then silently degrading the wire to H.264 is exactly the codec mismatch
/// that renders a permanent black screen on the viewer. Cached for the process lifetime.
pub fn nvenc_codecs() -> &'static [Codec] {
	use std::sync::OnceLock;
	static CACHE: OnceLock<Vec<Codec>> = OnceLock::new();
	CACHE.get_or_init(|| match unsafe { probe_nvenc_codecs() } {
		Ok(c) => {
			cap_dbg(&format!("nvenc_codecs: {c:?}"));
			c
		}
		Err(e) => {
			cap_dbg(&format!("nvenc_codecs: unsupported ({e})"));
			Vec::new()
		}
	})
}

/// True when NVENC can encode anything at all on this machine.
pub fn nvenc_supported() -> bool {
	!nvenc_codecs().is_empty()
}

/// Open one NVENC session on a fresh NVIDIA D3D11 device, enumerate the encode GUIDs it
/// supports, close it. Err = no usable NVENC (no NVIDIA adapter, no driver DLL, or no
/// encode silicon).
unsafe fn probe_nvenc_codecs() -> Result<Vec<Codec>, String> {
	use windows_core::Interface;
	// Keep the device alive for the whole session — NVENC does not AddRef it.
	let (device, _ctx) = d3d::create_nvidia_device()?;
	let lib =
		libloading::Library::new("nvEncodeAPI64.dll").map_err(|e| format!("nvenc dll: {e}"))?;
	let create_instance: libloading::Symbol<
		unsafe extern "C" fn(*mut nvenc::NV_ENCODE_API_FUNCTION_LIST) -> nvenc::NVENCSTATUS,
	> = lib
		.get(b"NvEncodeAPICreateInstance\0")
		.map_err(|e| format!("nvenc CreateInstance sym: {e}"))?;
	let mut fns: Box<nvenc::NV_ENCODE_API_FUNCTION_LIST> = Box::new(std::mem::zeroed());
	fns.version = nvenc::NV_ENCODE_API_FUNCTION_LIST_VER;
	d3d::chk(create_instance(&mut *fns), &fns, std::ptr::null_mut())
		.map_err(|e| format!("NvEncodeAPICreateInstance: {e}"))?;
	let mut open: nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
	open.version = nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
	open.deviceType = nvenc::NV_ENC_DEVICE_TYPE_DIRECTX;
	open.device = device.as_raw();
	open.apiVersion = nvenc::NVENCAPI_VERSION;
	let mut enc: *mut std::ffi::c_void = std::ptr::null_mut();
	let open_fn = fns
		.nvEncOpenEncodeSessionEx
		.ok_or("nvenc: nvEncOpenEncodeSessionEx missing")?;
	d3d::chk(open_fn(&mut open, &mut enc), &fns, std::ptr::null_mut())
		.map_err(|e| format!("nvEncOpenEncodeSessionEx: {e}"))?;
	// From here every exit must DestroyEncoder — collect into a Result and destroy once.
	let guids = (|| -> Result<Vec<windows_core::GUID>, String> {
		let count_fn = fns
			.nvEncGetEncodeGUIDCount
			.ok_or("nvenc: nvEncGetEncodeGUIDCount missing")?;
		let list_fn = fns
			.nvEncGetEncodeGUIDs
			.ok_or("nvenc: nvEncGetEncodeGUIDs missing")?;
		let mut n: u32 = 0;
		d3d::chk(count_fn(enc, &mut n), &fns, enc).map_err(|e| format!("GetEncodeGUIDCount: {e}"))?;
		if n == 0 {
			return Ok(Vec::new());
		}
		let mut buf: Vec<windows_core::GUID> = vec![windows_core::GUID::zeroed(); n as usize];
		let mut got: u32 = 0;
		d3d::chk(list_fn(enc, buf.as_mut_ptr(), n, &mut got), &fns, enc)
			.map_err(|e| format!("GetEncodeGUIDs: {e}"))?;
		buf.truncate(got as usize);
		Ok(buf)
	})();
	if let Some(destroy) = fns.nvEncDestroyEncoder {
		let _ = destroy(enc);
	}
	let guids = guids?;
	let mut codecs = Vec::new();
	for (guid, codec) in [
		(nvenc::NV_ENC_CODEC_H264_GUID, Codec::H264),
		(nvenc::NV_ENC_CODEC_HEVC_GUID, Codec::H265),
		(nvenc::NV_ENC_CODEC_AV1_GUID, Codec::Av1),
	] {
		if guids.contains(&guid) {
			codecs.push(codec);
		}
	}
	Ok(codecs)
}

/// Diagnostic log to a fixed file (the native encoder runs in the GUI host with no console).
fn cap_dbg(msg: &str) {
	use std::io::Write;
	if let Ok(mut f) = std::fs::OpenOptions::new()
		.create(true)
		.append(true)
		.open("C:\\Users\\Public\\pulsar-capture-dbg.txt")
	{
		let _ = writeln!(f, "{msg}");
	}
}

// ===========================================================================
// Tunables the host derives 1:1 from `on_stream` (see `lib.rs` adapter).
// ===========================================================================
pub struct EncParams {
	/// Encode (output) size — may be downscaled from native (auto caps at 1080p).
	pub width: u32,
	pub height: u32,
	/// Native capture size (duplicated output / pool texture). When != encode size the
	/// VideoProcessor Blt scales native→encode; the cross-adapter bridge is sized to THIS so
	/// the AMD→NVIDIA `CopyResource` of the native `frame.texture` matches (hybrid bug fix).
	pub capture_width: u32,
	pub capture_height: u32,
	pub fps: u32,
	pub bitrate_kbps: u32,
	pub dest: String, // "rtp://10.0.0.5:9000"
	pub codec: Codec, // native NVENC: H264 + HEVC + AV1 (AV1 needs Ada/Ampere+; else Err → ffmpeg)
	pub low_latency: bool,
	/// Host display rotation (degrees CW: 0/90/180/270). The BGRA→NV12 Blt rotates the captured
	/// frame by this so the STREAM is already upright for the viewer (no client-side rotation).
	pub rotation: u32,
}
