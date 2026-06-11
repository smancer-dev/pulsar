//! `Encoder::submit` (one paced tick: cross-adapter hop → BGRA→NV12 → encode → RTP)
//! and its `blt_bgra_to_nv12` helper. Split out of `encode.rs` unchanged; both are
//! `impl Encoder` methods on the type defined in `encoder.rs`.

use std::ptr;

use super::d3d::chk;
use super::encoder::Encoder;
use super::nvenc;
use crate::Frame;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct3D11::{
	ID3D11Texture2D, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
	D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEX2D_VPOV,
	D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
	D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
	D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};

impl Encoder {
	/// Encode ONE paced tick: BGRA→NV12 (GPU), map→encode→lock the bitstream,
	/// packetize the Annex-B AU to RTP, unmap. `is_new==false` still encodes (steady
	/// cadence; NVENC emits a tiny P-frame). Per-frame errors are non-fatal upstream,
	/// but we NEVER leave the input resource mapped on an early return.
	pub unsafe fn submit(&mut self, frame: &Frame, pts: i64) -> Result<(), String> {
		if self.closed {
			return Err("submit after close".into());
		}

		// (a) Get the BGRA the VideoProcessor will read. In the same-device fast path that
		//     is `frame.texture` directly. In the HYBRID path we must first bridge the
		//     capture surface from the AMD device to the NVIDIA device over the shared
		//     keyed-mutex texture (CopyResource is same-device only — this is the ONLY
		//     legal AMD<->NVIDIA copy). The mutex is held ONLY for the two copies, so the
		//     capture loop never blocks on NVENC.
		if self.amd_shared.is_some() {
			// CPU-staging cross-adapter copy (GPU-AGNOSTIC). Keyed-mutex shared textures aren't
			// supported on all AMD↔NVIDIA combos (OpenSharedResource1 E_INVALIDARG), so the hop
			// goes through system RAM: AMD copies the captured frame into a CPU-readable staging,
			// we map it, and upload the pixels to the NVIDIA BGRA the VideoProcessorBlt reads.
			let amd_staging = self.amd_shared.as_ref().unwrap();
			let amd_context = self.amd_context.as_ref().unwrap();
			let nvidia_bgra = self.nvidia_bgra.as_ref().unwrap();
			let nv_context = self._kept_context.as_ref().unwrap();
			// AMD GPU copy: captured BGRA → CPU-readable staging.
			amd_context.CopyResource(amd_staging, frame.texture);
			// Map the staging for readback (stalls until the copy completes — the cross-adapter
			// cost). On a Map failure skip this frame rather than tearing down.
			let mut mapped: D3D11_MAPPED_SUBRESOURCE = std::mem::zeroed();
			amd_context
				.Map(amd_staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
				.map_err(|e| format!("Map(amd_staging): {e}"))?;
			// Upload the mapped BGRA rows to the NVIDIA texture (CPU → NVIDIA GPU). NULL dst box =
			// whole subresource; SrcRowPitch = the AMD staging's row pitch.
			nv_context.UpdateSubresource(nvidia_bgra, 0, None, mapped.pData, mapped.RowPitch, 0);
			amd_context.Unmap(amd_staging, 0);
		}

		// (a.2) GPU colour-convert the (post-hop) BGRA → our single NV12 texture, all on
		//       the NVENC device. Hybrid reads `nvidia_bgra`; fast path reads frame.texture.
		//       If capture size != encode size the Blt scales (input BGRA rect → NV12 out).
		let bgra_src: ID3D11Texture2D = if let Some(nvidia_bgra) = self.nvidia_bgra.as_ref() {
			nvidia_bgra.clone()
		} else {
			frame.texture.clone()
		};
		self.blt_bgra_to_nv12(&bgra_src)?;

		// (b) Map our registered NV12 texture for this encode call.
		let map_fn = self
			.fns
			.nvEncMapInputResource
			.ok_or("nvEncMapInputResource missing")?;
		let mut map: nvenc::NV_ENC_MAP_INPUT_RESOURCE = std::mem::zeroed();
		map.version = nvenc::NV_ENC_MAP_INPUT_RESOURCE_VER;
		map.registeredResource = self.registered;
		chk(map_fn(self.enc, &mut map), &self.fns, self.enc)
			.map_err(|e| format!("nvEncMapInputResource: {e}"))?;
		let mapped = map.mappedResource;
		let mapped_fmt = map.mappedBufferFmt;

		// From here, any error path MUST unmap before returning. We funnel through a
		// closure so a single `unmap` covers every early exit.
		let res = (|| -> Result<(), String> {
			// (c) Encode the picture (SYNC, no B-frames ⇒ one AU emitted per call).
			let enc_fn = self
				.fns
				.nvEncEncodePicture
				.ok_or("nvEncEncodePicture missing")?;
			let mut pic: nvenc::NV_ENC_PIC_PARAMS = std::mem::zeroed();
			pic.version = nvenc::NV_ENC_PIC_PARAMS_VER;
			pic.inputWidth = self.width;
			pic.inputHeight = self.height;
			pic.inputPitch = 0; // ignored for registered DirectX resources
			pic.inputBuffer = mapped;
			pic.bufferFmt = mapped_fmt;
			pic.pictureStruct = nvenc::NV_ENC_PIC_STRUCT_FRAME;
			pic.outputBitstream = self.bitstream;
			pic.inputTimeStamp = pts as u64;
			// Force an IDR + in-band SPS/PPS on the FIRST frame (immediate connect) and then only
			// every `idr_interval` frames (a multi-second safety keyframe, NOT the old fps/4 =
			// 0.25 s tax that hitched the Pi 4×/s). frame_idx==0 ⇒ 0 % interval == 0 ⇒ first frame
			// is always a keyframe, so a joining client still decodes immediately.
			let interval = self.idr_interval.max(1) as u64;
			pic.encodePicFlags = if self.frame_idx % interval == 0 {
				nvenc::NV_ENC_PIC_FLAG_FORCEIDR | nvenc::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
			} else {
				0
			};
			self.frame_idx += 1;
			chk(enc_fn(self.enc, &mut pic), &self.fns, self.enc)
				.map_err(|e| format!("nvEncEncodePicture: {e}"))?;

			// (d) Lock the bitstream → Annex-B access unit.
			let lock_fn = self
				.fns
				.nvEncLockBitstream
				.ok_or("nvEncLockBitstream missing")?;
			let mut lock: nvenc::NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
			lock.version = nvenc::NV_ENC_LOCK_BITSTREAM_VER;
			lock.outputBitstream = self.bitstream;
			lock.set_flags(0); // blocking lock (we want the bytes now)
			chk(lock_fn(self.enc, &mut lock), &self.fns, self.enc)
				.map_err(|e| format!("nvEncLockBitstream: {e}"))?;

			// Hand the Annex-B AU (in the locked buffer, valid only until Unlock) to the
			// dedicated RTP sender thread. `RtpEgress::send_access_unit` copies it into an owned
			// buffer and ENQUEUES — it never touches the socket here, so a slow/wedged UDP send
			// can no longer stall this encode tick. The old code ran the blocking FU-A send loop
			// inline, which wedged this thread ~110 ms on every IDR/GOP (the opi5 freeze→jump).
			// On a full mailbox the stale backlog is dropped (newest-wins); a dropped AU recovers
			// on the next frame / the multi-second safety IDR.
			let size = lock.bitstreamSizeInBytes as usize;
			let data_ptr = lock.bitstreamBufferPtr as *const u8;
			if size > 0 && !data_ptr.is_null() {
				let annexb = std::slice::from_raw_parts(data_ptr, size);
				// DEBUG knob (env-gated, default off): tee the raw Annex-B access unit — exactly the
				// bytes the packetizer is about to fragment — to PULSAR_DUMP_BITSTREAM=<path> so a
				// dump can be inspected NAL-by-NAL (layer_id / nal_unit_type) off the wire. Appends
				// every AU; a non-zero layer_id or a bogus NAL type in the dump implicates the
				// encoder config, a clean dump implicates the packetizer (rtp.rs).
				dump_bitstream(annexb);
				// Rescale pts (frame index, 1/fps units) → 90 kHz RTP ticks. Truncate/wrap;
				// the client tolerates it (it uses ts only for AU grouping/jitter).
				let pts_90k = ((pts as i128 * 90_000) / self.fps as i128) as u32;
				self.rtp.send_access_unit(annexb, pts_90k);
			}

			// Unlock the bitstream (always — the copy inside send_access_unit already took the
			// bytes, so the buffer is free to reuse).
			if let Some(unlock_fn) = self.fns.nvEncUnlockBitstream {
				let _ = unlock_fn(self.enc, self.bitstream);
			}
			Ok(())
		})();

		// (e) ALWAYS unmap the input resource — a mid-frame error must not leak the map.
		if let Some(unmap_fn) = self.fns.nvEncUnmapInputResource {
			let _ = unmap_fn(self.enc, mapped);
		}

		res
	}

	// --- internal helpers -----------------------------------------------------------

	/// One ID3D11VideoProcessorBlt: read `src` (BGRA) → write our NV12 texture (NV12).
	/// This is the GPU colour conversion ddagrab's `format=nv12` filter did before. The
	/// destination is OUR single non-array NV12 texture, so a TEXTURE2D output view.
	unsafe fn blt_bgra_to_nv12(&mut self, src: &ID3D11Texture2D) -> Result<(), String> {
		// Input view onto the BGRA source (whole texture, array slice 0).
		let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
			FourCC: 0,
			ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
			Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
				Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VPIV {
					MipSlice: 0,
					ArraySlice: 0,
				},
			},
		};
		let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
		let vdevice = self._kept_vdevice.as_ref().unwrap();
		vdevice
			.CreateVideoProcessorInputView(src, &self.vp_enum, &in_desc, Some(&mut input_view))
			.map_err(|e| format!("CreateVideoProcessorInputView: {e}"))?;
		let input_view = input_view.ok_or("null input view")?;

