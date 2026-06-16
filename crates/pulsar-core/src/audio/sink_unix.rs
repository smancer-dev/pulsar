//! Host-silent-while-streaming on **Linux and macOS** via capture-source
//! **redirection** — the non-Windows analog of [`super::sink`]'s Windows
//! `IPolicyConfig` default-render-endpoint redirect.
//!
//! Why not mute: muting a sink ALSO silences its monitor/loopback. Verified on
//! Linux PulseAudio (`pactl set-sink-mute @DEFAULT_SINK@ 1`, then capturing the
//! sink's `.monitor` reads the digital noise floor, −91 dB) — exactly the bug the
//! Windows path hit at the endpoint level. So host-silent must **never** be done by
//! muting on any platform; it must REDIRECT capture to a sinkless device whose
//! monitor still carries the program audio while the real speakers stay quiet.
//!
//! **Linux (PulseAudio / PipeWire-pulse)** — ports Sunshine's null-sink model
//! (`_ref/Sunshine/src/platform/linux/audio.cpp`): on host-silent ON we
//! `module-null-sink` a sinkless device named `pulsar_silent`, make it the **default
//! sink**, and **move every currently-playing stream** onto it (so already-running
//! apps go silent on the real speakers immediately, not just future ones). The host
//! then captures `pulsar_silent.monitor` — which carries the full program audio even
//! though nothing reaches the speakers. On OFF / teardown we restore the previous
//! default sink and unload the module. A null sink has no hardware output, so the
//! host's real speakers are silent for the session's duration without any mute.
//!
//! **macOS** — conservative: detect an existing virtual/aggregate output device
//! (BlackHole / Loopback / a "*Pulsar*" aggregate) by name; if present, switch the
//! system default output to it (so program audio is routed there, off the speakers)
//! and capture it. If none is installed we do nothing and the caller falls back to
//! the historic `osascript` mute. A full CoreAudio process-tap (the only way to be
//! silent-but-captured with NO virtual device) is **out of scope** here — it's
//! untestable in this environment and a large surface; left as a TODO.
//!
//! Everything is **best-effort**: errors warn and never panic, and never block the
//! stream. The whole module is compiled only off Windows; the Windows host-silent
//! path ([`super::sink`]) is unchanged.

#![allow(clippy::result_large_err)]

/// The result of arming host-silent on this platform: the capture **source name**
/// the host must record from while the redirect is live (Linux: the null sink's
/// `.monitor`; macOS: the virtual output device name) plus the live RAII
/// [`HostSilentGuard`] that tears the redirect down on drop.
///
/// `source` is what the caller feeds to the audio ffmpeg in place of the user's
/// configured capture device — see the integration note in `handlers.rs`.
pub struct HostSilent {
	/// The capture source to record from while host-silent is active. On Linux this
	/// is `pulsar_silent.monitor`; on macOS it is the virtual output device's name
	/// (an AVFoundation/`:<idx>` mapping is the caller's concern — but in practice the
	/// Linux path is the one wired, see the module note).
	pub source: String,
	/// RAII guard: dropping it restores the previous default sink/output and unloads
	/// the null sink (Linux) / leaves the device in place (macOS).
	pub guard: HostSilentGuard,
}

/// RAII guard that holds the live host-silent redirect for one platform. Dropping it
/// restores the previous default device and tears down anything we created. Inert
/// when nothing was armed (so callers can always hold/drop one).
pub struct HostSilentGuard {
	#[cfg(target_os = "linux")]
	inner: Option<linux::NullSink>,
	#[cfg(target_os = "macos")]
	inner: Option<macos::OutputRedirect>,
	// Off Linux/macOS the guard carries nothing (no field would make it a unit struct
	// the `arm` stubs can't build with the same shape); a marker keeps the type alive.
	#[cfg(not(any(target_os = "linux", target_os = "macos")))]
	_inert: (),
}

impl Drop for HostSilentGuard {
	fn drop(&mut self) {
		#[cfg(target_os = "linux")]
		if let Some(n) = self.inner.take() {
			n.teardown();
		}
		#[cfg(target_os = "macos")]
		if let Some(r) = self.inner.take() {
			r.restore();
		}
	}
}

