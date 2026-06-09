//! Host → client audio streaming.
//!
//! Two user toggles drive this (persisted in [`crate::config::Config`]):
//!
//! * **transmit** — send the host's audio to the client.
//! * **mute_host** — silence the host's *local* speakers while streaming.
//!
//! **Game mode** overrides both: launching a game moves audio entirely to the
//! player — transmit ON and the host muted — so a remote gaming session never
//! blasts sound out of the host machine. [`AudioSettings::policy`] is the single
//! source of truth for that rule and is fully unit-tested.
//!
//! Transport mirrors video: a dedicated ffmpeg captures the OS audio, encodes
//! **Opus**, and ships **RTP over UDP** to the client (a second flow alongside the
//! H.264 video), where the viewer relays it to the webview for playback. The
//! command builders here are pure (argument vectors) so they're testable; process
//! spawning + the OS mute call live in the Tauri layer / behind `set_host_muted`.

mod command;
#[cfg(windows)]
mod loopback;
mod mute;
mod settings;

pub use command::{audio_command, opus_rtp_output};
#[cfg(windows)]
pub use loopback::{loopback_format, run_loopback_capture, LoopbackFormat};
pub use mute::set_host_muted;
pub use settings::{AudioInput, AudioPolicy, AudioSettings, CHANNELS, SAMPLE_RATE};
