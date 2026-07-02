//! Hardware cursor capture + GPU compositing (Sunshine technique).
//!
//! DXGI Desktop Duplication delivers the mouse cursor SEPARATELY from the desktop
//! image (so the encoder could draw it client-side); when we want it baked into the
//! stream we composite it ourselves. This mirrors Sunshine's `display_vram.cpp`:
//!   - cache the pointer POSITION every frame (cheap; from DXGI_OUTDUPL_FRAME_INFO),
//!   - re-fetch + decode the pointer SHAPE only when it changes (PointerShapeBufferSize>0),
//!   - convert COLOR / MASKED_COLOR / MONOCHROME shapes into up to two BGRA images
//!     (an alpha-blended image + an invert/XOR image — Windows cursors carry an XOR
//!     component plain alpha can't express),
//!   - blend them onto the frame with a viewport-positioned fullscreen-triangle pass.
//!
//! KEY TRICK: the cursor quad is NOT translated in the vertex shader. The VS emits a
//! fullscreen triangle with tex_coords spanning [0,1]; the cursor lands at the right
//! place/size purely via an `RSSetViewports` viewport set to the cursor's screen rect.

use windows::core::PCSTR;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
	ID3D11BlendState, ID3D11Device, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState,
	ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_SHADER_RESOURCE,
	D3D11_BLEND_DESC, D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_INV_SRC_ALPHA,
	D3D11_BLEND_INV_SRC_COLOR, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_ZERO,
	D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RENDER_TARGET_BLEND_DESC,
	D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
	D3D11_USAGE_IMMUTABLE, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
	IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, CURSORINFO, CURSOR_SHOWING};

use super::cursor_shape::{make_cursor_alpha_image, make_cursor_xor_image};
use super::device::CaptureDevice;

/// HLSL for the cursor blit. Fullscreen triangle (3 verts) with tex in [0,1]; the PS
/// just samples the cursor texture. Positioning is done by the viewport (see above).
const CURSOR_SHADER_HLSL: &[u8] = b"\
struct vertex_t { float4 pos : SV_Position; float2 tex : TEXCOORD; };\n\
vertex_t main_vs(uint id : SV_VertexID) {\n\
    vertex_t o;\n\
    if (id == 0)      { o.pos = float4(-1,-1,0,1); o.tex = float2(0,1); }\n\
    else if (id == 1) { o.pos = float4(-1, 3,0,1); o.tex = float2(0,-1); }\n\
    else              { o.pos = float4( 3,-1,0,1); o.tex = float2(2,1); }\n\
    return o;\n\
}\n\
Texture2D cursor : register(t0);\n\
SamplerState samp : register(s0);\n\
float4 main_ps(vertex_t i) : SV_Target { return cursor.Sample(samp, i.tex); }\n\0";

/// The GPU pipeline objects used to blend the cursor — built once (lazily, on the first
/// frame where `draw_cursor` is set) and reused. Failure to build any of these just
/// disables cursor drawing; capture is never broken.
pub(super) struct CursorCompositor {
	vs: ID3D11VertexShader,
	ps: ID3D11PixelShader,
	sampler: ID3D11SamplerState,
	/// Straight src-over alpha blend (draws the opaque body of the cursor).
	blend_alpha: ID3D11BlendState,
	/// Invert blend (XORs the destination where the cursor is white) for MASKED/MONO.
	blend_invert: ID3D11BlendState,
}

