//! The `Encoder` struct definition and its teardown (`flush_and_close` + `Drop`).
//! Construction lives in `new.rs` and the per-frame path in `submit.rs`; all three
//! are `impl Encoder` blocks on the type defined here. Behaviour is unchanged from
//! the original `encode.rs`.

use std::ffi::c_void;
use std::ptr;

use windows::Win32::Graphics::Direct3D11::{
	ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
	ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
};

use super::nvenc;
use crate::rtp::RtpEgress;

// ===========================================================================
// Encoder — owns the NVENC session + the GPU colour-converter + the RtpEgress.
// ===========================================================================

/// Owns the whole native NVENC encode chain for the encoder's life. Teardown is
/// in `flush_and_close` (idempotent): destroy NVENC objects, then drop the
/// AddRef'd D3D11 interface clones LAST (so NVENC isn't using a released device).
///
/// In the HYBRID case the NVENC / VideoProcessor / NV12 objects below all live on the
/// NVIDIA device; the `amd_*` / `nv_view` / `nvidia_bgra` / `shared_handle` fields are
/// the cross-adapter bridge. In the same-device fast path the bridge fields are `None`
/// and `frame.texture` is Blt'd directly (no hop).
pub struct Encoder {
	// --- NVENC (NVIDIA device) ---
	pub(super) fns: Box<nvenc::NV_ENCODE_API_FUNCTION_LIST>, // fn-pointer table (keep _lib alive!)
	pub(super) _lib: libloading::Library,                    // nvEncodeAPI64.dll — must outlive fns
	pub(super) enc: *mut c_void,                             // session handle (NV_ENC opaque encoder)
	pub(super) registered: *mut c_void,                      // registeredResource for nv12_tex
	pub(super) bitstream: *mut c_void,                       // output bitstream buffer

	// --- OUR own NV12 target (DEFAULT, BIND_RENDER_TARGET) registered with NVENC.
	//     HYBRID: allocated on the NVIDIA device (NV12 stays private to NVIDIA). ---
	pub(super) nv12_tex: ID3D11Texture2D,

	// --- D3D11 BGRA→NV12 colour converter (ID3D11VideoProcessor) on the NVENC device ---
	pub(super) vctx: ID3D11VideoContext,
	pub(super) vproc: ID3D11VideoProcessor,
	pub(super) vp_enum: ID3D11VideoProcessorEnumerator,

	// --- CROSS-ADAPTER BRIDGE (None in the same-device fast path) — CPU-staging roundtrip.
	//     Keyed-mutex shared textures aren't supported on all AMD↔NVIDIA combos, so the hop
	//     goes AMD→system RAM→NVIDIA (GPU-agnostic). See d3d.rs CrossAdapterBridge.
	/// AMD STAGING texture (CPU_READ): the AMD context copies `frame.texture` into this, then
	/// `submit` maps it for readback.
	pub(super) amd_shared: Option<ID3D11Texture2D>,
	/// AMD immediate context that performs the `frame.texture → amd_shared` copy + the Map.
	pub(super) amd_context: Option<ID3D11DeviceContext>,
	/// NVIDIA-side BGRA (DEFAULT): the mapped pixels are uploaded here (UpdateSubresource) and
	/// the VideoProcessorBlt reads it as the BGRA→NV12 source.
	pub(super) nvidia_bgra: Option<ID3D11Texture2D>,

	// --- transport ---
	/// Decoupled RTP egress: the blocking UDP send runs on a dedicated `pulsar-rtp-send`
	/// thread, so an IDR packet burst can no longer stall this encode thread (~110 ms/GOP).
	/// Dropping it (with the Encoder) closes the mailbox + joins that thread.
	pub(super) rtp: RtpEgress,
	/// Live target bitrate (kbps), shared with the RtpEgress pacing thread so Stage-1 pacing
	/// tracks it. Stage-3 adaptive bitrate updates this atom + calls `reconfigure_bitrate`.
	pub(super) bitrate_kbps: std::sync::Arc<std::sync::atomic::AtomicU32>,
	/// Retained NVENC init params + config (Boxed → STABLE heap address) so Stage-3 adaptive
	/// bitrate can `nvEncReconfigureEncoder` live. `init_params.encodeConfig` points into
	/// `enc_config`; both must outlive the encoder and never move (Box guarantees the address).
	pub(super) init_params: Box<nvenc::NV_ENC_INITIALIZE_PARAMS>,
	pub(super) enc_config: Box<nvenc::NV_ENC_CONFIG>,

