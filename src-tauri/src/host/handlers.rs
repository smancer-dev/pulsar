//! Per-session handler factories extracted from `go_online`'s serve loop. Each
//! `make_*` builds and returns the same inline closure the host wired before, so
//! behavior is identical; `spawn_loopback_audio` is the Windows WASAPI helper
//! `make_on_stream` calls within this module.

use super::*;

/// Windows: stream the host's system audio via **WASAPI loopback** (no
/// `virtual-audio-capturer` / Stereo Mix device required). Spawns the encoder ffmpeg
/// reading raw PCM on stdin (Opus/RTP to `dest`) plus a capture thread that feeds it
/// the default render endpoint's loopback. The ffmpeg is tracked in `procs`, so a
/// (re-)stream or session teardown kills it — which ends the capture thread via the
/// broken pipe. Returns whether it started (callers fall back to the dshow path).
///
/// `pinned_default` is the virtual-sink device-id when the host-silent sink redirect
/// is active for this stream (`Some` = the loopback re-asserts that id as the OS
/// default on every reinit so it keeps tapping the virtual sink — review HIGH-2);
/// `None` when not redirecting (or the mute-fallback path). `req_layout` is the
/// negotiated channel layout — the encode layout derives from the live endpoint's
/// real channel count but is CLAMPED to it (we never encode more channels than the
/// client asked for, even if the endpoint mix is wider).
#[cfg(windows)]
pub(super) fn spawn_loopback_audio(
	procs: &Arc<Mutex<Vec<Child>>>,
	ffmpeg: &str,
	dest: &str,
	pinned_default: Option<String>,
	req_layout: pulsar_core::audio::ChannelLayout,
) -> bool {
	use pulsar_core::audio::LoopbackFormat;
	use std::process::Stdio;
	// Read the format of the SAME endpoint the capture will tap: the pinned virtual sink
	// (host-silent) when set, else the OS default. Matching them keeps ffmpeg's -f/-ar/-ac
	// in lockstep with the PCM the loopback writes (and dodges the SetDefaultEndpoint
	// propagation race that querying the default right after a redirect would hit).
	let fmt = match pulsar_core::audio::loopback_format(pinned_default.as_deref()) {
		Ok(f) => f,
		Err(_) => {
			return false;
		}
	};
	// Build + spawn the encoder ffmpeg for a given loopback format. Returned so the capture
	// thread can RESPAWN it when the default render endpoint changes to one with a different
	// mix format (sample rate / channels / bit depth) mid-session: ffmpeg's -f/-ar/-ac are
	// fixed for its lifetime, so the re-opened device's PCM would otherwise be parsed wrong
	// (garbled / wrong-pitch audio for the rest of the session). On respawn the encode layout
	// is re-derived from the new channel count too.
	let ffmpeg_owned = ffmpeg.to_string();
	let dest_owned = dest.to_string();
	let spawn_encoder = move |fmt: LoopbackFormat| -> Option<(Child, std::process::ChildStdin)> {
		let mut args: Vec<String> = vec![
			"-hide_banner".into(),
			"-loglevel".into(),
			"error".into(),
			// Read the raw PCM pipe with NO input buffering / NO startup probe: ffmpeg would
			// otherwise read + hold a chunk to "analyze" the stream (plus whatever burst the
			// WASAPI loopback delivers at open) before emitting the first packet, baking a
			// fixed audio delay behind the ultra-low-latency video for the whole session.
			"-fflags".into(),
			"nobuffer".into(),
			"-probesize".into(),
			"32".into(),
			"-analyzeduration".into(),
			"0".into(),
			"-f".into(),
			fmt.ffmpeg_sample_fmt().into(),
			"-ar".into(),
			fmt.rate.to_string(),
			"-ac".into(),
			fmt.channels.to_string(),
			"-i".into(),
			"pipe:0".into(),
		];
		// Encode at the layout the loopback endpoint actually delivers: the WASAPI mix
		// format's channel count (`fmt.channels`) is authoritative — a 5.1/7.1 endpoint
		// streams 6/8 real channels, a stereo one 2 — so derive the Opus layout from it
		// rather than blindly trusting the request, which would make ffmpeg up/down-mix
		// silently (e.g. -ac 6 over a 2-channel capture = 4 silent surround channels).
		let endpoint_layout = match fmt.channels {
			6 => pulsar_core::audio::ChannelLayout::Surround51,
			8 => pulsar_core::audio::ChannelLayout::Surround71,
			_ => pulsar_core::audio::ChannelLayout::Stereo,
		};
		// Clamp to the negotiated request: never encode MORE channels than the client asked
		// for (a stereo client must get stereo even off a 5.1 endpoint), but never claim more
		// than the endpoint actually delivers either. The redirect already set the virtual
		// sink's format to `req_layout`, so on the host-silent path these usually agree.
		let cap_layout = if (req_layout.channels()) < endpoint_layout.channels() {
			req_layout
		} else {
			endpoint_layout
		};
		args.extend(pulsar_core::audio::opus_rtp_output_layout(&dest_owned, cap_layout));
		let mut cmd = std::process::Command::new(&ffmpeg_owned);
		cmd.args(&args).stdin(Stdio::piped());
		no_window(&mut cmd);
		let mut child = cmd.spawn().ok()?;
		let stdin = match child.stdin.take() {
			Some(s) => s,
			None => {
				let _ = child.kill();
				return None;
			}
		};
		crate::job::assign(&child); // tie ffmpeg to Pulsar's lifetime (job.rs), like spawn_tracked
		Some((child, stdin))
	};
	let (child, stdin) = match spawn_encoder(fmt) {
		Some(v) => v,
		None => return false,
	};
	let mut cur_pid = child.id();
	procs.lock().unwrap().push(child);
	let procs = procs.clone();
	std::thread::spawn(move || {
		// Runs until the pipe breaks (ffmpeg killed on teardown) or WASAPI errors — both expected.
		// When host-silent is active, `pinned_default` is the virtual sink's id: the capture
		// re-asserts it as the OS default before every (re)open, so a default-endpoint flip
		// mid-stream can't make us tap the host's real speakers (review HIGH-2). `None` =
		// the plain capture (identical to the old behavior).
		//
		// The capture is FORMAT-TRACKING (`fmt` = what the current ffmpeg was spawned with):
		// when a mid-session default-endpoint change re-opens an endpoint with a different mix
		// format, it stops (before any mismatched bytes are written) and hands the new format
		// back so we kill + respawn ffmpeg around it and resume — otherwise ffmpeg's fixed
		// -f/-ar/-ac would parse the new device's PCM wrong for the rest of the session.
		let mut stdin = stdin;
		let mut cur_fmt = fmt;
		loop {
			match pulsar_core::audio::run_loopback_capture_tracking(
				stdin,
				pinned_default.clone(),
				cur_fmt,
			) {
				Ok(Some(new_fmt)) => {
					// The default endpoint changed format. Spawn a new ffmpeg with the new
					// -f/-ar/-ac, then swap it into `procs` for the OLD one (matched by pid, so
					// we never touch the video/other tracked children) and kill the stale one.
					let (new_child, new_stdin) = match spawn_encoder(new_fmt) {
						Some(v) => v,
						None => return, // can't respawn (e.g. ffmpeg gone) → give up on audio
					};
					let new_pid = new_child.id();
					// Under the same lock teardown/(re)stream drain `procs`: if our old
					// child is already gone the session moved on, so don't track the new
					// one (it would be an untracked orphan) — kill it and stop.
					let mut old = {
						let mut g = procs.lock().unwrap();
						let Some(idx) = g.iter().position(|c| c.id() == cur_pid) else {
							drop(g);
							let mut new_child = new_child;
							let _ = new_child.kill();
							return;
						};
						let old = g.remove(idx);
						g.push(new_child);
						old
						// lock released here, before wait()
					};
					let _ = old.kill();
					let _ = old.wait();
					stdin = new_stdin;
					cur_fmt = new_fmt;
					cur_pid = new_pid;
					// loop: resume capture against the new endpoint/format
				}
				// Clean stop (pipe broke / ffmpeg gone) or a fatal WASAPI error — done.
				Ok(None) | Err(_) => return,
			}
		}
	});
	true
}

// ---- Host-silent via Sunshine-style default-render-endpoint REDIRECTION ----
//
// The host-silent intent (`req.mute_host`) is satisfied by REDIRECTING the default
// render endpoint to our bundled sinkless virtual sink (Virtual-Audio-Driver) and
// capturing THAT device's loopback — NOT by muting. Muting the captured endpoint
// latches silence into the loopback on post-mute codecs (the bug fixed 2026-06-13),
// so we never mute at capture-open. Restoring the original default on the last
// owner leaving puts the host's real speakers back. See `pulsar_core::audio::sink`.

/// Ordered candidates matched (case-insensitive substring) against the names of
/// **active** render endpoints to locate a sinkless virtual sink to redirect to.
///
/// We use whatever loadable virtual sink is PRESENT (Sunshine's model) instead of
/// hard-requiring one bundled driver. A kernel audio driver must be
/// **Microsoft-attestation-signed** to load on modern Windows; our bundled MIT/MS-PL
/// `Virtual-Audio-Driver` is only SignPath-code-signed, so it *installs* but won't
/// *load* (`CM_PROB_UNSIGNED_DRIVER`, code 52) until we get it MS-signed. So we prefer
/// **Steam Streaming Speakers** (MS-signed, present whenever Steam is installed —
/// exactly what Sunshine uses by default), then other common virtual cables, then our
/// own driver once it loads. [`find_render_device_by_name`] returns only ACTIVE
/// endpoints, so a non-loadable / Error-state device is skipped automatically.
///
/// [`find_render_device_by_name`]: pulsar_core::audio::find_render_device_by_name
const VIRTUAL_SINK_CANDIDATES: &[&str] = &[
	"Steam Streaming Speakers",
	"Virtual Audio Driver by MTT",
	"VB-Audio",
	"CABLE Input",
];

/// Locate the first present+active virtual sink from [`VIRTUAL_SINK_CANDIDATES`].
/// `Ok(None)` if none is installed (→ caller falls back to the endpoint-mute path).
#[cfg(windows)]
fn locate_virtual_sink() -> Result<Option<pulsar_core::audio::SinkDevice>, String> {
	for needle in VIRTUAL_SINK_CANDIDATES {
		if let Some(dev) = pulsar_core::audio::find_render_device_by_name(needle)? {
			return Ok(Some(dev));
		}
	}
	Ok(None)
}

/// Sessions (by sid) currently requesting the host be SILENT (their `mute_host`
/// intent). GLOBAL ownership so a same-peer reconnect's delayed teardown can't
/// strand the redirect. Paired with `REDIRECT_GUARD`.
#[cfg(windows)]
static REDIRECT_OWNERS: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
	std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// The single live sink-redirect guard. `Some` while the owner set is non-empty
/// (the host is redirected to the virtual sink); dropping it restores the saved
/// default. Held in a static so it outlives the per-stream closure and is released
/// exactly when the last owner leaves (or the process dies → crash marker recovers).
#[cfg(windows)]
static REDIRECT_GUARD: std::sync::LazyLock<
	Mutex<Option<pulsar_core::audio::SinkRedirectGuard>>,
> = std::sync::LazyLock::new(|| Mutex::new(None));

/// When the virtual sink is unavailable (driver not installed) we fall back to the
/// endpoint mute toggle for an EXPLICIT user host-silent request — but only ever
/// applied AFTER capture is live (mute_with_fallback is called from the per-stream
/// path, where the loopback is already opened), never at capture-open. Tracks
/// whether we took that fallback so the matching unmute runs on release.
#[cfg(windows)]
static REDIRECT_MUTE_FALLBACK: std::sync::atomic::AtomicBool =
	std::sync::atomic::AtomicBool::new(false);

/// The device-id of the virtual sink the default render endpoint is CURRENTLY
/// redirected to (`Some` while `REDIRECT_GUARD` is `Some`). The loopback capture is
/// pinned to this id (re-asserted as the OS default on every reinit — review HIGH-2)
/// so it keeps tapping the virtual sink even if Windows flips the default off it.
/// Read by `start_audio_and_mute` when spawning the loopback; cleared on release.
#[cfg(windows)]
static REDIRECT_SINK_ID: std::sync::LazyLock<Mutex<Option<String>>> =
	std::sync::LazyLock::new(|| Mutex::new(None));