impl CursorCompositor {
	/// Compile the shaders + create the sampler & blend states. Called once.
	unsafe fn create(device: &ID3D11Device) -> windows::core::Result<Self> {
		let vs_blob = compile_shader(CURSOR_SHADER_HLSL, b"main_vs\0", b"vs_5_0\0")?;
		let ps_blob = compile_shader(CURSOR_SHADER_HLSL, b"main_ps\0", b"ps_5_0\0")?;

		let mut vs: Option<ID3D11VertexShader> = None;
		device.CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))?;
		let mut ps: Option<ID3D11PixelShader> = None;
		device.CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut ps))?;

		// Linear-clamp sampler (matches Sunshine init L1565). Clamp so the [0,1] tex of the
		// fullscreen triangle never wraps the small cursor texture.
		let samp_desc = D3D11_SAMPLER_DESC {
			Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
			AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
			AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
			AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
			MaxLOD: f32::MAX,
			..Default::default()
		};
		let mut sampler: Option<ID3D11SamplerState> = None;
		device.CreateSamplerState(&samp_desc, Some(&mut sampler))?;

		let blend_alpha = make_blend(device, false)?;
		let blend_invert = make_blend(device, true)?;

		Ok(CursorCompositor {
			vs: vs.unwrap(),
			ps: ps.unwrap(),
			sampler: sampler.unwrap(),
			blend_alpha,
			blend_invert,
		})
	}
}

/// Cached cursor position + decoded shape + the GPU textures derived from it. Lives on the
/// `CaptureDevice`; position is updated every frame, the textures only on a shape change.
#[derive(Default)]
pub(super) struct CursorState {
	/// Whether the cursor is currently shown on THIS duplicated output. Only meaningful once
	/// a position update has arrived (LastMouseUpdateTime != 0); when the pointer moves to
	/// another monitor it goes invisible but we keep the cached shape.
	visible: bool,
	/// Top-left position of the cursor in this output's pixel space (DXGI pointer position).
	pos_x: i32,
	pos_y: i32,
	/// Cursor hotspot (from the DXGI shape). `GetCursorInfo` reports the hotspot screen point;
	/// subtract this to get the bitmap's top-left (DXGI's PointerPosition is already top-left).
	hot_x: i32,
	hot_y: i32,

	/// Logical (UNROTATED) cursor pixel size (for MONOCHROME the height is already halved).
	/// Drives the blend viewport — at 0° the texture is rendered at exactly this size at
	/// (pos_x, pos_y); under a rotated host the uploaded texture is the pre-rotated bitmap
	/// (dims swapped for 90/270) and `composite_cursor` derives the scan-out viewport from this.
	tex_w: u32,
	tex_h: u32,
	/// Alpha-blended image (always present for a valid shape).
	tex_alpha: Option<ID3D11ShaderResourceView>,
	/// Invert/XOR image (only MASKED_COLOR / MONOCHROME).
	tex_xor: Option<ID3D11ShaderResourceView>,
}

/// `D3DCompile` wrapper. `entry`/`target` are NUL-terminated byte strings. Returns the
/// compiled bytecode blob; on failure the HRESULT is surfaced (the error blob is dropped —
/// we only need the Result for the "fall back to no cursor" path).
unsafe fn compile_shader(
	src: &[u8],
	entry: &[u8],
	target: &[u8],
) -> windows::core::Result<ID3DBlob> {
	let mut blob: Option<ID3DBlob> = None;
	let mut errblob: Option<ID3DBlob> = None;
	// src includes a trailing NUL; pass the byte length WITHOUT it as the source size.
	let src_len = src.len().saturating_sub(1);
	let res = D3DCompile(
		src.as_ptr() as *const _,
		src_len,
		PCSTR::null(), // source name (for diagnostics) — none
		None,          // #defines — none
		None,          // include handler — none
		PCSTR(entry.as_ptr()),
		PCSTR(target.as_ptr()),
		D3DCOMPILE_ENABLE_STRICTNESS,
		0,
		&mut blob,
		Some(&mut errblob),
	);
	res?;
	// On S_OK the blob is always populated.
	Ok(blob.unwrap())
}

/// View a compiled `ID3DBlob` as the `&[u8]` slice `CreateVertexShader`/`CreatePixelShader`
/// want (they take the bytecode by pointer+len).
unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
	std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
}

