//! Host-silent-while-streaming via **default-render-endpoint redirection**
//! (Sunshine's model), not muting.
//!
//! The Pulsar host streams its audio by tapping the default render endpoint with
//! WASAPI loopback. On common codecs (verified Realtek) that loopback tap is
//! **post master-volume AND post endpoint-mute at OPEN time**: a loopback opened
//! while the endpoint is muted or at volume 0 captures **pure silence** and stays
//! latched. So we must **never** silence the host by muting or zeroing the
//! captured endpoint — see [`super::mute`] for why the user `mute_host` toggle is
//! handled carefully and never forced in game mode.
//!
//! Sunshine solves "play to the remote client but stay silent on the host
//! speakers" by **redirecting the default render endpoint to a sinkless virtual
//! audio device** (its "Steam Streaming Speakers"), capturing *that* device's
//! loopback, and **restoring the original default on teardown**. Pulsar bundles
//! the MIT/MS-PL `Virtual-Audio-Driver` as that virtual sink (same driver-bundling
//! pattern as ViGEm/Interception). This module is the sink-switching half: locate
//! the virtual sink, save/replace the default render endpoint for all three
//! `ERole`s, and restore it. Capturing the redirected device's loopback is the
//! existing [`super::loopback`] path, unchanged.
//!
//! The whole Windows implementation drives the **undocumented `IPolicyConfig`**
//! COM interface (CLSID `CPolicyConfigClient`) via raw IUnknown-vtable FFI — the
//! same interface Sunshine, EarTrumpet and NirCmd use. See `PolicyConfig.h` in the
//! Sunshine tree. Non-Windows targets get no-op stubs so the module can be exported
//! unconditionally from `audio.rs`.

#![allow(clippy::result_large_err)]

use super::{ChannelLayout, SAMPLE_RATE};

/// A located virtual-sink render endpoint: its stable device-id string (the value
/// `IPolicyConfig::SetDefaultEndpoint` and `IMMDevice::GetId` use) and the friendly
/// name it matched on, for logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkDevice {
	/// The Windows endpoint device-id, e.g.
	/// `{0.0.0.00000000}.{29dd7668-45b2-4846-882d-950f55bf7eb8}`.
	pub id: String,
	/// The endpoint friendly name that matched (for logs / diagnostics).
	pub friendly_name: String,
}

/// Find an **active render endpoint** whose friendly name contains `needle`
/// (case-insensitive), e.g. our bundled virtual sink. Returns the first match.
///
/// On non-Windows this always returns `None` (no Core Audio policy model).
pub fn find_render_device_by_name(needle: &str) -> Result<Option<SinkDevice>, String> {
	#[cfg(windows)]
	{
		imp::find_render_device_by_name(needle)
	}
	#[cfg(not(windows))]
	{
		let _ = needle;
		Ok(None)
	}
}

/// The device-id of the **current default render endpoint** (the `eConsole` role —
/// what loopback captures by default). `Ok(None)` if there is no default device.
///
/// On non-Windows this always returns `Ok(None)`.
pub fn default_render_device_id() -> Result<Option<String>, String> {
	#[cfg(windows)]
	{
		imp::default_render_device_id()
	}
	#[cfg(not(windows))]
	{
		Ok(None)
	}
}

/// Set `device_id` as the default render endpoint for **all three `ERole`s**
/// (`eConsole`, `eMultimedia`, `eCommunications`) via `IPolicyConfig`, exactly as
/// Sunshine's `set_sink` does. Mirrors Sunshine: a failure on any single role is
/// counted but the others are still attempted; an error is only returned if *every*
/// role failed (so a partial success — which still moves the active stream — wins).
///
/// On non-Windows this is a no-op returning `Ok(())`.
pub fn set_default_render_device(device_id: &str) -> Result<(), String> {
	#[cfg(windows)]
	{
		imp::set_default_render_device(device_id)
	}
	#[cfg(not(windows))]
	{
		let _ = device_id;
		Ok(())
	}
}

/// Set the **playback (device) format** of `device_id` to `layout`'s channel count
/// at 48 kHz via `IPolicyConfig::SetDeviceFormat`, ports Sunshine's `set_format`.
///
/// Pulsar's bundled virtual sink advertises stereo/5.1/7.1 mix formats; the OS only
/// captures (via loopback) the channels the endpoint's *current* device format
/// exposes, so to stream 5.1/7.1 we must first switch the sink's format to that
/// channel count. This builds a `WAVEFORMATEXTENSIBLE` (float or — mirroring
/// Sunshine — the **current default device's bit depth**, to dodge the 16→24-bit
/// glitch some users hit) with the standard `dwChannelMask` for 2/6/8 channels and
/// the matching `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`/`_PCM` subformat, then drives the
/// undocumented `SetDeviceFormat` vtable slot.
///
/// **Call this on the virtual sink BEFORE redirecting the default to it** (the
/// redirect's loopback capture then opens at the right channel count). Best-effort:
/// any failure returns `Err` and **never panics** — the caller falls back to the
/// sink's existing (typically stereo) format.
///
/// On non-Windows this is a no-op returning `Ok(())`.
pub fn set_render_device_format(device_id: &str, layout: ChannelLayout) -> Result<(), String> {
	#[cfg(windows)]
	{
		imp::set_render_device_format(device_id, layout)
	}
	#[cfg(not(windows))]
	{
		let _ = (device_id, layout);
		Ok(())
	}
}

