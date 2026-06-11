//! Real-time video pipeline driven by the system **ffmpeg/ffplay**.
//!
//! This is the concrete encode/decode the design's "Donanımsal kodlama
//! (NVENC / QuickSync / VideoToolbox)" refers to. It selects a hardware encoder
//! — **NVENC** (NVIDIA), **VAAPI** for an iGPU/AMD,
//! **QuickSync**, **VideoToolbox** on macOS, or a software fallback — captures
//! the screen, encodes low-latency, and ships MPEG-TS to the peer; the client
//! decodes + displays with ffplay.
//!
//! Everything here is pure (builds argument vectors / parses `ffmpeg -encoders`)
//! so it's unit-tested; the actual process spawning lives in the Tauri layer.

mod command;
pub mod gst;
mod types;

pub use command::{decode_command, encode_command, probe_command};
pub use types::{detect, resolve, resolve_codec, CaptureMethod, HwEncoder, StreamPlan, VCodec};

#[cfg(test)]
mod tests;