/// Build a render-target blend state. `invert=false` → straight src-over alpha blend;
/// `invert=true` → the XOR/invert blend (inverts dest where the cursor texture is white).
/// Mirrors Sunshine `make_blend` (display_vram.cpp L72).
unsafe fn make_blend(
	device: &ID3D11Device,
	invert: bool,
) -> windows::core::Result<ID3D11BlendState> {
	let mut rt = D3D11_RENDER_TARGET_BLEND_DESC {
		BlendEnable: true.into(),
		// .0 of the flag newtype, narrowed to the u8 mask field.
		RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
		BlendOp: D3D11_BLEND_OP_ADD,
		BlendOpAlpha: D3D11_BLEND_OP_ADD,
		SrcBlendAlpha: D3D11_BLEND_ZERO,
		DestBlendAlpha: D3D11_BLEND_ZERO,
		..Default::default()
	};
	if invert {
		rt.SrcBlend = D3D11_BLEND_INV_DEST_COLOR;
		rt.DestBlend = D3D11_BLEND_INV_SRC_COLOR;
	} else {
		rt.SrcBlend = D3D11_BLEND_SRC_ALPHA;
		rt.DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
	}
	let mut desc = D3D11_BLEND_DESC::default();
	desc.RenderTarget[0] = rt;
	let mut blend: Option<ID3D11BlendState> = None;
	device.CreateBlendState(&desc, Some(&mut blend))?;
	Ok(blend.unwrap())
}

/// Upload a packed BGRA image to an immutable D3D11 texture and return a shader-resource view
/// over it. Used for both the alpha and the xor cursor images (Sunshine `set_cursor_texture`).
unsafe fn make_cursor_srv(
	device: &ID3D11Device,
	bgra: &[u8],
	width: u32,
	height: u32,
) -> windows::core::Result<ID3D11ShaderResourceView> {
	let desc = D3D11_TEXTURE2D_DESC {
		Width: width,
		Height: height,
		MipLevels: 1,
		ArraySize: 1,
		Format: DXGI_FORMAT_B8G8R8A8_UNORM,
		SampleDesc: DXGI_SAMPLE_DESC {
			Count: 1,
			Quality: 0,
		},
		Usage: D3D11_USAGE_IMMUTABLE,
		BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
		..Default::default()
	};
	let init = D3D11_SUBRESOURCE_DATA {
		pSysMem: bgra.as_ptr() as *const _,
		SysMemPitch: width * 4, // our images are tightly packed (row pitch = width*4)
		SysMemSlicePitch: 0,
	};
	let mut tex: Option<ID3D11Texture2D> = None;
	device.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
	let tex = tex.unwrap();
	let mut srv: Option<ID3D11ShaderResourceView> = None;
	device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
	Ok(srv.unwrap())
}

// ---------------------------------------------------------------------------
// CaptureDevice cursor methods (cache refresh + GPU compositing).
// ---------------------------------------------------------------------------

/// Pre-rotate a tightly-packed BGRA cursor image (row pitch = w*4) by `deg` degrees
/// **counter-clockwise** so that after the encoder's clockwise bake (submit.rs
/// `VideoProcessorSetStreamRotation`) the cursor appears UPRIGHT at the true pointer
/// position. Returns `(rotated_bytes, out_w, out_h)`; for 90/270 the dims are swapped
/// (`out_w = h`, `out_h = w`). `deg` is the host display rotation (the amount the encoder
/// will rotate the present surface CW); we apply the inverse here so the two cancel:
///   - 0   → identity (caller skips this entirely)
///   - 90  → rotate the bitmap 90° CCW (encoder bakes 90° CW)
///   - 180 → rotate 180° (its own inverse)
///   - 270 → rotate 270° CCW = 90° CW (encoder bakes 270° CW)
/// A malformed (too-short) buffer is returned as an unchanged copy (no panic).
fn rotate_bgra(src: &[u8], w: usize, h: usize, deg: u32) -> (Vec<u8>, u32, u32) {
	let n = w.saturating_mul(h);
	if src.len() < n * 4 {
		return (src.to_vec(), w as u32, h as u32);
	}
	let px = |x: usize, y: usize| {
		let s = (y * w + x) * 4;
		[src[s], src[s + 1], src[s + 2], src[s + 3]]
	};
	let (ow, oh) = match deg {
		90 | 270 => (h, w),
		_ => (w, h),
	};
	let mut out = vec![0u8; ow * oh * 4];
	let mut put = |ox: usize, oy: usize, p: [u8; 4]| {
		let d = (oy * ow + ox) * 4;
		out[d..d + 4].copy_from_slice(&p);
	};
	match deg {
		// 90° CCW: out dims h×w. out(ox,oy) ← in(x = w-1-oy, y = ox).
		90 => {
			for y in 0..h {
				for x in 0..w {
					put(y, w - 1 - x, px(x, y));
				}
			}
		}
		// 270° CCW (= 90° CW): out dims h×w. out(ox,oy) ← in(x = oy, y = h-1-ox).
		270 => {
			for y in 0..h {
				for x in 0..w {
					put(h - 1 - y, x, px(x, y));
				}
			}
		}
		// 180°: reverse the pixel sequence (mirror both axes); dims unchanged.
		_ => {
			for y in 0..h {
				for x in 0..w {
					put(w - 1 - x, h - 1 - y, px(x, y));
				}
			}
		}
	}
	(out, ow as u32, oh as u32)
}

