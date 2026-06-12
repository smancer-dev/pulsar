//! Windows system-audio capture via **WASAPI loopback** on the default render
//! endpoint — the OBS/Sunshine "desktop audio" approach that needs no extra
//! capture device installed. The whole module is Windows-only.

/// Format of the default render endpoint's shared mix stream — what WASAPI loopback
/// hands us, and what the consuming ffmpeg must be told to expect on its stdin pipe.
#[derive(Clone, Copy, Debug)]
pub struct LoopbackFormat {
	/// Sample rate (Hz) — typically 48000.
	pub rate: u32,
	/// Channel count — typically 2 (downmixed to stereo by the encoder).
	pub channels: u16,
	/// `true` = 32-bit float samples (ffmpeg `f32le`), `false` = 16-bit PCM (`s16le`).
	pub float: bool,
}

impl LoopbackFormat {
	/// ffmpeg raw-input sample-format token for this mix format.
	pub fn ffmpeg_sample_fmt(&self) -> &'static str {
		if self.float {
			"f32le"
		} else {
			"s16le"
		}
	}
}

/// Query the default render endpoint's shared mix format. Call this before spawning
/// the ffmpeg that consumes the loopback PCM so it can be told the right `-f/-ar/-ac`.
pub fn loopback_format() -> Result<LoopbackFormat, String> {
	win_loopback::query_format()
}

/// Capture the host's system audio via **WASAPI loopback** on the default render
/// endpoint and write the raw interleaved PCM (the mix format — see
/// [`loopback_format`]) to `sink` until a write fails (e.g. the consuming ffmpeg
/// exits) or WASAPI errors. Blocking — run it on a dedicated thread.
///
/// This is how the host streams system audio with **no `virtual-audio-capturer` /
/// Stereo Mix device installed** (the same approach OBS "Desktop Audio" and Sunshine
/// use): it taps whatever is playing on the default output, so it always works.
pub fn run_loopback_capture<W: std::io::Write>(sink: W) -> Result<(), String> {
	win_loopback::run(sink)
}

/// Windows system-audio capture via **WASAPI loopback** on the default render endpoint
/// (`IAudioClient` initialized with `AUDCLNT_STREAMFLAGS_LOOPBACK` →
/// `IAudioCaptureClient`). Produces the device's shared mix PCM with no extra capture
/// device installed — what OBS/Sunshine do for "desktop audio".
mod win_loopback {
	use super::LoopbackFormat;
	use std::io::Write;
	use windows::Win32::Media::Audio::{
		eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
		MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
	};
	use windows::Win32::System::Com::{
		CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
	};

	// Set on a GetBuffer packet whose data is silence — its buffer may be uninitialized, so
	// we must emit zeros for it rather than the raw bytes.
	const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

	// The audio device backing our capture went away (unplugged / disabled) or the
	// user switched the default render endpoint — WASAPI fails every call with this
	// HRESULT. We recover by re-acquiring the (new) default endpoint and resuming,
	// rather than letting the capture thread die (which silently killed host audio
	// mid-session whenever the default output changed).
	const AUDCLNT_E_DEVICE_INVALIDATED: i32 = 0x88890004u32 as i32;

	// Open the default render endpoint's audio client and return it with its mix format
	// (a CoTaskMem-allocated WAVEFORMATEX the caller must free). COM is initialized MTA on
	// the calling thread (S_FALSE = already initialized — harmless).
	unsafe fn open() -> Result<(IAudioClient, *mut WAVEFORMATEX), String> {
		let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
		let enumerator: IMMDeviceEnumerator =
			CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
				.map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
		let device = enumerator
			.GetDefaultAudioEndpoint(eRender, eConsole)
			.map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?;
		let client: IAudioClient = device
			.Activate::<IAudioClient>(CLSCTX_ALL, None)
			.map_err(|e| format!("Activate IAudioClient: {e}"))?;
		let pwfx = client
			.GetMixFormat()
			.map_err(|e| format!("GetMixFormat: {e}"))?;
		if pwfx.is_null() {
			return Err("GetMixFormat returned null".into());
		}
		Ok((client, pwfx))
	}

