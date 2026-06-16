//! Host-output mute control (the [`AudioPolicy::mute_host`] action). Best-effort
//! and reversible: Linux via `pactl`, Windows via Core Audio, macOS via `osascript`.
//!
//! [`AudioPolicy::mute_host`]: super::AudioPolicy::mute_host

/// Path of the crash-recovery marker for the endpoint-mute fallback. Written when the
/// fallback mute is applied; deleted on the matching unmute. On the next launch
/// [`restore_stale_mute_fallback`] reads it and issues the unmute so a crashed process
/// can't leave the host output muted indefinitely.
pub fn mute_fallback_marker_path() -> std::path::PathBuf {
	std::env::temp_dir().join("pulsar-mute-fallback.active")
}

/// Startup crash-restore for the endpoint-mute fallback. If the previous process wrote
/// the marker (i.e. it applied the mute fallback and then died abnormally before the
/// session ended), unmute the default output now and remove the marker. No-op when the
/// marker is absent (clean previous exit or fallback was never used).
///
/// Gated by the marker so a deliberate user mute set independently of Pulsar is never
/// clobbered — we only unmute when WE are known to be the ones who muted it (same
/// guarantee C12 required for the unconditional startup-unmute it removed).
pub fn restore_stale_mute_fallback() {
	let path = mute_fallback_marker_path();
	if !path.exists() {
		return; // no marker → clean previous exit, nothing to restore
	}
	// Consume the marker first so a failure in set_host_muted can't loop us forever.
	let _ = std::fs::remove_file(&path);
	if let Err(e) = set_host_muted(false) {
		tracing::warn!("mute-fallback crash-restore: unmute failed: {e}");
	} else {
		tracing::info!("restored host output after a prior crash left the mute-fallback active");
	}
}

/// Mute or unmute the host's **default output device** for the duration of a
/// session (the [`AudioPolicy::mute_host`] action). Best-effort + reversible:
/// Linux uses `pactl`, Windows uses Core Audio (`IAudioEndpointVolume`), macOS uses
/// `osascript`. Returns an error string the caller can log; failing to mute never
/// breaks streaming.
///
/// [`AudioPolicy::mute_host`]: super::AudioPolicy::mute_host
pub fn set_host_muted(mute: bool) -> Result<(), String> {
	#[cfg(target_os = "linux")]
	{
		let arg = if mute { "1" } else { "0" };
		std::process::Command::new("pactl")
			.args(["set-sink-mute", "@DEFAULT_SINK@", arg])
			.status()
			.map_err(|e| format!("pactl: {e}"))
			.and_then(|st| {
				if st.success() {
					Ok(())
				} else {
					Err(format!("pactl exited {st}"))
				}
			})
	}
	#[cfg(windows)]
	{
		win_mute::set_default_render_muted(mute)
	}
	#[cfg(target_os = "macos")]
	{
		// macOS has no headless mute API like pactl/Core Audio, but AppleScript's
		// `set volume output muted` toggles the default output's mute flag. Best-effort
		// + reversible, same contract as the other platforms.
		let arg = if mute { "true" } else { "false" };
		std::process::Command::new("osascript")
			.args(["-e", &format!("set volume output muted {arg}")])
			.status()
			.map_err(|e| format!("osascript: {e}"))
			.and_then(|st| {
				if st.success() {
					Ok(())
				} else {
					Err(format!("osascript exited {st}"))
				}
			})
	}
	#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
	{
		let _ = mute;
		Err("host mute not implemented on this platform".to_string())
	}
}

/// Windows host-mute via Core Audio on the default render endpoint.
///
/// Uses the endpoint **MUTE flag** (`SetMute`), NOT the master volume. The earlier
/// approach dropped the master volume to 0 — but on common codecs (verified on this
/// Realtek endpoint) WASAPI **loopback capture taps POST master-volume**, so volume-0
/// silenced the captured stream too and the client went dead silent (measured at the
/// digital noise floor, -91 dBFS, live Pi←PC). It also latched: a loopback client
/// initialized while the volume was 0 stayed silent even after the volume was raised.
///
/// The endpoint mute flag, by contrast, is applied AFTER the loopback tap on this
/// hardware: muting silences the physical speakers while the loopback (and therefore
/// the streamed audio) keeps flowing — verified by capturing the live loopback while
/// toggling `SetMute(true)` mid-stream (samples stayed at full level). This also
/// matches the Linux (`pactl set-sink-mute`) and macOS (`set volume output muted`)
/// paths, which already mute via the mute flag, and removes all the volume
/// save/restore + crash-marker bookkeeping the volume approach needed.
///
/// Note: which endpoint control sits in the loopback path is codec-dependent. If a
/// host's hardware instead routes the mute flag through the loopback tap, the robust
/// answer is a dedicated virtual sink (Sunshine's "Steam Streaming Speakers" model),
/// not toggling the physical endpoint — left as a future option.
#[cfg(windows)]
mod win_mute {
	use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
	use windows::Win32::Media::Audio::{
		eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
	};
	use windows::Win32::System::Com::{
		CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
	};

	/// Open the default render endpoint's volume interface. COM is MTA-initialized on
	/// the calling thread (S_FALSE = already initialized — harmless).
	unsafe fn endpoint_volume() -> Result<IAudioEndpointVolume, String> {
		let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
		let enumerator: IMMDeviceEnumerator =
			CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
				.map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
		let device = enumerator
			.GetDefaultAudioEndpoint(eRender, eConsole)
			.map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?;
		device
			.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
			.map_err(|e| format!("IAudioEndpointVolume: {e}"))
	}

	/// (Un)mute the default render endpoint via the mute flag. No volume change → the
	/// WASAPI loopback the host streams from is never silenced (see module docs). A
	/// crash while muted leaves the endpoint muted; the user can always un-mute from
	/// the tray in the meantime (Pulsar never unconditionally clears the mute on
	/// startup — that would clobber a deliberate user mute).
	pub fn set_default_render_muted(mute: bool) -> Result<(), String> {
		unsafe {
			let volume = endpoint_volume()?;
			volume
				.SetMute(mute, std::ptr::null())
				.map_err(|e| format!("SetMute: {e}"))
		}
	}
}