#[cfg(test)]
mod tests {
	use super::rotate_bgra;

	#[test]
	fn rotate180_reverses_pixels() {
		// 2×1: [A,B] → [B,A].
		let a = [1u8, 2, 3, 4, 5, 6, 7, 8];
		assert_eq!(rotate_bgra(&a, 2, 1, 180), (vec![5, 6, 7, 8, 1, 2, 3, 4], 2, 1));
		// 1×2 (vertical): top/bottom swap.
		let b = [9u8, 9, 9, 9, 1, 1, 1, 1];
		assert_eq!(rotate_bgra(&b, 1, 2, 180), (vec![1, 1, 1, 1, 9, 9, 9, 9], 1, 2));
		// 2×2: TL,TR,BL,BR → BR,BL,TR,TL (both axes mirrored).
		let c = [0u8, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3];
		assert_eq!(
			rotate_bgra(&c, 2, 2, 180),
			(vec![3, 3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0], 2, 2)
		);
		// Malformed (too short for w*h) → unchanged copy, no panic.
		let d = [1u8, 2, 3];
		assert_eq!(rotate_bgra(&d, 2, 2, 180), (vec![1, 2, 3], 2, 2));
	}

	#[test]
	fn rotate90_270_swap_dims_and_pixels() {
		// 2×1 source: TL=A(0), TR=B(1). Pixels labelled by their B channel value.
		let a = [0u8, 0, 0, 0, 1, 1, 1, 1]; // (0,0)=A, (1,0)=B
		// 90° CCW: A(0,0)→(0, w-1-0)=(0,1); B(1,0)→(0, w-1-1)=(0,0). out dims 1×2.
		assert_eq!(
			rotate_bgra(&a, 2, 1, 90),
			(vec![1, 1, 1, 1, 0, 0, 0, 0], 1, 2)
		);
		// 270° CCW (= 90° CW): A(0,0)→(h-1-0, 0)=(0,0); B(1,0)→(h-1-0,1)=(0,1). out dims 1×2.
		assert_eq!(
			rotate_bgra(&a, 2, 1, 270),
			(vec![0, 0, 0, 0, 1, 1, 1, 1], 1, 2)
		);
		// 90° then 270° of the same image returns to the original (inverse rotations).
		let (r90, w90, h90) = rotate_bgra(&a, 2, 1, 90);
		let (back, bw, bh) = rotate_bgra(&r90, w90 as usize, h90 as usize, 270);
		assert_eq!((back, bw, bh), (a.to_vec(), 2, 1));
	}
}

