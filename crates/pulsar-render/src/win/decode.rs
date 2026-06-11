//! Media Foundation DXVA decode: Annex-B access units → NV12 `ID3D11Texture2D` on our D3D11
//! device (zero-copy, no GPU→CPU download).
//!
//! Call sequence (the canonical MF async/hardware decoder dance, kept synchronous here):
//!   MFStartup (once, guarded by Once)
//!   MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, HARDWARE) filtered by input subtype  → IMFActivate
//!     → IMFTransform   (fall back to SOFTWARE if no HW MFT is registered)
//!   MFCreateDXGIDeviceManager + ResetDevice(our ID3D11Device)   (D3D11 must be MT-protected:
//!     ID3D11Multithread::SetMultithreadProtected(true) on the immediate context)
//!   IMFTransform::ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager)         (zero-copy bind)
//!   SetInputType(MFVideoFormat_{H264,HEVC,AV1} + frame size/rate/progressive)
//!   GetOutputAvailableType loop → pick MFVideoFormat_NV12 → SetOutputType
//!   ProcessMessage(NOTIFY_BEGIN_STREAMING) + ProcessMessage(NOTIFY_START_OF_STREAM)
//!   per AU: MFCreateSample(MFCreateMemoryBuffer(copy bytes)) → ProcessInput
//!           → ProcessOutput loop:
//!               MF_E_TRANSFORM_NEED_MORE_INPUT → return what we have
//!               MF_E_TRANSFORM_STREAM_CHANGE   → re-pick NV12 SetOutputType, retry
//!               sample → IMFDXGIBuffer::GetResource(ID3D11Texture2D) + GetSubresourceIndex
//!                       → CopyResource the slice into a fresh single-slice NV12 texture
//!                         (SHADER_RESOURCE|RENDER_TARGET) so the VideoProcessor can read it.

#![allow(dead_code)]

use crate::stream::{AccessUnit, Codec};
use std::sync::Once;
use windows::core::{Interface, Result, GUID};
use windows::Win32::Graphics::Direct3D11::{
	ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
	D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
	D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Media::MediaFoundation::{
	IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample, IMFTransform,
	MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
	MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoFormat_AV1, MFVideoFormat_H264,
	MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_FULL,
	MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
	MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
	MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
	MFT_REGISTER_TYPE_INFO, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
	MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
	MF_VERSION,
};

static MF_INIT: Once = Once::new();

/// Pack two u32 into the hi/lo halves of a u64 the way MF stores 2D attributes
/// (`MF_MT_FRAME_SIZE` = width<<32|height, `MF_MT_FRAME_RATE` = num<<32|den).
#[inline]
fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | (lo as u64)
}

/// Wraps an `IMFTransform` H.264/HEVC/AV1 decoder bound to our D3D11 device via an
/// `IMFDXGIDeviceManager`, producing NV12 textures.
pub struct Decoder {
	transform: IMFTransform,
	device: ID3D11Device,
	context: ID3D11DeviceContext,
	// Keep the device manager alive for the lifetime of the decoder (the MFT holds it weakly
	// via the ULONG_PTR we passed in SET_D3D_MANAGER).
	_manager: IMFDXGIDeviceManager,
	subtype: GUID,
	width: u32,
	height: u32,
	fps: u32,
}

// The decoder runs on a dedicated decode thread. The contained COM interfaces are
// apartment-agnostic (MTA, free-threaded) and the D3D11 device is multithread-protected, so it
// is safe to move to that thread.
unsafe impl Send for Decoder {}

