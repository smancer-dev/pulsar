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

/// View-fit mode for presenting the video (AnyDesk-style), mirroring the Linux
/// backend's `video::FIT_MODE`: 0 = FIT (letterbox, default), 1 = STRETCH (fill,
/// may distort), 2 = ORIGINAL (1:1 source pixels, centered — larger streams crop).
/// The overlay's Display section / the frontend set it (`fit` stdin line or a
/// local `ov set fit` echo); `dest_rect` reads it every Blt.
static FIT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_fit(mode: &str) {
	let v = match mode {
		"stretch" => 1,
		"original" => 2,
		_ => 0,
	};
	FIT_MODE.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn fit_label() -> &'static str {
	match FIT_MODE.load(std::sync::atomic::Ordering::Relaxed) {
		1 => "stretch",
		2 => "original",
		_ => "fit",
	}
}

/// Fit-mode (source, dest) rect pair of an `iw×ih` source inside `ow×oh` —
/// shared by the GPU Blt below and the side-cursor math in `win/mod.rs` so the
/// cursor always lands where the frame is really drawn. Both rects MUST stay
/// inside their surfaces: a dest rect hanging outside the swapchain makes
/// `VideoProcessorBlt` fail every frame, which froze the whole render loop when
/// 1:1 was picked on a window smaller than the stream — so ORIGINAL is a center
/// CROP (source rect shrinks) rather than an oversized dest rect.
pub fn fit_rects(
	iw: f32,
	ih: f32,
	ow: f32,
	oh: f32,
) -> ((f32, f32, f32, f32), (f32, f32, f32, f32)) {
	if iw <= 0.0 || ih <= 0.0 {
		return ((0.0, 0.0, 1.0, 1.0), (0.0, 0.0, ow, oh));
	}
	match FIT_MODE.load(std::sync::atomic::Ordering::Relaxed) {
		1 => ((0.0, 0.0, iw, ih), (0.0, 0.0, ow, oh)),
		2 => {
			let vw = iw.min(ow);
			let vh = ih.min(oh);
			(
				(((iw - vw) / 2.0).round(), ((ih - vh) / 2.0).round(), vw, vh),
				(((ow - vw) / 2.0).round(), ((oh - vh) / 2.0).round(), vw, vh),
			)
		}
		_ => {
			let scale = (ow / iw).min(oh / ih);
			let w = (iw * scale).round();
			let h = (ih * scale).round();
			(
				(0.0, 0.0, iw, ih),
				(((ow - w) / 2.0).round(), ((oh - h) / 2.0).round(), w, h),
			)
		}
	}
}

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
		let s = Self {
			vdevice,
			vctx,
			vp_enum,
			vproc,
			in_w,
			in_h,
			out_w,
			out_h,
		};
		// Black bars for letterboxing.
		s.set_black_background();
		Ok(s)
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

	/// Set the video processor's output background color to opaque black (for letterbox bars).
	/// Must be called on `self` (uses `self.vctx` + `self.vproc`) — not inside `build()` which
	/// has no access to `vctx`.
	unsafe fn set_black_background(&self) {
		self.vctx.VideoProcessorSetOutputBackgroundColor(
			&self.vproc,
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
	}

	/// Rebuild the processor if the input (decoded) or output (window) size changed.
	pub unsafe fn resize(&mut self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Result<()> {
		if in_w == self.in_w && in_h == self.in_h && out_w == self.out_w && out_h == self.out_h {
			return Ok(());
		}
		let (e, p) = Self::build(&self.vdevice, in_w, in_h, out_w, out_h)?;
		self.vp_enum = e;
		self.vproc = p;
		// Re-apply the black background color: build() creates a fresh ID3D11VideoProcessor
		// which resets all state, so the letterbox bars would show swapchain garbage
		// (FLIP_DISCARD = back buffer undefined) without this call.
		self.set_black_background();
		self.in_w = in_w;
		self.in_h = in_h;
		self.out_w = out_w;
		self.out_h = out_h;
		Ok(())
	}

	/// Fit-mode (source, dest) rects for the current sizes (see `fit_rects`).
	fn blt_rects(&self) -> (RECT, RECT) {
		let (s, d) = fit_rects(
			self.in_w as f32,
			self.in_h as f32,
			self.out_w as f32,
			self.out_h as f32,
		);
		let to_rect = |(x, y, w, h): (f32, f32, f32, f32)| RECT {
			left: x as i32,
			top: y as i32,
			right: (x + w) as i32,
			bottom: (y + h) as i32,
		};
		(to_rect(s), to_rect(d))
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

		// Fit-mode rects: source crop (1:1 on a smaller window) + dest placement.
		let (src_r, dst) = self.blt_rects();
		self.vctx
			.VideoProcessorSetStreamSourceRect(&self.vproc, 0, true, Some(&src_r));
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