impl CaptureDevice {
	/// Refresh the cached cursor position and (when it changed) the decoded shape + GPU
	/// textures. Mirrors Sunshine `snapshot` (display_vram.cpp L1159+): position-only
	/// updates are cheap; the shape is re-fetched + re-uploaded only when DXGI signals a
	/// new one (`PointerShapeBufferSize > 0`). Errors are returned so the caller can drop
	/// the cursor for this frame; the cache is left in its previous (consistent) state.
	pub(super) unsafe fn update_cursor_cache(
		&mut self,
		dup: &IDXGIOutputDuplication,
		info: &DXGI_OUTDUPL_FRAME_INFO,
	) -> windows::core::Result<()> {
		// (a) New SHAPE bytes — only present when PointerShapeBufferSize > 0. Decode the
		//     raw buffer into up to two BGRA images and upload them as textures.
		// Host display rotation (deg CW) — read from the SAME DXGI desc the encoder bakes by
		// (rotation_deg() ← dup_desc.Rotation). For a 90/180/270° host the cursor bitmap is
		// pre-rotated here (by the INVERSE of the encoder's bake) so it ends UPRIGHT after the
		// encoder's bake; the viewport in `composite_cursor` is transformed into scan-out space
		// to match. The UPLOADED texture is the rotated bitmap (dims swapped for 90/270); we keep
		// the LOGICAL (unrotated) cursor size in `tex_w`/`tex_h` for the position transform. 0° = identity.
		let rot = self.rotation_deg();
		if info.PointerShapeBufferSize > 0 {
			let mut buf = vec![0u8; info.PointerShapeBufferSize as usize];
			let mut required = 0u32;
			let mut shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
			dup.GetFramePointerShape(
				buf.len() as u32,
				buf.as_mut_ptr() as *mut _,
				&mut required,
				&mut shape_info,
			)?;
			// Rebuild GPU textures from the decoded images. On a decode/upload failure clear
			// the cached textures (better to show no cursor than a stale/garbage one).
			self.cursor.tex_alpha = None;
			self.cursor.tex_xor = None;
			self.cursor.tex_w = 0;
			self.cursor.tex_h = 0;
			if let Some((alpha, w, h)) = make_cursor_alpha_image(&buf, &shape_info) {
				let (alpha, aw, ah) = if rot != 0 {
					rotate_bgra(&alpha, w as usize, h as usize, rot)
				} else {
					(alpha, w, h)
				};
				self.cursor.tex_alpha = Some(make_cursor_srv(&self.device, &alpha, aw, ah)?);
				// Store the LOGICAL (unrotated) cursor size — the viewport transform in
				// `composite_cursor` derives the (possibly swapped) scan-out footprint from it.
				self.cursor.tex_w = w;
				self.cursor.tex_h = h;
				self.cursor.hot_x = shape_info.HotSpot.x as i32;
				self.cursor.hot_y = shape_info.HotSpot.y as i32;
				// The xor image (if any) shares the alpha image's logical size.
				if let Some((xor, xw, xh)) = make_cursor_xor_image(&buf, &shape_info) {
					let (xor, rxw, rxh) = if rot != 0 {
						rotate_bgra(&xor, xw as usize, xh as usize, rot)
					} else {
						(xor, xw, xh)
					};
					self.cursor.tex_xor = Some(make_cursor_srv(&self.device, &xor, rxw, rxh)?);
				}
			}
		}

		// (b) New POSITION / visibility — only valid when LastMouseUpdateTime != 0. When the
		//     pointer leaves this output it goes Visible=false; we keep the cached shape but
		//     skip blending while invisible.
		if info.LastMouseUpdateTime != 0 {
			self.cursor.visible = info.PointerPosition.Visible.as_bool();
			self.cursor.pos_x = info.PointerPosition.Position.x;
			self.cursor.pos_y = info.PointerPosition.Position.y;
		}
		Ok(())
	}