/// Arm host-silent on this platform: redirect program audio to a sinkless device and
/// return the capture [`HostSilent`] (source name + teardown guard), or `Ok(None)` if
/// no sinkless device could be set up (the caller then falls back to the historic
/// endpoint-mute path — but **only** ever after capture is live, per the post-mute
/// latching caveat).
///
/// `layout` is the negotiated channel layout for the session (stereo / 5.1 / 7.1).
/// On Linux it is forwarded to `module-null-sink` so the null sink and its `.monitor`
/// are created with the correct number of channels — a null sink defaults to stereo,
/// so without this a 5.1/7.1 request silently gets a 2-channel monitor and ffmpeg
/// upmixes stereo into a fake surround container. On macOS the virtual device's
/// channel count is fixed by the device itself (e.g. BlackHole 2ch vs 16ch) and
/// cannot be changed by the redirect, so `layout` is accepted but not used — the
/// caller should clamp the encode layout to the device's real channel count if known.
///
/// Best-effort: any hard error is returned as `Err(String)` for the caller to log;
/// it never panics and never blocks the stream. On platforms other than Linux/macOS
/// this is always `Ok(None)`.
pub fn arm(
	// Used on Linux to set the null sink's channel count/map.
	// On macOS the virtual device's channel count is fixed by the device itself.
	// On other platforms the function always returns Ok(None).
	#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
	layout: super::settings::ChannelLayout,
) -> Result<Option<HostSilent>, String> {
	#[cfg(target_os = "linux")]
	{
		match linux::NullSink::create(layout)? {
			Some(null) => {
				let source = null.monitor_source();
				Ok(Some(HostSilent {
					source,
					guard: HostSilentGuard { inner: Some(null) },
				}))
			}
			None => Ok(None),
		}
	}
	#[cfg(target_os = "macos")]
	{
		match macos::OutputRedirect::create()? {
			Some(redir) => {
				let source = redir.capture_source();
				Ok(Some(HostSilent {
					source,
					guard: HostSilentGuard { inner: Some(redir) },
				}))
			}
			None => Ok(None),
		}
	}
	#[cfg(not(any(target_os = "linux", target_os = "macos")))]
	{
		Ok(None)
	}
}

#[cfg(target_os = "linux")]
mod linux {
	//! PulseAudio null-sink redirect via the `pactl` CLI (no libpulse link needed —
	//! `pactl` ships with every PulseAudio/PipeWire-pulse install, the same tool the
	//! mute path and the Settings device list already shell out to). Ports Sunshine's
	//! `server_t::load_null` / `set_sink` / `unload_null` (`audio.cpp`).

	use std::process::Command;

	/// The fixed name of our null sink. Sunshine uses per-layout sinks
	/// (`sink-sunshine-stereo` …); we use one sink because the capture-layout is
	/// derived from the source downstream and a null sink advertises whatever the
	/// monitor reader requests. A stable name lets a crashed-then-restarted host
	/// detect and reuse/unload a leftover one.
	const SINK_NAME: &str = "pulsar_silent";

	/// A live PulseAudio null-sink redirect. Holds the loaded module's id and the
	/// previous default sink so [`Self::teardown`] can restore exactly. Created by
	/// [`Self::create`]; torn down by `teardown` (called from the guard's `Drop`).
	pub(super) struct NullSink {
		/// The `module-null-sink` module index returned by `pactl load-module`.
		pub(super) module_id: u32,
		/// The default sink name to restore on teardown (`None` = there was none to
		/// save; restore is then skipped).
		pub(super) prev_default: Option<String>,
	}

	/// Run `pactl <args>` and return trimmed stdout on success, or an `Err` string.
	/// Centralizes the spawn + status check so every step reports the same way.
	fn pactl(args: &[&str]) -> Result<String, String> {
		let out = Command::new("pactl")
			.args(args)
			.output()
			.map_err(|e| format!("pactl {}: {e}", args.first().copied().unwrap_or("")))?;
		if out.status.success() {
			Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
		} else {
			Err(format!(
				"pactl {} exited {}: {}",
				args.first().copied().unwrap_or(""),
				out.status,
				String::from_utf8_lossy(&out.stderr).trim()
			))
		}
	}

	/// Query the current default sink name (`pactl get-default-sink`). `None` if the
	/// command isn't available or reports nothing — the caller treats "no previous
	/// default" as benign (nothing to restore).
	fn get_default_sink() -> Option<String> {
		let name = pactl(&["get-default-sink"]).ok()?;
		if name.is_empty() {
			None
		} else {
			Some(name)
		}
	}

