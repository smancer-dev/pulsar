//! Session file manager (AnyDesk-style): HOME-jailed directory listing + file
//! streaming. The host answers a client's `FsList`/`FsGet` through `make_on_fs`;
//! the client's left ("yerel") pane uses the same listing via the `local_ls`
//! command — one code path so both panes behave identically.
//!
//! Every request path is **relative to the user's HOME** ("" = HOME itself, `/`
//! separators). It is joined under HOME and canonicalized (which resolves `..`
//! AND symlinks), then prefix-checked against the canonicalized HOME — so a
//! symlink pointing outside HOME can't escape the jail either.

use std::path::{Component, Path, PathBuf};

use pulsar_core::service::{DataMsg, FsEntry};
use tokio::sync::mpsc::Sender;

/// Chunk size for file streaming. MUST mirror the client's `send_file` chunker
/// (io_cmds.rs): the session transport is one datagram per message, and
/// serde_json encodes `Vec<u8>` as a number array (≈4 chars/byte worst case),
/// so 2 KiB raw ≈ 8.3 KB JSON — under even macOS's default 9216-byte UDP send
/// limit (net.inet.udp.maxdgram). Bigger chunks fail EMSGSIZE and are silently
/// dropped (serve_with/hold swallow send errors) → broken transfers.
const CHUNK: usize = 2048;

