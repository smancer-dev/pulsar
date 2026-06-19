//! Windows **per-process** WASAPI loopback (Phase 4 same-host co-op).
//!
//! The default-endpoint loopback ([`super::loopback`]) taps the whole system mix, so two
//! sessions streaming from the SAME host both capture BOTH games' audio → the client plays
//! a doubled / echoed mix on each pane, with zero per-app isolation. This module captures
//! **only one process's render audio (and its child processes)** via the
//! `ActivateAudioInterfaceAsync` process-loopback contract introduced in **Windows 10
//! 20H1 (build 19041)**, so each co-op pane gets just its own game's sound.
//!
//! HONEST scope: process-loopback isolates a single process tree's *render* audio. On a
//! shared TV the two games' sound still mixes acoustically in the room — that's physics, not
//! something we can undo. What this kills is the **duplication / echo** (each stream no longer
//! carries the other game too) and it makes each stream **per-app**, which is what enables
//! downstream headphone / left-right separation. Requires Win10 20H1+; on older hosts (or if
//! activation fails) the caller falls back to the default-endpoint loopback unchanged.
//!
//! ## The WASAPI/COM contract a reviewer must verify
//!
//! * Activation is **asynchronous**: `ActivateAudioInterfaceAsync` returns immediately and
//!   the real result arrives later via an `IActivateAudioInterfaceCompletionHandler` callback
//!   invoked on an MTA worker thread. We bridge that with a manual-reset Win32 event the
//!   handler signals, then `WaitForSingleObject` + `GetActivateResult` on our thread. COM is
//!   initialized **MTA** on the calling thread (the handler must run in the MTA).
//! * Process-loopback does **NOT** support `GetMixFormat` (there is no real endpoint) — we
//!   must supply our own `WAVEFORMATEX` to `Initialize`. We pick a fixed **48 kHz / stereo /
//!   32-bit float** format (matches the encoder's existing f32le stereo expectation), exposed
//!   as a [`super::loopback::LoopbackFormat`] so the host's ffmpeg `-f/-ar/-ac` line is
//!   unchanged from the default-endpoint path.
//! * `Initialize` MUST be called with `AUDCLNT_STREAMFLAGS_LOOPBACK |
//!   AUDCLNT_STREAMFLAGS_EVENTCALLBACK` and `hnsBufferDuration`/`Periodicity` = 0 (the
//!   process-loopback contract REQUIRES event-driven; a poll-mode init fails). We then drive
//!   reads off the event handle, not a sleep loop.
//! * The activation params blob is passed inside a `PROPVARIANT` typed `VT_BLOB`. We build a
//!   `#[repr(C)]` PROPVARIANT-shaped struct by hand (the exact layout the C sample uses) and
//!   cast it — constructing the windows-rs nested `PROPVARIANT` unions for a BLOB is far more
//!   error-prone than matching the documented binary layout.

#![cfg(windows)]

use super::loopback::LoopbackFormat;
use std::io::Write;
use windows::core::{implement, IUnknown, Interface, PCWSTR};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
	ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
	IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
	IAudioCaptureClient, IAudioClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
	AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
	AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
	PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
	WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
	CreateEventW, SetEvent, WaitForSingleObject, INFINITE,
};

// `WAVEFORMATEX.wFormatTag` for 32-bit IEEE float PCM — not re-exported by the Audio module
// in this windows-rs version, so define it locally (the well-known mmreg.h value).
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;

// Set on a GetBuffer packet whose data is silence; mirror the default-endpoint loopback and
// emit zeros for it (its buffer may be uninitialized).
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

/// The fixed format every process-loopback capture delivers. Process-loopback has no real
/// endpoint to query, so we DICTATE the format at `Initialize`: 48 kHz stereo f32, exactly
/// what the default-endpoint stereo path hands ffmpeg (`f32le`/`-ar 48000`/`-ac 2`), so the
/// host's encoder spawn is identical whichever capture path is used.
pub fn process_loopback_format() -> LoopbackFormat {
	LoopbackFormat {
		rate: 48_000,
		channels: 2,
		float: true,
	}
}

