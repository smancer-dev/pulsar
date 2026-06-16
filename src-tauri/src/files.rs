//! Received-file helpers for the session file-transfer side channel. Extracted from
//! `lib.rs` (see PENDING-WORK #9).

use std::path::{Component, Path, PathBuf};

/// Hard ceiling on the total bytes a single inbound file transfer may reassemble in
/// memory before it is aborted. The chunk count (`FileBegin.chunks`) and chunk sizes
/// are peer-controlled, so without this a malicious/buggy peer can announce a huge
/// chunk count and stream distinct-index chunks forever (never sending `FileEnd`),
/// growing the receiver's reassembly buffer until the process is OOM-killed. Both the
/// host (`make_on_file`) and the client (`hold_session`) reassemblers clamp against
/// this. 2 GiB comfortably covers real transfers while bounding the worst case.
pub const MAX_XFER_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Strip path separators so a peer can't write outside the received-files dir.
pub fn sanitize_filename(name: &str) -> String {
	let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
	// On Windows ':' is also stripped: a separator-less name like "C:evil.exe" is a
	// drive-relative path — PathBuf::push with it REPLACES the received-files dir
	// (drive-CWD-relative write outside the jail); it also covers NTFS ADS names.
	// On macOS/Linux ':' is an ordinary, valid filename byte — leave it untouched.
	#[cfg(windows)]
	let cleaned: String = base
		.chars()
		.filter(|c| !matches!(c, '\0'..='\u{1f}' | ':'))
		.collect();
	#[cfg(not(windows))]
	let cleaned: String = base
		.chars()
		.filter(|c| !matches!(c, '\0'..='\u{1f}'))
		.collect();
	// Structural guard: the result must parse as exactly ONE normal path component
	// on THIS platform — anything else (empty, ".", "..", a surviving prefix/root)
	// falls back to a harmless fixed name.
	let mut comps = Path::new(&cleaned).components();
	let single_normal =
		matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
	if single_normal {
		escape_reserved(cleaned)
	} else {
		"dosya".into()
	}
}

/// On Windows, neutralise names that Win32 treats specially regardless of the
/// containing directory, so a peer can't redirect or break the received-file write:
/// reserved DOS device names (CON/PRN/AUX/NUL/COM1-9/LPT1-9, with or without an
/// extension) refer to a device rather than a file, and trailing dots/spaces are
/// silently stripped by the filesystem (so `report.txt.` would land as `report.txt`,
/// clobbering a different file). We prefix-escape with a leading underscore rather
/// than dropping the name so the user still gets a recognizable file. No-op on other
/// platforms, where these characters/names are ordinary.
#[cfg(windows)]
fn escape_reserved(name: String) -> String {
	// Trim trailing dots/spaces the way Win32 path canonicalisation would.
	let trimmed = name.trim_end_matches(['.', ' ']);
	let trimmed = if trimmed.is_empty() { name.as_str() } else { trimmed };
	// The reserved stem is everything before the first '.' (so "NUL.txt" is still NUL).
	let stem = trimmed.split('.').next().unwrap_or(trimmed);
	let reserved = matches!(
		stem.to_ascii_uppercase().as_str(),
		"CON" | "PRN" | "AUX" | "NUL"
			| "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
			| "COM6" | "COM7" | "COM8" | "COM9"
			| "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
			| "LPT6" | "LPT7" | "LPT8" | "LPT9"
	);
	if reserved || trimmed.len() != name.len() {
		format!("_{trimmed}")
	} else {
		name
	}
}

#[cfg(not(windows))]
#[inline]
fn escape_reserved(name: String) -> String {
	name
}

/// Directory incoming files are written to (`~/Pulsar Alınanlar`, created on
/// demand). Resolves the home base the same way as `fs_browse::home_dir` (the
/// file-manager jail root) so the receive dir and the browse/send root can't
/// diverge: `HOME` first, then Windows's `USERPROFILE`, falling back to the
/// system temp dir only if neither is set.
pub fn received_dir() -> PathBuf {
	let base = std::env::var_os("HOME")
		.or_else(|| std::env::var_os("USERPROFILE"))
		.map(PathBuf::from)
		.unwrap_or_else(std::env::temp_dir);
	let dir = base.join("Pulsar Alınanlar");
	let _ = std::fs::create_dir_all(&dir);
	dir
}

/// Write received bytes to the received-files dir, avoiding clobbering an existing
/// file by suffixing ` (n)`. Returns the final path on success.
pub fn save_received_file(name: &str, data: &[u8]) -> Option<PathBuf> {
	let dir = received_dir();
	let mut path = dir.join(name);
	let (stem, ext) = match name.rsplit_once('.') {
		Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
		_ => (name.to_string(), String::new()),
	};
	let mut n = 1;
	while path.exists() {
		path = dir.join(format!("{stem} ({n}){ext}"));
		n += 1;
	}
	std::fs::write(&path, data).ok().map(|_| path)
}

/// Write reassembled file chunks directly to disk without building a contiguous
/// intermediate buffer. Chunks must be in index order (i.e. from a `BTreeMap`
/// iterated in key order). Returns the final path and the total byte count on
/// success, or `None` on I/O error.
///
/// This avoids the ~2x peak-memory spike that occurs when `extend_from_slice`
/// builds a second contiguous `Vec` while the per-chunk `BTreeMap` is still live.
pub fn save_received_file_chunks<'a>(
	name: &str,
	chunks: impl Iterator<Item = &'a Vec<u8>>,
	total_bytes: u64,
) -> Option<(PathBuf, u64)> {
	use std::io::Write as _;
	let dir = received_dir();
	let mut path = dir.join(name);
	let (stem, ext) = match name.rsplit_once('.') {
		Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
		_ => (name.to_string(), String::new()),
	};
	let mut n = 1;
	while path.exists() {
		path = dir.join(format!("{stem} ({n}){ext}"));
		n += 1;
	}
	let mut file = std::fs::File::create(&path).ok()?;
	for chunk in chunks {
		file.write_all(chunk).ok()?;
	}
	Some((path, total_bytes))
}