	/// Override the cached cursor position from the LIVE OS cursor (`GetCursorInfo`) every
	/// tick, instead of waiting for DXGI's `PointerPosition`. DXGI only reports pointer
	/// updates bundled with `AcquireNextFrame`, and on hybrid-GPU hosts those arrive at only
	/// ~2-3 Hz on an otherwise-static desktop → the composited cursor teleports even though
	/// the OS cursor (moved by injected input) is smooth. `GetCursorInfo` is a cheap user32
	/// call that always reflects the current position. Shape + hotspot still come from DXGI;
	/// we map the screen point into this output's pixel space and subtract the hotspot
	/// (DXGI's PointerPosition is the bitmap top-left, GetCursorInfo's is the hotspot point).
	pub(super) unsafe fn refresh_live_cursor(&mut self) {
		let mut ci = CURSORINFO {
			cbSize: std::mem::size_of::<CURSORINFO>() as u32,
			..Default::default()
		};
		if GetCursorInfo(&mut ci).is_err() {
			return;
		}
		// B11: the monitor rect only changes on a resolution/output rebuild (which clears this
		// cache via build_pool/teardown_duplication), so fetch IDXGIOutput::GetDesc ONCE and reuse
		// it — instead of a GetDesc COM call every tick on the pacing-critical path.
		let r = match self.output_rect_cache {
			Some(r) => r,
			None => match self.output.GetDesc() {
				Ok(d) => {
					self.output_rect_cache = Some(d.DesktopCoordinates);
					d.DesktopCoordinates
				}
				Err(_) => return,
			},
		};
		let p = ci.ptScreenPos;
		let on_output = p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
		self.cursor.visible = ci.flags == CURSOR_SHOWING && on_output;
		self.cursor.pos_x = p.x - r.left - self.cursor.hot_x;
		self.cursor.pos_y = p.y - r.top - self.cursor.hot_y;
	}

