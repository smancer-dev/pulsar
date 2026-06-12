//! Host-output mute control (the [`AudioPolicy::mute_host`] action). Best-effort
//! and reversible: Linux via `pactl`, Windows via Core Audio, macOS via `osascript`.
//!
//! [`AudioPolicy::mute_host`]: super::AudioPolicy::mute_host

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
/// NOT `SetMute`: WASAPI **loopback capture taps the engine mix POST-mute** — an
/// endpoint mute zeroes the loopback stream too, so the "silence the host, keep
/// streaming" contract broke (the client went silent the moment the host muted;
/// observed live Pi←PC). Master VOLUME, by contrast, is applied after the loopback
/// tap (OBS's desktop capture famously ignores the system volume slider), so we
/// silence the host by dropping the master volume to 0 and restore the user's
/// previous level on unmute. ALSO clears the endpoint mute flag while "muted":
/// a user-muted endpoint would otherwise still kill the stream.
#[cfg(windows)]
mod win_mute {
	use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
	use windows::Win32::Media::Audio::{
		eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
	};
	use windows::Win32::System::Com::{
		CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
	};

	/// The user's master-volume scalar saved when we silenced the host, restored on
	/// unmute. NaN-free sentinel: negative = nothing saved (we never silenced).
	static SAVED_VOL: std::sync::Mutex<f32> = std::sync::Mutex::new(-1.0);

	pub fn set_default_render_muted(mute: bool) -> Result<(), String> {
		unsafe {
			// S_FALSE just means COM was already initialized on this thread — fine.
			let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
			let enumerator: IMMDeviceEnumerator =
				CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
					.map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
			let device = enumerator
				.GetDefaultAudioEndpoint(eRender, eConsole)
				.map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?;
			let volume: IAudioEndpointVolume = device
				.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
				.map_err(|e| format!("IAudioEndpointVolume: {e}"))?;
			if mute {
				// Save the user's level once (a repeated mute call must not overwrite
				// it with the 0 we set last time), then silence via volume.
				let mut saved = SAVED_VOL.lock().unwrap();
				if *saved < 0.0 {
					*saved = volume.GetMasterVolumeLevelScalar().unwrap_or(-1.0);
				}
				volume
					.SetMasterVolumeLevelScalar(0.0, std::ptr::null())
					.map_err(|e| format!("SetMasterVolumeLevelScalar: {e}"))?;
				// Keep the endpoint UN-muted so the loopback capture stays live.
				let _ = volume.SetMute(false, std::ptr::null());
			} else {
				let mut saved = SAVED_VOL.lock().unwrap();
				if *saved >= 0.0 {
					let _ = volume.SetMasterVolumeLevelScalar(*saved, std::ptr::null());
					*saved = -1.0;
				}
				let _ = volume.SetMute(false, std::ptr::null());
			}
		}
		Ok(())
	}
}