impl Decoder {
	pub fn new(device: &ID3D11Device, codec: Codec, w: u32, h: u32, fps: u32) -> Result<Self> {
		unsafe {
			// 1. Init Media Foundation (idempotent; guarded so multiple decoders are fine).
			MF_INIT.call_once(|| {
				let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
			});

			let subtype = match codec {
				Codec::H264 => MFVideoFormat_H264,
				Codec::H265 => MFVideoFormat_HEVC,
				Codec::Av1 => MFVideoFormat_AV1,
			};

			// 2. Find the decoder MFT for this input subtype. Prefer a hardware DXVA MFT (sync
			//    or async); fall back to a software MFT so the path works on any GPU.
			let transform = create_decoder_mft(subtype)?;

			// 3. Multithread-protect the caller's D3D11 device (it was created WITHOUT it), then
			//    build the DXGI device manager and hand it to the MFT for zero-copy output.
			let context = device.GetImmediateContext()?;
			if let Ok(mt) = context.cast::<ID3D11Multithread>() {
				let _ = mt.SetMultithreadProtected(true);
			}

			let mut reset_token: u32 = 0;
			let mut manager: Option<IMFDXGIDeviceManager> = None;
			MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)?;
			let manager = manager.unwrap();
			manager.ResetDevice(device, reset_token)?;

			// ProcessMessage takes the manager as a ULONG_PTR (usize). The MFT AddRefs it, so we
			// keep our own reference in `_manager` to balance lifetimes.
			transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

			// 4a. Input type: major=Video, subtype=codec, frame size/rate, progressive.
			let input = MFCreateMediaType()?;
			input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
			input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
			input.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(w, h))?;
			input.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(fps.max(1), 1))?;
			input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
			transform.SetInputType(0, &input, 0)?;

			// 4b. Output type: walk the available output types and pick NV12.
			set_nv12_output(&transform)?;

			// 5. Begin streaming.
			transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
			transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

			Ok(Self {
				transform,
				device: device.clone(),
				context,
				_manager: manager,
				subtype,
				width: w,
				height: h,
				fps,
			})
		}
	}

	/// Feed one access unit; drain any decoded NV12 textures (usually 0 or 1).
	pub fn decode(&mut self, au: &AccessUnit) -> Result<Vec<ID3D11Texture2D>> {
		unsafe {
			// 6a. Wrap the Annex-B / OBU bytes in an IMFSample backed by a single memory buffer.
			let sample = MFCreateSample()?;
			let buffer = MFCreateMemoryBuffer(au.data.len().max(1) as u32)?;
			{
				let mut ptr: *mut u8 = std::ptr::null_mut();
				let mut max_len: u32 = 0;
				buffer.Lock(&mut ptr, Some(&mut max_len), None)?;
				if !ptr.is_null() && !au.data.is_empty() {
					std::ptr::copy_nonoverlapping(au.data.as_ptr(), ptr, au.data.len());
				}
				buffer.Unlock()?;
			}
			buffer.SetCurrentLength(au.data.len() as u32)?;
			sample.AddBuffer(&buffer)?;

			// PTS: 90 kHz RTP ticks → 100 ns units. Use widening math to avoid overflow.
			let pts_100ns = (au.pts_90k as i64) * 10_000_000 / 90_000;
			sample.SetSampleTime(pts_100ns)?;

			// 6b. Submit. ProcessInput should normally accept (we drain fully each call).
			self.transform.ProcessInput(0, &sample, 0)?;

			// 6c. Drain all currently-available output frames.
			self.drain()
		}
	}

	/// Pull every ready output sample, converting each to a fresh single-slice NV12 texture.
	unsafe fn drain(&mut self) -> Result<Vec<ID3D11Texture2D>> {
		let mut out = Vec::new();
		loop {
			let mut data = [MFT_OUTPUT_DATA_BUFFER::default(); 1];
			// For a DXGI/D3D11 MFT, the transform allocates the output samples itself
			// (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES), so we leave pSample = null.
			let mut status: u32 = 0;
			let hr = self.transform.ProcessOutput(0, &mut data, &mut status);

			match hr {
				Ok(()) => {
					if let Some(sample) = data[0].pSample.take() {
						if let Ok(tex) = self.sample_to_texture(&sample) {
							out.push(tex);
						}
					}
				}
				Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
					// No more output for now; hand back what we decoded.
					break;
				}
				Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
					// The decoder discovered the real stream geometry; renegotiate NV12 and retry.
					set_nv12_output(&self.transform)?;
					continue;
				}
				Err(e) => return Err(e),
			}
		}
		Ok(out)
	}

	/// Extract the D3D11 NV12 texture from an output sample (zero-copy via IMFDXGIBuffer) and
	/// CopyResource the correct array slice into a fresh single-slice NV12 texture the
	/// downstream VideoProcessor can bind as a shader/render resource.
	unsafe fn sample_to_texture(&self, sample: &IMFSample) -> Result<ID3D11Texture2D> {
		let buffer = sample.GetBufferByIndex(0)?;
		let dxgi: IMFDXGIBuffer = buffer.cast()?;

		// The MFT-owned texture is often a texture-array; GetSubresourceIndex tells us our slice.
		let mut src: Option<ID3D11Texture2D> = None;
		dxgi.GetResource(&ID3D11Texture2D::IID, &mut src as *mut _ as *mut _)?;
		let src = src.unwrap();
		let slice = dxgi.GetSubresourceIndex()?;

		// Describe a single-slice copy with NV12 + SHADER_RESOURCE|RENDER_TARGET bind flags.
		let mut desc = D3D11_TEXTURE2D_DESC::default();
		src.GetDesc(&mut desc);
		desc.ArraySize = 1;
		desc.MipLevels = 1;
		desc.Format = DXGI_FORMAT_NV12;
		desc.Usage = D3D11_USAGE_DEFAULT;
		desc.BindFlags = (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32;
		desc.CPUAccessFlags = 0;
		desc.MiscFlags = 0;

		let mut dst: Option<ID3D11Texture2D> = None;
		self.device.CreateTexture2D(&desc, None, Some(&mut dst))?;
		let dst = dst.unwrap();

		// Copy just our array slice (dst subresource 0 ← src subresource `slice`).
		self.context
			.CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, slice, None);

		Ok(dst)
	}
}