/// RAII guard that on construction saves the current default render endpoint and
/// switches the default to a target device (all three roles), and on `Drop`
/// restores the saved default.
///
/// It also writes a **crash-recovery marker** holding the *saved* (original)
/// default device-id, mirroring the volume-marker pattern the mute path used: if
/// the process dies before `Drop` runs (crash / taskkill / tray-quit), the next
/// launch calls [`restore_stale_redirect`] to put the original default back so the
/// host is never left pointing at the sinkless virtual sink (which would leave the
/// user's real speakers silent until they re-pick the output by hand).
///
/// Construct it with [`SinkRedirectGuard::redirect_to`]. On non-Windows the guard
/// is a do-nothing placeholder so callers compile everywhere.
pub struct SinkRedirectGuard {
	/// The original default render device-id to restore on drop. `None` when there
	/// was no saved default (nothing to restore) or on non-Windows.
	saved_default: Option<String>,
	/// Set once we've actually switched, so `Drop` only restores if we changed it.
	active: bool,
}

impl SinkRedirectGuard {
	/// Save the current default render endpoint, then redirect the default to
	/// `target_device_id` for all three roles and persist the saved default to the
	/// crash-recovery marker. On any failure to switch, the marker is removed and
	/// the error is returned (nothing is left half-redirected from our side).
	///
	/// Before switching, this also clears any **stale** marker from a previous run
	/// (restoring that run's saved default first) so a crash-then-restart can't
	/// leave the marker pointing at an even-older default.
	///
	/// On non-Windows this returns an inert guard (`Ok`) without touching anything.
	pub fn redirect_to(target_device_id: &str) -> Result<Self, String> {
		#[cfg(windows)]
		{
			// Recover from any prior crash first so we don't overwrite an older saved
			// default with the (virtual-sink) value some dead process left active.
			restore_stale_redirect();

			let saved = default_render_device_id()?;

			// Persist the ORIGINAL default for crash recovery BEFORE applying the
			// redirect: the host can only end up on the virtual sink AFTER this marker
			// exists, so a crash/taskkill/relaunch in the redirect window always leaves a
			// recoverable marker (restore_stale_redirect on the next launch puts the real
			// default back). Writing it after the switch left a window where the host was
			// stranded on the sinkless virtual sink with no marker = no local audio until a
			// manual re-pick. An empty/absent prior default is an empty marker (no-op restore).
			match saved.as_deref() {
				Some(orig) => {
					let _ = std::fs::write(imp::marker_path(), orig.as_bytes());
				}
				None => {
					let _ = std::fs::write(imp::marker_path(), b"");
				}
			}

			// If the switch itself fails, drop the marker we just wrote (the host never
			// moved, so there is nothing to recover) and surface the error.
			if let Err(e) = set_default_render_device(target_device_id) {
				let _ = std::fs::remove_file(imp::marker_path());
				return Err(e);
			}

			Ok(Self {
				saved_default: saved,
				active: true,
			})
		}
		#[cfg(not(windows))]
		{
			let _ = target_device_id;
			Ok(Self {
				saved_default: None,
				active: false,
			})
		}
	}

	/// The original default render device-id this guard will restore, if any.
	pub fn saved_default(&self) -> Option<&str> {
		self.saved_default.as_deref()
	}
}

impl Drop for SinkRedirectGuard {
	fn drop(&mut self) {
		#[cfg(windows)]
		{
			if self.active {
				if let Some(orig) = self.saved_default.as_deref() {
					// Best-effort restore; a failure here is logged by the callee.
					let _ = set_default_render_device(orig);
				}
				// Clean teardown leaves no stale marker for the next launch to act on.
				let _ = std::fs::remove_file(imp::marker_path());
			}
		}
		#[cfg(not(windows))]
		{
			let _ = self.active;
		}
	}
}

/// Next-launch crash recovery: if a previous run redirected the default render
/// endpoint and died before its [`SinkRedirectGuard`] dropped, the marker file
/// holds the original default device-id — restore it and delete the marker so the
/// host is never stranded on the sinkless virtual sink. Safe to call
/// unconditionally and repeatedly; a no-op when no marker exists.
///
/// Call this once early on host startup (the Integrate phase wires it in), the same
/// way the volume path called `restore_stale_mute`.
pub fn restore_stale_redirect() {
	#[cfg(windows)]
	{
		imp::restore_stale_redirect();
	}
}

/// The pure, platform-independent parameters of the `WAVEFORMATEXTENSIBLE` we ask
/// `IPolicyConfig::SetDeviceFormat` to install on the virtual sink. Split out from
/// the (Windows-only) COM struct so the channel-mask / block-align arithmetic — the
/// bit that's easy to get wrong — is unit-testable on every platform.
///
/// The format is always 48 kHz (Opus's encode rate, matching the rest of the
/// pipeline) and `WAVE_FORMAT_EXTENSIBLE` (required once `channels > 2` or for an
/// explicit channel mask / float subtype).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaveFormatParams {
	/// Channel count (2 / 6 / 8).
	channels: u16,
	/// Sample rate, always [`super::SAMPLE_RATE`] (48 kHz).
	sample_rate: u32,
	/// Bits per (container) sample: 16 or 32. We store the *container* width here
	/// (`wBitsPerSample`); the valid-bits is carried separately.
	bits_per_sample: u16,
	/// Valid bits per sample (`Samples.wValidBitsPerSample`); ≤ `bits_per_sample`.
	valid_bits_per_sample: u16,
	/// `true` → IEEE float subtype, `false` → integer PCM subtype.
	is_float: bool,
	/// Standard `KSAUDIO_SPEAKER_*` `dwChannelMask` for the channel count.
	channel_mask: u32,
}