	/// The sink-input indices currently routed to a real sink (everything playing
	/// right now), from `pactl list short sink-inputs` (tab-separated; col 0 = index).
	/// Best-effort: a failure yields an empty list (nothing moved, but the default
	/// switch above still silences *new* streams).
	fn playing_sink_inputs() -> Vec<String> {
		let Ok(list) = pactl(&["list", "short", "sink-inputs"]) else {
			return Vec::new();
		};
		list.lines()
			.filter_map(|l| l.split('\t').next())
			.map(|s| s.trim().to_string())
			.filter(|s| !s.is_empty())
			.collect()
	}

	/// If a previous run leaked a `pulsar_silent` null sink (crash before teardown),
	/// unload it so we start clean and don't stack duplicates. Best-effort: scans
	/// `pactl list short modules` for our `sink_name=pulsar_silent` argument and
	/// unloads each match.
	fn unload_stale() {
		let Ok(list) = pactl(&["list", "short", "modules"]) else {
			return;
		};
		for line in list.lines() {
			// Columns: index \t module-name \t argument...
			if line.contains("module-null-sink")
				&& line.contains(&format!("sink_name={SINK_NAME}"))
			{
				if let Some(idx) = line.split('\t').next() {
					let _ = pactl(&["unload-module", idx.trim()]);
				}
			}
		}
	}

	impl NullSink {
		/// Load a `pulsar_silent` null sink, make it the default, and move every
		/// currently-playing stream onto it. Returns `Ok(None)` if `pactl` is missing
		/// or the module wouldn't load (caller falls back to the mute path); `Ok(Some)`
		/// once the redirect is live.
		///
		/// `layout` sets the sink (and its `.monitor`) to the correct channel count and
		/// channel map. A PulseAudio null sink with no `channels=` arg defaults to
		/// stereo, so without this a 5.1/7.1 capture would get a 2-channel monitor and
		/// ffmpeg would upmix stereo into a fake surround container. Ports Sunshine's
		/// per-layout null-sink args (`rate=48000 format=float32le channels=N
		/// channel_map=...`) from `_ref/Sunshine/src/platform/linux/audio.cpp`.
		pub(super) fn create(layout: super::super::settings::ChannelLayout) -> Result<Option<Self>, String> {
			// Clear any leftover from a prior crash before adding ours.
			unload_stale();

			let prev_default = get_default_sink();

			// Build the channel_map string for this layout. PulseAudio channel-map
			// names mirror Sunshine's `platf::speaker` constants.
			let channel_map = match layout {
				super::super::settings::ChannelLayout::Stereo => {
					"front-left,front-right".to_string()
				}
				super::super::settings::ChannelLayout::Surround51 => {
					"front-left,front-right,front-center,lfe,rear-left,rear-right".to_string()
				}
				super::super::settings::ChannelLayout::Surround71 => {
					"front-left,front-right,front-center,lfe,rear-left,rear-right,side-left,side-right"
						.to_string()
				}
			};
			let channels_arg = format!("channels={}", layout.channels());
			let channel_map_arg = format!("channel_map={channel_map}");

			// Load the null sink. Ports Sunshine's args: rate/format/channels/channel_map
			// + a friendly description so the user sees a sane name if they open a mixer.
			let load = pactl(&[
				"load-module",
				"module-null-sink",
				&format!("sink_name={SINK_NAME}"),
				"rate=48000",
				"format=float32le",
				&channels_arg,
				&channel_map_arg,
				"sink_properties=device.description=Pulsar",
			]);
			let module_id = match load {
				Ok(id_str) => match id_str.trim().parse::<u32>() {
					Ok(id) => id,
					Err(_) => {
						// pactl printed something non-numeric (or nothing) — treat as
						// "couldn't load" so the caller falls back rather than tracking a
						// bogus id we can never unload.
						return Ok(None);
					}
				},
				Err(e) => {
					// No pactl / module-null-sink unavailable → no redirect; the caller
					// falls back to the mute path. This is an expected, non-fatal outcome.
					tracing::warn!("pulsar host-silent: null-sink load failed: {e}");
					return Ok(None);
				}
			};

			// Make the null sink the default so NEW streams land on it silently.
			if let Err(e) = pactl(&["set-default-sink", SINK_NAME]) {
				tracing::warn!("pulsar host-silent: set-default-sink failed: {e} — unloading null sink");
				let _ = pactl(&["unload-module", &module_id.to_string()]);
				return Ok(None);
			}

			// Move every currently-playing stream onto the null sink so already-running
			// apps go silent on the real speakers immediately (Sunshine relies on the
			// default flip for new streams; existing ones must be moved explicitly).
			for input in playing_sink_inputs() {
				if let Err(e) = pactl(&["move-sink-input", &input, SINK_NAME]) {
					// A stream that vanished mid-move (or refuses) is non-fatal — keep going.
					tracing::debug!("pulsar host-silent: move-sink-input {input} failed: {e}");
				}
			}

			tracing::info!(
				module_id,
				channels = layout.channels(),
				prev_default = ?prev_default,
				"pulsar host-silent: redirected default sink to null sink (Linux)"
			);
			Ok(Some(Self {
				module_id,
				prev_default,
			}))
		}