/// Record `sid`'s host-silent wish and switch the redirect on/off when the owner set
/// flips between empty and non-empty. Sunshine model: redirect the default render
/// endpoint to the bundled virtual sink (never mute). `layout` is the negotiated
/// channel layout for this stream — BEFORE redirecting we set the virtual sink's
/// device format to that channel count (`set_render_device_format`), so the loopback
/// capture opens at the right width (a stereo-only sink would otherwise cap a 5.1/7.1
/// stream). A format failure is non-fatal (warn + keep the sink's existing format).
/// If the virtual sink can't be found (driver missing), signal the caller (via the
/// returned bool) to fall back to the endpoint mute toggle — applied by the caller
/// AFTER capture is live, never here (muting an already-open loopback is safe; opening
/// into a mute is not).
///
/// On the empty→non-empty transition the located virtual sink's device-id is stashed
/// in `REDIRECT_SINK_ID` so `start_audio_and_mute` can pin the loopback capture to it
/// (review HIGH-2 re-assert). Cleared on the non-empty→empty (restore) transition.
///
/// Returns `true` when no redirect could be set up and an endpoint-mute fallback is
/// WANTED — the caller (`start_audio_and_mute`) must then apply `mute_fallback(true)`
/// AFTER the loopback capture is spawned, never here: muting at capture-open latches
/// silence into the WASAPI loopback on post-mute codecs (the bug fixed 2026-06-13). The
/// `false`/restore transition undoes any prior mute fallback inline (unmuting an
/// already-open or closed capture is always safe).
#[cfg(windows)]
#[must_use = "returns true when a post-capture mute fallback must be applied by the caller"]
fn set_redirect_request(sid: u64, want: bool, layout: pulsar_core::audio::ChannelLayout) -> bool {
	let mut owners = REDIRECT_OWNERS.lock().unwrap();
	let was = !owners.is_empty();
	if want {
		owners.insert(sid);
	} else {
		owners.remove(&sid);
	}
	let now = !owners.is_empty();
	if was == now {
		return false;
	}
	tracing::info!(sid, want, ?layout, owners = ?owners, "host-silent (sink-redirect) request");
	drop(owners);
	if now {
		// Going silent: locate a virtual sink and redirect the default to it.
		match locate_virtual_sink() {
			Ok(Some(dev)) => {
				// Set the sink's device format to the negotiated channel count FIRST, so
				// the redirected loopback opens at the right width. Best-effort: a failure
				// just leaves the sink at its existing (usually stereo) format.
				if let Err(e) = pulsar_core::audio::set_render_device_format(&dev.id, layout) {
					tracing::warn!(?layout, "set virtual sink device format failed: {e} — keeping existing sink format");
				}
				match pulsar_core::audio::SinkRedirectGuard::redirect_to(&dev.id) {
					Ok(guard) => {
						tracing::info!(sink = %dev.friendly_name, ?layout, "redirected default render endpoint to virtual sink (host-silent)");
						*REDIRECT_GUARD.lock().unwrap() = Some(guard);
						// Pin the loopback to this exact device-id (re-asserted as the OS
						// default on every reinit — review HIGH-2).
						*REDIRECT_SINK_ID.lock().unwrap() = Some(dev.id.clone());
						false
					}
					Err(e) => {
						tracing::warn!("sink redirect failed: {e} — falling back to endpoint mute (deferred to post-capture)");
						true
					}
				}
			}
			Ok(None) => {
				tracing::warn!(
					candidates = ?VIRTUAL_SINK_CANDIDATES,
					"no virtual sink present (Steam Streaming Speakers / cable / our driver) — falling back to endpoint mute (deferred to post-capture)"
				);
				true
			}
			Err(e) => {
				tracing::warn!("enumerating render endpoints failed: {e} — falling back to endpoint mute (deferred to post-capture)");
				true
			}
		}
	} else {
		// Last owner left: restore. Drop the guard (restores the saved default), clear
		// the pinned-sink id, and/or undo any mute fallback we took.
		*REDIRECT_GUARD.lock().unwrap() = None;
		*REDIRECT_SINK_ID.lock().unwrap() = None;
		mute_fallback(false);
		false
	}
}

/// Endpoint-mute fallback for the rare no-virtual-sink case. Mirrors the redirect
/// transition: `true` mutes (only reached AFTER capture is live — see callers),
/// `false` unmutes. Tracks whether the fallback is active so we don't unmute a host
/// the user muted elsewhere.
#[cfg(windows)]
fn mute_fallback(mute: bool) {
	use std::sync::atomic::Ordering;
	if mute {
		if !REDIRECT_MUTE_FALLBACK.swap(true, Ordering::SeqCst) {
			if let Err(e) = pulsar_core::audio::set_host_muted(true) {
				tracing::warn!("host mute fallback failed: {e}");
			}
		}
	} else if REDIRECT_MUTE_FALLBACK.swap(false, Ordering::SeqCst) {
		if let Err(e) = pulsar_core::audio::set_host_muted(false) {
			tracing::warn!("host unmute (fallback) failed: {e}");
		}
	}
}

// ---- Host-silent on Linux/macOS via capture-source REDIRECTION (NOT muting) ----
//
// Muting a sink ALSO silences its monitor/loopback on Linux PulseAudio (verified:
// `pactl set-sink-mute @DEFAULT_SINK@ 1` then capturing the `.monitor` reads −91 dB)
// and on macOS, exactly like the Windows endpoint case. So the non-Windows
// host-silent intent is satisfied by REDIRECTING capture to a sinkless device — a
// PulseAudio null sink on Linux (Sunshine's model), a virtual-output switch on macOS
// — and recording THAT device's monitor, never by muting. `pulsar_core::audio::
// arm_host_silent` does the platform work and returns the capture source name + an
// RAII teardown guard. The owner set mirrors the Windows `REDIRECT_OWNERS` so a
// same-peer reconnect's delayed teardown can't strand the redirect.

/// Sessions (by sid) currently requesting the host be SILENT on Linux/macOS. GLOBAL
/// ownership like the Windows `REDIRECT_OWNERS`. Paired with `UNIX_SILENT_GUARD`.
#[cfg(not(windows))]
static UNIX_SILENT_OWNERS: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
	std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// The single live host-silent guard (null sink / virtual-output switch). `Some`
/// while the owner set is non-empty; dropping it restores the previous default
/// device and tears down the null sink. Held in a static so it outlives the
/// per-stream closure and is released exactly when the last owner leaves.
#[cfg(not(windows))]
static UNIX_SILENT_GUARD: std::sync::LazyLock<
	Mutex<Option<pulsar_core::audio::HostSilentGuard>>,
> = std::sync::LazyLock::new(|| Mutex::new(None));

/// The capture **source** the host must record from while the redirect is live
/// (Linux: `pulsar_silent.monitor`; macOS: the virtual device name). `Some` while
/// `UNIX_SILENT_GUARD` is `Some`; read by `start_audio_and_mute` so the audio ffmpeg
/// taps the redirected monitor instead of the user's configured device. Cleared on
/// release.
#[cfg(not(windows))]
static UNIX_SILENT_SOURCE: std::sync::LazyLock<Mutex<Option<String>>> =
	std::sync::LazyLock::new(|| Mutex::new(None));

/// The pre-resolved AVFoundation `AudioInput` for the redirected virtual output device
/// on macOS. Populated once at arm-time (inside `set_unix_silent_request`) by running
/// `avfoundation_input_index` so the result is **cached** for the lifetime of the
/// redirect and every re-stream can look it up without spawning another blocking ffmpeg
/// process. `None` while the redirect is not armed or the index could not be resolved.
/// Cleared on disarm alongside `UNIX_SILENT_SOURCE`.
#[cfg(all(not(windows), not(target_os = "linux")))]
static UNIX_SILENT_AUDIO_INPUT: std::sync::LazyLock<
	Mutex<Option<pulsar_core::audio::AudioInput>>,
> = std::sync::LazyLock::new(|| Mutex::new(None));

/// When no sinkless device could be set up (no `pactl`/null-sink on Linux, no virtual
/// output on macOS) we fall back to the historic `set_host_muted` toggle for an
/// EXPLICIT host-silent request — but only applied from the per-stream path AFTER
/// capture is live (the post-mute latching caveat applies on every platform). Tracks
/// whether we took that fallback so the matching unmute runs on release.
#[cfg(not(windows))]
static UNIX_SILENT_MUTE_FALLBACK: std::sync::atomic::AtomicBool =
	std::sync::atomic::AtomicBool::new(false);

/// The capture source to record from for host audio: the redirected monitor while
/// host-silent is armed (Linux/macOS), else the user's configured input. The Linux
/// monitor source is wrapped as `AudioInput::Pulse(...)`; on macOS the host-silent
/// redirect switched the default OUTPUT to a virtual device (e.g. BlackHole), so the
/// program audio now leaves the speakers there — we must CAPTURE that same device,
/// not the user's configured mic, or the client would hear the mic while the program
/// audio is lost. The virtual device's AVFoundation **input** index was resolved once
/// at arm-time (via `avfoundation_input_index`) and cached in `UNIX_SILENT_AUDIO_INPUT`
/// by `set_unix_silent_request`; this function is a pure cache lookup on re-streams
/// with no subprocess invocation.
#[cfg(not(windows))]
fn effective_audio_input(
	configured: pulsar_core::audio::AudioInput,
) -> pulsar_core::audio::AudioInput {
	// Linux: the null sink's monitor is a PulseAudio source — capture it directly.
	#[cfg(target_os = "linux")]
	{
		match UNIX_SILENT_SOURCE.lock().unwrap().clone() {
			Some(src) => pulsar_core::audio::AudioInput::Pulse(src),
			None => configured,
		}
	}
	// macOS: read the pre-resolved AVFoundation input from the arm-time cache
	// (UNIX_SILENT_AUDIO_INPUT). No subprocess is run here — the blocking
	// `avfoundation_input_index` probe was executed once in `set_unix_silent_request`
	// when the redirect was first armed, so re-streams do a pure lock+clone lookup.
	#[cfg(not(target_os = "linux"))]
	{
		let armed = UNIX_SILENT_SOURCE.lock().unwrap().is_some();
		if !armed {
			return configured;
		}
		match UNIX_SILENT_AUDIO_INPUT.lock().unwrap().clone() {
			Some(input) => input,
			None => {
				// The arm-time probe couldn't resolve the index (warn already emitted
				// at arm-time in set_unix_silent_request). Fall back to the configured
				// input; the host's PROGRAM AUDIO is NOT being captured in this case.
				configured
			}
		}
	}
}

/// Resolve the AVFoundation **input** (audio capture) index for a device by name.
/// `ffmpeg -f avfoundation -list_devices true -i ""` prints the device list to STDERR
/// under an `AVFoundation audio devices:` header, one line per device as `[idx] Name`.
/// Returns the index of the first audio device whose name matches `target`
/// (case-insensitive substring, the same loose matching the redirect used to find the
/// virtual output), or `None` if ffmpeg fails or no audio device matches.
#[cfg(all(not(windows), not(target_os = "linux")))]
fn avfoundation_input_index(ffmpeg: &str, target: &str) -> Option<u32> {
	let out = std::process::Command::new(ffmpeg)
		.args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
		.output()
		.ok()?;
	let text = String::from_utf8_lossy(&out.stderr);
	let target_lc = target.to_lowercase();
	let mut in_audio = false;
	for line in text.lines() {
		let lower = line.to_ascii_lowercase();
		if lower.contains("avfoundation") && lower.contains("audio") && lower.contains("devices") {
			in_audio = true;
			continue;
		}
		if lower.contains("avfoundation") && lower.contains("video") && lower.contains("devices") {
			in_audio = false;
			continue;
		}
		if !in_audio {
			continue;
		}
		// A device line carries `[<idx>] <name>` (after ffmpeg's log prefix). Find the
		// LAST `[idx]` bracket pair on the line so we skip the `[AVFoundation indev @ …]`
		// log prefix and read the device index that immediately precedes the name.
		let Some(open) = line.rfind('[') else { continue };
		let rest = &line[open + 1..];
		let Some(close) = rest.find(']') else { continue };
		let idx: u32 = match rest[..close].trim().parse() {
			Ok(i) => i,
			Err(_) => continue,
		};
		let name = rest[close + 1..].trim();
		if !name.is_empty() && name.to_lowercase().contains(&target_lc) {
			return Some(idx);
		}
	}
	None
}

