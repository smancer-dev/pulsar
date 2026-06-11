//! Minimal D3D11 painter for egui (egui 0.29 / windows 0.58) — vendored instead of
//! egui-directx11 (which forces egui 0.31 + windows 0.61, conflicting with our versions and the
//! Linux egui_glow 0.29 path).
//!
//! Consumes egui's tessellated output (`ClippedPrimitive` meshes + a `TexturesDelta`) and draws
//! it onto a D3D11 render target with alpha blending + per-primitive scissor, matching egui's
//! sRGB/gamma convention.
//!
//! ## Color / gamma convention (chosen)
//!
//! egui's `epaint::Vertex.color` and the texture pixels we upload are **sRGBA with premultiplied
//! alpha** (see `epaint::mesh::Vertex` doc: "sRGBA with premultiplied alpha"). The font atlas is
//! turned into premultiplied sRGBA via `FontImage::srgba_pixels(None)` (gamma `None` → egui's
//! default `GAMMA = 0.55` coverage-to-alpha curve), and `ColorImage` pixels are already
//! premultiplied `Color32`.
//!
//! We follow the **`egui_glow` "framebuffer is sRGB-but-treated-as-linear" convention**: the GPU
//! treats the stored sRGB bytes *as if* they were linear (we use a plain `R8G8B8A8_UNORM` target,
//! NOT `_SRGB`, so no hardware sRGB→linear/​linear→sRGB conversion happens), and we blend in that
//! space with **premultiplied-alpha** blending (`One` / `InvSrcAlpha`). This is exactly what
//! `egui_glow` does when the target framebuffer is not sRGB-aware, and it makes egui's own
//! antialiasing/text coverage look identical to the GL backend. Therefore the shader does **no**
//! sRGB↔linear conversion at all: the VS passes the (normalized 0..1) vertex color straight
//! through, and the PS computes `tex * color` with both operands premultiplied. The video sits
//! underneath on the same non-sRGB target, so it is left untouched where egui doesn't draw.

#![allow(dead_code)]

use std::collections::HashMap;

use windows::core::{Result, PCSTR};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
	ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11InputLayout,
	ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11SamplerState,
	ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER,
	D3D11_BIND_INDEX_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER,
	D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
	D3D11_BLEND_SRC_ALPHA, D3D11_BOX, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
	D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
	D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAPPED_SUBRESOURCE,
	D3D11_MAP_WRITE_DISCARD, D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC,
	D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
	D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
	DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};

/// The standard egui shader, HLSL flavor. See the module doc for the color/gamma convention:
/// the framebuffer is treated as linear (non-`_SRGB` target) and blending is premultiplied, so
/// the shader does NO sRGB↔linear conversion — it just normalizes the 0..255 color to 0..1 and
/// multiplies by the (premultiplied) texel.
const SHADER_HLSL: &str = r#"
cbuffer Constants : register(b0) {
    float2 screen_size_in_points;
    float2 _pad;
};

struct VsIn {
    float2 pos   : POSITION;   // egui logical points
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;     // R8G8B8A8_UNORM -> already normalized 0..1 (premultiplied sRGBA)
};

struct VsOut {
    float4 pos   : SV_POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

Texture2D    tex  : register(t0);
SamplerState samp : register(s0);

VsOut vs_main(VsIn input) {
    VsOut o;
    o.pos = float4(
        2.0 * input.pos.x / screen_size_in_points.x - 1.0,
        1.0 - 2.0 * input.pos.y / screen_size_in_points.y,
        0.0,
        1.0);
    o.uv = input.uv;
    o.color = input.color; // pass through; target is treated as linear (egui_glow convention)
    return o;
}

float4 ps_main(VsOut input) : SV_TARGET {
    return tex.Sample(samp, input.uv) * input.color;
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
	screen_size_in_points: [f32; 2],
	_pad: [f32; 2],
}

struct Texture {
	_tex: ID3D11Texture2D,
	srv: ID3D11ShaderResourceView,
	size: [u32; 2],
}

/// egui → D3D11 painter. Holds the shaders, input layout, sampler, blend/raster state, and the
/// uploaded egui textures (font atlas + user images), keyed by `egui::TextureId`.
pub struct EguiPaint {
	vs: ID3D11VertexShader,
	ps: ID3D11PixelShader,
	input_layout: ID3D11InputLayout,
	sampler: ID3D11SamplerState,
	blend: ID3D11BlendState,
	raster: ID3D11RasterizerState,
	constants: ID3D11Buffer,

