//! D3D11 / DXGI helpers and the cross-adapter (HYBRID) bridge for the native NVENC
//! encoder. These are the free helpers and RAII guards `Encoder::new`/`submit` rely on:
//! NV12 texture + VideoProcessor creation, the NVIDIA device enumeration, the shared
//! keyed-mutex BGRA bridge, the rtp-dest parser, the NVENC session guard and the
//! NVENC status check. Behaviour is unchanged from the original `encode.rs`.

use std::ffi::c_void;
use std::net::{SocketAddr, ToSocketAddrs};
use std::ptr;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_CPU_ACCESS_READ, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
};
use windows_core::Interface; // for `.cast::<T>()` and `.as_raw()`

use super::{cap_dbg, nvenc, VENDOR_NVIDIA};

/// RAII guard that destroys a half-built NVENC session if `new()` bails between
/// `nvEncOpenEncodeSessionEx` and the final `Ok`. Holds a copy of the destroy fn
/// pointer (not a borrow of the fn table) so the table can be moved into the
/// returned `Encoder` once `disarm()` is called.
pub(super) struct SessionGuard {
    pub(super) destroy: Option<nvenc::PFN_DestroyEncoder>,
    pub(super) enc: *mut c_void,
}
impl SessionGuard {
    pub(super) fn disarm(&mut self) {
        self.enc = ptr::null_mut();
    }
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if !self.enc.is_null() {
            if let Some(f) = self.destroy {
                unsafe {
                    let _ = f(self.enc);
                }
            }
        }
    }
}

/// Parse an `"rtp://host:port"` (or bare `"host:port"`) destination into a
/// `SocketAddr`, resolving the host via `to_socket_addrs()`.
pub(super) fn parse_rtp_dest(dest: &str) -> Result<SocketAddr, String> {
    let hostport = dest.strip_prefix("rtp://").unwrap_or(dest);
    // Strip any trailing path/query the URL form might carry (rtp URLs normally don't).
    let hostport = hostport.split('/').next().unwrap_or(hostport);
    hostport
        .to_socket_addrs()
        .map_err(|e| format!("rtp dest resolve '{hostport}': {e}"))?
        .next()
        .ok_or_else(|| format!("rtp dest '{hostport}' resolved to no address"))
}

/// Create OUR own NV12 `ID3D11Texture2D` (DEFAULT usage, BIND_RENDER_TARGET only —
/// the VideoProcessor output view + NVENC input register both accept that). One
/// non-array, single-mip texture; the encoder Blt's into it every frame.
pub(super) unsafe fn create_nv12_texture(
    device: &ID3D11Device,
    w: u32,
    h: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // RENDER_TARGET so the VideoProcessor can write it AND NVENC can read it as an
        // input image. No CPU access, no share flags (same device — Strategy A).
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut tex))
        .map_err(|e| format!("CreateTexture2D(NV12): {e}"))?;
    tex.ok_or_else(|| "CreateTexture2D(NV12) returned null".to_string())
}

/// Create an ID3D11VideoProcessor (+ its enumerator) configured for a BGRA→NV12
/// playback conversion at WxH. Colour-space defaults are fine for SDR.
pub(super) unsafe fn build_video_processor(
    vdevice: &ID3D11VideoDevice,
    in_w: u32,
    in_h: u32,
    out_w: u32,
    out_h: u32,
) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor), String> {
    // Input = native capture size, Output = (possibly downscaled) encode size — the
    // VideoProcessorBlt scales native→encode when they differ (the 1080p-cap downscale).
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        InputWidth: in_w,
        InputHeight: in_h,
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        OutputWidth: out_w,
        OutputHeight: out_h,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    let vp_enum = vdevice
        .CreateVideoProcessorEnumerator(&content)
        .map_err(|e| format!("CreateVideoProcessorEnumerator: {e}"))?;
    // windows 0.59 returns the processor directly (no out-param).
    let vproc = vdevice
        .CreateVideoProcessor(&vp_enum, 0)
        .map_err(|e| format!("CreateVideoProcessor: {e}"))?;
    Ok((vp_enum, vproc))
}