/// Record `sid`'s host-silent wish and arm/disarm the Linux/macOS capture-source
/// redirect when the owner set flips between empty and non-empty. Sunshine model:
/// route program audio to a sinkless device (null sink / virtual output) and capture
/// its monitor — never mute (muting silences the monitor too). If no sinkless device
/// is available, signal the caller (via the returned bool) to fall back to the
/// `set_host_muted` toggle, applied AFTER capture is live — never here.
///
/// On the empty→non-empty transition the redirect's capture source is stashed in
/// `UNIX_SILENT_SOURCE` so `start_audio_and_mute` taps it; cleared on restore.
///
/// Returns `true` when no sinkless device could be armed and an endpoint-mute fallback
/// is WANTED — the caller (`start_audio_and_mute`) must then apply `unix_mute_fallback(true)`
/// AFTER the capture is spawned, never here: a muted `@DEFAULT_SINK@` silences its
/// `.monitor` too (verified −91 dB), so opening the capture into the mute latches
/// silence. The `false`/restore transition undoes any prior mute fallback inline.
#[cfg(not(windows))]
#[must_use = "returns true when a post-capture mute fallback must be applied by the caller"]
fn set_unix_silent_request(
	sid: u64,
	want: bool,
	#[cfg_attr(target_os = "linux", allow(unused_variables))] ffmpeg: &str,
) -> bool {
	let mut owners = UNIX_SILENT_OWNERS.lock().unwrap();
	let was = !owners.is_empty();
	if want {
		owners.insert(sid);
	} else {
		owners.remove(&sid);
	}
	let now = !owners.is_empty();
	if was == now {
		return false;
	}
	tracing::info!(sid, want, owners = ?owners, "host-silent (unix sink-redirect) request");
	drop(owners);
	if now {
		// Going silent: arm the platform redirect (Linux null sink / macOS virtual out).
		match pulsar_core::audio::arm_host_silent() {
			Ok(Some(hs)) => {
				tracing::info!(source = %hs.source, "armed host-silent capture redirect (unix)");
				// macOS: resolve the AVFoundation input index for the virtual output device
				// NOW (at arm-time, once per redirect lifetime) and cache it in
				// UNIX_SILENT_AUDIO_INPUT. This avoids re-running the blocking
				// `ffmpeg -list_devices` probe on every re-stream inside the async session
				// loop — the cache is a pure lookup for all subsequent calls to
				// `effective_audio_input` until the redirect is disarmed.
				#[cfg(not(target_os = "linux"))]
				{
					let cached_input = match avfoundation_input_index(ffmpeg, &hs.source) {
						Some(idx) => {
							tracing::info!(
								device = %hs.source,
								idx,
								"host-silent (macOS): resolved AVFoundation input index (cached for redirect lifetime)"
							);
							Some(pulsar_core::audio::AudioInput::AvFoundation(idx))
						}
						None => {
							tracing::warn!(
								device = %hs.source,
								"host-silent (macOS): output redirected to virtual device but its \
								 AVFoundation input index could not be resolved at arm-time — \
								 will capture the configured input instead"
							);
							None
						}
					};
					*UNIX_SILENT_AUDIO_INPUT.lock().unwrap() = cached_input;
				}
				*UNIX_SILENT_SOURCE.lock().unwrap() = Some(hs.source);
				*UNIX_SILENT_GUARD.lock().unwrap() = Some(hs.guard);
				false
			}
			Ok(None) => {
				tracing::warn!(
					"no sinkless device for host-silent (pactl null-sink / macOS virtual output) — falling back to mute (deferred to post-capture)"
				);
				true
			}
			Err(e) => {
				tracing::warn!("arming host-silent redirect failed: {e} — falling back to mute (deferred to post-capture)");
				true
			}
		}
	} else {
		// Last owner left: restore. Drop the guard (restores the previous default
		// device + unloads the null sink), clear the source, and undo any mute fallback.
		*UNIX_SILENT_GUARD.lock().unwrap() = None;
		*UNIX_SILENT_SOURCE.lock().unwrap() = None;
		#[cfg(not(target_os = "linux"))]
		{
			*UNIX_SILENT_AUDIO_INPUT.lock().unwrap() = None;
		}
		unix_mute_fallback(false);
		false
	}
}

/// Mute fallback for the no-sinkless-device case (Linux/macOS). Mirrors the redirect
/// transition: `true` mutes (only reached AFTER capture is live — see callers),
/// `false` unmutes. Tracks whether the fallback is active so we don't unmute a host
/// the user muted elsewhere.
#[cfg(not(windows))]
fn unix_mute_fallback(mute: bool) {
	use std::sync::atomic::Ordering;
	if mute {
		if !UNIX_SILENT_MUTE_FALLBACK.swap(true, Ordering::SeqCst) {
			if let Err(e) = pulsar_core::audio::set_host_muted(true) {
				tracing::warn!("host mute fallback failed: {e}");
			}
		}
	} else if UNIX_SILENT_MUTE_FALLBACK.swap(false, Ordering::SeqCst) {
		if let Err(e) = pulsar_core::audio::set_host_muted(false) {
			tracing::warn!("host unmute (fallback) failed: {e}");
		}
	}
}

/// Teardown hook: drop `sid`'s host-silent request (restores the default render
/// endpoint when it was the last owner).
#[cfg(windows)]
pub(super) fn release_redirect(sid: u64) {
	// Layout is irrelevant on the release path (no format set, no redirect) — pass the
	// default; the owner-set transition only restores the saved default. The `false`
	// (release) transition never requests a mute fallback, so the return is moot here.
	let _ = set_redirect_request(sid, false, pulsar_core::audio::ChannelLayout::Stereo);
}
#[cfg(not(windows))]
pub(super) fn release_redirect(sid: u64) {
	// Drop this session's host-silent request; restores the host (default sink + null
	// sink teardown / output switch-back) when it was the last owner. The `false`
	// (release) transition never requests a mute fallback, so the return is moot here.
	// ffmpeg is not used on the disarm (want=false) path — pass "" as a placeholder.
	let _ = set_unix_silent_request(sid, false, "");
}

/// go_online re-run hook: a fresh serve loop has no live sessions, so clear any
/// stranded redirect owners and restore the host to a known-good default.
#[cfg(windows)]
pub(super) fn reset_redirect_all() {
	let mut owners = REDIRECT_OWNERS.lock().unwrap();
	if !owners.is_empty() {
		owners.clear();
		drop(owners);
		*REDIRECT_GUARD.lock().unwrap() = None;
		*REDIRECT_SINK_ID.lock().unwrap() = None;
		mute_fallback(false);
	}
}
#[cfg(not(windows))]
pub(super) fn reset_redirect_all() {
	let mut owners = UNIX_SILENT_OWNERS.lock().unwrap();
	if !owners.is_empty() {
		owners.clear();
		drop(owners);
		// Drop the guard (restores the default sink + unloads the null sink / switches
		// output back), clear the source, and undo any mute fallback.
		*UNIX_SILENT_GUARD.lock().unwrap() = None;
		*UNIX_SILENT_SOURCE.lock().unwrap() = None;
		#[cfg(not(target_os = "linux"))]
		{
			*UNIX_SILENT_AUDIO_INPUT.lock().unwrap() = None;
		}
		unix_mute_fallback(false);
	}
}

/// Startup crash-restore: a prior process that redirected the default render
/// endpoint to the virtual sink and died before its guard dropped left the host
/// pointing at the sinkless sink (real speakers silent). Restore the saved default
/// from the on-disk marker. No-op when there's no marker (clean previous exit).
pub(super) fn restore_stale_redirect() {
	pulsar_core::audio::restore_stale_redirect();
}

/// Start the host→client audio stream (Opus/RTP) and apply the requested host-silent
/// intent. Shared by the X11/Windows fall-through path and the Wayland branch so a
/// Wayland host streams audio + honors host-silent exactly like the X11 path.
/// Synchronous: it only spawns tracked children (`spawn_tracked`/`spawn_loopback_audio`)
/// and makes the (blocking) sink-redirect / mute call — it must run in the closure body,
/// not the async portal-capture task. Re-evaluated on every (re-)stream so live toggles
/// take effect.
///
/// **Host-silent is done by REDIRECTING the default render endpoint to the bundled
/// virtual sink (Sunshine's model), never by muting the captured endpoint** — muting it
/// latches silence into the WASAPI loopback on post-mute codecs. The redirect is applied
/// (on Windows) BEFORE the loopback capture is spawned, so the capture taps the new
/// (virtual-sink) default and stays live. On Linux/macOS, where capture is a separate
/// `.monitor`/system source (not the muted endpoint), the historic endpoint-mute path is
/// kept.
fn start_audio_and_mute(
	procs: &Arc<Mutex<Vec<Child>>>,
	ffmpeg: &str,
	app_h: &AppHandle,
	audio_dest: SocketAddr,
	req: &StreamReq,
	sid: u64,
) {
	// Audio: a second ffmpeg streams Opus/RTP to `audio_dest` — the client's audio
	// port directly (legacy), or the local media-over-session intake (the forwarder
	// ships it through the encrypted session). Transmit + host-silent are driven by
	// the session-menu toggles in the request.
	let acfg = pulsar_core::Config::load(config_path(app_h));
	// Negotiated channel layout = the SMALLER of what the client requested
	// (`req.audio_layout`) and what the host is configured to capture/encode
	// (`acfg.audio_settings().layout`): the host never claims more channels than it can
	// deliver, and the client never receives more than it asked for. Used to set the
	// virtual sink's device format (Windows host-silent), to pin/clamp the loopback
	// encode layout, and as the dshow/pulse fallback's layout.
	let host_layout = acfg.audio_settings().layout;
	let neg_layout = if req.audio_layout.channels() < host_layout.channels() {
		req.audio_layout
	} else {
		host_layout
	};
	// Host-silent (Windows): set the virtual sink's device format to the negotiated
	// channel count and redirect the default render endpoint to it BEFORE opening the
	// loopback capture below, so the capture taps the (never-muted) virtual sink at the
	// right width and the client gets real audio while the host stays silent. The
	// owner-set guard restores the original default when the last session releases it.
	// If no virtual sink is present it returns `true` to request the endpoint-mute
	// fallback — which we DEFER until after the loopback capture is spawned (muting at
	// capture-open latches silence into the WASAPI loopback on post-mute codecs).
	#[cfg(windows)]
	let want_mute_fallback = set_redirect_request(sid, req.mute_host, neg_layout);
	// Host-silent (Linux/macOS): arm the capture-source redirect BEFORE building the
	// capture command below, so the audio ffmpeg taps the redirected monitor (a null
	// sink on Linux) instead of the real default — the client gets audio while the
	// host's speakers stay silent, WITHOUT muting (muting silences the monitor too).
	// Re-evaluated every (re-)stream so a live toggle takes effect; the owner-set guard
	// restores the host when the last session releases it. If no sinkless device is
	// available it returns `true` to request the `set_host_muted` fallback — DEFERRED
	// to after the capture is spawned (a muted `@DEFAULT_SINK@` silences its `.monitor`
	// too, so opening the capture into the mute would latch silence).
	#[cfg(not(windows))]
	let want_mute_fallback = set_unix_silent_request(sid, req.mute_host, ffmpeg);
	if req.transmit_audio && audio_dest.port() > 0 {
		let dest = format!("rtp://{audio_dest}");
		// Windows: prefer WASAPI loopback — it taps whatever is playing on the
		// (possibly just-redirected) default output, so it works with NO
		// virtual-audio-capturer / Stereo Mix device installed. Falls back to the
		// dshow command if it can't start or a specific capture device name was configured.
		// When host-silent is active, pin the loopback to the redirected virtual sink's
		// id (re-asserted on every reinit — review HIGH-2) so a default-endpoint flip
		// can't make it tap the host's real speakers.
		#[cfg(windows)]
		let started_audio = {
			let pinned = REDIRECT_SINK_ID.lock().unwrap().clone();
			// When a host-silent sink redirect is armed (pinned is Some), ALWAYS try to capture
			// via WASAPI loopback pinned to the virtual sink — even if the user configured a
			// named dshow device (audio_loopback() is false with a named device). The redirect
			// has already moved program audio to the virtual sink, so a dshow tap on the user's
			// configured device would capture the wrong endpoint (the one that no longer receives
			// program audio) and the client would hear silence or the wrong source. Prefer the
			// loopback-of-redirected-sink path; only if that fails too, fall through to dshow.
			let pinned_active = pinned.is_some();
			(pinned_active || acfg.audio_loopback())
				&& spawn_loopback_audio(procs, ffmpeg, &dest, pinned, neg_layout)
		};
		#[cfg(not(windows))]
		let started_audio = false;
		if !started_audio {
			// Direct-capture (dshow / pulse `.monitor` / avfoundation) fallback. Honor the
			// NEGOTIATED channel layout so a 5.1/7.1 source is encoded as multistream Opus
			// (the loopback path above derives the layout from the live endpoint instead).
			// On Linux/macOS, when host-silent armed a redirect, capture the redirected
			// device (Linux: the null sink's `.monitor`; macOS: the virtual output device's
			// AVFoundation input) instead of the user's configured device, so the stream
			// carries program audio while the speakers are silent.
			// On Windows, if we reach here while a redirect is armed it means the virtual-sink
			// loopback failed; fall back to the mute path rather than capturing the now-wrong
			// dshow device (the endpoint-mute fallback below will silence the speakers, but at
			// least the dshow device will still carry program audio in that scenario).
			#[cfg(windows)]
			let input = acfg.audio_input();
			#[cfg(not(windows))]
			let input = effective_audio_input(acfg.audio_input());
			let (_, aargs) =
				pulsar_core::audio::audio_command_layout(&input, &dest, neg_layout);
			let _ = spawn_tracked(procs, ffmpeg, &aargs);
		}
	}
	// Apply the DEFERRED endpoint-mute fallback (no virtual sink / null sink present),
	// now that the loopback/monitor capture above has been spawned. Muting only here —
	// never at capture-open — keeps a muted endpoint from latching silence into an
	// already-opened WASAPI loopback (Windows) or a muted `@DEFAULT_SINK@` from
	// silencing the `.monitor` the capture taps (Linux). The matching unmute runs on
	// the owner-set release transition (`mute_fallback(false)`/`unix_mute_fallback(false)`).
	if want_mute_fallback {
		#[cfg(windows)]
		mute_fallback(true);
		#[cfg(not(windows))]
		unix_mute_fallback(true);
	}
}

/// Retransmit ring depth for media-over-session video (packets ≈ a few hundred ms
/// at 60 fps / 15 Mbit; ~1.5 MB worst case).
const NACK_RING: usize = 1024;

