//! File logging. Everything `tracing` emits goes to stdout (dev runs) AND to a daily
//! rotated file under the platform log directory, so a black screen on a release build
//! — Windows has no console at all there — leaves a trace the user can open and send.
//! The renderer's stderr and the encode ffmpeg's error tail already flow through
//! `tracing`, so they land in the same file.

use std::path::PathBuf;

/// Bundle identifier (`tauri.conf.json`); the log dir is keyed by it like every other
/// per-app path.
const APP_ID: &str = "dev.pulsar.app";
/// Daily files kept before the oldest is deleted.
const KEEP_FILES: usize = 7;

/// The directory Tauri's `app_log_dir()` resolves to — computed by hand because the
/// subscriber must be installed before an `AppHandle` exists.
pub fn log_dir() -> Option<PathBuf> {
	#[cfg(target_os = "windows")]
	{
		std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(APP_ID).join("logs"))
	}
	#[cfg(target_os = "macos")]
	{
		std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs").join(APP_ID))
	}
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		std::env::var_os("XDG_DATA_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
			.map(|d| d.join(APP_ID).join("logs"))
	}
}

/// Install the global subscriber: `RUST_LOG`-filtered (default `info`), stdout with
/// colour plus a plain-text daily file. Falls back to stdout alone when the directory
/// cannot be created (read-only home, sandbox) — logging must never stop startup.
pub fn init() {
	use tracing_subscriber::{fmt, prelude::*, EnvFilter};
	let filter =
		EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
	let file = log_dir().and_then(|dir| {
		std::fs::create_dir_all(&dir).ok()?;
		tracing_appender::rolling::Builder::new()
			.rotation(tracing_appender::rolling::Rotation::DAILY)
			.filename_prefix("pulsar")
			.filename_suffix("log")
			.max_log_files(KEEP_FILES)
			.build(&dir)
			.ok()
			.map(|appender| (dir, appender))
	});
	let stdout = fmt::layer().with_writer(std::io::stdout);
	match file {
		Some((dir, appender)) => {
			let file = fmt::layer().with_ansi(false).with_writer(appender);
			tracing_subscriber::registry()
				.with(filter)
				.with(stdout)
				.with(file)
				.init();
			tracing::info!(dir = %dir.display(), "log file enabled");
		}
		None => {
			tracing_subscriber::registry().with(filter).with(stdout).init();
			tracing::warn!("log directory unavailable — logging to stdout only");
		}
	}
}

/// Open the log directory in the platform file manager. Returns the path either way so
/// the UI can show it when no file manager is available.
pub fn open_log_dir() -> Result<String, String> {
	let dir = log_dir().ok_or_else(|| "log directory unavailable".to_string())?;
	std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
	let path = dir.to_string_lossy().into_owned();
	#[cfg(target_os = "windows")]
	let opener = "explorer";
	#[cfg(target_os = "macos")]
	let opener = "open";
	#[cfg(all(unix, not(target_os = "macos")))]
	let opener = "xdg-open";
	let mut cmd = std::process::Command::new(opener);
	cmd.arg(&dir);
	crate::process::no_window(&mut cmd);
	let _ = cmd.spawn();
	Ok(path)
}