	vertex_buffer: Option<ID3D11Buffer>,
	vertex_capacity: usize,
	index_buffer: Option<ID3D11Buffer>,
	index_capacity: usize,

	textures: HashMap<egui::TextureId, Texture>,
}

/// Compile one HLSL entry point to a blob via D3DCompile.
fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
	let src = SHADER_HLSL.as_bytes();
	// C-string entry / target for the PCSTR args.
	let entry_c = format!("{entry}\0");
	let target_c = format!("{target}\0");
	let mut blob: Option<ID3DBlob> = None;
	let mut errors: Option<ID3DBlob> = None;
	let hr = unsafe {
		D3DCompile(
			src.as_ptr() as *const _,
			src.len(),
			None,
			None,
			None,
			PCSTR(entry_c.as_ptr()),
			PCSTR(target_c.as_ptr()),
			D3DCOMPILE_OPTIMIZATION_LEVEL3,
			0,
			&mut blob,
			Some(&mut errors),
		)
	};
	hr?;
	blob.ok_or_else(|| windows::core::Error::from_win32())
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
	unsafe {
		let ptr = blob.GetBufferPointer() as *const u8;
		let len = blob.GetBufferSize();
		std::slice::from_raw_parts(ptr, len)
	}
}

impl EguiPaint {
	pub fn new(device: &ID3D11Device) -> Result<Self> {
		let vs_blob = compile("vs_main", "vs_5_0")?;
		let ps_blob = compile("ps_main", "ps_5_0")?;
		let vs_bytes = blob_bytes(&vs_blob);
		let ps_bytes = blob_bytes(&ps_blob);

		let mut vs: Option<ID3D11VertexShader> = None;
		let mut ps: Option<ID3D11PixelShader> = None;
		unsafe {
			device.CreateVertexShader(vs_bytes, None, Some(&mut vs))?;
			device.CreatePixelShader(ps_bytes, None, Some(&mut ps))?;
		}
		let vs = vs.unwrap();
		let ps = ps.unwrap();

		// Input layout matching epaint::Vertex { pos: f32x2, uv: f32x2, color: u8x4 } == 20 bytes.
		let pos_name = b"POSITION\0";
		let uv_name = b"TEXCOORD\0";
		let col_name = b"COLOR\0";
		let elements = [
			D3D11_INPUT_ELEMENT_DESC {
				SemanticName: PCSTR(pos_name.as_ptr()),
				SemanticIndex: 0,
				Format: DXGI_FORMAT_R32G32_FLOAT,
				InputSlot: 0,
				AlignedByteOffset: 0,
				InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
				InstanceDataStepRate: 0,
			},
			D3D11_INPUT_ELEMENT_DESC {
				SemanticName: PCSTR(uv_name.as_ptr()),
				SemanticIndex: 0,
				Format: DXGI_FORMAT_R32G32_FLOAT,
				InputSlot: 0,
				AlignedByteOffset: 8,
				InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
				InstanceDataStepRate: 0,
			},
			D3D11_INPUT_ELEMENT_DESC {
				SemanticName: PCSTR(col_name.as_ptr()),
				SemanticIndex: 0,
				Format: DXGI_FORMAT_R8G8B8A8_UNORM,
				InputSlot: 0,
				AlignedByteOffset: 16,
				InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
				InstanceDataStepRate: 0,
			},
		];
		let mut input_layout: Option<ID3D11InputLayout> = None;
		unsafe {
			device.CreateInputLayout(&elements, vs_bytes, Some(&mut input_layout))?;
		}
		let input_layout = input_layout.unwrap();

		// Linear-clamp sampler.
		let samp_desc = D3D11_SAMPLER_DESC {
			Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
			AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
			AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
			AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
			MipLODBias: 0.0,
			MaxAnisotropy: 1,
			ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_ALWAYS,
			BorderColor: [0.0; 4],
			MinLOD: 0.0,
			MaxLOD: f32::MAX,
		};
		let mut sampler: Option<ID3D11SamplerState> = None;
		unsafe {
			device.CreateSamplerState(&samp_desc, Some(&mut sampler))?;
		}
		let sampler = sampler.unwrap();

		// Premultiplied-alpha blend (egui colors/textures are premultiplied): One / InvSrcAlpha
		// for color, One / InvSrcAlpha for alpha. (SrcAlpha is left as documentation per the
		// task note, but premultiplied requires SrcBlend = One.)
		let mut blend_desc = D3D11_BLEND_DESC::default();
		blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
			BlendEnable: TRUE,
			SrcBlend: D3D11_BLEND_ONE,
			DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
			BlendOp: D3D11_BLEND_OP_ADD,
			SrcBlendAlpha: D3D11_BLEND_ONE,
			DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
			BlendOpAlpha: D3D11_BLEND_OP_ADD,
			RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
		};
		// Reference the alpha-source constants so the imports are used even if the math above
		// never names them (keeps SrcAlpha/InvSrcAlpha in scope for the documented variant).
		let _ = (D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA);
		let mut blend: Option<ID3D11BlendState> = None;
		unsafe {
			device.CreateBlendState(&blend_desc, Some(&mut blend))?;
		}
		let blend = blend.unwrap();