/// A process-unique id for one file transfer. Tags every `FileBegin`/`FileChunk`/
/// `FileEnd` of a single stream so concurrent transfers on the same session (two
/// downloads, an upload racing a download, …) can interleave on the unordered UDP
/// wire without their reassembly state colliding on the receiver. Starts at 1 so 0
/// stays reserved for an old peer's un-tagged (single-transfer) messages.
pub(crate) fn next_transfer_id() -> u32 {
	use std::sync::atomic::{AtomicU32, Ordering};
	static NEXT: AtomicU32 = AtomicU32::new(1);
	NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The user's HOME directory — the jail root for every file-manager path.
/// (`USERPROFILE` is the Windows equivalent; `HOME` is checked first so the
/// behavior matches `files::received_dir`.)
fn home_dir() -> Option<PathBuf> {
	std::env::var_os("HOME")
		.or_else(|| std::env::var_os("USERPROFILE"))
		.map(PathBuf::from)
}

/// The canonicalized HOME jail root — the prefix every resolved path must keep.
fn canonical_home() -> Option<PathBuf> {
	std::fs::canonicalize(home_dir()?).ok()
}

/// Resolve a HOME-relative request path to a real filesystem path, refusing
/// anything that escapes HOME. Returns `None` for absolute/rooted requests,
/// non-existent paths, or any resolution that lands outside the canonical HOME.
pub(crate) fn resolve_jailed(rel: &str) -> Option<PathBuf> {
	let home = canonical_home()?;
	// An absolute/rooted request would *replace* the base in `join` — reject it
	// outright instead of letting it address arbitrary paths.
	let p = Path::new(rel);
	if p.is_absolute()
		|| p.components()
			.any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
	{
		return None;
	}
	// Canonicalize resolves `..` and symlinks, so the prefix check below is the
	// actual security boundary (symlink-escape safe).
	let real = std::fs::canonicalize(home.join(p)).ok()?;
	real.starts_with(&home).then_some(real)
}

/// List a directory by its HOME-relative path, sorted dirs-first then
/// alphabetically (case-insensitive). An empty Vec doubles as the REJECT reply:
/// jailed-out, unreadable, or not-a-directory paths all answer with no entries
/// so the client always gets a response.
pub(crate) fn list_dir(rel: &str) -> Vec<FsEntry> {
	let home = canonical_home();
	let Some(dir) = resolve_jailed(rel) else {
		return Vec::new();
	};
	let Ok(rd) = std::fs::read_dir(&dir) else {
		return Vec::new();
	};
	let mut entries: Vec<FsEntry> = rd
		.filter_map(|e| e.ok())
		.filter_map(|e| {
			// `symlink_metadata` does NOT follow links, so the listing never
			// reflects a symlink TARGET's attributes by default. For a symlink we
			// canonicalize + jail-check the target: an in-jail target is followed
			// (so a linked dir lists as a dir and stays navigable), while an
			// out-of-jail (or broken) target is reported with neutral attributes —
			// resolve_jailed would refuse it on open anyway, so we don't disclose
			// its existence/type/size as a real dir/file.
			let path = e.path();
			let lmd = std::fs::symlink_metadata(&path).ok()?;
			let md = if lmd.file_type().is_symlink() {
				match (home.as_ref(), std::fs::canonicalize(&path).ok()) {
					(Some(home), Some(target)) if target.starts_with(home) => {
						std::fs::metadata(&path).ok()?
					}
					_ => {
						return Some(FsEntry {
							name: e.file_name().to_string_lossy().into_owned(),
							dir: false,
							size: 0,
							sentinel: false,
						});
					}
				}
			} else {
				lmd
			};
			Some(FsEntry {
				name: e.file_name().to_string_lossy().into_owned(),
				dir: md.is_dir(),
				size: if md.is_dir() { 0 } else { md.len() },
				sentinel: false,
			})
		})
		.collect();
	entries.sort_by(|a, b| {
		b.dir
			.cmp(&a.dir)
			.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
	});
	entries
}

/// Stream one jailed file into a session's data queue: `FileBegin` (name + size
/// + chunk count) → `CHUNK`-sized `FileChunk`s → `FileEnd` — the same wire flow as the
/// webview's `send_file`, so the existing reassembly/save path on the receiving
/// end applies unchanged. `None` = refused (jail) or an I/O error mid-stream.
pub(crate) async fn send_file_at(tx: &Sender<DataMsg>, rel: &str) -> Option<()> {
	use tokio::io::AsyncReadExt;
	let real = resolve_jailed(rel)?;
	let md = tokio::fs::metadata(&real).await.ok()?;
	if !md.is_file() {
		return None;
	}
	let name = real.file_name()?.to_string_lossy().into_owned();
	let size = md.len();
	let chunks = size.div_ceil(CHUNK as u64) as u32;
	let id = next_transfer_id();
	tx.send(DataMsg::FileBegin {
		id,
		name,
		size,
		chunks,
	})
	.await
	.ok()?;
	let mut f = tokio::fs::File::open(&real).await.ok()?;
	let mut buf = vec![0u8; CHUNK];
	let mut index = 0u32;
	loop {
		// Fill a whole chunk (short reads happen mid-file) so the chunk count
		// announced in FileBegin holds and the receiver's gap check stays valid.
		let mut filled = 0;
		while filled < CHUNK {
			match f.read(&mut buf[filled..]).await {
				Ok(0) => break,
				Ok(n) => filled += n,
				Err(_) => return None,
			}
		}
		if filled == 0 {
			break;
		}
		tx.send(DataMsg::FileChunk {
			id,
			index,
			data: buf[..filled].to_vec(),
		})
		.await
		.ok()?;
		index += 1;
	}
	tx.send(DataMsg::FileEnd { id }).await.ok()?;
	Some(())
}

/// Stream a file by its ABSOLUTE path into a session's data queue. This is the
/// internal path for the native file-picker flow (the user explicitly chose the
/// file through an OS dialog — no HOME-jail check is appropriate there). The wire
/// format is identical to `send_file_at` so the receiver's reassembly path is
/// unchanged. `None` = path is not a file or an I/O error occurred mid-stream.
pub(crate) async fn send_file_abs(
	tx: &Sender<DataMsg>,
	abs: &std::path::Path,
) -> Option<()> {
	use tokio::io::AsyncReadExt;
	let md = tokio::fs::metadata(abs).await.ok()?;
	if !md.is_file() {
		return None;
	}
	let name = abs.file_name()?.to_string_lossy().into_owned();
	let size = md.len();
	let chunks = size.div_ceil(CHUNK as u64) as u32;
	let id = next_transfer_id();
	tx.send(DataMsg::FileBegin {
		id,
		name,
		size,
		chunks,
	})
	.await
	.ok()?;
	let mut f = tokio::fs::File::open(abs).await.ok()?;
	let mut buf = vec![0u8; CHUNK];
	let mut index = 0u32;
	loop {
		let mut filled = 0;
		while filled < CHUNK {
			match f.read(&mut buf[filled..]).await {
				Ok(0) => break,
				Ok(n) => filled += n,
				Err(_) => return None,
			}
		}
		if filled == 0 {
			break;
		}
		tx.send(DataMsg::FileChunk {
			id,
			index,
			data: buf[..filled].to_vec(),
		})
		.await
		.ok()?;
		index += 1;
	}
	tx.send(DataMsg::FileEnd { id }).await.ok()?;
	Some(())
}

/// Trim a listing to the session's one-datagram wire budget. `FsEntries` goes out
/// as ONE serde_json datagram; a big directory (Downloads, node_modules) would
/// serialize past the UDP send limit, the send fails EMSGSIZE, serve_with swallows
/// it — and the client's remote pane silently never fills. Budget ≈ name bytes +
/// per-entry JSON overhead, kept conservatively under the smallest practical
/// datagram for the HOST platform (macOS only sends ~9216 bytes by default —
/// net.inet.udp.maxdgram; 65507 elsewhere). A "… N daha" sentinel entry marks
/// the cut.
fn cap_for_wire(mut entries: Vec<FsEntry>) -> Vec<FsEntry> {
	// Per-entry JSON beyond the name: {"name":…,"dir":false,"size":<u64>} ≈ 48 B.
	const ENTRY_OVERHEAD: usize = 48;
	const MAX_ENTRIES: usize = 500;
	let budget: usize = if cfg!(target_os = "macos") {
		7 * 1024
	} else {
		48 * 1024
	};
	let mut used = 0usize;
	let mut keep = 0usize;
	for e in &entries {
		used += e.name.len() + ENTRY_OVERHEAD;
		if used > budget || keep == MAX_ENTRIES {
			break;
		}
		keep += 1;
	}
	if keep < entries.len() {
		let more = entries.len() - keep;
		entries.truncate(keep);
		// `sentinel: true` marks this entry as a non-actionable truncation
		// notice so the client can render it as inert text instead of as a
		// clickable file row with a download button.
		entries.push(FsEntry {
			name: format!("… {more} daha"),
			dir: false,
			size: 0,
			sentinel: true,
		});
	}
	entries
}

/// Build the per-session `on_fs` handler (host side): `FsList` answers with the
/// jailed listing as `FsEntries`, `FsGet` streams the file back — both through
/// the same outbound queue the serve loop drains. Filesystem work runs off the
/// serve loop (blocking task / spawned task) so a slow disk can't stall it.
pub(crate) fn make_on_fs(out_tx: Sender<DataMsg>) -> impl FnMut(DataMsg) + Send + 'static {
	move |m: DataMsg| match m {
		DataMsg::FsList { path } => {
			let tx = out_tx.clone();
			tokio::spawn(async move {
				let entries = tokio::task::spawn_blocking({
					let path = path.clone();
					move || cap_for_wire(list_dir(&path))
				})
				.await
				.unwrap_or_default();
				let _ = tx.send(DataMsg::FsEntries { path, entries }).await;
			});
		}
		DataMsg::FsGet { path } => {
			let tx = out_tx.clone();
			tokio::spawn(async move {
				if send_file_at(&tx, &path).await.is_none() {
					// The host refused or failed (jailed path, missing/non-file, I/O
					// error mid-stream).  Send a synthetic FileBegin{chunks:1} +
					// FileEnd with no intervening chunk so the client's reassembler
					// sees expected=Some(1) but 0 chunks → complete=false →
					// file-recv{ok:false}.  This drains the client's concurrency slot
					// immediately instead of holding it for the full no-response
					// timeout.
					//
					// Name: the basename of the requested path as a plain String —
					// the client keys pendingDownloads by sanitizeFilename(basename),
					// so using the raw basename here lets the existing sanitize path
					// in files.rs (called on FileEnd save) emit the same key.
					let name = Path::new(&path)
						.file_name()
						.map(|n| n.to_string_lossy().into_owned())
						.unwrap_or_else(|| path.clone());
					let id = next_transfer_id();
					// FileBegin announces 1 chunk that will never arrive.
					let _ = tx.send(DataMsg::FileBegin {
						id,
						name,
						size: 0,
						chunks: 1,
					}).await;
					// FileEnd with no preceding chunk → reassembler marks incomplete
					// → file-recv{ok:false} fires on the client side promptly.
					let _ = tx.send(DataMsg::FileEnd { id }).await;
				}
			});
		}
		_ => {}
	}
}

/// Client-local listing for the file panel's LEFT ("yerel") pane — same JSON
/// shape and the same HOME jail as the remote side, so the two panes behave
/// identically.
#[tauri::command]
pub(crate) async fn local_ls(path: String) -> Result<Vec<FsEntry>, String> {
	tokio::task::spawn_blocking(move || list_dir(&path))
		.await
		.map_err(|e| e.to_string())
}
