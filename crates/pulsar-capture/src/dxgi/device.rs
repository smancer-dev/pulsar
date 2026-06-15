//! The `CaptureDevice`: owns the D3D11 device + the active output duplication + the BGRA
//! pool texture the encoder reads each tick. This module holds the struct definition, the
//! `Capture` outcome enum, the device/output enumeration + duplication (re)build path, and
//! the `Send` marker. The pacing loop lives in `pacing.rs`; cursor compositing in `cursor.rs`.

use windows::core::Interface;
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE};
use windows::Win32::Graphics::Direct3D::{
	D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
	D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
	D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
	D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
	CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
	IDXGIOutputDuplication, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_NOT_FOUND,
	DXGI_OUTDUPL_DESC,
};

use super::cursor::{CursorCompositor, CursorState};
use super::platform::Qpc;

/// How long (ms) a `Reinit`-triggered rebuild backs off before retrying `create()`.
pub(super) const REINIT_BACKOFF_MS: u64 = 200;

/// Why the pacing loop (`run`) returned. The capture thread (lib.rs) acts on it: tear the
/// stream down, or rebuild capture+encode on the newly-requested monitor.
pub enum RunExit {
	/// `stop` was set — the session is ending.
	Stop,
	/// The host asked to capture a different monitor (session-menu picker). The thread rebuilds
	/// `CaptureDevice`/`Encoder` on this output index — `create` re-picks the owning adapter, so
	/// a switch to a monitor on the OTHER GPU (MUX laptop) lands on the right device.
	Switch(u32),
}

/// Outcome of an `AcquireNextFrame` attempt, mapped from the raw HRESULT.
pub(super) enum Capture {
	/// A fresh desktop frame landed; the pool texture now holds it.
	Frame,
	/// Timed out — no screen change this interval. Reuse the last surface.
	Timeout,
	/// The duplication is stale (mode/format change, access lost, output reparented to
	/// the other GPU). Rebuild `dup` + pool and continue.
	Reinit,
	/// A hard error we couldn't classify. The loop does one reinit, then bails.
	Error(windows::core::Error),
}

/// Owns the D3D11 device + the active output duplication + the BGRA pool texture that
/// the encoder reads each tick. `device`/`context` are PUBLIC so `encode.rs` can hand
/// them to NVENC's `nvEncOpenEncodeSessionEx` (it must AddRef them — see encode.rs).
pub struct CaptureDevice {
	/// PUBLIC: encode.rs opens the NVENC session on THIS device.
	pub device: ID3D11Device,
	/// PUBLIC: the shared immediate context (encode's VideoProcessorBlt runs on it).
	pub context: ID3D11DeviceContext,

	// --- private capture state ---
	pub(super) factory: IDXGIFactory1,
	/// The output (monitor) we duplicate; kept so we can rebuild `dup` on Reinit.
	pub(super) output: IDXGIOutput1,
	/// The live duplication; `None` between a teardown and the next rebuild.
	pub(super) dup: Option<IDXGIOutputDuplication>,
	pub(super) dup_desc: DXGI_OUTDUPL_DESC,
	/// BGRA pool texture (DEFAULT, RT|SRV) — the surface handed to the encoder. We
	/// `CopyResource` each acquired DXGI frame into it so the duplication's own surface
	/// can be released immediately (DXGI only buffers one).
	pub(super) pool: Option<ID3D11Texture2D>,
	/// Which adapter output index was requested (for rebuilds).
	pub(super) output_idx: u32,
	pub(super) qpc: Qpc,
	/// Whether the LAST `snapshot` represented a real desktop change (→ `Frame.is_new`).
	/// Loop-local state shared between `snapshot` (writes) and `run` (reads) without an
	/// extra return value; only the single capture thread touches it.
	pub(super) last_was_new: bool,

