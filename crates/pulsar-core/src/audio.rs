//! Host → client audio streaming.
//!
//! Two user toggles drive this (persisted in [`crate::config::Config`]):
//!
//! * **transmit** — send the host's audio to the client.
//! * **mute_host** — silence the host's *local* speakers while streaming.
//!
//! **Game mode** forces **transmit ON** (the player must hear the game) but does
//! **NOT** force-mute the host. Muting the host means muting the very render
//! endpoint whose WASAPI loopback we capture, and on common codecs that tap is
//! *post-mute / post-master-volume*, so a capture opened while the endpoint is muted
//! (or at volume 0) streams **pure silence** to the client. [`AudioSettings::policy`]
//! is the single source of truth and is unit-tested. To keep a remote game from
//! blasting out of the host machine we instead **redirect** the default render
//! endpoint to a sinkless virtual sink (the Sunshine model — see [`sink`]) and
//! capture *that*, leaving the real speakers silent without ever muting the capture.
//! `mute_host` is still honored if the user asks, ideally mid-session once the
//! capture is live (muting an already-running loopback is safe; opening *into* a mute
//! is not).
//!
//! Transport mirrors video: a dedicated ffmpeg captures the OS audio, encodes
//! **Opus** (multistream for 5.1/7.1 — see [`ChannelLayout`]), and ships **RTP over
//! UDP** to the client (a second flow alongside the H.264 video). The command
//! builders here are pure (argument vectors) so they're testable; process spawning,
//! the sink redirect, and the OS mute call live in the Tauri layer / behind
//! [`sink::SinkRedirectGuard`] and `set_host_muted`.

mod command;
#[cfg(windows)]
mod loopback;
mod mute;
mod settings;
mod sink;
// Host-silent on Linux/macOS: the non-Windows analog of `sink`'s default-endpoint
// redirect (a PulseAudio null sink on Linux, a virtual-output switch on macOS).
// Compiled only off Windows — muting silences the monitor there too (see the module
// docs), so host-silent must redirect capture to a sinkless device, never mute.
#[cfg(not(windows))]
mod sink_unix;

pub use command::{
	audio_command, audio_command_layout, opus_bitrate, opus_rtp_output, opus_rtp_output_layout,
};
#[cfg(windows)]
pub use loopback::{
	loopback_format, run_loopback_capture, run_loopback_capture_pinned,
	run_loopback_capture_tracking, LoopbackFormat,
};
pub use mute::{mute_fallback_marker_path, restore_stale_mute_fallback, set_host_muted};
pub use settings::{
	AudioInput, AudioPolicy, AudioSettings, ChannelLayout, CHANNELS, SAMPLE_RATE,
};
pub use sink::{
	default_render_device_id, find_render_device_by_name, restore_stale_redirect,
	set_default_render_device, set_render_device_format, SinkDevice, SinkRedirectGuard,
};
// Linux/macOS host-silent: `arm()` redirects program audio to a sinkless device and
// returns the capture source name + an RAII teardown guard (Windows uses `sink`).
// `restore_stale_host_silent()` is the crash-recovery entry point called at startup.
#[cfg(not(windows))]
pub use sink_unix::{arm as arm_host_silent, restore_stale_host_silent, HostSilent, HostSilentGuard};