/// The standard `KSAUDIO_SPEAKER_*` `dwChannelMask` for a channel count: 2 → stereo,
/// 6 → 5.1 (FL FR FC LFE BL BR), 8 → 7.1 (… + SL SR). Any other count degrades to
/// the stereo mask (never panics). Speaker bit values are the fixed ksmedia.h
/// `SPEAKER_*` constants.
const fn channel_mask_for(channels: u16) -> u32 {
	// SPEAKER_*: FL=0x1 FR=0x2 FC=0x4 LFE=0x8 BL=0x10 BR=0x20 SL=0x200 SR=0x400.
	const FL: u32 = 0x1;
	const FR: u32 = 0x2;
	const FC: u32 = 0x4;
	const LFE: u32 = 0x8;
	const BL: u32 = 0x10;
	const BR: u32 = 0x20;
	const SL: u32 = 0x200;
	const SR: u32 = 0x400;
	match channels {
		6 => FL | FR | FC | LFE | BL | BR,
		8 => FL | FR | FC | LFE | BL | BR | SL | SR,
		_ => FL | FR,
	}
}

impl WaveFormatParams {
	/// Build the 48 kHz parameters for `layout`, retaining `bits` as the bit depth
	/// (16 → 16-bit PCM, 24 → 24-in-32 PCM, anything else → 32-bit float). Mirrors
	/// Sunshine, which retains the current default device's bit depth to dodge the
	/// 16→24-bit switch glitch — and matches the virtual sink's advertised mix
	/// formats.
	fn for_layout(layout: ChannelLayout, bits: u16) -> Self {
		let channels = layout.channels();
		let (bits_per_sample, valid_bits_per_sample, is_float) = match bits {
			16 => (16, 16, false),
			// 24-bit audio is carried in a 32-bit container (wValidBitsPerSample=24).
			24 => (32, 24, false),
			_ => (32, 32, true),
		};
		Self {
			channels,
			sample_rate: SAMPLE_RATE,
			bits_per_sample,
			valid_bits_per_sample,
			is_float,
			channel_mask: channel_mask_for(channels),
		}
	}

	/// `nBlockAlign` = channels × container-bytes-per-sample.
	fn block_align(&self) -> u16 {
		self.channels * (self.bits_per_sample / 8)
	}

	/// `nAvgBytesPerSec` = sample_rate × block_align.
	fn avg_bytes_per_sec(&self) -> u32 {
		self.sample_rate * self.block_align() as u32
	}
}

#[cfg(windows)]
mod imp {
	//! Windows `IPolicyConfig` sink-switching via raw IUnknown-vtable FFI.
	//!
	//! `IPolicyConfig` is undocumented and has no `windows`-crate binding, so we
	//! declare its vtable by hand and call `SetDefaultEndpoint` through it. Endpoint
	//! enumeration and the default-device query use the typed `windows`-crate Core
	//! Audio bindings (those ARE generated under the enabled features). Reading the
	//! endpoint **friendly name** also goes through a hand-rolled vtable call,
	//! because `IMMDevice::OpenPropertyStore` / `IPropertyStore` are only generated
	//! when the `Win32_UI_Shell_PropertiesSystem` feature is enabled (it is not —
	//! see the module-level concern), but the COM objects expose those slots
	//! regardless of which Rust bindings were generated.

	use std::ffi::c_void;

	use windows::core::{Interface, GUID, HRESULT, PCWSTR, PWSTR};
	use windows::Win32::Foundation::PROPERTYKEY;
	use windows::Win32::Media::Audio::{
		eCommunications, eConsole, eMultimedia, eRender, IMMDevice, IMMDeviceEnumerator,
		MMDeviceEnumerator, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
	};
	use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
	use windows::Win32::System::Com::{
		CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
	};

	// CLSID_CPolicyConfigClient {870af99c-171d-4f9e-af0d-e63df40c2bc9}
	const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
	// IID_IPolicyConfig {f8679f50-850a-41cf-9c72-430f290290c8} (Windows 7+, the Vista
	// IID differs but the SetDefaultEndpoint slot ordering used here is the same; the
	// modern IID resolves on every supported OS).
	const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

	// PKEY_Device_FriendlyName — DEVPROP {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 14.
	// Declared inline (à la Sunshine's DEFINE_PROPERTYKEY) so we don't need the
	// FunctionDiscovery / Devices_Properties feature just for this one constant.
	const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
		fmtid: GUID::from_values(
			0xa45c254e,
			0xdf1c,
			0x4efd,
			[0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
		),
		pid: 14,
	};

