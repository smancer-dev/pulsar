//! Pulsar desktop core.
//!
//! This crate holds the performance-sensitive, UI-agnostic logic that the Tauri
//! app drives: connection orchestration (register with a relay, get an ID, try
//! P2P, fall back to relay), end-to-end crypto, controller input, configuration,
//! and the streaming pipeline. Keeping it as a plain Rust library means it's
//! fully unit-testable without a webview.

#[doc(inline)]
pub use pulsar_proto as proto;

#[cfg(target_os = "linux")]
pub mod capture;
pub mod config;
pub mod connection;
pub mod crypto;
pub mod input;
pub mod media;
pub mod pipeline;
pub mod service;

pub use config::{Config, Language, NetworkMode};
pub use connection::{ConnError, Node, Session, Transport};
pub use crypto::{Identity, Role};
pub use input::{GamepadKind, GamepadState, VirtualGamepad};
pub use media::{Codec, EncodedPacket, RawFrame, StreamStats};
pub use pipeline::{HwEncoder, StreamPlan, VCodec};
pub use service::GameInfo;