	// The WASAPI shared mixer is 32-bit float; some endpoints expose 16-bit. Decide the
	// ffmpeg sample format from the bit depth (avoids needing the KS sub-format GUIDs).
	unsafe fn read_format(pwfx: *const WAVEFORMATEX) -> Result<LoopbackFormat, String> {
		let wf = &*pwfx;
		let float = match wf.wBitsPerSample {
			32 => true,
			16 => false,
			b => return Err(format!("unsupported loopback bit depth {b}")),
		};
		Ok(LoopbackFormat {
			rate: wf.nSamplesPerSec,
			channels: wf.nChannels,
			float,
		})
	}

	pub fn query_format() -> Result<LoopbackFormat, String> {
		unsafe {
			let (_client, pwfx) = open()?;
			let f = read_format(pwfx);
			CoTaskMemFree(Some(pwfx as *const _));
			f
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

	/// Why a `capture_loop` run ended — so `run` knows whether to give up (the
	/// consuming ffmpeg is gone) or to re-initialize against the new default
	/// endpoint (the device was invalidated / the default output changed).
	enum CaptureEnd {
		/// The output pipe broke (ffmpeg exited / session torn down) — terminal.
		PipeBroken,
		/// WASAPI signalled the device went away or the default endpoint changed —
		/// recoverable by re-opening the (new) default render endpoint. `productive`
		/// is true when a capture cycle actually ran before the failure (vs failing at
		/// open/setup) — `run` uses it to reset the re-init budget for a live cycle.
		DeviceInvalidated { productive: bool },
		/// Any other WASAPI failure — terminal (surfaced as an error string).
		Fatal(String),
	}

	pub fn run<W: Write>(mut sink: W) -> Result<(), String> {
		// Outer re-init loop: a device-invalidated / default-endpoint-change ends the
		// inner capture but we re-open the new default endpoint and resume, so host
		// audio survives the user switching outputs mid-session (the old code let the
		// thread die → silent stream). A broken pipe (ffmpeg gone) or a fatal error
		// exits for good.
		// Bound consecutive re-init failures so a permanently-gone device (no output at
		// all) can't spin forever; a successful capture cycle resets the budget.
		const MAX_REINIT: u32 = 50; // ~10 s at 200 ms backoff
		let mut reinit_left = MAX_REINIT;
		loop {
			match run_once(&mut sink) {
				CaptureEnd::PipeBroken => return Ok(()),
				CaptureEnd::Fatal(e) => return Err(e),
				CaptureEnd::DeviceInvalidated { productive } => {
					// A live cycle that ran then lost its device resets the budget, so
					// repeated, well-separated device changes each get the full allowance;
					// only back-to-back open/setup failures (no working endpoint) count down.
					if productive {
						reinit_left = MAX_REINIT;
					} else if reinit_left == 0 {
						return Err("WASAPI loopback: default render endpoint did not \
							recover after device change"
							.into());
					} else {
						reinit_left -= 1;
					}
					tracing::warn!(
						"WASAPI loopback device invalidated / default endpoint changed — \
						 re-initializing against the new default render endpoint"
					);
					// Brief backoff so a transient switch (the new endpoint not yet the
					// default) doesn't spin; the silence filler already kept the timeline
					// moving up to the failure, and resumes once we re-open.
					std::thread::sleep(std::time::Duration::from_millis(200));
					continue;
				}
			}
		}
	}

	/// One open→initialize→capture cycle against the CURRENT default render endpoint.
	/// Returns why it ended so `run` can re-init or stop. A WASAPI error during setup
	/// is treated as device-invalidated when its HRESULT says so (re-init), else fatal.
	fn run_once<W: Write>(sink: &mut W) -> CaptureEnd {
		unsafe {
			// Classify a SETUP failure (no capture cycle ran yet → not productive): a
			// device-invalidated HRESULT means re-init, anything else is fatal.
			let classify = |stage: &str, e: windows::core::Error| -> CaptureEnd {
				if e.code().0 == AUDCLNT_E_DEVICE_INVALIDATED {
					CaptureEnd::DeviceInvalidated { productive: false }
				} else {
					CaptureEnd::Fatal(format!("{stage}: {e}"))
				}
			};
			let (client, pwfx) = match open() {
				Ok(v) => v,
				// `open` returns a String (HRESULT lost). A failure here on a RE-INIT is
				// almost always the new default endpoint not yet ready — but to avoid an
				// infinite spin when there's genuinely no audio hardware, treat it as
				// fatal (the original behavior). The re-init path only re-opens AFTER a
				// successful first cycle, so a mid-session device change still recovers
				// via the in-loop DeviceInvalidated classification below.
				Err(e) => return CaptureEnd::Fatal(e),
			};
			let fmt = match read_format(pwfx) {
				Ok(f) => f,
				Err(e) => {
					CoTaskMemFree(Some(pwfx as *const _));
					return CaptureEnd::Fatal(e);
				}
			};
			let block_align = (*pwfx).nBlockAlign as usize;
			// 100 ms shared buffer; we poll every 10 ms, well inside it, so it never overruns.
			let init = client.Initialize(
				AUDCLNT_SHAREMODE_SHARED,
				AUDCLNT_STREAMFLAGS_LOOPBACK,
				1_000_000, // REFERENCE_TIME (100-ns units) = 100 ms
				0,
				pwfx,
				None,
			);
			CoTaskMemFree(Some(pwfx as *const _));
			if let Err(e) = init {
				return classify("IAudioClient::Initialize", e);
			}
			let capture: IAudioCaptureClient = match client.GetService::<IAudioCaptureClient>() {
				Ok(c) => c,
				Err(e) => return classify("GetService IAudioCaptureClient", e),
			};
			if let Err(e) = client.Start() {
				return classify("IAudioClient::Start", e);
			}

			// One 10 ms slice of silence, emitted whenever the host is silent (loopback then
			// delivers no packets at all — without this filler ffmpeg collapses the gap and
			// the audio timeline drifts ahead of the video).
			let period_frames = (fmt.rate / 100).max(1) as usize;
			let silence = vec![0u8; period_frames * block_align];

			let outcome = capture_loop(&capture, sink, block_align, &silence);
			let _ = client.Stop();
			outcome
		}
	}

	unsafe fn capture_loop<W: Write>(
		capture: &IAudioCaptureClient,
		sink: &mut W,
		block_align: usize,
		silence: &[u8],
	) -> CaptureEnd {
		// A WASAPI call failed mid-capture (a live cycle ran → productive): re-init on
		// device-invalidated, else fatal.
		let on_wasapi = |stage: &str, e: windows::core::Error| -> CaptureEnd {
			if e.code().0 == AUDCLNT_E_DEVICE_INVALIDATED {
				CaptureEnd::DeviceInvalidated { productive: true }
			} else {
				CaptureEnd::Fatal(format!("{stage}: {e}"))
			}
		};
		loop {
			let mut wrote_any = false;
			loop {
				let avail = match capture.GetNextPacketSize() {
					Ok(n) => n,
					Err(e) => return on_wasapi("GetNextPacketSize", e),
				};
				if avail == 0 {
					break;
				}
				let mut pdata: *mut u8 = std::ptr::null_mut();
				let mut nframes: u32 = 0;
				let mut flags: u32 = 0;
				if let Err(e) = capture.GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None) {
					return on_wasapi("GetBuffer", e);
				}
				let bytes = nframes as usize * block_align;
				let w = if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || pdata.is_null() {
					write_zeros(sink, bytes)
				} else {
					sink.write_all(std::slice::from_raw_parts(pdata, bytes))
				};
				// Always release, even if the write failed, so we don't wedge the WASAPI buffer.
				let _ = capture.ReleaseBuffer(nframes);
				// A pipe-write failure means the consuming ffmpeg is gone — terminal, do
				// NOT treat as a device change (we'd re-init forever against a dead sink).
				if w.is_err() {
					return CaptureEnd::PipeBroken;
				}
				wrote_any = true;
			}
			if !wrote_any {
				// Host silent this tick → keep the timeline moving with one period of silence.
				if sink.write_all(silence).is_err() {
					return CaptureEnd::PipeBroken;
				}
			}
			std::thread::sleep(std::time::Duration::from_millis(10));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn loopback_format_maps_to_ffmpeg_sample_fmt() {
		let f = LoopbackFormat {
			rate: 48000,
			channels: 2,
			float: true,
		};
		assert_eq!(f.ffmpeg_sample_fmt(), "f32le");
		let s = LoopbackFormat {
			rate: 44100,
			channels: 2,
			float: false,
		};
		assert_eq!(s.ffmpeg_sample_fmt(), "s16le");
	}
}
