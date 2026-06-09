//! Host-output mute control (the [`AudioPolicy::mute_host`] action). Best-effort
//! and reversible: Linux via `pactl`, Windows via Core Audio, macOS is a TODO.
//!
//! [`AudioPolicy::mute_host`]: super::AudioPolicy::mute_host

/// Mute or unmute the host's **default output device** for the duration of a
/// session (the [`AudioPolicy::mute_host`] action). Best-effort + reversible:
/// Linux uses `pactl`, Windows uses Core Audio (`IAudioEndpointVolume`), macOS is
/// a TODO. Returns an error string the caller can log; failing to mute never
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
	#[cfg(not(any(target_os = "linux", windows)))]
	{
		let _ = mute;
		Err("host mute not implemented on this platform".to_string())
	}
}

/// Windows host-mute via Core Audio (`IMMDeviceEnumerator` →
/// `IAudioEndpointVolume::SetMute` on the default render endpoint).
#[cfg(windows)]
mod win_mute {
	use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
	use windows::Win32::Media::Audio::{
		eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
	};
	use windows::Win32::System::Com::{
		CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
	};

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
			volume
				.SetMute(mute, std::ptr::null())
				.map_err(|e| format!("SetMute: {e}"))?;
		}
		Ok(())
	}
}