	// --- cursor compositing state (Sunshine technique; see CursorCompositor above) ---
	/// The texture handed to the encoder when `draw_cursor` is on: a per-tick copy of the
	/// clean `pool` desktop with the cursor blended on top. Kept SEPARATE from `pool` so the
	/// cursor is re-composited fresh every tick (incl. the static-desktop reuse path) and
	/// never smears a stale cursor into the desktop pixels. `None` until first built.
	pub(super) present: Option<ID3D11Texture2D>,
	/// Cached pointer position + decoded shape + GPU cursor textures.
	pub(super) cursor: CursorState,
	/// Lazily-built shader/sampler/blend pipeline for the blit. `None` until the first
	/// `draw_cursor` frame; stays `None` (cursor disabled) if building it ever fails.
	pub(super) compositor: Option<CursorCompositor>,
	/// Set once we've ATTEMPTED to build the compositor, so a build failure isn't retried
	/// every frame (it would just fail again, wasting time on the pacing-critical path).
	pub(super) compositor_tried: bool,
	/// Shared session-stop flag (cloned from the capture thread's `stop`). `build_duplication`
	/// polls it between its transient retry sleeps so an init/switch that hits the long
	/// `NOT_CURRENTLY_AVAILABLE` retry loop bails immediately when the session is torn down,
	/// instead of stalling the joining caller for the whole ~5 s budget.
	pub(super) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl CaptureDevice {
	/// Enumerate adapter→output by `DXGI_OUTPUT_DESC.AttachedToDesktop`, create a D3D11
	/// device on the *owning* adapter (driver `UNKNOWN` so DXGI picks it), set max frame
	/// latency = 1, and `DuplicateOutput1`→`DuplicateOutput` (retried ×2 w/ 200 ms).
	///
	/// On the hybrid laptop the display owner is the iGPU; building the device there is
	/// what lets NVENC do the cross-adapter copy itself (Strategy A).
	pub unsafe fn create(
		output_idx: u32,
		stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
		// `true` for an in-session SWITCH build (short transient-retry budget — see
		// `build_duplication`); `false` for the initial build (long budget, the monitor must
		// come up for the session to start).
		fast_transient: bool,
	) -> windows::core::Result<Self> {
		let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
		let (adapter, output) = Self::find_output(&factory, output_idx)?;

		// Create the device on the adapter that OWNS this output. DRIVER_TYPE_UNKNOWN is
		// mandatory when you pass a specific adapter (HARDWARE would assert).
		let (device, context) = Self::create_device(&adapter)?;

		// SetMaximumFrameLatency(1): never let the GPU queue more than one frame — keeps
		// capture→encode latency minimal. It lives on IDXGIDevice1 (NOT the base
		// IDXGIDevice), which every D3D11 device implements, so QI for it via .cast().
		let dxgi_dev: IDXGIDevice1 = device.cast()?;
		let _ = dxgi_dev.SetMaximumFrameLatency(1);

		let mut me = CaptureDevice {
			device,
			context,
			factory,
			output,
			dup: None,
			dup_desc: DXGI_OUTDUPL_DESC::default(),
			pool: None,
			output_idx,
			qpc: Qpc::new(),
			last_was_new: false,
			present: None,
			cursor: CursorState::default(),
			compositor: None,
			compositor_tried: false,
			stop,
		};
		me.build_duplication(fast_transient)?;
		Ok(me)
	}

	/// Walk adapters/outputs the SAME way `find_output` indexes them (attached-to-desktop,
	/// in adapter→output order) and return `(idx, name, width, height, primary)` per monitor.
	/// `idx` here is exactly what `find_output`/`create` accept, so an advertised list maps
	/// 1:1 to a capturable output. `primary` = anchored at the virtual-desktop origin (0,0).
	pub unsafe fn list_outputs() -> windows::core::Result<Vec<crate::DisplayDesc>> {
		let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
		let mut out = Vec::new();
		let mut idx: u32 = 0;
		let mut ai = 0u32;
		loop {
			let adapter = match factory.EnumAdapters1(ai) {
				Ok(a) => a,
				Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
				Err(e) => return Err(e),
			};
			let mut oi = 0u32;
			loop {
				let o: IDXGIOutput = match adapter.EnumOutputs(oi) {
					Ok(o) => o,
					Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
					Err(e) => return Err(e),
				};
				let desc = o.GetDesc()?;
				if desc.AttachedToDesktop.as_bool() {
					let r = desc.DesktopCoordinates;
					let w = (r.right - r.left).max(0) as u32;
					let h = (r.bottom - r.top).max(0) as u32;
					// DeviceName is a UTF-16 `\\.\DISPLAYn`; trim the `\\.\` prefix for the UI.
					let name = String::from_utf16_lossy(
						&desc.DeviceName[..desc
							.DeviceName
							.iter()
							.position(|&c| c == 0)
							.unwrap_or(desc.DeviceName.len())],
					);
					let name = name.trim_start_matches(r"\\.\").to_string();
					let primary = r.left == 0 && r.top == 0;
					out.push((idx, name, w, h, primary));
					idx += 1;
				}
				oi += 1;
			}
			ai += 1;
		}
		Ok(out)
	}

	/// Virtual-desktop geometry of the attached output at `output_idx` (same DXGI order as
	/// [`list_outputs`]) plus the bounding box of ALL attached outputs (== the Windows virtual
	/// screen, `SM_*VIRTUALSCREEN`). Lets the host map a normalized absolute pointer onto the
	/// streamed monitor's place in the virtual desktop. `None` if enumeration fails or the index
	/// has no attached output.
	pub unsafe fn output_rect(output_idx: u32) -> Option<crate::DisplayRect> {
		let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
		let mut idx: u32 = 0;
		// Bounding box of every attached output = the virtual desktop extent.
		let mut vl = i32::MAX;
		let mut vt = i32::MAX;
		let mut vr = i32::MIN;
		let mut vb = i32::MIN;
		let mut chosen: Option<(i32, i32, i32, i32)> = None;
		let mut ai = 0u32;
		loop {
			let adapter = match factory.EnumAdapters1(ai) {
				Ok(a) => a,
				Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
				Err(_) => return None,
			};
			let mut oi = 0u32;
			loop {
				let o: IDXGIOutput = match adapter.EnumOutputs(oi) {
					Ok(o) => o,
					Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
					Err(_) => return None,
				};
				let desc = o.GetDesc().ok()?;
				if desc.AttachedToDesktop.as_bool() {
					let r = desc.DesktopCoordinates;
					vl = vl.min(r.left);
					vt = vt.min(r.top);
					vr = vr.max(r.right);
					vb = vb.max(r.bottom);
					if idx == output_idx {
						chosen = Some((r.left, r.top, r.right - r.left, r.bottom - r.top));
					}
					idx += 1;
				}
				oi += 1;
			}
			ai += 1;
		}
		let (mon_left, mon_top, mon_width, mon_height) = chosen?;
		// Some outputs were found (chosen is Some ⇒ vl/vt/vr/vb were assigned at least once).
		Some(crate::DisplayRect {
			mon_left,
			mon_top,
			mon_width,
			mon_height,
			virt_left: vl,
			virt_top: vt,
			virt_width: vr - vl,
			virt_height: vb - vt,
		})
	}

	/// Walk adapters/outputs, returning the (adapter, output1) pair for `output_idx`,
	/// preferring outputs actually attached to the desktop.
	pub(super) unsafe fn find_output(
		factory: &IDXGIFactory1,
		output_idx: u32,
	) -> windows::core::Result<(IDXGIAdapter1, IDXGIOutput1)> {
		let mut global_idx: u32 = 0;
		// First pass: count attached-to-desktop outputs so output_idx maps to a real
		// monitor (skipping detached/virtual outputs that can't be duplicated).
		let mut ai = 0u32;
		loop {
			let adapter = match factory.EnumAdapters1(ai) {
				Ok(a) => a,
				// DXGI_ERROR_NOT_FOUND = no more adapters; stop scanning.
				Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
				Err(e) => return Err(e),
			};
			let mut oi = 0u32;
			loop {
				let out: IDXGIOutput = match adapter.EnumOutputs(oi) {
					Ok(o) => o,
					Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
					Err(e) => return Err(e),
				};
				// IDXGIOutput::GetDesc returns the desc BY VALUE (Result) in windows-rs.
				let desc = out.GetDesc()?;
				// AttachedToDesktop weeds out outputs that DuplicateOutput would reject.
				if desc.AttachedToDesktop.as_bool() {
					if global_idx == output_idx {
						let out1: IDXGIOutput1 = out.cast()?;
						return Ok((adapter, out1));
					}
					global_idx += 1;
				}
				oi += 1;
			}
			ai += 1;
		}
		// Requested index not found — fall back to the FIRST attached output so a single
		// wrong index still streams *something* rather than failing the whole session.
		let mut ai = 0u32;
		loop {
			let adapter = match factory.EnumAdapters1(ai) {
				Ok(a) => a,
				Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
				Err(e) => return Err(e),
			};
			let mut oi = 0u32;
			loop {
				let out: IDXGIOutput = match adapter.EnumOutputs(oi) {
					Ok(o) => o,
					Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
					Err(e) => return Err(e),
				};
				let desc = out.GetDesc()?;
				if desc.AttachedToDesktop.as_bool() {
					let out1: IDXGIOutput1 = out.cast()?;
					return Ok((adapter, out1));
				}
				oi += 1;
			}
			ai += 1;
		}
		// No attached desktop output anywhere (session-0 / headless) — surface NOT_FOUND.
		Err(windows::core::Error::from(DXGI_ERROR_NOT_FOUND))
	}

	/// `D3D11CreateDevice` on a specific adapter (UNKNOWN driver), feature level 11_1→11_0,
	/// BGRA support for the duplication's B8G8R8A8 format.
	unsafe fn create_device(
		adapter: &IDXGIAdapter1,
	) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
		let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
		let mut device: Option<ID3D11Device> = None;
		let mut context: Option<ID3D11DeviceContext> = None;
		let mut got_level = D3D_FEATURE_LEVEL_11_0;
		// BGRA_SUPPORT is required: Desktop Duplication surfaces are B8G8R8A8_UNORM and the
		// VideoProcessor / D2D interop assumes BGRA-capable devices.
		D3D11CreateDevice(
			adapter, // explicit adapter ⇒ DRIVER_TYPE_UNKNOWN
			D3D_DRIVER_TYPE_UNKNOWN,
			HMODULE::default(), // software rasterizer module: none (null HMODULE)
			D3D11_CREATE_DEVICE_BGRA_SUPPORT,
			Some(&levels),
			D3D11_SDK_VERSION,
			Some(&mut device),
			Some(&mut got_level),
			Some(&mut context),
		)?;
		// Both out-params are populated on S_OK; unwrap is sound here.
		Ok((device.unwrap(), context.unwrap()))
	}

	/// (Re)build the output duplication and the matching BGRA pool texture. Called from
	/// `create` and from the Reinit path in `run`.
	///
	/// `fast_transient` caps the TRANSIENT-error retry to a short (~600 ms) budget instead of
	/// the long ~5 s one. It's set for an in-session monitor SWITCH build: during a switch the
	/// pacing loop isn't running, so every retry second is frozen on-screen video; and a failed
	/// switch reverts to the previous good output and rebuilds AGAIN, so the long budget would be
	/// paid twice (~10 s freeze). A short budget lets the switch fall back fast to the still-good
	/// previous monitor (stream keeps flowing) and the user can retry. The INITIAL build and the
	/// in-session Reinit recovery keep the long budget (the monitor genuinely needs to come up).
	pub(super) unsafe fn build_duplication(&mut self, fast_transient: bool) -> windows::core::Result<()> {
		// Try DuplicateOutput, retrying on the TRANSIENT failures. `NOT_CURRENTLY_AVAILABLE`
		// and `ACCESS_DENIED` are exactly what DXGI returns while a target monitor is mid
		// fullscreen/mode transition or a hybrid-GPU output is being reparented — and these can
		// persist for SECONDS (a fullscreen game on the target panel). The old 3×200 ms (~600 ms)
		// budget gave up far too early: a switch TO a fullscreen monitor failed → the capture
		// thread died → the client saw a multi-second packet drought + stuck "switching". So
		// retry the transient errors for a generous ~5 s with re-enumeration each round; a
		// NON-transient error (a real failure) still bails fast after a few tries.
		const TRANSIENT_ATTEMPTS: u32 = 25; // ~5 s at REINIT_BACKOFF_MS (200 ms)
		// A SWITCH build pays the retry as a visible freeze (no pacing) and re-pays it on revert,
		// so cap it short and let the caller fall back to the previous good monitor (B30).
		const SWITCH_TRANSIENT_ATTEMPTS: u32 = 3; // ~600 ms
		let transient_budget = if fast_transient {
			SWITCH_TRANSIENT_ATTEMPTS
		} else {
			TRANSIENT_ATTEMPTS
		};
		let mut last_err: Option<windows::core::Error> = None;
		let mut attempt: u32 = 0;
		loop {
			// Prefer the plain DuplicateOutput — DuplicateOutput1 needs an IDXGIOutput5
			// and a format list; the SDR BGRA path here doesn't benefit from it, and
			// DuplicateOutput is the most broadly available entry point.
			match self.output.DuplicateOutput(&self.device) {
				Ok(dup) => {
					// IDXGIOutputDuplication::GetDesc returns the desc BY VALUE (infallible,
					// no out-param) in windows-rs.
					self.dup_desc = dup.GetDesc();
					self.dup = Some(dup);
					self.build_pool()?;
					return Ok(());
				}
				Err(e) => {
					let transient = e.code() == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
						|| e.code() == E_ACCESSDENIED;
					last_err = Some(e);
					// Transient (fullscreen/mode/reparent): re-enumerate the owning output and
					// keep retrying up to the budget. Non-transient: give up after 3 tries.
					let max = if transient { transient_budget } else { 3 };
					if transient {
						if let Ok((_, out1)) = Self::find_output(&self.factory, self.output_idx) {
							self.output = out1;
						}
					}
					attempt += 1;
					if attempt >= max {
						break;
					}
					// Bail out of the long transient-retry budget the moment the session is torn
					// down: the capture thread's caller sets `stop` then joins, and without this
					// check the join would block for the whole remaining ~5 s while we sleep here
					// on a DuplicateOutput that no longer matters (B29).
					if self.stop.load(std::sync::atomic::Ordering::Relaxed) {
						break;
					}
					std::thread::sleep(std::time::Duration::from_millis(REINIT_BACKOFF_MS));
				}
			}
		}
		Err(last_err
			.unwrap_or_else(|| windows::core::Error::from(DXGI_ERROR_NOT_CURRENTLY_AVAILABLE)))
	}

	/// Create the BGRA pool texture sized to the duplicated output. DEFAULT usage with
	/// RT|SRV binds so the encoder's VideoProcessorBlt can read it as an SRV.
	pub(super) unsafe fn build_pool(&mut self) -> windows::core::Result<()> {
		let w = self.dup_desc.ModeDesc.Width;
		let h = self.dup_desc.ModeDesc.Height;
		let desc = D3D11_TEXTURE2D_DESC {
			Width: w,
			Height: h,
			MipLevels: 1,
			ArraySize: 1,
			// Duplication surfaces are B8G8R8A8_UNORM; match it so CopyResource is a
			// straight blit (no format mismatch).
			Format: DXGI_FORMAT_B8G8R8A8_UNORM,
			SampleDesc: DXGI_SAMPLE_DESC {
				Count: 1,
				Quality: 0,
			},
			Usage: D3D11_USAGE_DEFAULT,
			// RENDER_TARGET so cursor compositing can draw into it; SHADER_RESOURCE so the
			// VideoProcessor can sample it as the BGRA→NV12 source.
			BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
			// GPU-only texture — no CPU access (the field is a raw u32 bitmask; 0 = none).
			CPUAccessFlags: 0,
			// Strategy A (same device): NO share flags. (Strategy B would add
			// D3D11_RESOURCE_MISC_SHARED | _SHARED_KEYEDMUTEX here for the cross-adapter copy.)
			MiscFlags: 0,
		};
		let mut tex: Option<ID3D11Texture2D> = None;
		self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
		self.pool = tex;

		// The cursor-composite present texture mirrors the pool desc (same DEFAULT BGRA,
		// RT|SRV binds) so we can RTV-blend the cursor onto it and the encoder can SRV-read
		// it exactly like the pool. Built here so a post-Reinit size change rebuilds it too.
		let mut present: Option<ID3D11Texture2D> = None;
		self.device
			.CreateTexture2D(&desc, None, Some(&mut present))?;
		self.present = present;
		Ok(())
	}

	/// Decide the ENCODE size. Native = the duplicated output size. An EXPLICIT request
	/// (non-zero w&h) is honored exactly (clamped to native, even-aligned). AUTO (0/0) caps at
	/// 1080p — the encoder's VideoProcessor downscales native→encode — keeping the default
	/// bandwidth/latency sane (Parsec/Moonlight default ~1080p); larger needs an explicit pick.
	pub fn target_size(&self, want_w: u32, want_h: u32) -> (u32, u32) {
		let nw = self.dup_desc.ModeDesc.Width.max(2);
		let nh = self.dup_desc.ModeDesc.Height.max(2);
		let (w, h) = if want_w > 0 && want_h > 0 {
			// Explicit: honor it, clamped to native (can't meaningfully upscale a duplication).
			// The VideoProcessor downscales native→encode in the Blt.
			(want_w.min(nw), want_h.min(nh))
		} else if std::env::var("PULSAR_CAP_1080")
			.map(|v| v == "1")
			.unwrap_or(false)
			&& nh > 1080
		{
			// Auto-1080p cap is OPT-IN (PULSAR_CAP_1080=1) until the native→encode VideoProcessor
			// downscale is verified on the hybrid AMD+RTX3080 bridge; default = native (the
			// known-good path), so an explicit client size still gets the downscale.
			((nw as u64 * 1080 / nh as u64) as u32, 1080)
		} else {
			(nw, nh)
		};
		(w & !1, h & !1) // NV12 needs even dims
	}

	/// Native (full) duplicated output size — the capture/pool texture dimensions. The encoder
	/// needs this separately from the (possibly downscaled) encode size to size the cross-adapter
	/// bridge + the VideoProcessor INPUT so the native→encode scale happens in the Blt.
	pub fn native_size(&self) -> (u32, u32) {
		(self.dup_desc.ModeDesc.Width, self.dup_desc.ModeDesc.Height)
	}

	/// Display rotation (degrees CW) the host's screen is set to, from the DXGI duplication
	/// desc. Desktop Duplication hands back the UNROTATED native surface + this rotation, so the
	/// encoder rotates the captured frame BY this amount (in the BGRA→NV12 Blt) to stream what
	/// the viewer actually sees — so every client renders upright with no client-side rotation.
	pub fn rotation_deg(&self) -> u32 {
		use windows::Win32::Graphics::Dxgi::Common::{
			DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_ROTATE90,
		};
		match self.dup_desc.Rotation {
			r if r == DXGI_MODE_ROTATION_ROTATE90 => 90,
			r if r == DXGI_MODE_ROTATION_ROTATE180 => 180,
			r if r == DXGI_MODE_ROTATION_ROTATE270 => 270,
			_ => 0,
		}
	}

	/// Drop the live duplication + pool (so a rebuild starts clean).
	pub(super) unsafe fn teardown_duplication(&mut self) {
		// Releasing the IDXGIOutputDuplication is just dropping the COM ref. Drop the pool
		// too — its size may be wrong after a mode change. Drop the present texture for the
		// same reason; build_pool() rebuilds it at the new size. The compositor pipeline
		// (shaders/blends) is size-independent, so it survives a reinit.
		self.dup = None;
		self.pool = None;
		self.present = None;
	}

	/// Re-enumerate the owning output (it may have moved to the other GPU) and rebuild the
	/// duplication. Backs off `REINIT_BACKOFF_MS` first to let the transition settle.
	pub(super) unsafe fn reinit(&mut self) -> windows::core::Result<()> {
		std::thread::sleep(std::time::Duration::from_millis(REINIT_BACKOFF_MS));
		// Re-pick the output that currently owns the desktop (hybrid-GPU reparenting).
		if let Ok((_, out1)) = Self::find_output(&self.factory, self.output_idx) {
			self.output = out1;
		}
		// In-session recovery (Hz change / transient ACCESS_LOST): keep the long retry budget —
		// the stream is already established and the monitor genuinely needs to come back.
		self.build_duplication(false)
	}
}

// Mark the COM-handle-bearing capture device as Send: it's created and only ever touched
// on the single dedicated capture thread (lib.rs spawns it, never shares it). The raw
// pointers inside the windows-rs interfaces aren't auto-Send, but our usage is
// single-threaded, so this is sound for the thread-handoff in `start_capture_encode`.
unsafe impl Send for CaptureDevice {}