		// Output view onto OUR NV12 texture. It is a single non-array Texture2D, so a
		// TEXTURE2D output view (MipSlice 0) — NOT the Texture2DArray slice the ffmpeg
		// hwframe pool needed.
		let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
			ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
			Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
				Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
			},
		};
		let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
		vdevice
			.CreateVideoProcessorOutputView(
				&self.nv12_tex,
				&self.vp_enum,
				&out_desc,
				Some(&mut output_view),
			)
			.map_err(|e| format!("CreateVideoProcessorOutputView: {e}"))?;
		let output_view = output_view.ok_or("null output view")?;

		// Rotate the stream by the host display's orientation so the encoded frame is upright
		// for the viewer (no client-side rotation needed). IDENTITY for 0°. Set on the stream
		// BEFORE the Blt; the output view is already sized to the rotated dims (see new.rs).
		{
			use windows::Win32::Graphics::Direct3D11::{
				D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270,
				D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
			};
			let rot = match self.rotation {
				90 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
				180 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
				270 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
				_ => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
			};
			self.vctx
				.VideoProcessorSetStreamRotation(&self.vproc, 0, true, rot);
		}

		// One input stream. windows-rs models the COM-interface fields as `ManuallyDrop<Option<…>>`. The
		// struct is a non-owning *view* the API only reads during the call, so we MOVE
		// the input_view in (no clone → no extra ref), run the Blt, then ManuallyDrop::drop
		// the field to release it. Skipping that drop would leak one ref PER FRAME.
		let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
			Enable: BOOL(1),
			OutputIndex: 0,
			InputFrameOrField: 0,
			PastFrames: 0,
			FutureFrames: 0,
			ppPastSurfaces: ptr::null_mut(),
			pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
			ppFutureSurfaces: ptr::null_mut(),
			ppPastSurfacesRight: ptr::null_mut(),
			pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
			ppFutureSurfacesRight: ptr::null_mut(),
		};
		// The Blt does the BGRA→NV12 conversion on the GPU (RGB→YUV + chroma subsample).
		// Pass the stream by BORROW (the API only reads it) — do NOT clone the struct.
		let blt = self.vctx.VideoProcessorBlt(
			&self.vproc,
			&output_view,
			0,
			std::slice::from_ref(&stream),
		);
		// Release the moved-in input surface now that the call has returned.
		std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
		std::mem::ManuallyDrop::drop(&mut stream.pInputSurfaceRight);
		blt.map_err(|e| format!("VideoProcessorBlt: {e}"))?;
		Ok(())
	}
}

/// DEBUG: append the raw Annex-B access unit to the file named by `PULSAR_DUMP_BITSTREAM` (if set).
/// Default OFF — a no-op when the env var is unset, so it costs nothing in normal runs. Best-effort:
/// a failed open/write is silently ignored (a diagnostic must never break the encode path). The
/// resulting file is a concatenated Annex-B elementary stream playable/inspectable as `.h265`/`.h264`
/// (e.g. `ffmpeg -i dump.h265`, or a NAL walker that prints each `nal_unit_type` + `nuh_layer_id`).
fn dump_bitstream(annexb: &[u8]) {
	use std::io::Write;
	let Ok(path) = std::env::var("PULSAR_DUMP_BITSTREAM") else {
		return;
	};
	if path.is_empty() {
		return;
	}
	if let Ok(mut f) = std::fs::OpenOptions::new()
		.create(true)
		.append(true)
		.open(&path)
	{
		let _ = f.write_all(annexb);
	}
}
