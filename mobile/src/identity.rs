//! Persistent X25519 identity for the mobile app.
//!
//! Wraps [`Identity::load_or_create`] from pulsar-core so the relay-assigned
//! 9-digit ID is **stable across restarts** and distinct per OS user — exactly
//! mirroring the desktop app's `util::identity_path` + `load_or_create` logic
//! (see `desktop-app/src-tauri/src/util.rs`).

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};

use pulsar_core::Identity;

/// Returns the path where the mobile identity key is persisted:
/// `<app_data_dir>/identity.key` — same convention as the desktop.
pub fn identity_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("identity.key")
}

/// Load the persistent identity (or generate + save a fresh one if the file is
/// absent or corrupted). This is the **only** place in the mobile crate that
/// touches `identity.key`; both `connect_host` and `go_online` call this instead
/// of `Identity::generate()`.
pub fn load_identity<R: Runtime>(app: &AppHandle<R>) -> Identity {
    Identity::load_or_create(identity_path(app))
}
