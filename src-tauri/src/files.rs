//! Received-file helpers for the session file-transfer side channel. Extracted from
//! `lib.rs` (see PENDING-WORK #9).

use std::path::PathBuf;

/// Strip path separators so a peer can't write outside the received-files dir.
pub fn sanitize_filename(name: &str) -> String {
	let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
	let cleaned: String = base
		.chars()
		.filter(|c| !matches!(c, '\0'..='\u{1f}'))
		.collect();
	if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
		"dosya".into()
	} else {
		cleaned
	}
}

/// Directory incoming files are written to (`~/Pulsar Alınanlar`, created on
/// demand). Falls back to the system temp dir if `$HOME` is unset.
pub fn received_dir() -> PathBuf {
	let base = std::env::var("HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|_| std::env::temp_dir());
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