/// Create the decoder `IMFTransform` for `subtype`. Tries a hardware DXVA MFT first (sync or
/// async), then a software MFT, so the path works regardless of GPU vendor.
unsafe fn create_decoder_mft(subtype: GUID) -> Result<IMFTransform> {
	let in_info = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: subtype,
	};

	// Try HW (async + sync), then SW. Each returns a list of IMFActivate.
	let attempts = [
		MFT_ENUM_FLAG_HARDWARE
			| MFT_ENUM_FLAG_ASYNCMFT
			| MFT_ENUM_FLAG_SYNCMFT
			| MFT_ENUM_FLAG_SORTANDFILTER,
		// Software fallback: no HARDWARE flag (there is no SOFTWARE flag in this crate version;
		// omitting HARDWARE yields software/registered MFTs).
		MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
	];

	for flags in attempts {
		let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
		let mut count: u32 = 0;
		let hr = MFTEnumEx(
			MFT_CATEGORY_VIDEO_DECODER,
			flags,
			Some(&in_info),
			None, // any output (we negotiate NV12 ourselves)
			&mut activates,
			&mut count,
		);
		if hr.is_err() || count == 0 || activates.is_null() {
			if !activates.is_null() {
				windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
			}
			continue;
		}

		// Activate the first MFT in the (sorted/filtered) list.
		let list = std::slice::from_raw_parts(activates, count as usize);
		let mut transform: Option<IMFTransform> = None;
		for activate in list {
			if let Some(act) = activate {
				if let Ok(t) = act.ActivateObject::<IMFTransform>() {
					transform = Some(t);
					break;
				}
			}
		}
		windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));

		if let Some(t) = transform {
			return Ok(t);
		}
	}

	Err(windows::core::Error::from(
		windows::Win32::Foundation::E_FAIL,
	))
}

/// Enumerate the MFT's available output types and `SetOutputType` to the first NV12 one.
unsafe fn set_nv12_output(transform: &IMFTransform) -> Result<()> {
	let mut i = 0u32;
	loop {
		match transform.GetOutputAvailableType(0, i) {
			Ok(t) => {
				let sub = t.GetGUID(&MF_MT_SUBTYPE);
				if matches!(sub, Ok(s) if s == MFVideoFormat_NV12) {
					transform.SetOutputType(0, &t, 0)?;
					return Ok(());
				}
				i += 1;
			}
			Err(_) => break, // ran out of available types
		}
	}

	// Fallback: explicitly request NV12 (some MFTs accept a constructed type even if they don't
	// enumerate it before the input is fully known).
	let t = MFCreateMediaType()?;
	t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
	t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
	transform.SetOutputType(0, &t, 0)
}