/// Is per-process loopback available on this host? `true` only on Windows 10 build 19041
/// (20H1) or newer — the OS version that introduced the `ActivateAudioInterfaceAsync`
/// process-loopback activation. Uses `RtlGetVersion` (ntdll), the UN-shimmed build query:
/// `GetVersionExW` lies (caps at 6.2) for apps without an explicit OS-compatibility manifest,
/// which would wrongly gate this OFF on a capable host. Best-effort: a query failure returns
/// `false` (we fall back to the default-endpoint loopback rather than risk a failed activate).
pub fn process_loopback_supported() -> bool {
	use windows::Wdk::System::SystemServices::RtlGetVersion;
	use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
	let mut info = OSVERSIONINFOW {
		dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
		..Default::default()
	};
	// RtlGetVersion returns STATUS_SUCCESS (0) and never really fails on a sane system.
	let status = unsafe { RtlGetVersion(&mut info) };
	if status.0 != 0 {
		return false;
	}
	// Win10 = major 10; 20H1 = build 19041. (Win11 reports major 10 too, with a higher build,
	// so the build floor covers both.)
	info.dwMajorVersion >= 10 && info.dwBuildNumber >= 19041
}

// ---- async activation completion handler ----------------------------------------------
//
// `ActivateAudioInterfaceAsync` calls `ActivateCompleted` on an MTA worker thread when the
// async activation finishes. We just signal a Win32 event; the spawning thread waits on it,
// then reads the real result with `GetActivateResult`. (We deliberately do NOT touch the
// IAudioClient here — it's retrieved on the caller's thread to keep ownership simple.)
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateHandler {
	done: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
	fn ActivateCompleted(
		&self,
		_op: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
	) -> windows::core::Result<()> {
		// Signal the spawning thread; it pulls the activation result off the operation.
		unsafe {
			let _ = SetEvent(self.done);
		}
		Ok(())
	}
}

/// RAII wrapper so the completion event is always closed even on the error paths.
struct EventHandle(HANDLE);
impl Drop for EventHandle {
	fn drop(&mut self) {
		if !self.0.is_invalid() {
			unsafe {
				let _ = windows::Win32::Foundation::CloseHandle(self.0);
			}
		}
	}
}

/// Activate an `IAudioClient` for **process-loopback** of `pid`'s render audio (and its
/// child process tree). Blocking: it kicks off the async activation, waits on the completion
/// event, and returns the activated client (or an error describing where it failed).
///
/// Apartment: COM is initialized MTA on the calling thread — the completion handler runs in
/// the MTA, and the activation operation is an MTA object.
unsafe fn activate_process_client(pid: u32) -> Result<IAudioClient, String> {
	let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

	// AUDIOCLIENT_ACTIVATION_PARAMS describing "loopback the target PID + its process tree".
	let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
		ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
		Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
			ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
				TargetProcessId: pid,
				ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
			},
		},
	};

	// Wrap the params blob in a PROPVARIANT typed VT_BLOB. Constructing windows-rs's deeply
	// nested PROPVARIANT unions for a BLOB is error-prone, so we match the documented binary
	// layout directly (this is exactly what the MS ApplicationLoopback C sample does).
	const VT_BLOB: u16 = 65; // wtypes.h VARENUM
	#[repr(C)]
	struct Blob {
		cb_size: u32,
		p_blob_data: *mut u8,
	}
	#[repr(C)]
	struct PropVariantBlob {
		vt: u16,
		w_reserved1: u16,
		w_reserved2: u16,
		w_reserved3: u16,
		blob: Blob,
	}
	let pv = PropVariantBlob {
		vt: VT_BLOB,
		w_reserved1: 0,
		w_reserved2: 0,
		w_reserved3: 0,
		blob: Blob {
			cb_size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
			p_blob_data: &mut params as *mut _ as *mut u8,
		},
	};
	// SAFETY: PropVariantBlob has the same layout as PROPVARIANT's first union arm for a BLOB.
	let pv_ptr = &pv as *const PropVariantBlob
		as *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

	// Manual-reset, initially-unsignaled completion event.
	let done = CreateEventW(None, true, false, PCWSTR::null())
		.map_err(|e| format!("CreateEventW: {e}"))?;
	let _done_guard = EventHandle(done);

	let handler: IActivateAudioInterfaceCompletionHandler = ActivateHandler { done }.into();

	// Kick off the async activation against the magic process-loopback device id.
	let op = ActivateAudioInterfaceAsync(
		VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
		&IAudioClient::IID,
		Some(pv_ptr),
		&handler,
	)
	.map_err(|e| format!("ActivateAudioInterfaceAsync: {e}"))?;

	// Block until the handler signals. INFINITE is safe: activation either completes promptly
	// or the call above already errored; a never-firing handler is not a documented state.
	if WaitForSingleObject(done, INFINITE) != WAIT_OBJECT_0 {
		return Err("WaitForSingleObject on activation event failed".into());
	}

	// Pull the activation HRESULT + the activated interface off the operation.
	let mut activate_hr = windows::core::HRESULT(0);
	let mut activated: Option<IUnknown> = None;
	op.GetActivateResult(&mut activate_hr, &mut activated)
		.map_err(|e| format!("GetActivateResult: {e}"))?;
	activate_hr
		.ok()
		.map_err(|e| format!("process-loopback activation HRESULT: {e}"))?;
	let unknown = activated.ok_or_else(|| "activation returned a null interface".to_string())?;
	unknown
		.cast::<IAudioClient>()
		.map_err(|e| format!("cast activated interface to IAudioClient: {e}"))
}