		/// The capture source the host must record from while this redirect is live:
		/// the null sink's monitor, which carries the full program audio.
		pub(super) fn monitor_source(&self) -> String {
			format!("{SINK_NAME}.monitor")
		}

		/// Restore the previous default sink and unload the null sink. Moving the
		/// streams back is unnecessary: PulseAudio re-homes a stream whose sink
		/// disappears onto the (now-restored) default automatically. Best-effort.
		pub(super) fn teardown(self) {
			if let Some(prev) = self.prev_default.as_deref() {
				if let Err(e) = pactl(&["set-default-sink", prev]) {
					tracing::warn!("pulsar host-silent: restore default sink failed: {e}");
				}
			}
			if let Err(e) = pactl(&["unload-module", &self.module_id.to_string()]) {
				tracing::warn!(
					module_id = self.module_id,
					error = %e,
					"pulsar host-silent: unload null sink failed"
				);
			} else {
				tracing::info!("pulsar host-silent: restored host audio (Linux)");
			}
		}
	}
}

#[cfg(target_os = "macos")]
mod macos {
	//! macOS conservative redirect: switch the default OUTPUT to an existing virtual
	//! device (BlackHole / Loopback / a "*Pulsar*" aggregate) and capture it, so
	//! program audio is routed off the speakers while the host still captures it.
	//!
	//! Implemented via the `SwitchAudioSource` CLI when present (a common Homebrew
	//! tool: `-a -t output` lists output devices, `-c -t output` reads the current
	//! one, `-s <name> -t output` switches). If that tool isn't installed, or no
	//! virtual device is found, we return `Ok(None)` and the caller falls back to the
	//! `osascript` mute. A native CoreAudio
	//! `AudioObjectSetPropertyData(kAudioHardwarePropertyDefaultOutputDevice)` switch
	//! would remove the CLI dependency; left as a TODO — untestable in this
	//! environment, and the CLI path is the conservative, reversible MVP.
	//!
	//! TODO(macos): a full CoreAudio **process-tap** (ScreenCaptureKit /
	//! `AudioHardwareCreateProcessTap`, macOS 14.4+) would let us capture the system
	//! mix while the speakers stay live — the true "silent host" with no virtual
	//! device. Out of scope here (large, untestable surface).

	use std::process::Command;

	/// Names we consider "virtual/sinkless-ish" output devices to redirect to. Matched
	/// case-insensitively as a substring of a device name from `SwitchAudioSource`.
	const VIRTUAL_OUTPUT_CANDIDATES: &[&str] = &["BlackHole", "Loopback", "Pulsar", "Soundflower"];

	/// A live macOS default-output redirect: the device we switched away from, so
	/// [`Self::restore`] can switch back. The device we switched TO is the capture
	/// source.
	pub(super) struct OutputRedirect {
		/// The default-output device name to restore on teardown (`None` = couldn't
		/// read it; restore is skipped).
		pub(super) prev_output: Option<String>,
		/// The virtual device we switched to (also the capture source name).
		pub(super) virtual_name: String,
	}

	/// Run `SwitchAudioSource <args>` and return trimmed stdout, or `None` if the tool
	/// is missing / failed (the caller treats every failure as "no redirect available").
	fn switch_audio(args: &[&str]) -> Option<String> {
		let out = Command::new("SwitchAudioSource").args(args).output().ok()?;
		if out.status.success() {
			Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
		} else {
			None
		}
	}