// ===========================================================================
// Cross-adapter (HYBRID) support: NVIDIA device + shared keyed-mutex bridge
// ===========================================================================

/// Read the PCI `VendorId` of the adapter backing a D3D11 device. We walk
/// device → IDXGIDevice → IDXGIAdapter → GetDesc. Used to decide fast path
/// (capture already on NVIDIA) vs hybrid (AMD/Intel display owner + NVIDIA NVENC).
pub(super) unsafe fn device_vendor_id(device: &ID3D11Device) -> Option<u32> {
    use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
    // ID3D11Device QIs to IDXGIDevice; GetAdapter → GetDesc carries the VendorId.
    let dxgi_dev: IDXGIDevice = device.cast().ok()?;
    let adapter: IDXGIAdapter = dxgi_dev.GetAdapter().ok()?;
    let desc = adapter.GetDesc().ok()?;
    Some(desc.VendorId)
}

/// Enumerate adapters and create a D3D11 device on the FIRST hardware NVIDIA adapter
/// (VendorId 0x10DE), with `BGRA_SUPPORT | VIDEO_SUPPORT` (the VideoProcessor needs
/// VIDEO_SUPPORT — the AMD capture device in dxgi.rs does NOT set it). This is the
/// device the whole NVENC + VideoProcessor + NV12 chain runs on in the hybrid case.
/// Returns Err (→ host falls back to ffmpeg) if there is no usable NVIDIA adapter.
pub(super) unsafe fn create_nvidia_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let factory: IDXGIFactory1 =
        CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

    let mut i = 0u32;
    loop {
        let adapter = match factory.EnumAdapters1(i) {
            Ok(a) => a,
            // DXGI_ERROR_NOT_FOUND = enumerated all adapters without finding NVIDIA.
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => {
                return Err("no NVIDIA adapter found for NVENC".into());
            }
            Err(e) => return Err(format!("EnumAdapters1({i}): {e}")),
        };
        i += 1;

        let desc = match adapter.GetDesc1() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let name = String::from_utf16_lossy(
            &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(0)],
        );
        cap_dbg(&format!(
            "adapter[{}] vendor=0x{:04X} device=0x{:04X} flags=0x{:X} \"{}\"",
            i - 1, desc.VendorId, desc.DeviceId, desc.Flags, name
        ));
        // Skip the WARP/software adapter (DXGI_ADAPTER_FLAG_SOFTWARE bit) — NVENC needs a
        // real NVIDIA GPU. `.0` is i32; mask against the flags u32.
        if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
            continue;
        }
        if desc.VendorId != VENDOR_NVIDIA {
            continue;
        }
        cap_dbg(&format!("→ picked NVIDIA adapter \"{}\" for NVENC", name));

        // Found an NVIDIA adapter — create the device. DRIVER_TYPE_UNKNOWN is mandatory
        // when an explicit adapter is passed. VIDEO_SUPPORT is REQUIRED (VideoProcessor).
        let mut dev: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        match D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        ) {
            Ok(()) => {
                let dev = dev.ok_or("D3D11CreateDevice(NVIDIA) returned null device")?;
                let ctx = ctx.ok_or("D3D11CreateDevice(NVIDIA) returned null context")?;
                return Ok((dev, ctx));
            }
            // This NVIDIA adapter rejected the device (e.g. no video support) — keep
            // scanning in case a second NVIDIA adapter works, else the loop returns Err.
            Err(_) => continue,
        }
    }
}