/// A `WAVEFORMATEX` for the fixed [`process_loopback_format`] (48 kHz stereo f32). Built as a
/// plain `WAVE_FORMAT_IEEE_FLOAT` block (not EXTENSIBLE) so we needn't carry the KS sub-format
/// GUIDs — process-loopback accepts this for a 2-channel float stream.
fn fixed_waveformat() -> WAVEFORMATEX {
	let fmt = process_loopback_format();
	let bytes_per_sample = 4u16; // f32
	let block_align = fmt.channels * bytes_per_sample;
	WAVEFORMATEX {
		wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
		nChannels: fmt.channels,
		nSamplesPerSec: fmt.rate,
		nAvgBytesPerSec: fmt.rate * block_align as u32,
		nBlockAlign: block_align,
		wBitsPerSample: bytes_per_sample * 8,
		cbSize: 0,
	}
}

/// Capture `pid`'s process-tree render audio via WASAPI process-loopback and write the raw
/// interleaved PCM (48 kHz stereo f32 — [`process_loopback_format`]) to `sink` until the pipe
/// breaks (the consuming ffmpeg exits / session torn down) or WASAPI errors. **Blocking** —
/// run on a dedicated thread, exactly like [`super::loopback::run_loopback_capture`].
///
/// Unlike the default-endpoint loopback this is **event-driven** (the process-loopback
/// `Initialize` contract requires `EVENTCALLBACK`): we wait on the audio event between reads
/// rather than sleeping. There is **no silence-fill** — process-loopback delivers silent
/// packets (flagged `AUDCLNT_BUFFERFLAGS_SILENT`) while the app is quiet, so the timeline
/// stays wall-clock-aligned without us padding it (the default-endpoint path pads only
/// because a truly-idle endpoint stops delivering packets entirely).
///
/// Returns `Ok(())` on a clean stop (pipe broken). Any setup/activation/WASAPI failure is an
/// `Err(String)` so the caller can fall back to the default-endpoint loopback.
pub fn run_process_loopback_capture<W: Write>(sink: W, pid: u32) -> Result<(), String> {
	unsafe { run(sink, pid) }
}

unsafe fn run<W: Write>(mut sink: W, pid: u32) -> Result<(), String> {
	let client = activate_process_client(pid)?;

	let mut wfx = fixed_waveformat();
	let block_align = wfx.nBlockAlign as usize;

	// The audio event WASAPI signals each period in event-driven mode. Auto-reset so a wait
	// consumes one signal; the capture drains every available packet after each wake.
	let audio_event =
		CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| format!("CreateEventW(audio): {e}"))?;
	let _audio_guard = EventHandle(audio_event);

	// Process-loopback REQUIRES shared mode + LOOPBACK + EVENTCALLBACK, and buffer
	// duration / periodicity both 0 (let WASAPI pick). A poll-mode init is rejected.
	client
		.Initialize(
			AUDCLNT_SHAREMODE_SHARED,
			AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
			0,
			0,
			&mut wfx as *const WAVEFORMATEX,
			None,
		)
		.map_err(|e| format!("IAudioClient::Initialize (process-loopback): {e}"))?;

	client
		.SetEventHandle(audio_event)
		.map_err(|e| format!("IAudioClient::SetEventHandle: {e}"))?;

	let capture: IAudioCaptureClient = client
		.GetService::<IAudioCaptureClient>()
		.map_err(|e| format!("GetService IAudioCaptureClient (process-loopback): {e}"))?;

	client
		.Start()
		.map_err(|e| format!("IAudioClient::Start (process-loopback): {e}"))?;

	let outcome = capture_loop(&capture, &mut sink, block_align, audio_event);
	let _ = client.Stop();
	outcome
}