	/// List output device names (`SwitchAudioSource -a -t output`), one per line.
	fn list_outputs() -> Vec<String> {
		switch_audio(&["-a", "-t", "output"])
			.map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
			.unwrap_or_default()
	}

	/// The current default output device name (`SwitchAudioSource -c -t output`).
	fn current_output() -> Option<String> {
		switch_audio(&["-c", "-t", "output"]).filter(|s| !s.is_empty())
	}

	impl OutputRedirect {
		/// Find a virtual output device and switch the default output to it. `Ok(None)`
		/// if `SwitchAudioSource` is absent or no virtual device is installed (caller
		/// falls back to the `osascript` mute).
		pub(super) fn create() -> Result<Option<Self>, String> {
			let outputs = list_outputs();
			if outputs.is_empty() {
				// No SwitchAudioSource (or it returned nothing) → no redirect available.
				tracing::warn!(
					"pulsar host-silent (macOS): SwitchAudioSource unavailable — falling back to mute. \
					 Install a virtual output (e.g. BlackHole) + SwitchAudioSource for true host-silent."
				);
				return Ok(None);
			}
			let virtual_name = outputs.into_iter().find(|name| {
				let lc = name.to_lowercase();
				VIRTUAL_OUTPUT_CANDIDATES
					.iter()
					.any(|c| lc.contains(&c.to_lowercase()))
			});
			let Some(virtual_name) = virtual_name else {
				tracing::warn!(
					candidates = ?VIRTUAL_OUTPUT_CANDIDATES,
					"pulsar host-silent (macOS): no virtual output device present — falling back to mute"
				);
				return Ok(None);
			};

			let prev_output = current_output();
			if switch_audio(&["-s", &virtual_name, "-t", "output"]).is_none() {
				return Err(format!(
					"SwitchAudioSource -s {virtual_name} -t output failed"
				));
			}
			tracing::info!(
				%virtual_name,
				prev_output = ?prev_output,
				"pulsar host-silent: redirected default output to virtual device (macOS)"
			);
			Ok(Some(Self {
				prev_output,
				virtual_name,
			}))
		}

		/// The capture source the host records from while the redirect is live (the
		/// virtual device name). The caller maps this to an AVFoundation input.
		pub(super) fn capture_source(&self) -> String {
			self.virtual_name.clone()
		}

		/// Switch the default output back to the saved device. Best-effort.
		pub(super) fn restore(self) {
			if let Some(prev) = self.prev_output.as_deref() {
				if switch_audio(&["-s", prev, "-t", "output"]).is_none() {
					tracing::warn!(
						prev,
						"pulsar host-silent (macOS): restore default output failed"
					);
				} else {
					tracing::info!("pulsar host-silent: restored host audio (macOS)");
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn arm_is_inert_on_unsupported_platforms() {
		// On a platform with neither pactl nor SwitchAudioSource the call must be a
		// clean, non-panicking best-effort. On Linux/macOS CI without the tools this
		// returns Ok(None) (tool missing); off both it's a compile-time Ok(None). Either
		// way it must never panic and the guard (if any) must drop cleanly.
		match arm(super::settings::ChannelLayout::Stereo) {
			Ok(None) => {}
			Ok(Some(hs)) => {
				// A redirect actually armed (a dev box WITH the tools): the source must be
				// non-empty and dropping the guard must restore cleanly.
				assert!(!hs.source.is_empty());
				drop(hs.guard);
			}
			Err(e) => {
				// A hard error is acceptable (e.g. the tool exists but the switch failed);
				// it must be a descriptive string, not a panic.
				assert!(!e.is_empty());
			}
		}
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn linux_monitor_source_name_is_the_null_sink_monitor() {
		// The capture source name the host must record from is the null sink's monitor.
		// (Constructed without touching pactl so the test is hermetic.)
		let ns = super::linux::NullSink {
			module_id: 42,
			prev_default: Some("alsa_output.pci-0000_00_1f.3.analog-stereo".into()),
		};
		assert_eq!(ns.monitor_source(), "pulsar_silent.monitor");
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn macos_capture_source_is_the_virtual_device_name() {
		let r = super::macos::OutputRedirect {
			prev_output: Some("Built-in Output".into()),
			virtual_name: "BlackHole 2ch".into(),
		};
		assert_eq!(r.capture_source(), "BlackHole 2ch");
	}
}
