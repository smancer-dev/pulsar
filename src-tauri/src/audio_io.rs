//! Host-side PCM audio I/O for the mic side channel: a real-time player (host plays
//! the client's mic) and a recorder (client captures its mic). Linux audio tools
//! (`paplay`/`pw-cat`/`aplay`, `parecord`/`pw-record`/`arecord`). Extracted from
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

/// Spawn a mic recorder (`parecord`/`pw-record`/`arecord`) producing raw PCM on
/// stdout. `None` if no recorder is available.
pub fn spawn_mic_recorder() -> Option<Child> {
	let candidates: [(&str, Vec<String>); 3] = [
		("parecord", AUDIO_ARGS.iter().map(|s| s.to_string()).collect()),
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
	for (prog, args) in candidates {
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