// Drive the event-driven read loop: wait for the period event, drain every queued packet,
// repeat. A pipe-write failure means ffmpeg is gone → clean stop; a WASAPI error is fatal so
// the caller can fall back. (No re-init loop: process-loopback has no "default endpoint
// changed" notion — the target either keeps rendering or its tree exits; on exit the activate
// is already done and reads simply yield silence, and teardown breaks the pipe.)
unsafe fn capture_loop<W: Write>(
	capture: &IAudioCaptureClient,
	sink: &mut W,
	block_align: usize,
	audio_event: HANDLE,
) -> Result<(), String> {
	loop {
		// Wait up to 2s for the next period; a timeout just loops (treated like a quiet tick)
		// so a momentarily-idle app can't wedge us, and teardown is still seen via the pipe.
		let _ = WaitForSingleObject(audio_event, 2000);
		loop {
			let avail = match capture.GetNextPacketSize() {
				Ok(n) => n,
				Err(e) => return Err(format!("GetNextPacketSize (process-loopback): {e}")),
			};
			if avail == 0 {
				break;
			}
			let mut pdata: *mut u8 = std::ptr::null_mut();
			let mut nframes: u32 = 0;
			let mut flags: u32 = 0;
			if let Err(e) = capture.GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None) {
				return Err(format!("GetBuffer (process-loopback): {e}"));
			}
			let bytes = nframes as usize * block_align;
			let w = if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || pdata.is_null() {
				write_zeros(sink, bytes)
			} else {
				sink.write_all(std::slice::from_raw_parts(pdata, bytes))
			};
			// Always release so we never wedge the WASAPI ring, even on a failed write.
			let _ = capture.ReleaseBuffer(nframes);
			if w.is_err() {
				// The consuming ffmpeg exited / session torn down — terminal, not an error.
				return Ok(());
			}
		}
	}
}

fn write_zeros<W: Write>(sink: &mut W, mut n: usize) -> std::io::Result<()> {
	const Z: [u8; 4096] = [0u8; 4096];
	while n > 0 {
		let c = n.min(Z.len());
		sink.write_all(&Z[..c])?;
		n -= c;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fixed_format_is_48k_stereo_float() {
		// The host's ffmpeg `-f/-ar/-ac` is derived from this, and must stay 48 kHz stereo f32 so
		// the encoder spawn is identical to the default-endpoint stereo loopback path (the WAVEFORMATEX
		// we hand Initialize is built from the same numbers — see `fixed_waveformat`).
		let f = process_loopback_format();
		assert_eq!(f.rate, 48_000);
		assert_eq!(f.channels, 2);
		assert!(f.float);
		assert_eq!(f.ffmpeg_sample_fmt(), "f32le");
		assert_eq!(f.block_align(), 2 * 4); // stereo × 4 bytes (f32)
	}

	#[test]
	fn waveformat_matches_fixed_format() {
		// The WAVEFORMATEX supplied to Initialize must agree with `process_loopback_format` (the
		// format ffmpeg is told to expect) or the client would parse garbled / wrong-pitch audio.
		let wfx = fixed_waveformat();
		let f = process_loopback_format();
		// WAVEFORMATEX is #[repr(packed)] — copy each field to a local before comparing (taking a
		// reference to a packed field is UB).
		let (tag, ch, rate, bits, block, avg, cb) = (
			wfx.wFormatTag,
			wfx.nChannels,
			wfx.nSamplesPerSec,
			wfx.wBitsPerSample,
			wfx.nBlockAlign,
			wfx.nAvgBytesPerSec,
			wfx.cbSize,
		);
		assert_eq!(tag, WAVE_FORMAT_IEEE_FLOAT);
		assert_eq!(ch, f.channels);
		assert_eq!(rate, f.rate);
		assert_eq!(bits, 32);
		assert_eq!(block as usize, f.block_align());
		assert_eq!(avg, f.rate * block as u32);
		assert_eq!(cb, 0); // plain WAVEFORMATEX, not EXTENSIBLE
	}

	#[test]
	fn supported_probe_does_not_panic() {
		// Just exercise the RtlGetVersion gate — the exact bool depends on the test host's OS, but
		// the call must be sound (no UB / panic) so the host can rely on it at runtime.
		let _ = process_loopback_supported();
	}
}