	// PKEY_DeviceInterface_FriendlyName — the *adapter/interface* friendly name
	// {026e516e-b814-414b-83cd-856d6fef4822}, pid 2. Sunshine matches the needle
	// against this too (e.g. "Steam Streaming Speakers" lives here, not in the
	// endpoint friendly name).
	const PKEY_DEVICEINTERFACE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
		fmtid: GUID::from_values(
			0x026e516e,
			0xb814,
			0x414b,
			[0x83, 0xcd, 0x85, 0x6d, 0x6f, 0xef, 0x48, 0x22],
		),
		pid: 2,
	};

	// PKEY_Device_DeviceDesc — the endpoint *description* (shorter than the friendly
	// name) {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 2. Same fmtid as
	// PKEY_Device_FriendlyName (pid 14); only the pid differs.
	const PKEY_DEVICE_DEVICEDESC: PROPERTYKEY = PROPERTYKEY {
		fmtid: GUID::from_values(
			0xa45c254e,
			0xdf1c,
			0x4efd,
			[0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
		),
		pid: 2,
	};

	// PKEY_AudioEngine_DeviceFormat — the endpoint's current mix WAVEFORMATEX, stored
	// as a VT_BLOB. {f19f064d-082c-4e27-bc73-6882a1bb8e4c}, pid 0. Sunshine reads the
	// default device's bit depth from here to avoid the 16→24-bit switch glitch.
	const PKEY_AUDIOENGINE_DEVICEFORMAT: PROPERTYKEY = PROPERTYKEY {
		fmtid: GUID::from_values(
			0xf19f064d,
			0x082c,
			0x4e27,
			[0xbc, 0x73, 0x68, 0x82, 0xa1, 0xbb, 0x8e, 0x4c],
		),
		pid: 0,
	};

	// ---- WAVEFORMATEXTENSIBLE construction constants ----
	//
	// These live in ksmedia.h / mmreg.h and are not surfaced by the enabled
	// `windows`-crate features, so declare them inline (à la the PROPERTYKEYs above).

	/// `WAVE_FORMAT_EXTENSIBLE` format tag (mmreg.h).
	const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

	/// `KSDATAFORMAT_SUBTYPE_PCM` {00000001-0000-0010-8000-00aa00389b71}.
	const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_values(
		0x00000001,
		0x0000,
		0x0010,
		[0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
	);
	/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` {00000003-0000-0010-8000-00aa00389b71}.
	const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_values(
		0x00000003,
		0x0000,
		0x0010,
		[0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
	);

	// ---- IPolicyConfig: hand-declared vtable (only the slots we need are typed) ----

	/// The `IPolicyConfig` vtable. Layout MUST match `PolicyConfig.h` exactly:
	/// IUnknown (3) then GetMixFormat, GetDeviceFormat, ResetDeviceFormat,
	/// SetDeviceFormat, GetProcessingPeriod, SetProcessingPeriod, GetShareMode,
	/// SetShareMode, GetPropertyValue, SetPropertyValue, **SetDefaultEndpoint**,
	/// SetEndpointVisibility. We call `SetDefaultEndpoint` and **`SetDeviceFormat`**;
	/// every other slot is declared (as opaque `usize`) so its offset is correct.
	///
	/// `SetDeviceFormat(PCWSTR deviceId, WAVEFORMATEX* endpointFormat, WAVEFORMATEX*
	/// mixFormat)` — the third arg is an in/out mix-format buffer the OS fills; we pass
	/// a scratch `WAVEFORMATEXTENSIBLE` for it (Sunshine passes a stack `p`).
	#[repr(C)]
	#[allow(non_snake_case)]
	struct IPolicyConfigVtbl {
		// IUnknown
		QueryInterface:
			unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
		AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
		Release: unsafe extern "system" fn(*mut c_void) -> u32,
		// IPolicyConfig (slots we don't call are opaque so their offsets stay correct)
		GetMixFormat: usize,
		GetDeviceFormat: usize,
		ResetDeviceFormat: usize,
		SetDeviceFormat: unsafe extern "system" fn(
			*mut c_void,
			PCWSTR,
			*const WAVEFORMATEX,
			*mut WAVEFORMATEX,
		) -> HRESULT,
		GetProcessingPeriod: usize,
		SetProcessingPeriod: usize,
		GetShareMode: usize,
		SetShareMode: usize,
		GetPropertyValue: usize,
		SetPropertyValue: usize,
		SetDefaultEndpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT,
		SetEndpointVisibility: usize,
	}

	/// Thin owning wrapper around a raw `IPolicyConfig*`. Releases on drop.
	struct PolicyConfig(*mut c_void);

	impl PolicyConfig {
		/// `CoCreateInstance(CLSID_CPolicyConfigClient, IID_IPolicyConfig)`.
		///
		/// We create the object as a generic `IUnknown` (the `windows` crate has no
		/// `IPolicyConfig` binding) and `QueryInterface` it to the `IPolicyConfig`
		/// pointer we drive through our own vtable.
		unsafe fn create() -> Result<Self, String> {
			let unknown: windows::core::IUnknown =
				CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
					.map_err(|e| format!("CoCreateInstance(CPolicyConfigClient): {e}"))?;
			let mut ptr: *mut c_void = std::ptr::null_mut();
			let q = (unknown.vtable().QueryInterface)(
				unknown.as_raw(),
				&IID_IPOLICY_CONFIG,
				&mut ptr,
			);
			if q.is_err() || ptr.is_null() {
				return Err(format!("QueryInterface(IPolicyConfig): 0x{:08x}", q.0));
			}
			Ok(Self(ptr))
		}

		unsafe fn vtbl(&self) -> &IPolicyConfigVtbl {
			&**(self.0 as *const *const IPolicyConfigVtbl)
		}

		/// `SetDefaultEndpoint(device_id, role)`. `role` is the raw `ERole` i32.
		unsafe fn set_default_endpoint(&self, device_id: &[u16], role: i32) -> HRESULT {
			(self.vtbl().SetDefaultEndpoint)(self.0, PCWSTR(device_id.as_ptr()), role)
		}

		/// `SetDeviceFormat(device_id, format, &mut scratch_mix)`. Per Sunshine we copy
		/// the format locally and hand the undocumented API a private scratch buffer for
		/// the in/out mix-format slot (slot index 3 of the vtable).
		unsafe fn set_device_format(
			&self,
			device_id: &[u16],
			format: &WAVEFORMATEXTENSIBLE,
		) -> HRESULT {
			// Copy so the undocumented callee never sees a borrowed/aliased pointer
			// (Sunshine copies the waveformat too before the call).
			let format_copy = *format;
			let mut scratch = WAVEFORMATEXTENSIBLE::default();
			(self.vtbl().SetDeviceFormat)(
				self.0,
				PCWSTR(device_id.as_ptr()),
				&format_copy as *const WAVEFORMATEXTENSIBLE as *const WAVEFORMATEX,
				&mut scratch as *mut WAVEFORMATEXTENSIBLE as *mut WAVEFORMATEX,
			)
		}
	}

	impl Drop for PolicyConfig {
		fn drop(&mut self) {
			unsafe {
				if !self.0.is_null() {
					(self.vtbl().Release)(self.0);
				}
			}
		}
	}

	// ---- IPropertyStore: minimal hand-declared vtable for GetValue ----

	#[repr(C)]
	#[allow(non_snake_case)]
	struct IPropertyStoreVtbl {
		QueryInterface:
			unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
		AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
		Release: unsafe extern "system" fn(*mut c_void) -> u32,
		GetCount: usize,
		GetAt: usize,
		GetValue: unsafe extern "system" fn(
			*mut c_void,
			*const PROPERTYKEY,
			*mut PROPVARIANT,
		) -> HRESULT,
		SetValue: usize,
		Commit: usize,
	}

	/// `IMMDevice` vtable — only the slots up to `OpenPropertyStore` are typed (we
	/// call that), the rest are reached via the typed `windows` binding. Layout per
	/// `mmdeviceapi.h`: IUnknown (3), Activate, **OpenPropertyStore**, GetId,
	/// GetState. We call `OpenPropertyStore` by hand because the typed binding for it
	/// is compiled out without the `Win32_UI_Shell_PropertiesSystem` feature.
	#[repr(C)]
	#[allow(non_snake_case)]
	struct IMMDeviceVtbl {
		QueryInterface:
			unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
		AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
		Release: unsafe extern "system" fn(*mut c_void) -> u32,
		Activate: usize,
		OpenPropertyStore:
			unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
		GetId: usize,
		GetState: usize,
	}

	/// Open a device's `IPropertyStore` (read-only) via the hand-rolled
	/// `OpenPropertyStore` slot. Returns the raw store pointer (the caller must
	/// `Release` it) or `None`.
	unsafe fn open_property_store(device: &IMMDevice) -> Option<*mut c_void> {
		let dev_raw = device.as_raw();
		let dev_vtbl = &**(dev_raw as *const *const IMMDeviceVtbl);
		let mut store: *mut c_void = std::ptr::null_mut();
		// STGM_READ is a STGM(u32) newtype; pass its raw value.
		let hr = (dev_vtbl.OpenPropertyStore)(dev_raw, STGM_READ.0, &mut store);
		if hr.is_err() || store.is_null() {
			return None;
		}
		Some(store)
	}

	/// Read one **string** property (`key`) from an already-open property store via
	/// `IPropertyStore::GetValue`. Returns the UTF-8 value, or `None` if the property
	/// is absent / not a string. Does not release the store.
	unsafe fn store_string(store: *mut c_void, key: &PROPERTYKEY) -> Option<String> {
		let store_vtbl = &**(store as *const *const IPropertyStoreVtbl);
		let mut prop = PROPVARIANT::default();
		let getv = (store_vtbl.GetValue)(store, key, &mut prop);
		let val = if getv.is_ok() {
			// PROPVARIANT for a string property holds pwszVal in the inner union.
			let pwsz: PWSTR = prop.Anonymous.Anonymous.Anonymous.pwszVal;
			if pwsz.is_null() {
				None
			} else {
				pwsz.to_string().ok()
			}
		} else {
			None
		};
		let _ = PropVariantClear(&mut prop);
		val
	}

	/// If `needle_lc` (already lower-cased) is a substring of any of the device's
	/// **friendly name**, **adapter/interface friendly name**, or **device
	/// description**, return the endpoint friendly name to report (falling back to
	/// whichever of the three we could read, so the log line is never blank).
	/// Otherwise `None`. Mirrors Sunshine's `match_all_fields`: the needle (e.g.
	/// "Steam Streaming Speakers" / "Virtual Audio") can live in any of these.
	unsafe fn device_matches(device: &IMMDevice, needle_lc: &str) -> Option<String> {
		let store = open_property_store(device)?;
		let store_vtbl = &**(store as *const *const IPropertyStoreVtbl);

		let friendly = store_string(store, &PKEY_DEVICE_FRIENDLY_NAME);
		let adapter = store_string(store, &PKEY_DEVICEINTERFACE_FRIENDLY_NAME);
		let desc = store_string(store, &PKEY_DEVICE_DEVICEDESC);
		(store_vtbl.Release)(store);

		let hit = [friendly.as_deref(), adapter.as_deref(), desc.as_deref()]
			.into_iter()
			.flatten()
			.any(|v| v.to_lowercase().contains(needle_lc));

		if hit {
			friendly.or(adapter).or(desc).or_else(|| Some(String::new()))
		} else {
			None
		}
	}

	/// Read the current default render device's container bit depth from
	/// `PKEY_AudioEngine_DeviceFormat` (a `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` blob).
	/// Best-effort: returns the **valid** bits-per-sample (e.g. 16, 24, 32) so the
	/// virtual sink can match it (Sunshine does this to avoid the 16→24-bit glitch),
	/// or `None` if there is no default device / the property is unavailable.
	unsafe fn default_device_bits() -> Option<u16> {
		let enumerator = device_enumerator().ok()?;
		let device = enumerator
			.GetDefaultAudioEndpoint(eRender, eConsole)
			.ok()?;
		let store = open_property_store(&device)?;
		let store_vtbl = &**(store as *const *const IPropertyStoreVtbl);

		let mut prop = PROPVARIANT::default();
		let getv = (store_vtbl.GetValue)(store, &PKEY_AUDIOENGINE_DEVICEFORMAT, &mut prop);
		let bits = if getv.is_ok() {
			// VT_BLOB: blob.pBlobData points at a WAVEFORMATEX (+ extension). The
			// extensible form carries wValidBitsPerSample; the base form only has
			// wBitsPerSample. Read defensively via unaligned loads (packed struct).
			let blob = prop.Anonymous.Anonymous.Anonymous.blob;
			let p = blob.pBlobData as *const WAVEFORMATEX;
			if p.is_null() || (blob.cbSize as usize) < std::mem::size_of::<WAVEFORMATEX>() {
				None
			} else {
				let wfx = std::ptr::read_unaligned(p);
				if wfx.wFormatTag == WAVE_FORMAT_EXTENSIBLE
					&& (blob.cbSize as usize) >= std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
				{
					let wfex = std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE);
					Some(wfex.Samples.wValidBitsPerSample)
				} else {
					Some(wfx.wBitsPerSample)
				}
			}
		} else {
			None
		};
		let _ = PropVariantClear(&mut prop);
		(store_vtbl.Release)(store);
		bits.filter(|&b| b > 0)
	}

	/// MTA-init COM on the calling thread (S_FALSE = already initialized — fine) and
	/// create the device enumerator. Shared by every entry point.
	unsafe fn device_enumerator() -> Result<IMMDeviceEnumerator, String> {
		let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
		CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
			.map_err(|e| format!("MMDeviceEnumerator: {e}"))
	}

	/// Read an `IMMDevice`'s endpoint id as a Rust `String`, freeing the COM buffer.
	unsafe fn device_id_string(device: &IMMDevice) -> Result<String, String> {
		let pwstr: PWSTR = device.GetId().map_err(|e| format!("GetId: {e}"))?;
		if pwstr.is_null() {
			return Err("GetId returned null".into());
		}
		let s = pwstr.to_string().map_err(|e| format!("GetId utf16: {e}"))?;
		CoTaskMemFree(Some(pwstr.0 as *const c_void));
		Ok(s)
	}

	pub(super) fn find_render_device_by_name(
		needle: &str,
	) -> Result<Option<super::SinkDevice>, String> {
		let needle_lc = needle.to_lowercase();
		unsafe {
			let enumerator = device_enumerator()?;
			let collection = enumerator
				.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
				.map_err(|e| format!("EnumAudioEndpoints: {e}"))?;
			let count = collection
				.GetCount()
				.map_err(|e| format!("GetCount: {e}"))?;
			for i in 0..count {
				let device = match collection.Item(i) {
					Ok(d) => d,
					Err(_) => continue,
				};
				// Match the needle across friendly name / adapter name / description
				// (Sunshine's match_all_fields), not just the endpoint friendly name.
				let Some(name) = device_matches(&device, &needle_lc) else {
					continue;
				};
				let id = device_id_string(&device)?;
				return Ok(Some(super::SinkDevice {
					id,
					friendly_name: name,
				}));
			}
			Ok(None)
		}
	}

	pub(super) fn default_render_device_id() -> Result<Option<String>, String> {
		unsafe {
			let enumerator = device_enumerator()?;
			// No default device (e.g. all endpoints removed) is `Ok(None)`, not an
			// error — the caller treats "nothing to save/restore" as benign.
			let device = match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
				Ok(d) => d,
				Err(_) => return Ok(None),
			};
			Ok(Some(device_id_string(&device)?))
		}
	}

	pub(super) fn set_default_render_device(device_id: &str) -> Result<(), String> {
		// Wide, NUL-terminated copy for the undocumented API (never hand it a
		// non-owned / non-terminated pointer — Sunshine copies the string too).
		let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
		unsafe {
			let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
			let policy = PolicyConfig::create()?;
			let roles = [eConsole, eMultimedia, eCommunications];
			let mut failures = 0u32;
			let mut last_err = HRESULT(0);
			for role in roles {
				let hr = policy.set_default_endpoint(&wide, role.0);
				if hr.is_err() {
					failures += 1;
					last_err = hr;
				}
			}
			if failures == roles.len() as u32 {
				Err(format!(
					"SetDefaultEndpoint failed for all roles: 0x{:08x}",
					last_err.0
				))
			} else {
				Ok(())
			}
		}
	}

	/// Materialize the pure [`super::WaveFormatParams`] into a real
	/// `WAVEFORMATEXTENSIBLE` (the COM struct the undocumented `SetDeviceFormat`
	/// expects). `cbSize` is the WAVEFORMATEXTENSIBLE extension size (22 bytes).
	fn build_waveformatextensible(p: &super::WaveFormatParams) -> WAVEFORMATEXTENSIBLE {
		let mut wf = WAVEFORMATEXTENSIBLE::default();
		wf.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
		wf.Format.nChannels = p.channels;
		wf.Format.nSamplesPerSec = p.sample_rate;
		wf.Format.wBitsPerSample = p.bits_per_sample;
		wf.Format.nBlockAlign = p.block_align();
		wf.Format.nAvgBytesPerSec = p.avg_bytes_per_sec();
		// cbSize for WAVE_FORMAT_EXTENSIBLE = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX) = 22.
		wf.Format.cbSize = (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
			- std::mem::size_of::<WAVEFORMATEX>()) as u16;
		wf.Samples.wValidBitsPerSample = p.valid_bits_per_sample;
		wf.dwChannelMask = p.channel_mask;
		wf.SubFormat = if p.is_float {
			KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
		} else {
			KSDATAFORMAT_SUBTYPE_PCM
		};
		wf
	}

	pub(super) fn set_render_device_format(
		device_id: &str,
		layout: super::ChannelLayout,
	) -> Result<(), String> {
		// Wide, NUL-terminated copy for the undocumented API (Sunshine copies too).
		let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
		unsafe {
			let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
			// Retain the current default device's bit depth (Sunshine's anti-glitch
			// trick); fall back to 32-bit float if we can't read it.
			let bits = default_device_bits().unwrap_or(32);
			let params = super::WaveFormatParams::for_layout(layout, bits);
			let wf = build_waveformatextensible(&params);

			let policy = PolicyConfig::create()?;
			let hr = policy.set_device_format(&wide, &wf);
			if hr.is_err() {
				Err(format!(
					"SetDeviceFormat({} ch, {} bit) failed: 0x{:08x}",
					params.channels, params.valid_bits_per_sample, hr.0
				))
			} else {
				Ok(())
			}
		}
	}

	/// Crash-recovery marker holding the saved (original) default render device-id,
	/// mirroring the `pulsar-host-mute.vol` marker the volume path used.
	pub(super) fn marker_path() -> std::path::PathBuf {
		std::env::temp_dir().join("pulsar-sink-redirect.dev")
	}

	pub(super) fn restore_stale_redirect() {
		let path = marker_path();
		let Ok(bytes) = std::fs::read(&path) else {
			return; // no marker → clean previous exit, nothing to restore
		};
		// Consume the marker first so a failure below can't loop us forever.
		let _ = std::fs::remove_file(&path);
		let Ok(orig) = String::from_utf8(bytes) else {
			return;
		};
		let orig = orig.trim();
		if orig.is_empty() {
			// The previous run had no saved default (no device to restore) — nothing
			// to do beyond having cleared the marker.
			return;
		}
		if let Err(e) = set_default_render_device(orig) {
			tracing::warn!("sink redirect crash-restore failed: {e}");
		} else {
			tracing::info!("restored default render endpoint after a prior crash");
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sink_device_roundtrips() {
		let d = SinkDevice {
			id: "{0.0.0.00000000}.{deadbeef}".to_string(),
			friendly_name: "Virtual Audio Device".to_string(),
		};
		assert_eq!(d.clone(), d);
		assert!(d.friendly_name.contains("Virtual"));
	}

	#[test]
	fn guard_saved_default_accessor() {
		// An inert guard (the non-Windows shape, and the Windows shape when nothing
		// was saved) reports no saved default and is safe to drop.
		let g = SinkRedirectGuard {
			saved_default: None,
			active: false,
		};
		assert_eq!(g.saved_default(), None);
		drop(g);
	}

	#[test]
	fn guard_reports_saved_default() {
		let g = SinkRedirectGuard {
			saved_default: Some("{orig-device}".to_string()),
			active: false, // inert: drop must NOT try to call Core Audio in a unit test
		};
		assert_eq!(g.saved_default(), Some("{orig-device}"));
	}

	#[cfg(not(windows))]
	#[test]
	fn stubs_are_inert_off_windows() {
		// The cross-platform surface is a well-defined no-op away from Windows.
		assert_eq!(find_render_device_by_name("anything").unwrap(), None);
		assert_eq!(default_render_device_id().unwrap(), None);
		assert!(set_default_render_device("{x}").is_ok());
		// The new set-device-format entry point is also an inert Ok off-Windows.
		assert!(set_render_device_format("{x}", ChannelLayout::Surround51).is_ok());
		assert!(set_render_device_format("{x}", ChannelLayout::Surround71).is_ok());
		let g = SinkRedirectGuard::redirect_to("{x}").unwrap();
		assert_eq!(g.saved_default(), None);
		restore_stale_redirect(); // no-op, must not panic
	}

	// ---- pure WAVEFORMATEXTENSIBLE parameter surface (platform-independent) ----

	#[test]
	fn channel_mask_matches_ksaudio_standard_masks() {
		// The exact KSAUDIO_SPEAKER_STEREO / _5POINT1 / _7POINT1_SURROUND bit sets.
		// Stereo: FL|FR = 0x3.
		assert_eq!(channel_mask_for(2), 0x3);
		// 5.1: FL|FR|FC|LFE|BL|BR = 0x3F.
		assert_eq!(channel_mask_for(6), 0x3F);
		// 7.1 (side variant): FL|FR|FC|LFE|BL|BR|SL|SR = 0x3F | 0x600 = 0x63F.
		assert_eq!(channel_mask_for(8), 0x63F);
		// Any unexpected count degrades to the stereo mask (never panics).
		assert_eq!(channel_mask_for(1), 0x3);
		assert_eq!(channel_mask_for(3), 0x3);
	}

	#[test]
	fn wave_format_params_channels_and_mask_per_layout() {
		let p2 = WaveFormatParams::for_layout(ChannelLayout::Stereo, 32);
		assert_eq!(p2.channels, 2);
		assert_eq!(p2.channel_mask, channel_mask_for(2));
		assert_eq!(p2.sample_rate, SAMPLE_RATE);

		let p6 = WaveFormatParams::for_layout(ChannelLayout::Surround51, 32);
		assert_eq!(p6.channels, 6);
		assert_eq!(p6.channel_mask, channel_mask_for(6));

		let p8 = WaveFormatParams::for_layout(ChannelLayout::Surround71, 32);
		assert_eq!(p8.channels, 8);
		assert_eq!(p8.channel_mask, channel_mask_for(8));
	}

	#[test]
	fn wave_format_params_retain_bit_depth() {
		// 16-bit → 16-bit integer PCM container, not float.
		let p16 = WaveFormatParams::for_layout(ChannelLayout::Stereo, 16);
		assert_eq!(p16.bits_per_sample, 16);
		assert_eq!(p16.valid_bits_per_sample, 16);
		assert!(!p16.is_float);

		// 24-bit → 32-bit container carrying 24 valid bits, integer PCM (not float).
		let p24 = WaveFormatParams::for_layout(ChannelLayout::Surround51, 24);
		assert_eq!(p24.bits_per_sample, 32);
		assert_eq!(p24.valid_bits_per_sample, 24);
		assert!(!p24.is_float);

		// 32-bit → IEEE float, the common WASAPI mix.
		let p32 = WaveFormatParams::for_layout(ChannelLayout::Surround71, 32);
		assert_eq!(p32.bits_per_sample, 32);
		assert_eq!(p32.valid_bits_per_sample, 32);
		assert!(p32.is_float);

		// Anything unexpected falls back to 32-bit float (universally present).
		let pweird = WaveFormatParams::for_layout(ChannelLayout::Stereo, 20);
		assert_eq!(pweird.bits_per_sample, 32);
		assert!(pweird.is_float);
	}

	#[test]
	fn wave_format_params_block_and_avg_bytes_consistent() {
		// nBlockAlign = channels * (container bits / 8); nAvgBytesPerSec = rate * block.
		let p = WaveFormatParams::for_layout(ChannelLayout::Surround51, 32);
		assert_eq!(p.block_align(), 6 * 4); // 6 ch * 4 bytes
		assert_eq!(p.avg_bytes_per_sec(), SAMPLE_RATE * (6 * 4));

		let p16 = WaveFormatParams::for_layout(ChannelLayout::Stereo, 16);
		assert_eq!(p16.block_align(), 2 * 2); // 2 ch * 2 bytes
		assert_eq!(p16.avg_bytes_per_sec(), SAMPLE_RATE * (2 * 2));

		// The defining PCM identity: avg = rate * channels * bytes-per-sample.
		for layout in [
			ChannelLayout::Stereo,
			ChannelLayout::Surround51,
			ChannelLayout::Surround71,
		] {
			for bits in [16u16, 24, 32] {
				let pp = WaveFormatParams::for_layout(layout, bits);
				assert_eq!(
					pp.avg_bytes_per_sec(),
					pp.sample_rate * pp.block_align() as u32
				);
				// Container is always 48 kHz and a byte-aligned width.
				assert_eq!(pp.sample_rate, SAMPLE_RATE);
				assert_eq!(pp.bits_per_sample % 8, 0);
				assert!(pp.valid_bits_per_sample <= pp.bits_per_sample);
			}
		}
	}
}