	/// Produce the texture handed to the encoder for this tick.
	///
	/// When `draw_cursor` is off (or anything cursor-related fails / the cursor is hidden /
	/// no shape yet), returns the clean `pool` desktop directly. Otherwise copies `pool` →
	/// `present` and blends the cached cursor onto `present` at its position, returning
	/// `present`. Re-running this every tick over a FRESH pool→present copy is what keeps a
	/// cursor moving on a static desktop animating without smearing (the plan's core
	/// pacing constraint). Cursor-path errors fall back to the clean desktop — capture is
	/// never broken.
	///
	/// Returns the texture to emit, or `None` if neither pool nor present is available.
	pub(super) unsafe fn composite_cursor(
		&mut self,
		draw_cursor: bool,
	) -> Option<&ID3D11Texture2D> {
		// The clean desktop is always our fallback.
		let pool = self.pool.clone();
		let pool = match pool.as_ref() {
			Some(p) => p,
			None => return None,
		};

		// Fast path: nothing to draw → hand back the clean desktop unchanged.
		let want_cursor = draw_cursor
			&& self.cursor.visible
			&& self.cursor.tex_alpha.is_some()
			&& self.cursor.tex_w > 0
			&& self.cursor.tex_h > 0;
		if !want_cursor {
			return self.pool.as_ref();
		}

		// Lazily build the shader/sampler/blend pipeline once. If it fails, disable the
		// cursor permanently (compositor_tried gate) and fall back to the clean desktop.
		if self.compositor.is_none() && !self.compositor_tried {
			self.compositor_tried = true;
			match CursorCompositor::create(&self.device) {
				Ok(c) => self.compositor = Some(c),
				Err(_) => return self.pool.as_ref(),
			}
		}
		// Bail before any GPU work if the compositor isn't available (a prior build failed).
		if self.compositor.is_none() {
			return self.pool.as_ref();
		}
		let present = match self.present.as_ref() {
			Some(p) => p.clone(),
			None => return self.pool.as_ref(),
		};

		// 1. Fresh copy of the clean desktop into `present` (erases last tick's cursor).
		self.context.CopyResource(&present, pool);

		// 2. Blend the cursor onto `present`. B11: CACHE the RTV across ticks — `present` only
		//    changes on a resize/reinit, which clears `present_rtv` (build_pool/teardown), so an
		//    RTV built once stays valid until then; this drops a CreateRenderTargetView from every
		//    pacing tick. Built (and the cache filled) BEFORE `comp` is borrowed so the mutable
		//    cache write doesn't collide with the compositor borrow held through the draws below.
		//    On a build failure fall back to the clean desktop.
		if self.present_rtv.is_none() {
			let mut rtv: Option<ID3D11RenderTargetView> = None;
			if self
				.device
				.CreateRenderTargetView(&present, None, Some(&mut rtv))
				.is_err()
			{
				return self.pool.as_ref();
			}
			self.present_rtv = rtv;
		}
		let rtv = match self.present_rtv.as_ref() {
			Some(r) => r.clone(),
			None => return self.pool.as_ref(),
		};

		// Now take the compositor pipeline (immutable borrow, held through the draws below).
		let comp = match self.compositor.as_ref() {
			Some(c) => c,
			None => return self.pool.as_ref(),
		};

		let ctx = &self.context;
		// Viewport positions+sizes the cursor: the VS emits a fullscreen triangle with tex
		// [0,1], so it covers exactly this viewport → the (pre-rotated) cursor texture lands at
		// the right scan-out rect. The cursor is composited on the PRE-bake (scan-out-oriented)
		// `present` surface, so we transform the LOGICAL pointer rect into scan-out space;
		// combined with the inverse-pre-rotated cursor bitmap (update_cursor_cache), the encoder's
		// bake then lands the cursor UPRIGHT at the true pointer position. Mirrors Sunshine's
		// `gpu_cursor_t::update_viewport`. `pw`/`ph` = scan-out (unrotated) surface dims; for
		// 90/270 the logical desktop is the transpose. `cw`/`ch` = logical cursor size; the drawn
		// footprint is swapped (ch×cw) for 90/270. 0° = identity.
		let pw = self.dup_desc.ModeDesc.Width as f32;
		let ph = self.dup_desc.ModeDesc.Height as f32;
		let (px, py) = (self.cursor.pos_x as f32, self.cursor.pos_y as f32);
		let (cw, ch) = (self.cursor.tex_w as f32, self.cursor.tex_h as f32);
		let (vx, vy, vw, vh) = match self.rotation_deg() {
			// Logical desktop is ph(wide)×pw(tall); cursor footprint is ch×cw.
			90 => (py, ph - cw - px, ch, cw),
			180 => (pw - cw - px, ph - ch - py, cw, ch),
			270 => (pw - ch - py, px, ch, cw),
			_ => (px, py, cw, ch),
		};
		let vp = D3D11_VIEWPORT {
			TopLeftX: vx,
			TopLeftY: vy,
			Width: vw,
			Height: vh,
			MinDepth: 0.0,
			MaxDepth: 1.0,
		};

		ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
		ctx.VSSetShader(&comp.vs, None);
		ctx.PSSetShader(&comp.ps, None);
		ctx.PSSetSamplers(0, Some(&[Some(comp.sampler.clone())]));
		ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
		ctx.RSSetViewports(Some(&[vp]));

		// Pass 1: alpha image (always present for a valid shape) — straight src-over.
		if let Some(srv) = self.cursor.tex_alpha.as_ref() {
			ctx.OMSetBlendState(&comp.blend_alpha, None, 0xFFFF_FFFF);
			ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
			ctx.Draw(3, 0);
		}
		// Pass 2: xor image (MASKED_COLOR / MONOCHROME only) — invert blend, alpha untouched
		// (sample mask 0x00FFFFFF so the invert doesn't disturb the frame's alpha channel).
		if let Some(srv) = self.cursor.tex_xor.as_ref() {
			ctx.OMSetBlendState(&comp.blend_invert, None, 0x00FF_FFFF);
			ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
			ctx.Draw(3, 0);
		}

		// Restore: unbind RTV + SRV and disable blending so the encoder's later draws on the
		// shared context start from a clean state.
		ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
		ctx.OMSetRenderTargets(Some(&[None]), None);
		ctx.PSSetShaderResources(0, Some(&[None]));

		self.present.as_ref()
	}
}