/// Cross-adapter bridge via a CPU-staging roundtrip (GPU-AGNOSTIC). AMD copies the captured
/// frame into a CPU-readable STAGING texture; `submit` maps it and uploads the pixels to the
/// NVIDIA BGRA (DEFAULT) the VideoProcessor reads. Replaces the keyed-mutex shared-texture path:
/// D3D11 cross-adapter shared keyed-mutex resources are NOT supported on all AMD↔NVIDIA combos
/// (`OpenSharedResource1` → E_INVALIDARG, e.g. this RTX3080 laptop's iGPU↔dGPU), so the CPU hop is
/// the reliable universal path for ANY display-owner GPU. Costs a per-frame readback+upload (a few
/// ms at 1080p); the same-GPU fast path avoids the bridge entirely.
pub(super) struct CrossAdapterBridge {
    /// AMD STAGING (CPU_READ) — the captured `frame.texture` is CopyResource'd here, then mapped.
    pub(super) amd_shared: ID3D11Texture2D,
    pub(super) amd_context: ID3D11DeviceContext,
    /// NVIDIA DEFAULT BGRA — the mapped pixels are uploaded here; the VideoProcessor's input.
    pub(super) nvidia_bgra: ID3D11Texture2D,
    pub(super) kept_amd_device: ID3D11Device,
    pub(super) kept_amd_context: ID3D11DeviceContext,
}

impl CrossAdapterBridge {
    /// Create the shared BGRA on the AMD (capture) device, open it on the NVIDIA device,
    /// and allocate the NVIDIA-side staging. `amd_device`/`amd_ctx` are the capture device;
    /// `nv_device` is the NVENC device; `w`/`h` is the bridge surface size.
    pub(super) unsafe fn create(
        amd_device: &ID3D11Device,
        amd_ctx: &ID3D11DeviceContext,
        nv_device: &ID3D11Device,
        w: u32,
        h: u32,
    ) -> Result<Self, String> {
        // -- 1. AMD STAGING texture (CPU-readable). Each tick the captured BGRA `frame.texture`
        //    is CopyResource'd into this on the AMD device, then mapped for readback. STAGING
        //    can carry NO bind flags. This is the cross-adapter hop's source-of-pixels.
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut amd_staging: Option<ID3D11Texture2D> = None;
        amd_device
            .CreateTexture2D(&staging_desc, None, Some(&mut amd_staging))
            .map_err(|e| format!("CreateTexture2D(AMD staging): {e}"))?;
        let amd_staging = amd_staging.ok_or("AMD staging texture null")?;

        // -- 2. NVIDIA BGRA (DEFAULT, RT|SRV). `submit` uploads the AMD-mapped pixels here
        //    (UpdateSubresource on the NVIDIA context) and the VideoProcessor reads it as the
        //    BGRA→NV12 source. No share flags — it lives entirely on the NVIDIA device.
        let nv_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut nvidia_bgra: Option<ID3D11Texture2D> = None;
        nv_device
            .CreateTexture2D(&nv_desc, None, Some(&mut nvidia_bgra))
            .map_err(|e| format!("CreateTexture2D(NVIDIA BGRA): {e}"))?;
        let nvidia_bgra = nvidia_bgra.ok_or("NVIDIA BGRA null")?;

        Ok(CrossAdapterBridge {
            amd_shared: amd_staging,
            // AddRef the AMD capture device/context so the staging stays valid for the
            // encoder's whole life (balanced in flush_and_close).
            amd_context: amd_ctx.clone(),
            nvidia_bgra,
            kept_amd_device: amd_device.clone(),
            kept_amd_context: amd_ctx.clone(),
        })
    }
}

/// Map a non-success NVENC status to an error string, appending the driver's last
/// error message when available.
pub(super) unsafe fn chk(
    s: nvenc::NVENCSTATUS,
    fns: &nvenc::NV_ENCODE_API_FUNCTION_LIST,
    enc: *mut c_void,
) -> Result<(), String> {
    if s == nvenc::NV_ENC_SUCCESS {
        return Ok(());
    }
    // nvEncGetLastErrorString(enc) → const char*; only meaningful once a session exists.
    let mut msg = String::new();
    if !enc.is_null() {
        if let Some(f) = fns.nvEncGetLastErrorString {
            let p = f(enc);
            if !p.is_null() {
                let cstr = std::ffi::CStr::from_ptr(p);
                msg = cstr.to_string_lossy().into_owned();
            }
        }
    }
    if msg.is_empty() {
        Err(format!("nvenc status {s}"))
    } else {
        Err(format!("nvenc status {s} ({msg})"))
    }
}
