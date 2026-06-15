//! Windows system-audio capture via **WASAPI loopback** on the default render
//! endpoint — the OBS/Sunshine "desktop audio" approach that needs no extra
//! capture device installed. The whole module is Windows-only.

/// Format of the default render endpoint's shared mix stream — what WASAPI loopback
/// hands us, and what the consuming ffmpeg must be told to expect on its stdin pipe.
#[derive(Clone, Copy, Debug)]
pub struct LoopbackFormat {
	/// Sample rate (Hz) — typically 48000.
	pub rate: u32,
	/// Channel count — 2 (stereo), 6 (5.1) or 8 (7.1) for ordinary endpoints. This
	/// is the **real** channel count of the mix the capture emits, so the encoder is
	/// always told the right `-ac` (the silence-fill is sized from it too).
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

	/// Bytes per interleaved audio frame (one sample per channel) — `channels ×
	/// bytes-per-sample`. This is WASAPI's `nBlockAlign`; the silence-fill buffer and
	/// every packet copy are sized from it, so 5.1/7.1 devices are handled correctly
	/// (6/8 channels ⇒ a wider frame) with no stereo-only assumptions.
	pub fn block_align(&self) -> usize {
		let bytes_per_sample = if self.float { 4 } else { 2 };
		self.channels as usize * bytes_per_sample
	}

	/// Whether two formats yield the SAME raw PCM byte stream — i.e. ffmpeg's fixed
	/// `-f/-ar/-ac` (derived from one) would correctly parse bytes captured at the
	/// other. All three of rate, channel count and sample width must match; if any
	/// differs the consuming ffmpeg must be respawned (see `run_loopback_capture_tracking`).
	pub fn matches(&self, other: &LoopbackFormat) -> bool {
		self.rate == other.rate && self.channels == other.channels && self.float == other.float
	}
}

/// Query the shared mix format of the capture target. Pass the SAME `device_id` the
/// capture will use (`Some(id)` for the pinned host-silent virtual sink, `None` for the
/// OS default) so the format handed to ffmpeg matches the device the loopback actually
/// taps. Call this before spawning the ffmpeg that consumes the loopback PCM so it can
/// be told the right `-f/-ar/-ac`.
pub fn loopback_format(device_id: Option<&str>) -> Result<LoopbackFormat, String> {
	win_loopback::query_format(device_id)
}

/// Capture the host's system audio via **WASAPI loopback** on the default render
/// endpoint and write the raw interleaved PCM (the mix format — see
/// [`loopback_format`]) to `sink` until a write fails (e.g. the consuming ffmpeg
/// exits) or WASAPI errors. Blocking — run it on a dedicated thread.
///
/// This is how the host streams system audio with **no `virtual-audio-capturer` /
/// Stereo Mix device installed** (the same approach OBS "Desktop Audio" and Sunshine
/// use): it taps whatever is playing on the default output, so it always works.
///
/// This captures whatever the OS default render endpoint is at each (re)open. For the
/// host-silent (sink-redirect) path — where the capture MUST stay pinned to our bundled
/// virtual sink even if the OS flips the default off it mid-stream — use
/// [`run_loopback_capture_pinned`] instead; this is the no-pin convenience wrapper.
pub fn run_loopback_capture<W: std::io::Write>(sink: W) -> Result<(), String> {
	run_loopback_capture_pinned(sink, None)
}

