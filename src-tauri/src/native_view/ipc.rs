//! mpv JSON-IPC helpers: talk to the embedded `--wid` mpv over its `--input-ipc-server`
//! Unix socket (pause/resume for the gaming-overlay toggle, and polling numeric properties
//! for the perf HUD). Fire-and-forget; a not-yet-ready socket is treated as a no-op.

#[cfg(not(windows))]
use std::path::Path;

/// Send one JSON IPC command line to the embedded `--wid` mpv over its `--input-ipc-server`
/// Unix socket. Fire-and-forget: opens the socket, writes `json_cmd` + `\n`, returns. The
/// socket only exists once mpv has started, so callers `let _ =` the result and treat a
/// connect-refused/not-found as a no-op (the next poll/toggle retries).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn mpv_ipc(sock: &Path, json_cmd: &str) -> std::io::Result<()> {
	use std::io::Write;
	use std::os::unix::net::UnixStream;
	let mut stream = UnixStream::connect(sock)?;
	stream.write_all(json_cmd.as_bytes())?;
	stream.write_all(b"\n")?;
	Ok(())
}

/// Pause/resume the embedded mpv via JSON IPC (gaming-overlay toggle, Faz 3). Pause — not
/// hide — is deliberate: the last frame stays on screen, resume is instant, and the
/// decoder/socket stay warm (mpv keeps draining the bounded UDP buffer, so we never SIGSTOP
/// the read). Fire-and-forget; the caller ignores the Result so a not-yet-ready socket is a
/// silent no-op.
#[cfg(all(unix, not(target_os = "macos")))]
#[allow(dead_code)] // utility; the overlay now kills+respawns mpv instead of pausing it
pub fn mpv_set_pause(sock: &Path, paused: bool) -> std::io::Result<()> {
	let cmd = format!("{{\"command\":[\"set_property\",\"pause\",{paused}]}}");
	mpv_ipc(sock, &cmd)
}

/// Read a numeric mpv property over JSON IPC (perf HUD poller, Faz 4). Connects, sends
/// `{"command":["get_property",<prop>]}`, reads one reply line, and parses the `"data"`
/// number out of `{"data":<num>,"error":"success"}`. Returns None on any error (socket not
/// ready, property missing/unavailable, non-numeric `data` such as `null`) so the poller can
/// fall back to 0.0. Used for `estimated-vf-fps` / `decoder-frame-drop-count` /
/// `video-bitrate` / `vo-delay`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn mpv_ipc_get_f64(sock: &Path, prop: &str) -> Option<f64> {
	use std::io::{BufRead, BufReader, Write};
	use std::os::unix::net::UnixStream;
	let mut stream = UnixStream::connect(sock).ok()?;
	let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
	let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
	let cmd = format!("{{\"command\":[\"get_property\",\"{prop}\"]}}\n");
	stream.write_all(cmd.as_bytes()).ok()?;
	// mpv replies with one JSON object per line. The first line is the reply to our request
	// (no async events are subscribed on this short-lived connection).
	let mut reader = BufReader::new(&stream);
	let mut line = String::new();
	reader.read_line(&mut line).ok()?;
	parse_ipc_data_f64(&line)
}

/// Read a boolean mpv property over JSON IPC (same one-shot connection as
/// `mpv_ipc_get_f64`). Used to poll `focused` for the standalone (no `--wid`) mpv
/// fallback window: its focus is invisible to Tauri, but the evdev capture's
/// focus/engage gates need it. None on any error or non-boolean `data`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn mpv_ipc_get_bool(sock: &Path, prop: &str) -> Option<bool> {
	use std::io::{BufRead, BufReader, Write};
	use std::os::unix::net::UnixStream;
	let mut stream = UnixStream::connect(sock).ok()?;
	let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
	let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
	let cmd = format!("{{\"command\":[\"get_property\",\"{prop}\"]}}\n");
	stream.write_all(cmd.as_bytes()).ok()?;
	let mut reader = BufReader::new(&stream);
	let mut line = String::new();
	reader.read_line(&mut line).ok()?;
	let after = line.split_once("\"data\":")?.1;
	let end = after.find(|c| c == ',' || c == '}').unwrap_or(after.len());
	match after[..end].trim() {
		"true" => Some(true),
		"false" => Some(false),
		_ => None,
	}
}

/// Pull the numeric `"data"` field out of an mpv IPC reply line without a JSON dep:
/// finds `"data":`, then parses the following number up to the next `,`/`}`. Returns None if
/// the field is absent or its value isn't a finite number (e.g. `"data":null` when the
/// property is currently unavailable).
#[cfg(all(unix, not(target_os = "macos")))]
fn parse_ipc_data_f64(line: &str) -> Option<f64> {
	let after = line.split_once("\"data\":")?.1;
	let end = after.find(|c| c == ',' || c == '}').unwrap_or(after.len());
	let v: f64 = after[..end].trim().parse().ok()?;
	v.is_finite().then_some(v)
}
