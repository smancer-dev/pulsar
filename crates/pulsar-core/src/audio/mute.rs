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
	/// unmute. NaN-free sentinel: negative = nothing saved (we never silenced, or we
	/// already restored). Never holds 0.0 as a "saved level" — see `set_default_render_muted`.
	static SAVED_VOL: std::sync::Mutex<f32> = std::sync::Mutex::new(-1.0);

	/// Crash-restore marker: the saved volume is also written to a tiny file when we
	/// silence the host, so an abnormal exit (crash / taskkill / tray-quit) that
	/// skips the unmute path doesn't strand the host at volume 0. The NEXT launch
	/// (`restore_stale_mute`) reads it, restores the level, and deletes it. The file
	/// is removed on a clean unmute so a normal session leaves no stale marker.
	fn marker_path() -> std::path::PathBuf {
		std::env::temp_dir().join("pulsar-host-mute.vol")
	}

	/// Open the default render endpoint's volume interface (shared by mute/unmute and
	/// the crash-restore path). COM is MTA-initialized on the calling thread
	/// (S_FALSE = already initialized — harmless).
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

	/// Restore a stale crash marker exactly once per process, the FIRST time the
	/// mute control is touched. `set_default_render_muted` calls this up front so a
	/// previous run that died mid-mute (marker on disk, SAVED_VOL gone with the dead
	/// process) gets the user's level back before we do anything else. Guarded so the
	/// restore never fights an active mute later in the same process.
	static RESTORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

	pub fn set_default_render_muted(mute: bool) -> Result<(), String> {
		// One-time crash-restore: if a prior process left a marker, recover the level
		// before this call's own mute/unmute logic runs (and before we'd overwrite the
		// marker). Only meaningful on the very first touch of the mute control.
		if !RESTORED.swap(true, std::sync::atomic::Ordering::SeqCst) {
			restore_stale_mute();
		}
		unsafe {
			let volume = endpoint_volume()?;
			if mute {
				// Save the user's level ONCE per unmuted→muted transition (a repeated
				// mute call — e.g. a re-stream while another client still wants mute —
				// must not overwrite it with the 0 we set last time). MUTE_OWNERS in
				// handlers.rs only calls us on the true empty↔non-empty edge, but guard
				// here too so the scalar swap can never drift from the owner set.
				let mut saved = SAVED_VOL.lock().unwrap();
				if *saved < 0.0 {
					let cur = volume.GetMasterVolumeLevelScalar().unwrap_or(-1.0);
					// NEVER record 0.0 (or a read error) as the "user level": if we did,
					// a later unmute would restore the host to silence FOREVER. A current
					// reading of 0 means there is nothing meaningful to restore — leave
					// the sentinel negative so unmute simply doesn't touch the volume.
					if cur > 0.0 {
						*saved = cur;
						// Persist for crash-restore (best-effort; mute still works without it).
						let _ = std::fs::write(marker_path(), cur.to_le_bytes());
					}
				}
				volume
					.SetMasterVolumeLevelScalar(0.0, std::ptr::null())
					.map_err(|e| format!("SetMasterVolumeLevelScalar: {e}"))?;
				// Keep the endpoint UN-muted so the loopback capture stays live.
				let _ = volume.SetMute(false, std::ptr::null());
			} else {
				let mut saved = SAVED_VOL.lock().unwrap();
				if *saved > 0.0 {
					let _ = volume.SetMasterVolumeLevelScalar(*saved, std::ptr::null());
				}
				// Always reset the sentinel + clear the crash marker on unmute, even if
				// nothing was saved (cur read 0 at mute time): a clean unmute must leave
				// NO stale "muted" state behind for the next launch to act on.
				*saved = -1.0;
				let _ = std::fs::remove_file(marker_path());
				let _ = volume.SetMute(false, std::ptr::null());
			}
		}
		Ok(())
	}

	/// Next-launch crash-restore: if a previous run silenced the host and then died
	/// before unmuting, its marker file holds the user's level — restore it and
	/// delete the marker so the host is never stranded at volume 0. Safe to call
	/// unconditionally; a no-op when no marker exists.
	fn restore_stale_mute() {
		let path = marker_path();
		let Ok(bytes) = std::fs::read(&path) else {
			return; // no marker → clean previous exit, nothing to restore
		};
		let _ = std::fs::remove_file(&path);
		if bytes.len() != 4 {
			return;
		}
		let level = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
		if !(level > 0.0) {
			return; // never restore to silence / a garbage value
		}
		unsafe {
			if let Ok(volume) = endpoint_volume() {
				let _ = volume.SetMasterVolumeLevelScalar(level, std::ptr::null());
				let _ = volume.SetMute(false, std::ptr::null());
			}
		}
	}
}
