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

	pub fn run<W: Write>(mut sink: W) -> Result<(), String> {
		unsafe {
			let (client, pwfx) = open()?;
			let fmt = read_format(pwfx)?;
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
			init.map_err(|e| format!("IAudioClient::Initialize: {e}"))?;
			let capture: IAudioCaptureClient = client
				.GetService::<IAudioCaptureClient>()
				.map_err(|e| format!("GetService IAudioCaptureClient: {e}"))?;
			client
				.Start()
				.map_err(|e| format!("IAudioClient::Start: {e}"))?;

			// One 10 ms slice of silence, emitted whenever the host is silent (loopback then
			// delivers no packets at all — without this filler ffmpeg collapses the gap and
			// the audio timeline drifts ahead of the video).
			let period_frames = (fmt.rate / 100).max(1) as usize;
			let silence = vec![0u8; period_frames * block_align];

			let outcome = capture_loop(&capture, &mut sink, block_align, &silence);
			let _ = client.Stop();
			outcome
		}
	}

	unsafe fn capture_loop<W: Write>(
		capture: &IAudioCaptureClient,
		sink: &mut W,
		block_align: usize,
		silence: &[u8],
	) -> Result<(), String> {
		loop {
			let mut wrote_any = false;
			loop {
				let avail = capture
					.GetNextPacketSize()
					.map_err(|e| format!("GetNextPacketSize: {e}"))?;
				if avail == 0 {
					break;
				}
				let mut pdata: *mut u8 = std::ptr::null_mut();
				let mut nframes: u32 = 0;
				let mut flags: u32 = 0;
				capture
					.GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None)
					.map_err(|e| format!("GetBuffer: {e}"))?;
				let bytes = nframes as usize * block_align;
				let w = if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || pdata.is_null() {
					write_zeros(sink, bytes)
				} else {
					sink.write_all(std::slice::from_raw_parts(pdata, bytes))
				};
				// Always release, even if the write failed, so we don't wedge the WASAPI buffer.
				let _ = capture.ReleaseBuffer(nframes);
				w.map_err(|e| format!("pipe write: {e}"))?;
				wrote_any = true;
			}
			if !wrote_any {
				// Host silent this tick → keep the timeline moving with one period of silence.
				sink.write_all(silence).map_err(|e| format!("pipe write: {e}"))?;
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