/// Sane lower bounds + defaults for the resolved stream geometry. The client
/// request and the host config can BOTH be 0 (request unset → fall to cfg; cfg
/// never configured → 0), and a 0 here is poison downstream: `-r 0` / GStreamer
/// `framerate=0/1` make ffmpeg/gst error out, and a 0×0 size reaches the native
/// NVENC/DXGI capture on Windows (where the `display_size` clamp is compiled out
/// and only ever shrinks) → an init crash or a dead stream. Clamp every resolved
/// value to a usable floor on ALL platforms before it flows into an encoder.
const MIN_FPS: u32 = 15;
const DEFAULT_FPS: u32 = 60;
const MIN_DIM: u32 = 320;
const DEFAULT_W: u32 = 1280;
const DEFAULT_H: u32 = 720;

/// Clamp a resolved fps to the usable floor: a 0 (both request and config unset)
/// becomes a sensible default rather than `-r 0`/`framerate=0/1`.
fn clamp_fps(fps: u32) -> u32 {
	if fps == 0 {
		DEFAULT_FPS
	} else {
		fps.max(MIN_FPS)
	}
}

/// Clamp resolved width/height to a usable floor: a 0 (both request and config
/// unset) becomes a default size; anything positive is floored to `MIN_DIM` so a
/// tiny/degenerate request can't crash the encoder.
fn clamp_dims(w: u32, h: u32) -> (u32, u32) {
	let w = if w == 0 { DEFAULT_W } else { w.max(MIN_DIM) };
	let h = if h == 0 { DEFAULT_H } else { h.max(MIN_DIM) };
	(w, h)
}

/// Media-over-session: bind the two LOOPBACK intake sockets the encoders will
/// target, and spawn the forwarder tasks that ship every received RTP datagram
/// through the encrypted session (`[tag][rtp…]` frames). The video forwarder keeps
/// a retransmit ring and serves NACK requests (registered into `nack_slot`).
/// Returns the (video, audio) intake ports, or `None` if binding failed (caller
/// falls back to the legacy direct flows).
fn spawn_media_forwarders(
	media_tx: &pulsar_core::SessionSender,
	nack_slot: &Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>>,
	fwd_slot: &Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
) -> Option<(u16, u16)> {
	use pulsar_core::service::media;
	let bind = || -> Option<(tokio::net::UdpSocket, u16)> {
		// BIG receive buffer (like pulsar-core node.rs): the encoder bursts a whole
		// IDR into this loopback intake at once, and the OS default (64 KiB on
		// Windows) overflows instantly at high fps — at 1080p120 NVENC virtually
		// every packet was dropped here (the client saw a 1 fps green stream).
		let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).ok()?;
		let _ = s.set_recv_buffer_size(4 * 1024 * 1024);
		let _ = s.set_send_buffer_size(4 * 1024 * 1024);
		s.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
			.ok()?;
		let s: std::net::UdpSocket = s.into();
		let port = s.local_addr().ok()?.port();
		s.set_nonblocking(true).ok()?;
		Some((tokio::net::UdpSocket::from_std(s).ok()?, port))
	};
	let (vsock, vport) = bind()?;
	let (asock, aport) = bind()?;

	let (nack_tx, mut nack_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u16>>();
	*nack_slot.lock().unwrap() = Some(nack_tx);

	let vtx = media_tx.clone();
	let vh = tokio::spawn(async move {
		// seq → datagram retransmit ring (linear scan is fine at this size).
		let mut ring: std::collections::VecDeque<(u16, Vec<u8>)> =
			std::collections::VecDeque::with_capacity(NACK_RING);
		let mut buf = vec![0u8; 2048];
		// 1 Hz throughput meter (intake pkts/bytes + session send failures) — the
		// "video reaches the client mangled/not at all" debugging needs to know
		// WHERE the chain loses data, and this stage was silent.
		let (mut m_pkts, mut m_bytes, mut m_gaps) = (0u64, 0u64, 0u64);
		let mut m_last_seq: Option<u16> = None;
		let mut m_at = std::time::Instant::now();
		loop {
			tokio::select! {
				r = vsock.recv(&mut buf) => {
					let Ok(n) = r else { break };
					let rtp = &buf[..n];
					if let Some(seq) = media::rtp_seq(rtp) {
						if let Some(last) = m_last_seq {
							let d = media::seq_forward(last, seq);
							if d > 1 && d < 0x8000 {
								m_gaps += (d - 1) as u64;
							}
						}
						m_last_seq = Some(seq);
						if ring.len() == NACK_RING {
							ring.pop_front();
						}
						ring.push_back((seq, rtp.to_vec()));
					}
					m_pkts += 1;
					m_bytes += n as u64;
					if m_at.elapsed().as_secs() >= 1 {
						tracing::info!(
							pkts = m_pkts,
							mbit = (m_bytes * 8) / 1_000_000,
							gaps_before_intake = m_gaps,
							"mos video forwarder throughput"
						);
						(m_pkts, m_bytes, m_gaps) = (0, 0, 0);
						m_at = std::time::Instant::now();
					}
					if vtx.send(&media::frame(media::TAG_VIDEO, rtp)).await.is_err() {
						break; // session gone
					}
				}
				q = nack_rx.recv() => {
					let Some(seqs) = q else { break };
					// The peer controls `seqs` length and content (a relayed session
					// datagram can carry thousands of u16s), so DON'T do a linear ring
					// scan per requested seq (O(seqs*ring) CPU amplification). De-dup the
					// request into a set capped at the ring depth (honoring more is
					// pointless — the ring holds at most `NACK_RING` packets), then scan
					// the ring ONCE and re-send the matches. This bounds the work per NACK
					// datagram to O(ring + seqs) regardless of how big `seqs` is.
					let mut want: std::collections::HashSet<u16> =
						std::collections::HashSet::with_capacity(NACK_RING);
					for seq in seqs {
						if want.len() >= NACK_RING {
							break;
						}
						want.insert(seq);
					}
					for (s, pkt) in ring.iter() {
						if want.contains(s) {
							let _ = vtx.send(&media::frame(media::TAG_VIDEO, pkt)).await;
						}
					}
				}
			}
		}
	});
	let atx = media_tx.clone();
	let ah = tokio::spawn(async move {
		let mut buf = vec![0u8; 2048];
		while let Ok(n) = asock.recv(&mut buf).await {
			if atx
				.send(&media::frame(media::TAG_AUDIO, &buf[..n]))
				.await
				.is_err()
			{
				break;
			}
		}
	});
	fwd_slot.lock().unwrap().extend([vh, ah]);
	Some((vport, aport))
}

/// If `req` differs from the previous native request ONLY in fields the LIVE `CaptureHandle` can
/// absorb without a full capture+encode+audio rebuild — the monitor (`display_idx`) and/or the
/// target bitrate (`bitrate_kbps`, to a concrete >0 value) — return `(display_changed,
/// bitrate_changed)`. Otherwise `None` (codec/encoder/res/fps/audio/etc. changed → full rebuild).
///
/// This is what keeps a session-menu MONITOR switch AND the client's adaptive-bitrate steps off
/// the slow path: a monitor change is an in-thread re-capture (`switch_output`), a bitrate change
/// is a live `nvEncReconfigureEncoder` (`set_bitrate`) — neither spawns a second encoder. Before
/// this, an adaptive bitrate step landing during a switch made the restream "not display-only" →
/// a full rebuild that STACKED with the switch (two encoders → the host's encode load doubled,
/// and the switch time went erratic 1-8 s). Comparing via the derived `PartialEq` (with both live
/// fields normalized) means any field added to `StreamReq` later defaults to the safe full path.
#[cfg(windows)]
fn live_change(prev: &StreamReq, req: &StreamReq) -> Option<(bool, bool)> {
	let display_changed = prev.display_idx != req.display_idx;
	let bitrate_changed = prev.bitrate_kbps != req.bitrate_kbps;
	if !display_changed && !bitrate_changed {
		return None;
	}
	// A STANDALONE bitrate change to "0" (auto → host default) must take the full path so the
	// host re-resolves the default. But a MONITOR switch must NEVER be demoted to a full restart
	// just because a bitrate step (incl. →0) coincided with it — that full restart is what
	// stacked a 2nd encoder + caused the erratic/stuck switches. When the display also changed we
	// handle the switch live and simply keep the current live bitrate (ignore the →0).
	if bitrate_changed && req.bitrate_kbps == 0 && !display_changed {
		return None;
	}
	let mut probe = prev.clone();
	probe.display_idx = req.display_idx;
	probe.bitrate_kbps = req.bitrate_kbps;
	if probe == *req {
		// Only signal a live bitrate apply for a concrete >0 target (set_bitrate(0) is meaningless);
		// a coincident →0 is dropped here and the rebuilt encoder keeps the current live bitrate.
		Some((display_changed, bitrate_changed && req.bitrate_kbps > 0))
	} else {
		None
	}
}

