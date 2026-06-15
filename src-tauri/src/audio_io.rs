//! Host-side PCM audio I/O for the mic side channel: a real-time player (host plays
//! the client's mic) and a recorder (client captures its mic). Linux audio tools
//! (`paplay`/`pw-cat`/`aplay`, `parecord`/`pw-record`/`arecord`); Windows via
//! bundled ffmpeg WASAPI; macOS via bundled ffmpeg AVFoundation. Extracted from
//! `lib.rs` (see PENDING-WORK #9).

use std::process::{Child, Stdio};

/// Raw-PCM audio format used for the mic side channel (s16le, 48kHz, mono).
const AUDIO_ARGS: &[&str] = &["--rate=48000", "--channels=1", "--format=s16le", "--raw"];

/// Spawn a real-time PCM player on the host (`paplay`/`pw-cat`/`aplay`), returning
/// the child + its stdin to pipe frames into. `None` if no player is available.
pub fn spawn_audio_player() -> Option<(Child, std::process::ChildStdin)> {
	let candidates: [(&str, Vec<String>); 3] = [
		("paplay", AUDIO_ARGS.iter().map(|s| s.to_string()).collect()),
		(
			"pw-cat",
			[
				"--playback",
				"--rate",
				"48000",
				"--channels",
				"1",
				"--format",
				"s16",
				"-",
			]
			.iter()
			.map(|s| s.to_string())
			.collect(),
		),
		(
			"aplay",
			["-q", "-f", "S16_LE", "-r", "48000", "-c", "1"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
	];
	for (prog, args) in candidates {
		if let Ok(mut child) = std::process::Command::new(prog)
			.args(&args)
			.stdin(Stdio::piped())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
		{
			if let Some(stdin) = child.stdin.take() {
				return Some((child, stdin));
			}
			let _ = child.kill();
		}
	}
	None
}

/// Spawn a mic recorder producing raw s16le 48 kHz mono PCM on stdout.
///
/// - Linux: tries `parecord` / `pw-record` / `arecord` in order.
/// - Windows: uses the bundled ffmpeg with the WASAPI input (`-f wasapi -i default`).
/// - macOS: uses the bundled ffmpeg with AVFoundation (`-f avfoundation -i ":0"`).
///
/// `ffmpeg_path` is the resolved bundled ffmpeg path (from `process::ffmpeg_bin`).
/// On Linux the argument is unused (the native recorders are tried first; ffmpeg is
/// only the last-resort fallback so the function still works on headless Pi builds
/// where the bundled ffmpeg may lack avfoundation/wasapi).
///
/// Returns `None` if no suitable recorder could be spawned.
pub fn spawn_mic_recorder(ffmpeg_path: &str) -> Option<Child> {
	// Common ffmpeg output args: raw s16le 48 kHz mono on stdout, no info spam.
	let ff_out: &[&str] = &[
		"-hide_banner",
		"-loglevel",
		"error",
		"-vn",
		"-f",
		"s16le",
		"-ar",
		"48000",
		"-ac",
		"1",
		"pipe:1",
	];

	// Platform-specific input candidates (prog, args).  We try them in order and
	// return the first child that spawns successfully.
	#[cfg(windows)]
	{
		// WASAPI capture of the default microphone (input device, not loopback).
		// `-f wasapi -i default` selects the system default recording endpoint.
		let mut args: Vec<&str> = vec!["-f", "wasapi", "-i", "default"];
		args.extend_from_slice(ff_out);
		let mut cmd = std::process::Command::new(ffmpeg_path);
		cmd.args(&args)
			.stdout(Stdio::piped())
			.stderr(Stdio::null());
		crate::process::no_window(&mut cmd);
		if let Ok(child) = cmd.spawn() {
			return Some(child);
		}
	}

	#[cfg(target_os = "macos")]
	{
		// AVFoundation: `:0` = first (default) audio capture device, no video.
		let mut args: Vec<&str> = vec!["-f", "avfoundation", "-i", ":0"];
		args.extend_from_slice(ff_out);
		if let Ok(child) = std::process::Command::new(ffmpeg_path)
			.args(&args)
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
		{
			return Some(child);
		}
	}

	// Linux (and fallback on any platform): native PulseAudio / PipeWire / ALSA tools.
	let linux_candidates: [(&str, Vec<String>); 3] = [
		(
			"parecord",
			AUDIO_ARGS.iter().map(|s| s.to_string()).collect(),
		),
		(
			"pw-record",
			["--rate", "48000", "--channels", "1", "--format", "s16", "-"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
		(
			"arecord",
			["-q", "-f", "S16_LE", "-r", "48000", "-c", "1"]
				.iter()
				.map(|s| s.to_string())
				.collect(),
		),
	];
	for (prog, args) in linux_candidates {
		if let Ok(child) = std::process::Command::new(prog)
			.args(&args)
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
		{
			return Some(child);
		}
	}
	None
}