/// Like [`run_loopback_capture`], but with an optional **pinned default render
/// device-id** that is re-asserted as the OS default on *every* (re)open of the
/// capture.
///
/// Why: the host-silent feature redirects the default render endpoint to a bundled
/// sinkless virtual sink and captures *that* endpoint's loopback (Sunshine's model).
/// But the OS default can be flipped off the virtual sink mid-stream — by another app,
/// a device hotplug, or Windows itself re-picking an endpoint — at which point a plain
/// re-open (on `AUDCLNT_E_DEVICE_INVALIDATED` / default-endpoint change) would re-open
/// whatever the NEW default is: the host's real speakers come back on and we capture the
/// wrong endpoint, silently losing host-silent.
///
/// When `pinned_default` is `Some(id)`, before each (re)open we compare it against the
/// current default render device-id and, if they differ, call
/// [`crate::audio::sink::set_default_render_device`] to RE-ASSERT the pinned sink as the
/// default *before* opening the capture — so we always tap the pinned (virtual) sink.
/// This mirrors Sunshine's `default_endpt_changed_cb`, which re-sets its virtual sink
/// whenever the default endpoint changes out from under it.
///
/// When `pinned_default` is `None` this behaves **exactly** like
/// [`run_loopback_capture`]: no default-device query and no re-assert ever happen.
pub fn run_loopback_capture_pinned<W: std::io::Write>(
	sink: W,
	pinned_default: Option<String>,
) -> Result<(), String> {
	// `expected = None` → never compare the re-opened device's format, so this path
	// behaves byte-for-byte as before (the example / standalone capture test use it).
	win_loopback::run(sink, pinned_default.as_deref(), None).map(|_| ())
}