/// Build the per-session `on_stream` handler. A (re-)stream request restarts
/// capture: it kills any ffmpeg/native capture already running for this session,
/// then spawns the new encode (native DXGI+NVENC on Windows, else ffmpeg), pushes
/// the encode summary + display rotation to the client, starts the audio stream,
/// and applies the requested host-mute. On Wayland it routes through the portals.
#[allow(clippy::too_many_arguments)]
pub(super) fn make_on_stream(
	stream_cfg: Arc<Mutex<crate::state::StreamCfg>>,
	procs: Arc<Mutex<Vec<Child>>>,
	active: Arc<Mutex<std::collections::HashMap<String, crate::state::ConnInfo>>>,
	incoming: Arc<Mutex<std::collections::HashMap<String, (u64, oneshot::Sender<()>)>>>,
	host_out: Arc<
		Mutex<std::collections::HashMap<String, (u64, tokio::sync::mpsc::Sender<DataMsg>)>>,
	>,
	stop_tx: oneshot::Sender<()>,
	out_tx: tokio::sync::mpsc::Sender<DataMsg>,
	since_ms: u64,
	sid: u64,
	self_name: String,
	#[cfg(windows)] native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>>,
	// The monitor (`display_idx`) the current stream captures, published for the input path so an
	// absolute (webview) pointer lands on the streamed screen, not always the primary. Windows-only.
	#[cfg(windows)] cur_display: Arc<std::sync::atomic::AtomicU32>,
	// Live-confirmed output Arc from the active CaptureHandle (C4): the input closure reads from
	// this instead of cur_display on the native path so it sees the monitor the capture thread is
	// ACTUALLY streaming — not the optimistically-requested value written before the rebuild completes.
	#[cfg(windows)] native_out_arc: Arc<Mutex<Option<Arc<std::sync::atomic::AtomicU32>>>>,
	// Monotonic build-generation Arc from the active CaptureHandle (C8): the input closure
	// re-resolves display_rect/set_monitor when this advances — catching same-index resolution
	// changes that shift the virtual-desktop geometry without changing the monitor index.
	#[cfg(windows)] native_gen_arc: Arc<Mutex<Option<Arc<std::sync::atomic::AtomicU32>>>>,
	stats_out: tokio::sync::mpsc::Sender<DataMsg>,
	app_h: AppHandle,
	peer: String,
	media_tx: pulsar_core::SessionSender,
	nack_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>>,
	fwd_slot: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
	#[cfg(target_os = "linux")] restore_token: Arc<Mutex<Option<String>>>,
	#[cfg(target_os = "linux")] cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>>,
	#[cfg(target_os = "linux")] cap_gen: Arc<std::sync::atomic::AtomicU64>,
	// Records the latest StreamReq so the host-side display-mode watcher (non-Windows) can
	// re-issue it to restart capture at the new geometry. Windows self-heals in pulsar-capture.
	#[cfg(not(windows))] last_req_store: Arc<Mutex<Option<StreamReq>>>,
) -> impl FnMut(StreamReq, SocketAddr) + Send + 'static {
	let mut announced = false;
	let mut stop_tx = Some(stop_tx);
	// Cursor side-channel poller's liveness flag (Linux KMS path). Held across re-streams so a
	// re-stream can stop the prior poller before (maybe) starting a new one — avoids stacking
	// pollers when a session re-requests while still on the cursorless KMS capture.
	#[cfg(target_os = "linux")]
	let mut cursor_alive: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> = None;
	// The previous request that started a LIVE native (DXGI+NVENC) capture, kept so a re-stream
	// that only changed `display_idx` can switch the monitor IN PLACE (CaptureHandle::switch_output)
	// instead of tearing the whole pipeline down. `None` whenever the current stream isn't native
	// (ffmpeg fallback) — then a monitor change falls through to the normal restart.
	#[cfg(windows)]
	let mut last_native_req: Option<StreamReq> = None;
	move |req: StreamReq, addr: SocketAddr| {
		// Remember the latest request so the host-side display-mode watcher can re-issue it to
		// restart capture at the new geometry when the host's resolution/refresh changes.
		#[cfg(not(windows))]
		{
			*last_req_store.lock().unwrap() = Some(req.clone());
		}
		// FAST PATH — a live, in-session restream the running native capture can absorb without a
		// full rebuild: a MONITOR switch (`switch_output` → in-thread re-capture on the new GPU)
		// and/or an adaptive BITRATE step (`set_bitrate` → live nvEncReconfigureEncoder). Crucially
		// this keeps a bitrate step from forcing a full restart that STACKS a second encoder onto a
		// concurrent switch (the doubled encode load + erratic switch time). Audio, the media
		// forwarders, the ffmpeg probe, and the announce state are all left untouched.
		#[cfg(windows)]
		if let Some((disp, bitrate)) =
			last_native_req.as_ref().and_then(|prev| live_change(prev, &req))
		{
			let handled = {
				let slot = native_slot.lock().unwrap();
				match slot.as_ref() {
					Some(h) => {
						// Bitrate FIRST: an accompanying switch_output rebuilds the encoder, which
						// seeds itself from the live target — so the new target must be set before it.
						if bitrate && req.bitrate_kbps > 0 {
							h.set_bitrate(req.bitrate_kbps);
						}
						if disp {
							h.switch_output(req.display_idx);
						}
						// Publish the ACTUALLY-confirmed output for the input path (not the
						// optimistically-requested one). The capture thread writes current_output()
						// after each successful build including reverts, so this reflects the real
						// streaming monitor. Using req.display_idx here would point input at monitor
						// B even when the thread reverted to A after a failed switch-build (C1).
						let actual = h.current_output();
						cur_display.store(actual, std::sync::atomic::Ordering::Relaxed);
						// Keep last_native_req.display_idx pinned to the CONFIRMED output, not the
						// requested one. This means a retry of the same monitor B (after a revert
						// to A) still sees live_change(A, B) → display_changed=true → issues
						// switch_output(B) again, rather than being a silent live_change(B,B)=None
						// that falls to the slow path. Once the thread lands on B, current_output()
						// returns B and subsequent fast-path calls advance the baseline to B. (C1)
						let mut confirmed_req = req.clone();
						confirmed_req.display_idx = actual;
						last_native_req = Some(confirmed_req);
						true
					}
					None => false,
				}
			};
			if handled {
				tracing::info!(display_idx = req.display_idx, disp, bitrate, "native live restream (fast path)");
				return;
			}
			// The native handle is gone (a prior restream fell back to ffmpeg) — fall through to
			// the normal restart below.
		}
		let cfg = stream_cfg.lock().unwrap().clone();
		// A (re-)stream supersedes any prior cursor poller; the (maybe) new KMS branch below
		// starts a fresh one. Stopping here keeps it at most one per session.
		#[cfg(target_os = "linux")]
		if let Some(flag) = cursor_alive.take() {
			flag.store(false, std::sync::atomic::Ordering::SeqCst);
		}

		// Media destinations. Legacy: plain UDP straight to the client's ports.
		// Media-over-session (client opted in + this host advertised `mos`): the
		// encoders target LOCAL loopback intakes whose forwarders ship each datagram
		// through the encrypted session — ONE external socket total. A re-stream
		// replaces the forwarders (fresh retransmit ring / NACK channel).
		for h in fwd_slot.lock().unwrap().drain(..) {
			h.abort();
		}
		*nack_slot.lock().unwrap() = None;
		let lo = std::net::IpAddr::from([127, 0, 0, 1]);
		let (vdest, adest) = if req.media_over_session {
			match spawn_media_forwarders(&media_tx, &nack_slot, &fwd_slot) {
				Some((vp, ap)) => (
					SocketAddr::new(lo, vp),
					SocketAddr::new(lo, if req.audio_port > 0 { ap } else { 0 }),
				),
				None => (
					SocketAddr::new(addr.ip(), req.port),
					SocketAddr::new(addr.ip(), req.audio_port),
				),
			}
		} else {
			(
				SocketAddr::new(addr.ip(), req.port),
				SocketAddr::new(addr.ip(), req.audio_port),
			)
		};

		// First stream request reveals this connection's mode: register it and open the
		// dedicated connections window — brought forward for a Remote connection, opened
		// hidden for a Game one (so it doesn't disrupt / leak into the streamed game).
		// Done once (not on re-streams) so a live resolution change doesn't re-pop it.
		if !announced {
			announced = true;
			let mode = if req.game_mode {
				crate::state::ConnMode::Game
			} else {
				crate::state::ConnMode::Remote
			};
			// Registration happens HERE, not at accept (see go_online): a control
			// session that never streams must not clobber a live same-peer session's
			// entries — overwriting `incoming` drops the live stop_tx and instantly
			// tears its stream down. A second STREAMING session still takes over
			// (the overwritten stop_tx drop ends the old session cleanly).
			active.lock().unwrap().insert(
				peer.clone(),
				crate::state::ConnInfo {
					sid,
					since_ms,
					mode,
					view_only: false,
				},
			);
			if let Some(tx) = stop_tx.take() {
				incoming.lock().unwrap().insert(peer.clone(), (sid, tx));
			}
			host_out
				.lock()
				.unwrap()
				.insert(peer.clone(), (sid, out_tx.clone()));
			crate::connections::open_or_update(&app_h, !req.game_mode);
			// Identity push (host → client) is ALSO deferred to here: at accept time
			// the client is still inside query_stream_caps' wait loop, which discards
			// every non-StreamCaps frame — a PeerName/Avatar queued there never
			// reached its UI. By the first stream request the client's hold loop owns
			// the read side. Avatar resolve may decode a full-size wallpaper → too
			// slow for this closure, so it runs on a blocking thread; honors the
			// avatar_mode setting (anonymous = nothing sent); best-effort.
			let _ = stats_out.try_send(DataMsg::PeerName(self_name.clone()));
			let av_tx = stats_out.clone();
			let av_app = app_h.clone();
			let av_mode = tauri::Manager::state::<AppState>(&app_h)
				.config
				.lock()
				.unwrap()
				.avatar_mode
				.clone();
			tokio::task::spawn_blocking(move || {
				if let Some(png) = crate::avatar::avatar_png(&av_app, &av_mode) {
					let _ = av_tx.try_send(DataMsg::Avatar(png));
				}
			});
		}

		// Wayland: x11grab of rootless Xwayland is black, so capture the
		// real screen (and inject input) through the desktop portals.
		#[cfg(target_os = "linux")]
		if pulsar_core::capture::is_wayland() {
			// A (re-)stream restarts capture: kill any audio ffmpeg already running for
			// this session before spawning the new one, so a live re-stream (resolution/
			// codec/fps/audio-toggle change) doesn't stack audio encoders. (The video
			// capture is restarted separately via `cap_slot` in the async task below.)
			for mut child in procs.lock().unwrap().drain(..) {
				let _ = child.kill();
				let _ = child.wait();
			}
			let ip = vdest.ip().to_string();
			let port = vdest.port();
			// Client-requested bitrate wins; 0 falls back to the host config.
			let eff_bitrate = if req.bitrate_kbps > 0 {
				req.bitrate_kbps
			} else {
				cfg.bitrate_kbps
			};
			let req_fps = if req.fps > 0 { req.fps } else { cfg.fps };
			// Negotiate against the host panel (see the main path below for the rationale).
			let panel_hz = crate::util::host_panel_hz();
			let eff_fps = match panel_hz {
				Some(hz) => req_fps.min(hz),
				None => req_fps,
			};
			// Floor it: req_fps/cfg.fps can both be 0 → GStreamer framerate=0/1 errors.
			let eff_fps = clamp_fps(eff_fps);
			tracing::info!(
				req_fps,
				cfg_fps = cfg.fps,
				panel_hz = panel_hz.unwrap_or(0),
				eff_fps,
				"host stream fps resolved (wayland)"
			);
			let (bitrate, fps) = (eff_bitrate, eff_fps);
			// Pick the gst encoder family + codec from what THIS box validated
			// (mpp/vaapi/nv hardware first, x264 terminal) honoring the request.
			// Falls back to plain x264/H.264 when nothing probed (gst missing —
			// the spawn then fails visibly, same as the old hardcoded pipeline).
			let enc_pref = if req.encoder.is_empty() {
				cfg.encoder.clone()
			} else {
				req.encoder.clone()
			};
			let validated = crate::process::validated_gst_encoders();
			let (genc, gcodec) = crate::process::pick_gst(&validated, &enc_pref, &req.codec)
				.unwrap_or((pipeline::gst::GstEncoder::X264, pipeline::VCodec::H264));
			let fragment = pipeline::gst::encoder_fragment(genc, gcodec, bitrate, fps)
				.unwrap_or_else(|| {
					pipeline::gst::encoder_fragment(
						pipeline::gst::GstEncoder::X264,
						pipeline::VCodec::H264,
						bitrate,
						fps,
					)
					.expect("x264/h264 fragment always builds")
				});
			// Encode summary for the client's stats panel (the Wayland path never sent
			// one before, so the panel showed nothing).
			let fps_part = if eff_fps != req_fps {
				format!("{}fps ({} {})", fps, crate::i18n::t("stream.fpsRequested"), req_fps)
			} else {
				format!("{}fps", fps)
			};
			let _ = stats_out.try_send(DataMsg::Stats(format!(
				"{} · {} · — · {} · {} {}",
				vcodec_label(gcodec),
				genc.label(),
				fps_part,
				(bitrate as f32 / 1000.0).round() as u32,
				crate::i18n::t("stream.mbitTarget")
			)));
			let token = restore_token.lock().unwrap().clone();
			let restore_token = restore_token.clone();
			let cap_slot = cap_slot.clone();
			// This (re-)stream's generation: capture::start can sit in the portal
			// dialog for seconds; teardown and any newer re-stream bump the counter,
			// telling a stale task to discard its fresh capture (see go_online).
			let gen = cap_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
			let cap_gen = cap_gen.clone();
			// Clone for the spawned capture task; the param `app_h` stays owned so the
			// synchronous audio+host-mute below (and FnMut re-calls) can still use it.
			let app_h_task = app_h.clone();
			let peer = peer.clone();
			tokio::spawn(async move {
				use std::sync::atomic::Ordering;
				// Already superseded before we even started (overlapping re-streams):
				// leave the prior capture to the newer task.
				if cap_gen.load(Ordering::SeqCst) != gen {
					return;
				}
				// A (re-)stream restarts capture: stop the prior WaylandCapture FIRST —
				// kill its gst-launch AND close the portal session. WaylandCapture has no
				// Drop and the gst child has PR_SET_PDEATHSIG, so a bare overwrite would
				// neither kill it nor stop the duplicate RTP (and the compositor would keep
				// showing "screen is being shared"). Take the Option in its own statement so
				// the MutexGuard is dropped before the `.await` (std guards are !Send).
				let prev = cap_slot.lock().unwrap().take();
				if let Some(p) = prev {
					p.stop().await;
				}
				match pulsar_core::capture::start(&ip, port, &fragment, token).await {
					Ok((cap, new_token)) => {
						if let Some(t) = new_token {
							*restore_token.lock().unwrap() = Some(t);
						}
						// Store under the slot lock ONLY while still current: a session
						// that tore down (or re-streamed) during the portal dialog gets
						// its fresh capture STOPPED instead of stored into a dead slot
						// (orphaned "screen is being shared" + gst until app exit).
						let stale = {
							let mut slot = cap_slot.lock().unwrap();
							if cap_gen.load(Ordering::SeqCst) == gen {
								*slot = Some(cap);
								None
							} else {
								Some(cap)
							}
						};
						if let Some(c) = stale {
							c.stop().await;
							return;
						}
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: "Wayland · ekran + kontrol".into(),
							},
						);
					}
					Err(e) => {
						// LOG it (not just the UI event): a headless/CLI host has no webview for the
						// toast, so a Wayland capture failure (portal/pipewire/gst) was otherwise invisible.
						tracing::error!(err = %e, "wayland capture::start failed");
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: format!("Wayland yakalama başarısız: {e}"),
							},
						);
					}
				}
			});
			// Audio + host-mute on Wayland too (the early return above used to skip both,
			// so a Wayland host streamed silent video and never muted in game mode). Runs
			// synchronously in the closure body, like the X11 path; the Pulse `.monitor`
			// ffmpeg is tracked in `procs` and killed on (re-)stream/teardown.
			let ffmpeg = ffmpeg_bin(&app_h);
			start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
			return;
		}

		// A (re-)stream request restarts capture: kill any ffmpeg already
		// running for this session before spawning the new one (this is how
		// a live resolution change from the client takes effect).
		for mut child in procs.lock().unwrap().drain(..) {
			let _ = child.kill();
			let _ = child.wait();
		}
		// Same for the native capture thread, if the prior (re)stream used it.
		#[cfg(windows)]
		if let Some(h) = native_slot.lock().unwrap().take() {
			h.stop();
		}
		// Clear the live-confirmed output Arc so on_input falls back to cur_display while
		// no native handle is active (ffmpeg path or between teardown and a new handle). (C4)
		// Also clear the build-generation Arc (C8) for the same reason.
		#[cfg(windows)]
		{
			*native_out_arc.lock().unwrap() = None;
			*native_gen_arc.lock().unwrap() = None;
		}
		// Client-requested size/fps/bitrate win; 0 falls back to the host config.
		let eff_w = if req.width > 0 { req.width } else { cfg.width };
		let eff_h = if req.height > 0 {
			req.height
		} else {
			cfg.height
		};
		let req_fps = if req.fps > 0 { req.fps } else { cfg.fps };
		// Negotiate against the host panel: encoding above the host's own refresh just
		// produces duplicate frames at extra cost (the user-visible "120 seçtim,
		// değişmiyor" was a 120-req on a slower panel), so clamp to it when known.
		let panel_hz = crate::util::host_panel_hz();
		let eff_fps = match panel_hz {
			Some(hz) => req_fps.min(hz),
			None => req_fps,
		};
		// Diagnostic ceiling (`PULSAR_MAX_FPS`): bisect client-decoder fps limits live.
		let eff_fps = match std::env::var("PULSAR_MAX_FPS")
			.ok()
			.and_then(|v| v.parse::<u32>().ok())
		{
			Some(m) => eff_fps.min(m),
			None => eff_fps,
		};
		// Floor it AFTER every adjustment: req_fps, cfg.fps, the panel clamp and the
		// diagnostic ceiling can all drive this to 0 → `-r 0` / native NVENC with a
		// 0 fps. A 0 here is poison on every path, so clamp last.
		let eff_fps = clamp_fps(eff_fps);
		tracing::info!(
			req_fps,
			cfg_fps = cfg.fps,
			panel_hz = panel_hz.unwrap_or(0),
			eff_fps,
			"host stream fps resolved"
		);
		let eff_bitrate = if req.bitrate_kbps > 0 {
			req.bitrate_kbps
		} else {
			cfg.bitrate_kbps
		};
		// Clamp the capture resolution to the host's actual screen. ffmpeg's x11grab/gdigrab grab a
		// REGION of size `-video_size`, which must be ≤ the screen or ffmpeg dies ("Capture area …
		// outside the screen size") and streams NO video — hit when a 1440p-configured stream
		// targets a 1080p host (e.g. an Orange Pi acting as host). Windows captures via the native
		// DXGI path (scales internally), so this only guards the ffmpeg capture path.
		#[cfg(not(windows))]
		let (eff_w, eff_h) = match crate::util::display_size(&cfg.display) {
			Some((sw, sh)) if eff_w > sw || eff_h > sh => (sw, sh),
			_ => (eff_w, eff_h),
		};
		// Floor the resolved size on ALL platforms: req+cfg can both be 0 (0×0 reaches
		// native NVENC/DXGI on Windows, where the clamp above is compiled out and only
		// shrinks anyway) → encoder init crash / dead stream. Apply after the Unix
		// screen-clamp so a legitimately-clamped size is preserved, only floored.
		let (eff_w, eff_h) = clamp_dims(eff_w, eff_h);
		let ffmpeg = ffmpeg_bin(&app_h);
		// The viewer picks the encoder live from the session menu (sent in the
		// stream request); an empty request falls back to the host's own setting.
		// `resolve` still degrades gracefully if this host lacks that encoder.
		let enc_pref = if req.encoder.is_empty() {
			cfg.encoder.clone()
		} else {
			req.encoder.clone()
		};
		// Probe the bundled ffmpeg ONCE: which backends exist, and (per backend) which
		// codecs. `resolve` degrades the encoder; `resolve_codec` then degrades the codec
		// to what that encoder can actually emit (requested → H.264 → first available), so a
		// HEVC/AV1 request on a build lacking it falls back instead of failing.
		let enc_text = crate::process::encoders_text(&ffmpeg);
		let encoder = pipeline::resolve(encoder_from_str(&enc_pref), &pipeline::detect(&enc_text));
		// Off-Windows, ffmpeg is the ONLY encode path, so an encoder ffmpeg merely *lists* (a generic
		// build lists h264_nvenc even with no NVIDIA GPU) but can't initialize here must be dropped,
		// not used — else it fails at spawn and sends no video (the Orange-Pi-as-host case:
		// h264_nvenc → "Cannot load libcuda.so.1"). Validate + degrade to a working encoder
		// (ultimately libx264). Windows keeps its native-NVENC path + hybrid guard (compiled out here).
		#[cfg(not(windows))]
		let encoder = crate::process::resolve_encoder_validated(
			&ffmpeg,
			encoder,
			&enc_text,
			&cfg.vaapi_device,
		);
		// Validate the codec with a real one-frame encode probe (cached) — catches "listed
		// but the GPU/driver can't init it" (e.g. av1_nvenc on Ampere), degrading to a codec
		// that actually works rather than producing a dead stream.
		let codec = crate::process::resolve_codec_validated(
			&ffmpeg,
			encoder,
			codec_from_str(&req.codec),
			&cfg.vaapi_device,
		);
		// Clamp to what the CLIENT can decode (its startup probe travels in the
		// request): never stream a codec the other side can't show. H.264 software
		// decode exists almost everywhere, so it is the usual meeting point — but the
		// client occasionally prunes h264 out of its set entirely (e.g. only HEVC
		// validated), so prefer h264 only when it is actually listed, otherwise fall
		// back to the FIRST codec the client really advertised rather than blindly
		// streaming a codec it can't show.
		let codec = if !req.decode_codecs.is_empty()
			&& !req.decode_codecs.iter().any(|c| codec_from_str(c) == codec)
		{
			if req.decode_codecs.iter().any(|c| codec_from_str(c) == pipeline::VCodec::H264) {
				pipeline::VCodec::H264
			} else {
				codec_from_str(&req.decode_codecs[0])
			}
		} else {
			codec
		};
		// Linux X11: when ffmpeg has no working HW encoder (terminal Software — the
		// Orange Pi 5 case: rkmpp encode exists only as GStreamer mpph26Xenc) or the
		// client explicitly asked for "rkmpp", route the encode through a gst
		// `ximagesrc → mpp/vaapi/nv → RTP` pipeline instead of ffmpeg+libx264.
		// gst x264 is NOT used here — ffmpeg's own libx264 path has more knobs.
		#[cfg(target_os = "linux")]
		{
			// Client-selected host monitor → ximagesrc capture region (None = whole root /
			// single-monitor host). The idx indexes process::linux_displays() (xrandr order),
			// the same list StreamCaps advertised.
			let region: Option<(i32, i32, u32, u32)> = {
				let displays = crate::process::linux_displays();
				displays
					.get(req.display_idx as usize)
					.filter(|_| displays.len() > 1)
					.map(|(_, x, y, w, h, _)| (*x, *y, *w, *h))
			};
			let want_gst = enc_pref == "rkmpp" || encoder == HwEncoder::Software;
			if want_gst {
				let hw: Vec<_> = crate::process::validated_gst_encoders()
					.into_iter()
					.filter(|(e, _)| *e != pipeline::gst::GstEncoder::X264)
					.collect();
				tracing::info!(
					families = hw.len(),
					%enc_pref,
					req_codec = %req.codec,
					"x11 gst hw-encode candidates"
				);
				if let Some((genc, gcodec)) = crate::process::pick_gst(&hw, &enc_pref, &req.codec) {
					if let Some(fragment) =
						pipeline::gst::encoder_fragment(genc, gcodec, eff_bitrate, eff_fps)
					{
						// Encode-pace meter: an identity right AFTER the encoder (the
						// fragment's first ` ! ` joins encoder→parse; props carry no `!`).
						let metered =
							fragment.replacen(" ! ", " ! identity name=encpace silent=false ! ", 1);
						// Zero-copy KMS capture (scanout DMABuf → MPP): game mode only — the
						// X HW cursor is NOT in the KMS frame (own DRM plane), fine in-game,
						// unusable for remote desktop. Probed, never assumed.
						// `PULSAR_KMS`: 0 = never (bisect back to ximagesrc), 1 = force even
						// for remote sessions (testing / hosts running a software cursor),
						// unset = game-mode-gated default — EXTENDED so a remote session can
						// also use KMS when the client draws the cursor itself
						// (`cursor_external`): the missing hardware cursor is then supplied
						// out-of-band (see `cursor.rs`), which was the only thing that pinned
						// KMS to game mode. The cursor side-channel + PULSAR_KMS=1 stay the
						// safety net (side-channel down / explicit force → old behavior).
						let kms_mode = match std::env::var("PULSAR_KMS").as_deref() {
							Ok("0") => false,
							Ok("1") => true,
							_ => req.game_mode || req.cursor_external,
						};
						let kms = kms_mode
							&& genc == pipeline::gst::GstEncoder::Mpp
							&& crate::process::kms_encode_ok(genc, gcodec);
						// Cursor side-channel: the KMS scan-out frame has NO hardware cursor
						// (own DRM plane), so when the client asked to draw it itself
						// (`cursor_external`) start the X pointer poller that streams the cursor
						// position+shape out-of-band. The poller stops when `stats_out` closes
						// (session teardown) — a re-stream that drops KMS simply doesn't start a
						// new one and the old one self-stops with the prior session.
						if kms && req.cursor_external {
							let flag =
								std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
							cursor_alive = Some(flag.clone());
							crate::host::cursor::spawn(stats_out.clone(), flag);
						}
						// MPP eats BGRx via the RGA blitter — skip the CPU convert.
						let direct_bgrx = genc == pipeline::gst::GstEncoder::Mpp;
						let pipeline_str = if kms {
							pipeline::gst::kms_pipeline(
								eff_fps,
								&metered,
								&vdest.ip().to_string(),
								vdest.port(),
							)
						} else {
							pipeline::gst::x11_pipeline(
								&cfg.display,
								eff_fps,
								&metered,
								&vdest.ip().to_string(),
								vdest.port(),
								direct_bgrx,
								region,
							)
						};
						let fps_part = if eff_fps != req_fps {
							format!(
								"{}fps ({} {})",
								eff_fps,
								crate::i18n::t("stream.fpsRequested"),
								req_fps
							)
						} else {
							format!("{}fps", eff_fps)
						};
						// Resolution part: surface a host-clamped request ("1080p (istenen
						// 1440p)") so the overlay res "Aktif:" line reads as a negotiation —
						// same pattern as `fps_part`. eff_h was clamped to the host screen above.
						let res_part = if req.height > 0 && req.height != eff_h {
							format!(
								"{}p ({} {}p)",
								eff_h,
								crate::i18n::t("stream.fpsRequested"),
								req.height
							)
						} else {
							format!("{}p", eff_h)
						};
						let base_label = format!(
							"{} · {}{} · {} · {} · {} {}",
							vcodec_label(gcodec),
							genc.label(),
							if kms { " (KMS)" } else { "" },
							res_part,
							fps_part,
							(eff_bitrate as f32 / 1000.0).round() as u32,
							crate::i18n::t("stream.mbitTarget")
						);
						let stats_enc = stats_out.clone();
						let label_enc = base_label.clone();
						// `PULSAR_GST_METER=0` bypasses the pace meter (plain -q spawn) —
						// the regression-bisect knob for the encode-ms instrumentation.
						let started = if std::env::var("PULSAR_GST_METER").as_deref() == Ok("0") {
							spawn_gst_tracked(&procs, &pipeline_str).is_ok()
						} else {
							match spawn_gst_paced(&procs, &pipeline_str, move |ms| {
								let _ = stats_enc.try_send(DataMsg::Stats(format!(
									"{label_enc} · {ms:.1} {}",
									crate::i18n::t("stream.msEncode")
								)));
							}) {
								Ok((pid, ticked)) => {
									if !kms && direct_bgrx {
										// RGA >4G watchdog. The BGRx-direct path hands
										// ximagesrc's malloc'd pool straight to the RGA
										// blitter, and the RGA2 core can't map memory
										// above 4 GiB phys (dmesg: "RGA_MMU unsupported
										// Memory larger than 4G!"). The pool is allocated
										// once per spawn, so it's allocation luck: same
										// pipeline sometimes runs at 76 fps, sometimes
										// never emits a single frame (black screen). No
										// first frame within 2.6 s → kill + respawn with
										// the CPU-convert (I420) variant, which feeds the
										// encoder RGA-safe buffers.
										let fb_pipeline = pipeline::gst::x11_pipeline(
											&cfg.display,
											eff_fps,
											&metered,
											&vdest.ip().to_string(),
											vdest.port(),
											false,
											region,
										);
										let procs_fb = procs.clone();
										let stats_fb = stats_out.clone();
										let label_fb = base_label.clone();
										std::thread::spawn(move || {
											std::thread::sleep(std::time::Duration::from_millis(
												2600,
											));
											if ticked.load(std::sync::atomic::Ordering::Relaxed) {
												return;
											}
											// The whole kill-and-respawn runs under the procs lock,
											// keyed to the ORIGINAL child: teardown and a re-stream
											// drain `procs` under this same lock, so once our child
											// is gone from it the session moved on — a fallback
											// spawned past that point would run as an untracked
											// orphan (or duplicate the new encode's RTP).
											let mut g = procs_fb.lock().unwrap();
											let Some(idx) = g.iter().position(|c| c.id() == pid)
											else {
												return;
											};
											tracing::warn!(
												pid,
												"gst BGRx-direct produced no frames in 2.6 s (RGA >4G map failure) — respawning with CPU convert"
											);
											let mut old = g.remove(idx);
											let _ = old.kill();
											let _ = old.wait();
											let _ = spawn_gst_paced_locked(
												&mut g,
												&fb_pipeline,
												move |ms| {
													let _ = stats_fb.try_send(DataMsg::Stats(
														format!("{label_fb} · {ms:.1} {}", crate::i18n::t("stream.msEncode")),
													));
												},
											);
										});
									}
									true
								}
								Err(_) => false,
							}
						};
						tracing::info!(encoder = ?genc, codec = ?gcodec, started, "x11 gst encode spawned");
						let _ = stats_out.try_send(DataMsg::Stats(base_label));
						let _ = stats_out.try_send(DataMsg::DisplayRotation(display_rotation()));
						start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
						let _ = app_h.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer: peer.clone(),
								detail: format!("{} · {}p", genc.label(), eff_h)
									+ if started { "" } else { crate::i18n::t("host.gstFailed") },
							},
						);
						return;
					}
				}
			}
		}
		let capture = capture_from_str(&cfg.capture);
		// NVENC + ddagrab: probe ONCE whether the fully zero-copy
		// D3D11→CUDA→NVENC path works (display on the NVIDIA GPU). On a
		// hybrid box it doesn't, and we use the GPU-scale path instead.
		let gpu_zerocopy = if encoder == HwEncoder::Nvenc && capture == CaptureMethod::Ddagrab {
			let ff = ffmpeg.clone();
			*DDAGRAB_ZEROCOPY.get_or_init(|| probe_ddagrab_zerocopy(&ff))
		} else {
			false
		};
		let plan = StreamPlan {
			encoder,
			codec,
			width: eff_w,
			height: eff_h,
			fps: eff_fps,
			bitrate_kbps: eff_bitrate,
			capture,
			display: cfg.display.clone(),
			vaapi_device: cfg.vaapi_device.clone(),
			dest: format!("rtp://{vdest}"),
			// Quality bias: explicit client preference wins; `Balanced` defers to
			// game_mode (no regression — game mode → lowest latency, remote → quality).
			low_latency: match req.quality {
				QualityPref::Quality => false,
				QualityPref::Latency => true,
				QualityPref::Balanced => req.game_mode,
			},
			gpu_zerocopy,
			hdr: req.hdr,
			yuv444: req.yuv444,
		};
		// NATIVE WINDOWS path: DXGI Desktop Duplication + NVENC SDK → RTP
		// (Sunshine-technique, steady client-fps). Used for NVENC on Windows
		// unless PULSAR_FFMPEG_CAPTURE=1. Init happens inside the capture thread
		// and is reported back synchronously — Ok ⇒ streaming started; Err ⇒ fall
		// back to ffmpeg with zero behaviour change.
		#[cfg(windows)]
		let native_started = if encoder == HwEncoder::Nvenc
			&& capture == CaptureMethod::Ddagrab
			&& !req.hdr
			&& !req.yuv444
			&& std::env::var_os("PULSAR_FFMPEG_CAPTURE").is_none()
		{
			let ncodec = match codec {
				pipeline::VCodec::H264 => pulsar_capture::Codec::H264,
				pipeline::VCodec::H265 => pulsar_capture::Codec::H265,
				pipeline::VCodec::Av1 => pulsar_capture::Codec::Av1,
			};
			// Clamp the client-requested monitor index to the current attached-output count.
			// A stale client picker (built from an earlier QueryStreamCaps) may send an index
			// that no longer exists after a hot-unplug, and find_output's silent fallback to
			// output 0 would leave cur_out_t / current_output() reporting the stale index while
			// actually streaming output 0 — desyncing video and absolute-pointer input (C8).
			// Clamping here is a cheap early guard; find_output's own actual_idx tracking in
			// device.rs is the definitive fix for the case where an unplug happens mid-session.
			let display_count = pulsar_capture::list_displays().len() as u32;
			let clamped_display_idx = if display_count > 0 {
				req.display_idx.min(display_count - 1)
			} else {
				0
			};
			match pulsar_capture::start_capture_encode(pulsar_capture::CaptureConfig {
				width: eff_w,
				height: eff_h,
				fps: eff_fps,
				bitrate_kbps: eff_bitrate,
				dest: format!("rtp://{vdest}"),
				codec: ncodec,
				// Client-selected host monitor (session menu); DXGI maps this index 1:1
				// to the attached-output list `StreamCaps::displays` was built from.
				// Clamped above against the live display count so an out-of-range index
				// (stale client picker after a hot-unplug) is corrected before it reaches
				// find_output and causes a silent fallback + input-mapping desync (C8).
				output_idx: clamped_display_idx,
				low_latency: plan.low_latency,
				draw_mouse: true,
			}) {
				Ok(h) => {
					// Publish the confirmed capture monitor for the input path. Use
					// h.current_output() rather than clamped_display_idx: the thread writes
					// current_output() from the actual_idx returned by find_output (which may
					// differ from clamped_display_idx if a second unplug happened between our
					// clamp above and the thread's first build). This is the confirmed real idx.
					cur_display.store(h.current_output(), std::sync::atomic::Ordering::Relaxed);
					// Expose the handle's current_output Arc to on_input so the input closure can
					// track the LIVE confirmed output without locking native_slot per event (C4).
					// The atom is written by the capture thread after every build (including reverts),
					// so on_input always reads the monitor that is actually being streamed — not the
					// optimistically-requested value that cur_display carries before a rebuild lands.
					*native_out_arc.lock().unwrap() = Some(h.current_output_arc());
					// Expose the build-generation Arc so on_input can detect same-index resolution
					// changes and re-resolve display_rect/set_monitor even when the index is unchanged (C8).
					*native_gen_arc.lock().unwrap() = Some(h.build_gen_arc());
					*native_slot.lock().unwrap() = Some(h);
					true
				}
				Err(_) => false,
			}
		} else {
			false
		};
		#[cfg(not(windows))]
		let native_started = false;

		// Remember this request for the in-place monitor-switch fast path — but ONLY when the
		// native capture actually started. If we fell back to ffmpeg there's no CaptureHandle to
		// switch_output on, so a later monitor change must take the full restart path.
		#[cfg(windows)]
		{
			last_native_req = if native_started { Some(req.clone()) } else { None };
		}
		// C19: When the native path was NOT taken (native_started=false) the ffmpeg fallback
		// always captures the PRIMARY monitor (gdigrab/ddagrab via StreamPlan has no output_idx).
		// native_out_arc was already cleared above so on_input falls back to cur_display — but
		// cur_display still holds the previously-confirmed native monitor index (e.g. monitor B
		// from a prior native session), causing absolute-pointer events to be mapped onto the wrong
		// screen for the rest of the session. Reset cur_display to 0 (primary) so the input
		// mapping matches what is actually being streamed.
		#[cfg(windows)]
		if !native_started {
			cur_display.store(0, std::sync::atomic::Ordering::Relaxed);
		}

		// Encode summary (codec · encoder · res · fps · bitrate target) — the base the
		// client's stats panel shows; the ffmpeg path appends a live "… ms kodlama"
		// part from the encode-pace meter below.
		// Reflect the RESOLVED codec (after `resolve_codec` fallback), not the request —
		// the client uses this to pick its decoder, so it must match what we actually send.
		// The fps part is the overlay's FPS-combo "Aktif:" truth line (overlay.rs act(3)).
		// When the host clamped the request to its panel, surface BOTH so "120 seçtim,
		// değişmiyor" reads as a negotiation, not a bug: "60fps (istenen 120)".
		let fps_part = if eff_fps != req_fps {
			format!("{}fps ({} {})", eff_fps, crate::i18n::t("stream.fpsRequested"), req_fps)
		} else {
			format!("{}fps", eff_fps)
		};
		// Resolution part: when the client asked for a height the host couldn't honor
		// (clamped to the host screen / config), surface BOTH so the overlay's res
		// "Aktif:" line reads as a negotiation ("1080p (istenen 1440p)"), not a bug —
		// same pattern as `fps_part` above.
		let res_part = if req.height > 0 && req.height != eff_h {
			format!("{}p ({} {}p)", eff_h, crate::i18n::t("stream.fpsRequested"), req.height)
		} else {
			format!("{}p", eff_h)
		};
		let base_label = format!(
			"{} · {} · {} · {} · {} {}",
			vcodec_label(codec),
			encoder.label(),
			res_part,
			fps_part,
			(eff_bitrate as f32 / 1000.0).round() as u32,
			crate::i18n::t("stream.mbitTarget")
		);
		// encode_command always yields ("ffmpeg", args); run the bundled
		// ffmpeg binary directly rather than relying on a system ffmpeg.
		let started = if native_started {
			true
		} else {
			let (_, args) = pipeline::encode_command(&plan);
			// Encode-pace meter: ffmpeg `-progress` ticks → per-frame wall ms → re-push
			// the Stats label with a live "kodlama" part (~2 Hz, tiny control message).
			let stats_enc = stats_out.clone();
			let label_enc = base_label.clone();
			crate::process::spawn_tracked_enc_paced(&procs, &ffmpeg, &args, move |ms| {
				let _ =
					stats_enc.try_send(DataMsg::Stats(format!("{label_enc} · {ms:.1} {}", crate::i18n::t("stream.msEncode"))));
			})
			.is_ok()
		};
		let _ = stats_out.try_send(DataMsg::Stats(base_label));
		// Tell the client our display orientation so it can render the video upright even if
		// this host's screen is rotated (e.g. a tent-mode laptop). The NATIVE capture path
		// (pulsar-capture) already BAKES the rotation into the encoded stream via the
		// VideoProcessor Blt, so we report 0 then (avoids double-rotation); the ffmpeg fallback
		// path does NOT rotate, so it reports the real rotation for the client to apply.
		let reported_rotation = if native_started {
			0
		} else {
			display_rotation()
		};
		let _ = stats_out.try_send(DataMsg::DisplayRotation(reported_rotation));
		start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
		let _ = app_h.emit(
			"session",
			SessionEvent {
				kind: "stream".into(),
				peer: peer.clone(),
				detail: format!("{} · {}p", encoder.label(), eff_h)
					+ if started { "" } else { crate::i18n::t("host.ffmpegFailed") },
			},
		);
	}
}