	pub(super) width: u32,
	pub(super) height: u32,
	/// Host display rotation (deg CW: 0/90/180/270) applied in the BGRA→NV12 Blt so the stream
	/// is upright for the viewer regardless of the host screen's physical orientation.
	pub(super) rotation: u32,
	pub(super) fps: u32,
	/// Forced-IDR interval in frames (= gopLength). Keyframes are rare (connect + this safety
	/// period), not a per-quarter-second tax — see the GOP comment in `new`.
	pub(super) idr_interval: u32,

	// --- AddRef'd interface clones; DROP LAST (NVENC doesn't AddRef the device).
	//     HYBRID: these are the NVIDIA device/context/video-device/video-context. ---
	pub(super) _kept_device: Option<ID3D11Device>,
	pub(super) _kept_context: Option<ID3D11DeviceContext>,
	pub(super) _kept_vdevice: Option<ID3D11VideoDevice>,
	pub(super) _kept_vcontext: Option<ID3D11VideoContext>,
	/// AMD capture device/context clones — kept alive so `amd_shared` + `amd_mutex`
	/// outlive the bridge. Dropped after the shared handle is closed. `None` fast path.
	pub(super) _kept_amd_device: Option<ID3D11Device>,
	pub(super) _kept_amd_context: Option<ID3D11DeviceContext>,

	pub(super) closed: bool,
	/// Frame counter for periodic FORCED IDR + SPS/PPS (gopLength config alone proved
	/// ineffective → infinite GOP → late-joining RTP clients never get an IDR).
	pub(super) frame_idx: u64,
	/// One-shot forced-IDR request (set by `request_idr`, consumed in `submit`). Set by the
	/// capture loop after a SAME-resolution duplication reinit (host refresh-rate change /
	/// transient ACCESS_LOST) so the client gets a keyframe at once instead of freezing until
	/// the next multi-second safety GOP. A true RESOLUTION change rebuilds the whole encoder
	/// (capture loop) instead, since the NV12/VideoProcessor are sized to the old dimensions.
	pub(super) force_idr_once: bool,
}

// The encoder lives entirely on the capture thread; the raw NVENC/COM pointers are
// not shared. We mark Send so `lib.rs` can move it into the spawned thread.
unsafe impl Send for Encoder {}

impl Encoder {
	/// Live bitrate change (Stage-3 adaptive bitrate) via `nvEncReconfigureEncoder` — NO session
	/// re-init (cheap; encodeWidth/Height/GUIDs unchanged). Updates the retained CBR rate params,
	/// forces an IDR so the client recovers cleanly, and pushes the new bitrate into the RtpEgress
	/// pacing atom so packet pacing tracks it. Returns Err on a bad reconfigure (caller logs; the
	/// old bitrate stays in effect).
	/// Request that the NEXT submitted frame be a forced IDR (+ in-band SPS/PPS). One-shot,
	/// consumed in `submit`. Cheap (a flag) — the capture loop calls it after a same-resolution
	/// reinit so a client mid-GOP re-syncs immediately rather than waiting the safety interval.
	pub fn request_idr(&mut self) {
		self.force_idr_once = true;
	}