/// Like [`run_loopback_capture_pinned`], but tracks the capture format against
/// `expected` — the [`LoopbackFormat`] the consuming ffmpeg was spawned with (its
/// fixed `-f/-ar/-ac`).
///
/// The re-init loop re-opens whatever the default render endpoint is after a
/// device-invalidated / default-endpoint change, and that NEW endpoint can have a
/// different mix format (sample rate / channels / bit depth). The capture then writes
/// PCM in the new format while ffmpeg still parses it as `expected` → garbled / wrong-
/// pitch audio for the rest of the session. To let the caller recover, when a re-opened
/// endpoint's format differs from `expected` this stops the capture and returns
/// `Ok(Some(new_format))` *before* any mismatched bytes are written, so the caller can
/// respawn ffmpeg with the new format. A normal end (the pipe broke / ffmpeg exited)
/// returns `Ok(None)`; a fatal WASAPI error returns `Err`.
pub fn run_loopback_capture_tracking<W: std::io::Write>(
	sink: W,
	pinned_default: Option<String>,
	expected: LoopbackFormat,
) -> Result<Option<LoopbackFormat>, String> {
	win_loopback::run(sink, pinned_default.as_deref(), Some(expected))
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
		MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
		AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
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

	// Open the capture target's audio client and return it with its mix format (a
	// CoTaskMem-allocated WAVEFORMATEX the caller must free). COM is initialized MTA on
	// the calling thread (S_FALSE = already initialized — harmless).
	//
	// `device_id`:
	// * `Some(id)` → open THAT exact endpoint via `GetDevice` (the pinned host-silent
	//   virtual sink). This is **load-bearing**: `IPolicyConfig::SetDefaultEndpoint` (the
	//   redirect) propagates ASYNCHRONOUSLY, so calling `GetDefaultAudioEndpoint` right
	//   after a redirect races and can still return the OLD default (the host's real
	//   speakers) — we'd then tap a device the game no longer plays to and capture pure
	//   silence while the client gets nothing. `GetDevice(id)` is immediate and exact, so
	//   we always tap the sink the game was redirected to. (The OS default is *also* kept
	//   on the sink, by `reassert_pinned_default`, so the game renders there — but the
	//   capture no longer DEPENDS on that default having propagated.)
	// * `None` → the OS default render endpoint (`eRender`/`eConsole`) — the plain path.
	unsafe fn open(device_id: Option<&str>) -> Result<(IAudioClient, *mut WAVEFORMATEX), String> {
		let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
		let enumerator: IMMDeviceEnumerator =
			CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
				.map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
		let device = match device_id {
			Some(id) => {
				let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
				enumerator
					.GetDevice(windows::core::PCWSTR(wide.as_ptr()))
					.map_err(|e| format!("GetDevice({id}): {e}"))?
			}
			None => enumerator
				.GetDefaultAudioEndpoint(eRender, eConsole)
				.map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?,
		};
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
	// `nChannels` is taken verbatim so 5.1 (6) / 7.1 (8) endpoints report their true
	// width — the encoder's `-ac` and the silence-fill both derive from it.
	unsafe fn read_format(pwfx: *const WAVEFORMATEX) -> Result<LoopbackFormat, String> {
		let wf = &*pwfx;
		let float = match wf.wBitsPerSample {
			32 => true,
			16 => false,
			b => return Err(format!("unsupported loopback bit depth {b}")),
		};
		if wf.nChannels == 0 {
			return Err("loopback mix format reports zero channels".into());
		}
		Ok(LoopbackFormat {
			rate: wf.nSamplesPerSec,
			channels: wf.nChannels,
			float,
		})
	}

	pub fn query_format(device_id: Option<&str>) -> Result<LoopbackFormat, String> {
		unsafe {
			let (_client, pwfx) = open(device_id)?;
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
		/// A re-opened endpoint reports a DIFFERENT mix format than the one ffmpeg was
		/// spawned with (`expected`). No capture bytes were written in the new format,
		/// so `run` returns the new format to the caller to respawn ffmpeg around it.
		/// Only produced when an `expected` format was supplied (the tracking path).
		FormatChanged(LoopbackFormat),
		/// Any other WASAPI failure — terminal (surfaced as an error string).
		Fatal(String),
	}

	/// Re-assert a **pinned default render endpoint** before a (re)open of the capture.
	///
	/// For the host-silent (sink-redirect) path the capture must stay tapped on our
	/// bundled virtual sink. If the OS default has been flipped off it (another app, a
	/// hotplug, or Windows re-picking), opening loopback would tap the wrong endpoint —
	/// the host's real speakers — silently losing host-silent. So when a pin is set and
	/// the current default differs from it, we put the pinned sink back as the default
	/// (all three roles, via the `sink` module) BEFORE opening, mirroring Sunshine's
	/// `default_endpt_changed_cb`.
	///
	/// Best-effort: a failed query or re-assert is logged and we proceed to open against
	/// whatever the default is (no worse than the un-pinned behavior). `None` (the
	/// no-pin path) returns immediately — no default-device query is ever made, so the
	/// un-pinned capture behaves exactly as before. Uses the `sink` module's COM helpers;
	/// no COM is duplicated here.
	///
	/// Returns `true` iff it took the pinned branch and actually queried the default
	/// (i.e. a pin was set) — `false` for the `None` no-pin path. The bool is only used
	/// by the unit test to assert the no-op path; callers ignore it.
	pub(super) fn reassert_pinned_default(pinned_default: Option<&str>) -> bool {
		let Some(pinned) = pinned_default else {
			return false; // no pin → never query/re-assert; identical to the old behavior
		};
		match crate::audio::sink::default_render_device_id() {
			Ok(Some(current)) if current == pinned => {
				// Already on the pinned sink — nothing to do.
			}
			Ok(_) => {
				// The default is some OTHER endpoint (or none) — put our pinned sink back
				// before we open the capture, so we tap the virtual sink, not the host's
				// real output.
				if let Err(e) = crate::audio::sink::set_default_render_device(pinned) {
					tracing::warn!(
						"loopback: re-asserting pinned default render endpoint failed: {e} \
						 — opening against the current default instead"
					);
				} else {
					tracing::info!(
						"loopback: OS default drifted off the pinned (virtual-sink) endpoint \
						 — re-asserted it as default before re-opening capture"
					);
				}
			}
			Err(e) => {
				tracing::warn!(
					"loopback: querying default render endpoint to re-assert the pin failed: \
					 {e} — opening against the current default"
				);
			}
		}
		true
	}

	pub fn run<W: Write>(
		mut sink: W,
		pinned_default: Option<&str>,
		expected: Option<LoopbackFormat>,
	) -> Result<Option<LoopbackFormat>, String> {
		// Outer re-init loop: a device-invalidated / default-endpoint-change ends the
		// inner capture but we re-open the new default endpoint and resume, so host
		// audio survives the user switching outputs mid-session (the old code let the
		// thread die → silent stream). A broken pipe (ffmpeg gone) or a fatal error
		// exits for good.
		// Bound consecutive re-init failures so a permanently-gone device (no output at
		// all) can't spin forever; a successful capture cycle resets the budget.
		const MAX_REINIT: u32 = 50; // ~10 s at 200 ms backoff
		let mut reinit_left = MAX_REINIT;
		// `false` for the very first open (a failure there is genuine no-audio-hardware →
		// fail fast / fatal); `true` after a device-invalidated re-open, where a transient
		// `open()` failure is almost always the just-promoted default endpoint not being
		// ready yet, so it must consume a retry from the budget instead of killing the
		// stream (see `run_once`'s open() error handling).
		let mut reopen = false;
		loop {
			// Before each (re)open, re-assert the pinned default render endpoint (the
			// host-silent virtual sink) if the OS default has drifted off it — see the
			// `reassert_pinned_default` doc. A no-op when `pinned_default` is None.
			reassert_pinned_default(pinned_default);
			match run_once(&mut sink, pinned_default, expected, reopen) {
				CaptureEnd::PipeBroken => return Ok(None),
				CaptureEnd::Fatal(e) => return Err(e),
				// A re-opened endpoint has a format ffmpeg can't parse — hand the new
				// format back so the caller respawns ffmpeg around it (the tracking path).
				CaptureEnd::FormatChanged(new_fmt) => return Ok(Some(new_fmt)),
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
					// The next iteration is a re-open after a real device change: a transient
					// open() failure (new default not yet Activatable) must be retried within
					// the budget, not treated as fatal.
					reopen = true;
					continue;
				}
			}
		}
	}

	/// One open→initialize→capture cycle against `device_id` (the pinned host-silent
	/// virtual sink when `Some`, else the OS default render endpoint). Returns why it
	/// ended so `run` can re-init or stop. A WASAPI error during setup is treated as
	/// device-invalidated when its HRESULT says so (re-init), else fatal.
	///
	/// `expected` is the format ffmpeg was spawned with (the tracking path). When set
	/// and the opened endpoint's mix format differs from it, we return
	/// `FormatChanged(new)` BEFORE writing any mismatched bytes, so the caller can
	/// respawn ffmpeg; `None` skips the check (identical to the old behavior).
	///
	/// `reopen` is `true` when this is a re-open after a device-invalidated / default-
	/// endpoint change (see `run`): a transient `open()` failure then is almost always
	/// the just-promoted default not being ready yet, so it is classified as
	/// `DeviceInvalidated { productive: false }` (consumes a retry from the budget with
	/// backoff) instead of fatal. On the FIRST open (`false`) an `open()` failure is
	/// genuine no-audio-hardware and stays fatal so we fail fast.
	fn run_once<W: Write>(
		sink: &mut W,
		device_id: Option<&str>,
		expected: Option<LoopbackFormat>,
		reopen: bool,
	) -> CaptureEnd {
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
			let (client, pwfx) = match open(device_id) {
				Ok(v) => v,
				// `open` returns a String (HRESULT lost). On a RE-OPEN after a device change
				// this is almost always the just-promoted default endpoint not being ready
				// yet (SetDefaultEndpoint propagates asynchronously / hotplug takes time), so
				// treat it as a non-productive device-invalidation: `run` retries it within
				// the bounded MAX_REINIT budget with backoff rather than killing the stream.
				// On the FIRST open (`!reopen`) a failure is genuine no-audio-hardware → stay
				// fatal so we fail fast instead of spinning the whole budget at session start.
				Err(e) => {
					if reopen {
						tracing::warn!(
							"WASAPI loopback re-open failed ({e}) — new default endpoint likely \
							 not ready yet; retrying within the re-init budget"
						);
						return CaptureEnd::DeviceInvalidated { productive: false };
					}
					return CaptureEnd::Fatal(e);
				}
			};
			let fmt = match read_format(pwfx) {
				Ok(f) => f,
				Err(e) => {
					CoTaskMemFree(Some(pwfx as *const _));
					return CaptureEnd::Fatal(e);
				}
			};
			// Tracking path: ffmpeg's -f/-ar/-ac are fixed for the life of its process, so a
			// re-opened endpoint with a different mix format would feed it bytes it parses
			// wrong (garbled / wrong-pitch audio for the rest of the session). Bail BEFORE
			// initializing/capturing so no mismatched bytes reach the pipe, handing the new
			// format up so the caller respawns ffmpeg around it.
			if let Some(expected) = expected {
				if !fmt.matches(&expected) {
					CoTaskMemFree(Some(pwfx as *const _));
					tracing::warn!(
						"WASAPI loopback re-opened a different mix format \
						 ({}Hz/{}ch/{} vs spawned {}Hz/{}ch/{}) — respawning ffmpeg to match",
						fmt.rate,
						fmt.channels,
						fmt.ffmpeg_sample_fmt(),
						expected.rate,
						expected.channels,
						expected.ffmpeg_sample_fmt(),
					);
					return CaptureEnd::FormatChanged(fmt);
				}
			}
			// Size every copy/silence-fill from the device's real frame stride. We trust
			// WASAPI's nBlockAlign (== fmt.block_align() for sane PCM/float formats) so
			// 2/6/8-channel mixes all stride correctly without a stereo assumption.
			let block_align = (*pwfx).nBlockAlign as usize;
			// Initialize the loopback. Try Sunshine's full flag set first — AUTOCONVERTPCM +
			// SRC_DEFAULT_QUALITY let quirky fixed-format endpoints open via auto-resample.
			// BUT some VIRTUAL sinks reject those flags with AUDCLNT_E_UNSUPPORTED_FORMAT —
			// VERIFIED on **Steam Streaming Speakers**, the sink the host-silent redirect uses:
			// Initialize failed 0x88890003, the capture thread died, and the client got a
			// dead-silent stream (this was THE host-silent "no audio" bug). An IAudioClient can
			// be Initialize()'d only once, so on that specific rejection we open a FRESH client
			// and retry with plain `LOOPBACK` (the universally-supported baseline — what every
			// independent capturer uses, proven to work on this sink). LOOPBACK stays set in
			// both so we tap the render mix, not a capture line.
			const AUDCLNT_E_UNSUPPORTED_FORMAT: i32 = 0x8889_0003u32 as i32;
			let init = client.Initialize(
				AUDCLNT_SHAREMODE_SHARED,
				AUDCLNT_STREAMFLAGS_LOOPBACK
					| AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
					| AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
				1_000_000, // REFERENCE_TIME (100-ns units) = 100 ms; we poll every 10 ms, well inside it.
				0,
				pwfx,
				None,
			);
			CoTaskMemFree(Some(pwfx as *const _));
			let client = match init {
				Ok(()) => client,
				Err(e) if e.code().0 == AUDCLNT_E_UNSUPPORTED_FORMAT => {
					// The device (a virtual sink) rejected the auto-convert flags. Re-open a
					// fresh client and initialize with plain LOOPBACK only.
					drop(client);
					let (client2, pwfx2) = match open(device_id) {
						Ok(v) => v,
						Err(e) => return CaptureEnd::Fatal(e),
					};
					let init2 = client2.Initialize(
						AUDCLNT_SHAREMODE_SHARED,
						AUDCLNT_STREAMFLAGS_LOOPBACK,
						1_000_000,
						0,
						pwfx2,
						None,
					);
					CoTaskMemFree(Some(pwfx2 as *const _));
					if let Err(e2) = init2 {
						return classify("IAudioClient::Initialize (plain-loopback retry)", e2);
					}
					client2
				}
				Err(e) => return classify("IAudioClient::Initialize", e),
			};
			let capture: IAudioCaptureClient = match client.GetService::<IAudioCaptureClient>() {
				Ok(c) => c,
				Err(e) => return classify("GetService IAudioCaptureClient", e),
			};
			if let Err(e) = client.Start() {
				return classify("IAudioClient::Start", e);
			}

			// One 10 ms slice of silence, emitted whenever the host is silent (loopback then
			// delivers no packets at all — without this filler ffmpeg collapses the gap and
			// the audio timeline drifts ahead of the video). Sized by the REAL channel count
			// (via block_align), so a 5.1/7.1 endpoint gets a full-width silent frame.
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

	#[test]
	fn block_align_scales_with_channel_count_and_sample_width() {
		// The silence-fill is `period_frames * block_align`, so block_align is what makes
		// 5.1/7.1 capture emit full-width silent frames instead of a stereo-sized gap that
		// would desync a 6/8-channel stream. Float = 4 bytes/sample, s16 = 2.
		let stereo_f32 = LoopbackFormat { rate: 48000, channels: 2, float: true };
		assert_eq!(stereo_f32.block_align(), 2 * 4); // 8

		let surround51_f32 = LoopbackFormat { rate: 48000, channels: 6, float: true };
		assert_eq!(surround51_f32.block_align(), 6 * 4); // 24

		let surround71_f32 = LoopbackFormat { rate: 48000, channels: 8, float: true };
		assert_eq!(surround71_f32.block_align(), 8 * 4); // 32

		// 16-bit PCM endpoints halve the per-sample width.
		let surround51_s16 = LoopbackFormat { rate: 48000, channels: 6, float: false };
		assert_eq!(surround51_s16.block_align(), 6 * 2); // 12

		// One 10 ms period of 48 kHz 7.1 float silence = 480 frames * 32 bytes.
		let period_frames = (surround71_f32.rate / 100) as usize;
		let silence_len = period_frames * surround71_f32.block_align();
		assert_eq!(silence_len, 480 * 32);
	}

	#[test]
	fn matches_requires_rate_channels_and_width_to_agree() {
		// `matches` decides whether a re-opened endpoint's PCM is byte-compatible with the
		// format ffmpeg was spawned with; any differing dimension means ffmpeg must respawn
		// (otherwise it parses the new device's samples wrong → garbled / wrong-pitch audio).
		let base = LoopbackFormat { rate: 48000, channels: 2, float: true };
		assert!(base.matches(&LoopbackFormat { rate: 48000, channels: 2, float: true }));
		// Different sample rate (48k → 44.1k).
		assert!(!base.matches(&LoopbackFormat { rate: 44100, channels: 2, float: true }));
		// Different channel count (stereo → 5.1).
		assert!(!base.matches(&LoopbackFormat { rate: 48000, channels: 6, float: true }));
		// Different sample width (f32 → s16).
		assert!(!base.matches(&LoopbackFormat { rate: 48000, channels: 2, float: false }));
	}

	#[test]
	fn pinned_reassert_none_is_a_no_op() {
		// The whole point of the additive pin param: when no pin is set (the
		// `run_loopback_capture` / un-pinned path), the re-assert helper must NOT query
		// the default render endpoint or touch IPolicyConfig at all — it returns
		// immediately, so the capture behaves byte-for-byte as it did before this change.
		// `reassert_pinned_default` returns `false` exactly on that no-pin early-return,
		// so this proves the None path never reaches the sink-module COM calls.
		assert!(
			!win_loopback::reassert_pinned_default(None),
			"None (no-pin) must take the early-return no-op path and never query the default"
		);
	}
}