/// Short human label for a codec (stats panel strings).
fn vcodec_label(c: pipeline::VCodec) -> &'static str {
	match c {
		pipeline::VCodec::H265 => "H.265",
		pipeline::VCodec::Av1 => "AV1",
		pipeline::VCodec::H264 => "H.264",
	}
}

/// Like `spawn_gst_tracked`, but with the ENCODE-PACE meter: the pipeline carries an
/// `identity name=encpace silent=false` right after the encoder, and `-v` makes
/// gst-launch print one `last-message = chain …` line per encoded frame. The parser
/// thread times the line gaps (Δwall per frame ≈ encode pace, same semantics as the
/// ffmpeg `-progress` meter) and calls `on_ms` about once a second — so a gst host
/// (Pi MPP) finally feeds the client's "Kodlama ms" tile instead of "—".
/// Returns the child's pid + a flag that flips on the FIRST encoded frame — the
/// caller's no-output watchdog reads it (see the BGRx-direct RGA fallback).
#[cfg(target_os = "linux")]
fn spawn_gst_paced(
	procs: &Arc<Mutex<Vec<Child>>>,
	pipeline: &str,
	on_ms: impl Fn(f32) + Send + 'static,
) -> Result<(u32, Arc<std::sync::atomic::AtomicBool>), String> {
	let mut g = procs.lock().unwrap();
	spawn_gst_paced_locked(&mut g, pipeline, on_ms)
}