	pub unsafe fn reconfigure_bitrate(&mut self, kbps: u32) -> Result<(), String> {
		if self.closed {
			return Err("reconfigure after close".into());
		}
		let bps = kbps.saturating_mul(1000).max(1);
		let vbv = bps / self.fps.max(1); // single-frame VBV (low latency), mirrors new.rs
		self.enc_config.rcParams.averageBitRate = bps;
		self.enc_config.rcParams.maxBitRate = bps;
		self.enc_config.rcParams.vbvBufferSize = vbv;
		self.enc_config.rcParams.vbvInitialDelay = vbv;
		// Re-assert the self-referential pointer (defensive; Box address is stable anyway).
		self.init_params.encodeConfig = &mut *self.enc_config;
		let mut rc: nvenc::NV_ENC_RECONFIGURE_PARAMS = std::mem::zeroed();
		rc.version = nvenc::NV_ENC_RECONFIGURE_PARAMS_VER;
		// Bitwise copy of the retained init params (not Copy; plain repr(C), no Drop → safe).
		// The copied `encodeConfig` still points into our stable boxed `enc_config`.
		rc.reInitEncodeParams = std::ptr::read(&*self.init_params);
		rc.set_reset_encoder(false);
		rc.set_force_idr(true);
		let f = self
			.fns
			.nvEncReconfigureEncoder
			.ok_or("nvenc: nvEncReconfigureEncoder missing")?;
		super::d3d::chk(f(self.enc, &mut rc), &self.fns, self.enc)
			.map_err(|e| format!("nvEncReconfigureEncoder({kbps} kbps): {e}"))?;
		// Pacing (Stage 1) reads this for packets_per_ms; keep it in lockstep with the encoder.
		self.bitrate_kbps
			.store(kbps, std::sync::atomic::Ordering::Relaxed);
		Ok(())
	}

	/// Tear down the NVENC session + resources. Idempotent — safe to call from both
	/// `stop()` and `Drop`. No encoder drain needed (SYNC + no B-frames ⇒ every frame
	/// already emitted). The `RtpEgress` field drops with the struct — closing its mailbox
	/// and joining the `pulsar-rtp-send` thread (which closes the UDP socket); there is no
	/// muxer, so no trailer.
	pub unsafe fn flush_and_close(&mut self) {
		if self.closed {
			return;
		}
		self.closed = true;

		// Destroy NVENC objects in reverse creation order: bitstream → registration →
		// encoder. Unregister BEFORE DestroyEncoder (the registration belongs to enc).
		if !self.bitstream.is_null() {
			if let Some(f) = self.fns.nvEncDestroyBitstreamBuffer {
				let _ = f(self.enc, self.bitstream);
			}
			self.bitstream = ptr::null_mut();
		}
		if !self.registered.is_null() {
			if let Some(f) = self.fns.nvEncUnregisterResource {
				let _ = f(self.enc, self.registered);
			}
			self.registered = ptr::null_mut();
		}
		if !self.enc.is_null() {
			if let Some(f) = self.fns.nvEncDestroyEncoder {
				let _ = f(self.enc);
			}
			self.enc = ptr::null_mut();
		}

		// -- CROSS-ADAPTER BRIDGE teardown (hybrid only; all None in the fast path). ----
		// Order matters: drop the NVIDIA-side views/mutex/staging + the NV12/VideoProcessor
		// (they live on the NVIDIA device and were touched by NVENC, now destroyed), THEN
		// close the NT shared handle, THEN drop the AMD-side shared texture + mutex. The
		// handle is closed only AFTER both keyed-mutex textures' interface refs are released
		// here (we drop the Options below before CloseHandle).
		//
		// CPU-staging bridge teardown: just plain textures + the AMD context — no NT shared
		// handle or keyed mutex exist in this path, so dropping the Options IS the teardown.
		self.nvidia_bgra = None;
		self.amd_shared = None;
		self.amd_context = None;

		// FINALLY drop our AddRef'd interface clones (balances our AddRef in `new`).
		// These MUST drop AFTER nvEncDestroyEncoder so NVENC isn't using a released
		// device. nv12_tex / vproc / vp_enum / vctx (windows-rs handles) Release on
		// struct drop — fine after the NVENC objects are gone. Drop the NVIDIA video
		// ctx/device first, then the NVIDIA device/context (the ones NVENC held a raw
		// pointer to) LAST among NVENC-touched objects, then the AMD capture clones.
		self._kept_vcontext = None;
		self._kept_vdevice = None;
		self._kept_context = None;
		self._kept_device = None;
		// AMD capture device/context outlived the bridge; release them last of all.
		self._kept_amd_context = None;
		self._kept_amd_device = None;
	}
}

impl Drop for Encoder {
	fn drop(&mut self) {
		unsafe { self.flush_and_close() };
	}
}