		// Rasterizer: no cull, solid fill, scissor enabled.
		let raster_desc = D3D11_RASTERIZER_DESC {
			FillMode: D3D11_FILL_SOLID,
			CullMode: D3D11_CULL_NONE,
			FrontCounterClockwise: false.into(),
			DepthBias: 0,
			DepthBiasClamp: 0.0,
			SlopeScaledDepthBias: 0.0,
			DepthClipEnable: TRUE,
			ScissorEnable: TRUE,
			MultisampleEnable: false.into(),
			AntialiasedLineEnable: false.into(),
		};
		let mut raster: Option<ID3D11RasterizerState> = None;
		unsafe {
			device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;
		}
		let raster = raster.unwrap();

		// Constant buffer (b0), dynamic so we can update screen size each paint.
		let cb_desc = D3D11_BUFFER_DESC {
			ByteWidth: std::mem::size_of::<Constants>() as u32,
			Usage: D3D11_USAGE_DYNAMIC,
			BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
			CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
			MiscFlags: 0,
			StructureByteStride: 0,
		};
		let mut constants: Option<ID3D11Buffer> = None;
		unsafe {
			device.CreateBuffer(&cb_desc, None, Some(&mut constants))?;
		}
		let constants = constants.unwrap();

		Ok(Self {
			vs,
			ps,
			input_layout,
			sampler,
			blend,
			raster,
			constants,
			vertex_buffer: None,
			vertex_capacity: 0,
			index_buffer: None,
			index_capacity: 0,
			textures: HashMap::new(),
		})
	}

	/// Apply a `TexturesDelta` (upload `set`, free `free`) before painting.
	pub fn update_textures(
		&mut self,
		device: &ID3D11Device,
		ctx: &ID3D11DeviceContext,
		delta: &egui::TexturesDelta,
	) -> Result<()> {
		for (id, image_delta) in &delta.set {
			// Build premultiplied sRGBA8 pixels (see module gamma note).
			let [w, h] = image_delta.image.size();
			let pixels: Vec<u8> = match &image_delta.image {
				egui::ImageData::Color(image) => {
					image.pixels.iter().flat_map(|c| c.to_array()).collect()
				}
				egui::ImageData::Font(font) => {
					font.srgba_pixels(None).flat_map(|c| c.to_array()).collect()
				}
			};

			match image_delta.pos {
				Some([x, y]) => {
					// Partial update into an existing texture.
					if let Some(existing) = self.textures.get(id) {
						let box_ = D3D11_BOX {
							left: x as u32,
							top: y as u32,
							front: 0,
							right: (x + w) as u32,
							bottom: (y + h) as u32,
							back: 1,
						};
						unsafe {
							ctx.UpdateSubresource(
								&existing._tex,
								0,
								Some(&box_),
								pixels.as_ptr() as *const _,
								(w * 4) as u32,
								0,
							);
						}
					}
				}
				None => {
					// (Re)create the whole texture.
					let tex = create_texture(device, w as u32, h as u32, &pixels)?;
					self.textures.insert(*id, tex);
				}
			}
		}

		for id in &delta.free {
			self.textures.remove(id);
		}
		Ok(())
	}

	/// Draw the tessellated primitives onto `rtv`. `size_px` = target size in physical pixels;
	/// `pixels_per_point` = egui scale. The caller already presented the video underneath and
	/// must NOT have its target cleared by us.
	pub fn paint(
		&mut self,
		device: &ID3D11Device,
		ctx: &ID3D11DeviceContext,
		rtv: &ID3D11RenderTargetView,
		size_px: [u32; 2],
		pixels_per_point: f32,
		primitives: &[egui::ClippedPrimitive],
	) -> Result<()> {
		if size_px[0] == 0 || size_px[1] == 0 {
			return Ok(());
		}

		// Update the constant buffer with the logical (points) screen size.
		let screen_points = [
			size_px[0] as f32 / pixels_per_point,
			size_px[1] as f32 / pixels_per_point,
		];
		let constants = Constants {
			screen_size_in_points: screen_points,
			_pad: [0.0; 2],
		};
		unsafe {
			let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
			ctx.Map(
				&self.constants,
				0,
				D3D11_MAP_WRITE_DISCARD,
				0,
				Some(&mut mapped),
			)?;
			std::ptr::copy_nonoverlapping(
				&constants as *const Constants as *const u8,
				mapped.pData as *mut u8,
				std::mem::size_of::<Constants>(),
			);
			ctx.Unmap(&self.constants, 0);
		}

		// Bind pipeline state.
		unsafe {
			ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);

			let viewport = D3D11_VIEWPORT {
				TopLeftX: 0.0,
				TopLeftY: 0.0,
				Width: size_px[0] as f32,
				Height: size_px[1] as f32,
				MinDepth: 0.0,
				MaxDepth: 1.0,
			};
			ctx.RSSetViewports(Some(&[viewport]));
			ctx.RSSetState(&self.raster);

			ctx.IASetInputLayout(&self.input_layout);
			ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

			ctx.VSSetShader(&self.vs, None);
			ctx.VSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
			ctx.PSSetShader(&self.ps, None);
			ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

			let blend_factor = [0.0f32; 4];
			ctx.OMSetBlendState(&self.blend, Some(&blend_factor), 0xffffffff);
		}

		for prim in primitives {
			let mesh = match &prim.primitive {
				egui::epaint::Primitive::Mesh(mesh) => mesh,
				egui::epaint::Primitive::Callback(_) => continue,
			};
			if mesh.vertices.is_empty() || mesh.indices.is_empty() {
				continue;
			}

			// Scissor from clip_rect (logical points -> physical pixels), clamped to target.
			let clip = prim.clip_rect;
			let min_x = (clip.min.x * pixels_per_point).round().max(0.0) as i32;
			let min_y = (clip.min.y * pixels_per_point).round().max(0.0) as i32;
			let max_x = (clip.max.x * pixels_per_point)
				.round()
				.min(size_px[0] as f32) as i32;
			let max_y = (clip.max.y * pixels_per_point)
				.round()
				.min(size_px[1] as f32) as i32;
			if max_x <= min_x || max_y <= min_y {
				continue;
			}
			let scissor = RECT {
				left: min_x,
				top: min_y,
				right: max_x,
				bottom: max_y,
			};
			unsafe {
				ctx.RSSetScissorRects(Some(&[scissor]));
			}

			// Bind the mesh's texture SRV (skip if unknown).
			let srv = match self.textures.get(&mesh.texture_id) {
				Some(t) => t.srv.clone(),
				None => continue,
			};
			unsafe {
				ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
			}

			// Upload vertices + indices into the (growable) dynamic buffers.
			self.upload_mesh(device, ctx, mesh)?;

			let stride = std::mem::size_of::<egui::epaint::Vertex>() as u32;
			let offset = 0u32;
			let vb_array = [self.vertex_buffer.clone()];
			unsafe {
				ctx.IASetVertexBuffers(0, 1, Some(vb_array.as_ptr()), Some(&stride), Some(&offset));
				ctx.IASetIndexBuffer(self.index_buffer.as_ref(), DXGI_FORMAT_R32_UINT, 0);
				ctx.DrawIndexed(mesh.indices.len() as u32, 0, 0);
			}
		}

		Ok(())
	}

	/// Map the mesh into the dynamic vertex/index buffers, growing them if needed.
	fn upload_mesh(
		&mut self,
		device: &ID3D11Device,
		ctx: &ID3D11DeviceContext,
		mesh: &egui::epaint::Mesh,
	) -> Result<()> {
		let vtx_bytes = std::mem::size_of_val(mesh.vertices.as_slice());
		let idx_bytes = std::mem::size_of_val(mesh.indices.as_slice());

		if self.vertex_buffer.is_none() || self.vertex_capacity < mesh.vertices.len() {
			let cap = mesh.vertices.len().next_power_of_two().max(1024);
			let desc = D3D11_BUFFER_DESC {
				ByteWidth: (cap * std::mem::size_of::<egui::epaint::Vertex>()) as u32,
				Usage: D3D11_USAGE_DYNAMIC,
				BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
				CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
				MiscFlags: 0,
				StructureByteStride: 0,
			};
			let mut buf: Option<ID3D11Buffer> = None;
			unsafe {
				device.CreateBuffer(&desc, None, Some(&mut buf))?;
			}
			self.vertex_buffer = buf;
			self.vertex_capacity = cap;
		}
		if self.index_buffer.is_none() || self.index_capacity < mesh.indices.len() {
			let cap = mesh.indices.len().next_power_of_two().max(2048);
			let desc = D3D11_BUFFER_DESC {
				ByteWidth: (cap * std::mem::size_of::<u32>()) as u32,
				Usage: D3D11_USAGE_DYNAMIC,
				BindFlags: D3D11_BIND_INDEX_BUFFER.0 as u32,
				CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
				MiscFlags: 0,
				StructureByteStride: 0,
			};
			let mut buf: Option<ID3D11Buffer> = None;
			unsafe {
				device.CreateBuffer(&desc, None, Some(&mut buf))?;
			}
			self.index_buffer = buf;
			self.index_capacity = cap;
		}

		unsafe {
			let vb = self.vertex_buffer.as_ref().unwrap();
			let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
			ctx.Map(vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
			std::ptr::copy_nonoverlapping(
				mesh.vertices.as_ptr() as *const u8,
				mapped.pData as *mut u8,
				vtx_bytes,
			);
			ctx.Unmap(vb, 0);

			let ib = self.index_buffer.as_ref().unwrap();
			let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
			ctx.Map(ib, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
			std::ptr::copy_nonoverlapping(
				mesh.indices.as_ptr() as *const u8,
				mapped.pData as *mut u8,
				idx_bytes,
			);
			ctx.Unmap(ib, 0);
		}
		Ok(())
	}
}

/// Create an immutable-ish RGBA8 texture + SRV from premultiplied sRGBA pixels.
fn create_texture(
	device: &ID3D11Device,
	width: u32,
	height: u32,
	pixels: &[u8],
) -> Result<Texture> {
	let desc = D3D11_TEXTURE2D_DESC {
		Width: width,
		Height: height,
		MipLevels: 1,
		ArraySize: 1,
		// Non-`_SRGB` format: the stored sRGB bytes are sampled as-is (no HW conversion), per
		// the egui_glow "treat framebuffer as linear" convention documented at module top.
		Format: DXGI_FORMAT_R8G8B8A8_UNORM,
		SampleDesc: DXGI_SAMPLE_DESC {
			Count: 1,
			Quality: 0,
		},
		// DEFAULT usage so UpdateSubresource (partial font-atlas patches) works.
		Usage: D3D11_USAGE_DEFAULT,
		BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
		CPUAccessFlags: 0,
		MiscFlags: 0,
	};
	let init = D3D11_SUBRESOURCE_DATA {
		pSysMem: pixels.as_ptr() as *const _,
		SysMemPitch: width * 4,
		SysMemSlicePitch: 0,
	};
	let mut tex: Option<ID3D11Texture2D> = None;
	unsafe {
		device.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
	}
	let tex = tex.unwrap();

	let mut srv: Option<ID3D11ShaderResourceView> = None;
	unsafe {
		device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
	}
	let srv = srv.unwrap();

	Ok(Texture {
		_tex: tex,
		srv,
		size: [width, height],
	})
}