/// `spawn_gst_paced` with the `procs` lock already held: the BGRx-direct RGA
/// watchdog needs its find-dead → kill → respawn → push sequence atomic against
/// the teardown/re-stream drains (which run under this same lock).
#[cfg(target_os = "linux")]
fn spawn_gst_paced_locked(
	procs: &mut Vec<Child>,
	pipeline: &str,
	on_ms: impl Fn(f32) + Send + 'static,
) -> Result<(u32, Arc<std::sync::atomic::AtomicBool>), String> {
	use std::os::unix::process::CommandExt;
	let mut cmd = std::process::Command::new(crate::process::gst_launch_bin());
	cmd.arg("-v").args(pipeline.split_whitespace());
	cmd.stdout(std::process::Stdio::piped());
	// stderr to a per-host scratch file (truncated each spawn): gst dying at start
	// is otherwise invisible — this is the only channel that says WHY.
	let errlog = std::fs::File::create(std::env::temp_dir().join("pulsar-gst-stderr.log")).ok();
	match errlog {
		Some(f) => {
			cmd.stderr(f);
		}
		None => {
			cmd.stderr(std::process::Stdio::null());
		}
	}
	unsafe {
		cmd.pre_exec(|| {
			// SAFETY: async-signal-safe libc calls only.
			libc::prctl(
				libc::PR_SET_PDEATHSIG,
				libc::SIGKILL as libc::c_ulong,
				0,
				0,
				0,
			);
			if libc::getppid() == 1 {
				libc::_exit(0);
			}
			Ok(())
		});
	}
	let ticked = Arc::new(std::sync::atomic::AtomicBool::new(false));
	match cmd.spawn() {
		Ok(mut child) => {
			if let Some(mut stdout) = child.stdout.take() {
				let ticked = ticked.clone();
				std::thread::spawn(move || {
					use std::io::Read;
					// BYTE-safe line scan — `BufRead::lines()` returns Err on any
					// non-UTF-8 byte in the `-v` dump and a `break` there DROPS the
					// pipe: gst-launch then dies of SIGPIPE on its next print (the
					// "video never starts" regression). This reader only ends at EOF,
					// i.e. when gst itself exited.
					let mut buf = [0u8; 8192];
					let mut line: Vec<u8> = Vec::with_capacity(256);
					let mut last: Option<std::time::Instant> = None;
					let mut ema_ms: f32 = 0.0;
					let mut last_push = std::time::Instant::now();
					loop {
						let n = match stdout.read(&mut buf) {
							Ok(0) | Err(_) => break,
							Ok(n) => n,
						};
						for &b in &buf[..n] {
							if b != b'\n' {
								// Cap pathological unterminated lines; the marker fits well within.
								if line.len() < 4096 {
									line.push(b);
								}
								continue;
							}
							let is_tick = {
								let s = String::from_utf8_lossy(&line);
								s.contains("encpace") && s.contains("last-message")
							};
							line.clear();
							if !is_tick {
								continue;
							}
							ticked.store(true, std::sync::atomic::Ordering::Relaxed);
							let now = std::time::Instant::now();
							if let Some(t0) = last {
								let ms = now.duration_since(t0).as_secs_f32() * 1000.0;
								ema_ms = if ema_ms == 0.0 {
									ms
								} else {
									ema_ms * 0.9 + ms * 0.1
								};
								if last_push.elapsed().as_millis() >= 1000 {
									on_ms(ema_ms);
									last_push = now;
								}
							}
							last = Some(now);
						}
					}
				});
			}
			let pid = child.id();
			procs.push(child);
			Ok((pid, ticked))
		}
		Err(e) => Err(format!("gst-launch-1.0 {}: {e}", crate::i18n::t("err.spawn"))),
	}
}

