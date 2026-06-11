//! NV12 → swapchain RGB present via `ID3D11VideoProcessor` (GPU colour-convert + scale, the
//! reverse of `pulsar-capture`'s BGRA→NV12 Blt). Zero-copy: the decoder's NV12 texture is the
//! VideoProcessor input; the swapchain back buffer is the output. Letterboxed to preserve aspect.

#![allow(dead_code)]

use windows::core::{Interface, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
	ID3D11Device, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
	ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
	D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
	D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
	D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
	D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
	D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
	D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL;

/// GPU NV12→RGB scaler/letterboxer for the present path.
pub struct Present {
	vdevice: ID3D11VideoDevice,
	vctx: ID3D11VideoContext,
	vp_enum: ID3D11VideoProcessorEnumerator,
	vproc: ID3D11VideoProcessor,
	in_w: u32,
	in_h: u32,
	out_w: u32,
	out_h: u32,
}

impl Present {
	pub unsafe fn new(
		device: &ID3D11Device,
		in_w: u32,
		in_h: u32,
		out_w: u32,
		out_h: u32,
	) -> Result<Self> {
		let vdevice: ID3D11VideoDevice = device.cast()?;
		let ctx = device.GetImmediateContext()?;
		let vctx: ID3D11VideoContext = ctx.cast()?;
		let (vp_enum, vproc) = Self::build(&vdevice, in_w, in_h, out_w, out_h)?;
		// Black bars for letterboxing.
		vctx.VideoProcessorSetOutputBackgroundColor(
			&vproc,
			false,
			&windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR {
				Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR_0 {
					RGBA: windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR_RGBA {
						R: 0.0,
						G: 0.0,
						B: 0.0,
						A: 1.0,
					},
				},
			},
		);
		Ok(Self {
			vdevice,
			vctx,
			vp_enum,
			vproc,
			in_w,
			in_h,
			out_w,
			out_h,
		})
	}

	unsafe fn build(
		vdevice: &ID3D11VideoDevice,
		in_w: u32,
		in_h: u32,
		out_w: u32,
		out_h: u32,
	) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor)> {
		let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
			InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
			InputFrameRate: DXGI_RATIONAL {
				Numerator: 60,
				Denominator: 1,
			},
			InputWidth: in_w.max(1),
			InputHeight: in_h.max(1),
			OutputFrameRate: DXGI_RATIONAL {
				Numerator: 60,
				Denominator: 1,
			},
			OutputWidth: out_w.max(1),
			OutputHeight: out_h.max(1),
			Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
		};
		let vp_enum = vdevice.CreateVideoProcessorEnumerator(&content)?;
		let vproc = vdevice.CreateVideoProcessor(&vp_enum, 0)?;
		Ok((vp_enum, vproc))
	}

	/// Rebuild the processor if the input (decoded) or output (window) size changed.
	pub unsafe fn resize(&mut self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Result<()> {
		if in_w == self.in_w && in_h == self.in_h && out_w == self.out_w && out_h == self.out_h {
			return Ok(());
		}
		let (e, p) = Self::build(&self.vdevice, in_w, in_h, out_w, out_h)?;
		self.vp_enum = e;
		self.vproc = p;
		self.in_w = in_w;
		self.in_h = in_h;
		self.out_w = out_w;
		self.out_h = out_h;
		Ok(())
	}

	/// Letterbox dest rect (aspect-preserving) of the `in_w×in_h` source inside `out_w×out_h`.
	fn dest_rect(&self) -> RECT {
		let (iw, ih, ow, oh) = (
			self.in_w as f32,
			self.in_h as f32,
			self.out_w as f32,
			self.out_h as f32,
		);
		if iw <= 0.0 || ih <= 0.0 {
			return RECT {
				left: 0,
				top: 0,
				right: self.out_w as i32,
				bottom: self.out_h as i32,
			};
		}
		let scale = (ow / iw).min(oh / ih);
		let w = (iw * scale).round();
		let h = (ih * scale).round();
		let x = ((ow - w) / 2.0).round();
		let y = ((oh - h) / 2.0).round();
		RECT {
			left: x as i32,
			top: y as i32,
			right: (x + w) as i32,
			bottom: (y + h) as i32,
		}
	}

	/// Convert+scale the NV12 `src` onto the swapchain `back` buffer (RGB). One GPU Blt.
	pub unsafe fn blt(&self, src: &ID3D11Texture2D, back: &ID3D11Texture2D) -> Result<()> {
		// Input view onto the NV12 source.
		let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
			FourCC: 0,
			ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
			Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
				Texture2D: D3D11_TEX2D_VPIV {
					MipSlice: 0,
					ArraySlice: 0,
				},
			},
		};
		let mut iv: Option<ID3D11VideoProcessorInputView> = None;
		self.vdevice
			.CreateVideoProcessorInputView(src, &self.vp_enum, &in_desc, Some(&mut iv))?;
		let iv = iv.unwrap();

		// Output view onto the swapchain back buffer.
		let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
			ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
			Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
				Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
			},
		};
		let mut ov: Option<ID3D11VideoProcessorOutputView> = None;
		self.vdevice.CreateVideoProcessorOutputView(
			back,
			&self.vp_enum,
			&out_desc,
			Some(&mut ov),
		)?;
		let ov = ov.unwrap();

		// Letterbox: clear to background + place the video in the aspect-fit dest rect.
		let dst = self.dest_rect();
		self.vctx
			.VideoProcessorSetStreamDestRect(&self.vproc, 0, true, Some(&dst));

		let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
			Enable: true.into(),
			OutputIndex: 0,
			InputFrameOrField: 0,
			PastFrames: 0,
			FutureFrames: 0,
			ppPastSurfaces: std::ptr::null_mut(),
			pInputSurface: std::mem::ManuallyDrop::new(Some(iv)),
			ppFutureSurfaces: std::ptr::null_mut(),
			ppPastSurfacesRight: std::ptr::null_mut(),
			pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
			ppFutureSurfacesRight: std::ptr::null_mut(),
		};
		let r = self
			.vctx
			.VideoProcessorBlt(&self.vproc, &ov, 0, std::slice::from_ref(&stream));
		std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
		std::mem::ManuallyDrop::drop(&mut stream.pInputSurfaceRight);
		r
	}
}