/// Spawn a `gst-launch-1.0` encode pipeline tracked in `procs` (killed on
/// (re-)stream/teardown like the ffmpeg children). `PR_SET_PDEATHSIG` mirrors the
/// Wayland capture spawn so an app crash can never orphan a streaming encoder.
#[cfg(target_os = "linux")]
fn spawn_gst_tracked(procs: &Arc<Mutex<Vec<Child>>>, pipeline: &str) -> Result<(), String> {
	use std::os::unix::process::CommandExt;
	let mut cmd = std::process::Command::new(crate::process::gst_launch_bin());
	cmd.arg("-q").args(pipeline.split_whitespace());
	unsafe {
		cmd.pre_exec(|| {
			// SAFETY: async-signal-safe libc calls only.
			libc::prctl(
				libc::PR_SET_PDEATHSIG,
				libc::SIGKILL as libc::c_ulong,
				0,
				0,
				0,
			);
			if libc::getppid() == 1 {
				libc::_exit(0);
			}
			Ok(())
		});
	}
	match cmd.spawn() {
		Ok(child) => {
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("gst-launch-1.0 {}: {e}", crate::i18n::t("err.spawn"))),
	}
}

/// Build the per-session `on_file` handler: reassemble an inbound file transfer
/// (Begin → buffer, Chunk → append + detect gaps, End → save) and surface the
/// result to the host UI.
pub(super) fn make_on_file(app_h: AppHandle, peer: String) -> impl FnMut(DataMsg) + Send + 'static {
	// Reassemble: Begin → state, Chunk → store BY INDEX, End → save.
	// The session transport is unordered UDP, so chunks can arrive out of order
	// or duplicated; keying by index (instead of appending in arrival order) lets
	// reorders and duplicates self-heal — only a genuinely lost chunk fails the
	// transfer. The transfer is complete iff every index `0..expected` arrived.
	//
	// Each transfer carries a per-stream `id` (FileBegin/Chunk/End), so concurrent
	// transfers (two uploads, an upload racing a download reply) keep SEPARATE
	// reassembly state keyed by that id — interleaved messages no longer clobber
	// each other. An old peer sends id 0 for everything → it falls back to a single
	// keyed-by-0 transfer, exactly the previous single-stream behavior.
	/// Idle transfers older than this are swept from the map on the next FileBegin.
	const XFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
	/// Maximum concurrent in-flight transfers. If a new FileBegin would push us over
	/// this limit we first try to evict a headless entry (`expected == None` — its
	/// FileBegin was lost, so it can't complete anyway) and only fall back to the
	/// oldest-by-last_activity among active entries to avoid silently killing a
	/// slow-but-legitimate transfer.
	const MAX_CONCURRENT_XFERS: usize = 8;
	struct Reasm {
		/// Set by FileBegin. `None` means chunks arrived before FileBegin (UDP
		/// reorder); the entry was lazily created to buffer them. A FileEnd with
		/// `expected == None` is treated as incomplete (Begin never arrived).
		name: String,
		expected: Option<u32>,
		chunks: std::collections::BTreeMap<u32, Vec<u8>>,
		// Running total of buffered bytes (sum of every stored chunk's len, dup-safe).
		// Capped against MAX_XFER_BYTES so a peer can't OOM us by announcing a huge
		// chunk count and streaming distinct-index chunks without ever sending FileEnd.
		received: u64,
		/// Updated on FileBegin and every FileChunk; entries idle beyond
		/// XFER_IDLE_TIMEOUT are swept on the next FileBegin so a lost FileEnd
		/// (UDP, no retransmit) can't leak buffered bytes for the session lifetime.
		last_activity: std::time::Instant,
	}
	let mut xfers: std::collections::HashMap<u32, Reasm> = std::collections::HashMap::new();
	move |m: DataMsg| match m {
		DataMsg::FileBegin {
			id,
			name: n,
			size: _,
			chunks: n_chunks,
		} => {
			// Evict stale entries (lost FileEnd / lost middle chunk) so a long-lived
			// session over a lossy link can't accumulate unbounded dead reassemblers.
			let now = std::time::Instant::now();
			xfers.retain(|_, r| now.duration_since(r.last_activity) < XFER_IDLE_TIMEOUT);
			// Hard cap: if still at the concurrent limit, evict the least-harmful
			// entry — but only if it is not the lazy placeholder for THIS id.
			// Priority: prefer headless entries (expected == None; their FileBegin
			// was lost so they can never complete) over active ones; among peers
			// of the same class pick the oldest by last_activity.
			if xfers.len() >= MAX_CONCURRENT_XFERS && !xfers.contains_key(&id) {
				let victim = xfers
					.iter()
					// headless orphans first (0), then active (1), tie-break oldest
					.min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
					.map(|(k, _)| *k);
				if let Some(vid) = victim {
					xfers.remove(&vid);
				}
			}
			// If early FileChunks created a lazy entry (UDP reorder), merge the
			// name + expected into it to preserve buffered chunks.
			// Otherwise insert a fresh entry.
			if let Some(r) = xfers.get_mut(&id) {
				r.name = sanitize_filename(&n);
				r.expected = Some(n_chunks);
				r.last_activity = now;
				// Prune any pre-buffered chunks whose index is now >= n_chunks.
				// They arrived before FileBegin (expected was None) so the
				// in_range guard passed them all through; now that we know the
				// count, out-of-range indices must be removed so they cannot
				// substitute for a genuinely lost in-range chunk.
				r.chunks.retain(|&idx, data| {
					if idx < n_chunks {
						true
					} else {
						r.received = r.received.saturating_sub(data.len() as u64);
						false
					}
				});
			} else {
				xfers.insert(
					id,
					Reasm {
						name: sanitize_filename(&n),
						expected: Some(n_chunks),
						chunks: std::collections::BTreeMap::new(),
						received: 0,
						last_activity: now,
					},
				);
			}
		}
		DataMsg::FileChunk { id, index, data } => {
			// If no entry exists yet (FileBegin hasn't arrived — UDP reorder),
			// create a lazy placeholder so the chunk is buffered rather than
			// dropped. FileBegin will fill in `name` and `expected` when it
			// arrives, keeping the already-stored chunks intact.
			if !xfers.contains_key(&id) {
				// Mirror the FileBegin guard: sweep idle entries and enforce the
				// concurrent-transfer cap before inserting a new headless entry.
				// Without this a chunk-only flood (FileBegin never arrives) bypasses
				// both the retain() and the cap, allowing unbounded HashMap growth.
				let now = std::time::Instant::now();
				xfers.retain(|_, r| now.duration_since(r.last_activity) < XFER_IDLE_TIMEOUT);
				if xfers.len() >= MAX_CONCURRENT_XFERS {
					let victim = xfers
						.iter()
						.min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
						.map(|(k, _)| *k);
					if let Some(vid) = victim {
						xfers.remove(&vid);
					}
				}
				xfers.insert(
					id,
					Reasm {
						name: String::new(),
						expected: None,
						chunks: std::collections::BTreeMap::new(),
						received: 0,
						last_activity: now,
					},
				);
			}
			// Ignore an index past the announced count (bogus/duplicate-after-resize);
			// a re-sent index simply overwrites with the identical bytes. Before
			// FileBegin (expected == None) buffer all indices — the overflow check
			// runs once expected is known.
			let overflow = if let Some(r) = xfers.get_mut(&id) {
				r.last_activity = std::time::Instant::now();
				let in_range = r.expected.map_or(true, |e| index < e);
				if in_range {
					let prev_len = r.chunks.get(&index).map(|p| p.len() as u64).unwrap_or(0);
					let projected = r.received - prev_len + data.len() as u64;
					if projected > crate::files::MAX_XFER_BYTES {
						true
					} else {
						r.chunks.insert(index, data);
						r.received = projected;
						false
					}
				} else {
					false
				}
			} else {
				false
			};
			if overflow {
				// Peer is overshooting the sane transfer ceiling — drop the whole
				// transfer so the buffer can't grow unbounded. Emit file-recv{ok:false}
				// BEFORE removing so the client learns the transfer was rejected rather
				// than silently showing 'gönderildi' for a file the host never saved.
				if let Some(r) = xfers.remove(&id) {
					let _ = app_h.emit(
						"file-recv",
						FilePayload {
							peer: peer.clone(),
							name: r.name.clone(),
							bytes: 0,
							ok: false,
						},
					);
				}
			}
		}
		DataMsg::FileEnd { id } => {
			// End the transfer: a repeated/stray FileEnd (no matching Begin) must not
			// re-save — `remove` drops the state so a second End for the same id is a
			// no-op.
			let Some(r) = xfers.remove(&id) else {
				return;
			};
			// Complete iff FileBegin was seen (expected is Some) and every index
			// 0..expected is present in the BTreeMap. A bare cardinality check
			// (len == e) would pass even when an out-of-range chunk substituted
			// for a lost in-range one — the contiguous check catches that gap.
			// `Some(0)` is a legitimate empty file. If `expected` is still `None`,
			// FileBegin never arrived; treat that as a failed transfer.
			let complete = r.expected.map_or(false, |e| {
				r.chunks.len() == e as usize
					&& (e == 0 || r.chunks.contains_key(&0) && r.chunks.contains_key(&(e - 1)))
					&& (0..e).all(|i| r.chunks.contains_key(&i))
			});
			// Write chunks directly to disk (in index order via BTreeMap) without
			// building a second contiguous Vec — avoids ~2x peak memory at the
			// MAX_XFER_BYTES ceiling (C24 fix).
			let saved = if complete {
				save_received_file_chunks(&r.name, r.chunks.values(), r.received)
			} else {
				None
			};
			let ok = saved.is_some();
			let written = saved.as_ref().map(|(_, b)| *b).unwrap_or(0);
			let _ = app_h.emit(
				"file-recv",
				FilePayload {
					peer: peer.clone(),
					name: r.name.clone(),
					bytes: written,
					ok,
				},
			);
			if ok {
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "file".into(),
						peer: peer.clone(),
						detail: format!("{} · {} B", r.name, written),
					},
				);
			}
		}
		_ => {}
	}
}

/// Audio player child killed AND reaped on drop. The closure itself dropping
/// (client vanished mid-mic) must reap too — the player would otherwise exit on
/// stdin EOF and linger as a zombie for the host app's lifetime, one per
/// mic stop/disconnect.
struct PlayerGuard(Child);
impl Drop for PlayerGuard {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

/// Build the per-session `on_audio` handler: lazily spawn an audio player and
/// pipe received PCM frames to it, tearing it down on `AudioEnd` / write error.
pub(super) fn make_on_audio() -> impl FnMut(DataMsg) + Send + 'static {
	use std::io::Write;
	// Lazily spawn an audio player and pipe received PCM frames to it.
	let mut sink: Option<std::process::ChildStdin> = None;
	let mut player: Option<PlayerGuard> = None;
	move |m: DataMsg| match m {
		DataMsg::Audio(frame) => {
			if sink.is_none() {
				if let Some((c, s)) = spawn_audio_player() {
					player = Some(PlayerGuard(c));
					sink = Some(s);
				}
			}
			if let Some(s) = sink.as_mut() {
				if s.write_all(&frame).is_err() {
					sink = None;
					player = None; // PlayerGuard::drop kills + reaps
				}
			}
		}
		DataMsg::AudioEnd => {
			sink = None;
			player = None; // PlayerGuard::drop kills + reaps
		}
		_ => {}
	}
}
