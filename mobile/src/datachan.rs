//! Side-channel decode, reassembly, and send commands (W4-rust-data).
//!
//! ## Receive path
//!
//! `route(&app, slot, &bytes)` is called from the `client.rs` read loop whenever
//! `service::media::parse` returns `None` (i.e. the frame is NOT a media/RTP
//! frame). It runs `service::decode_data` and dispatches the matching Tauri event.
//!
//! File reassembly (`DataMsg::FileBegin/FileChunk/FileEnd`) is handled here:
//! - Per-slot, per-id `BTreeMap`-based reassembler (`XferMap` managed state).
//! - 8 concurrent in-flight transfers per slot (W4 cap).
//! - Idle entries (no activity for 60 s) are swept on the next `FileBegin`.
//! - Total received bytes capped at `MAX_XFER_BYTES` (2 GiB) to prevent OOM.
//! - Completed file is saved to "Pulsar Alınanlar" inside the app data dir.
//!
//! ## Send commands
//!
//! - `send_clipboard` — `DataMsg::Clipboard(text)` → host.
//! - `send_chat`      — `DataMsg::Chat(text)` → host.
//! - `fs_list`        — `DataMsg::FsList{path}` → host; reply via `fs-entries`.
//! - `fs_get`         — `DataMsg::FsGet{path}` → host; reply via `file-*` events.
//! - `send_file`      — base64 bytes from JS → `FileBegin/FileChunk/FileEnd` → host.
//!
//! All send commands look up the `SessionSender` for `slot` from the `InputSenders`
//! managed state (owned by `client.rs`) and forward via `service::send_data_via`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use pulsar_core::service::{decode_data, send_data_via, DataMsg};

use crate::client::InputSenders;

// ── File reassembly constants ────────────────────────────────────────────────

/// Hard ceiling on the total bytes a single inbound file transfer may
/// reassemble before it is aborted (OOM guard). Mirrors the desktop's value.
const MAX_XFER_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Concurrent in-flight transfer cap per slot.
const MAX_CONCURRENT_XFERS: usize = 8;

/// Idle entries (no FileChunk / FileBegin for this long) are swept on the next
/// FileBegin so a lost FileEnd over a lossy link can't leak buffers for the
/// session's lifetime.
const XFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Chunk size used when *uploading* (sending) a file to the host, in bytes.
/// Mirrors the desktop file-manager's chunk size.
const UPLOAD_CHUNK_BYTES: usize = 2048;

// ── File reassembler state ────────────────────────────────────────────────────

/// In-flight inbound file transfer.
pub(crate) struct FileReasm {
    /// Filename set by `FileBegin`. Empty when chunks arrived before `FileBegin`
    /// (UDP reorder — lazy placeholder).
    name: String,
    /// Total chunk count set by `FileBegin`; `None` until `FileBegin` arrives.
    expected: Option<u32>,
    /// Total bytes the host said the file is.
    total_size: u64,
    /// Received chunks, keyed by index.
    chunks: BTreeMap<u32, Vec<u8>>,
    /// Running tally of buffered bytes (dup-safe: keyed by index).
    received: u64,
    /// Updated on every incoming message; idle sweeper uses this.
    last_activity: Instant,
}

/// Per-slot file reassembly state: outer key = session slot, inner key = xfer id.
#[derive(Default)]
pub struct XferMap(pub Mutex<HashMap<u8, HashMap<u32, FileReasm>>>);

// ── Event payload types ───────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ClipboardInPayload {
    slot: u8,
    text: String,
}

/// The host pushed its display name (`DataMsg::PeerName`) for this session. JS
/// caches it per connected device id so recents/LAN rows can show the name.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PeerNamePayload {
    slot: u8,
    name: String,
}

/// The host pushed its identity image (`DataMsg::Avatar`, raw PNG/JPEG) — emitted
/// as a ready-to-use data URL so JS can cache + render it without re-encoding.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PeerAvatarPayload {
    slot: u8,
    data_url: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatMsgPayload {
    slot: u8,
    text: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FsEntriesPayload {
    slot: u8,
    path: String,
    entries: Vec<FsEntryDto>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FsEntryDto {
    name: String,
    dir: bool,
    size: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileBeginPayload {
    slot: u8,
    id: u32,
    name: String,
    size: u64,
    chunks: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileProgressPayload {
    slot: u8,
    id: u32,
    received: u64,
    total: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileRecvPayload {
    slot: u8,
    id: u32,
    name: String,
    saved_path: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RumblePayload {
    slot: u8,
    large: u8,
    small: u8,
}

// ── SendFileResult ────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendFileResult {
    pub id: u32,
}

// ── Received-files directory helper ──────────────────────────────────────────

/// Return the directory where received files are saved ("Pulsar Alınanlar").
///
/// On Android we use `app_data_dir()` (internal private storage, no extra
/// permissions needed). If the path can't be resolved we fall back to a temp dir.
/// The directory is created if it does not exist.
fn received_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join("Pulsar Alınanlar");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// ── Filename sanitization ────────────────────────────────────────────────────

/// Strip path separators so a peer can't write outside the received-files dir.
/// Ported from `desktop-app/src-tauri/src/files.rs::sanitize_filename`.
/// Encode raw image bytes as a `data:` URL (PNG default; JPEG by magic bytes).
/// Inline base64 keeps the mobile crate free of an extra dependency.
fn avatar_data_url(img: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::with_capacity(img.len().div_ceil(3) * 4);
    for c in img.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        b64.push(T[(n >> 18 & 63) as usize] as char);
        b64.push(T[(n >> 12 & 63) as usize] as char);
        b64.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        b64.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    let mime = if img.starts_with(&[0xFF, 0xD8]) { "image/jpeg" } else { "image/png" };
    format!("data:{mime};base64,{b64}")
}

fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '\0'..='\u{1f}'))
        .collect();
    // Must parse as exactly one Normal path component.
    let mut comps = Path::new(&cleaned).components();
    let ok = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
    if ok && !cleaned.is_empty() { cleaned } else { "dosya".into() }
}

/// Save reassembled file chunks to the received-files dir, dedup-suffixed.
/// Returns the final saved path on success. Chunks are written in index order
/// (BTreeMap iteration) without building a contiguous buffer.
fn save_chunks<'a>(
    dir: &Path,
    name: &str,
    chunks: impl Iterator<Item = &'a Vec<u8>>,
) -> Option<PathBuf> {
    use std::io::{ErrorKind, Write as _};
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 1;
    // Atomically reserve the destination path (create_new → no TOCTOU race).
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => break, // reservation created; rename will replace it
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                path = dir.join(format!("{stem} ({n}){ext}"));
                n += 1;
            }
            Err(_) => return None,
        }
    }
    // Write to a sibling .part file first, then rename atomically.
    let tmp = path.with_extension(format!("{}.part", ext.trim_start_matches('.')));
    let write_ok = (|| {
        let mut f = std::fs::File::create(&tmp).ok()?;
        for chunk in chunks {
            f.write_all(chunk).ok()?;
        }
        Some(())
    })();
    if write_ok.is_none() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&path);
        return None;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

// ── Self-avatar (this device's identity image) ───────────────────────────────

/// Where a user-picked avatar photo is stored: `<app_data_dir>/avatar.jpg`.
pub(crate) fn avatar_file_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("avatar.jpg")
}

/// Resolve THIS device's avatar as raw image bytes, honoring `mode`:
/// - `"anonymous"` → `None` (no image).
/// - `"photo"`     → the user-picked image saved by `set_avatar_image`.
/// - else (`"wallpaper"`, legacy `"user"`) → a gradient built from the
///   wallpaper's dominant colors via the plugin (`get_wallpaper_avatar`, no
///   permission). Returns `None` on any failure so callers fall back to none.
pub(crate) fn resolve_self_avatar_png<R: Runtime>(app: &AppHandle<R>, mode: &str) -> Option<Vec<u8>> {
    use tauri_plugin_pulsar_video::PulsarVideoExt as _;
    match mode {
        "anonymous" => None,
        "photo" => std::fs::read(avatar_file_path(app))
            .ok()
            .filter(|b| !b.is_empty()),
        _ => {
            let resp = app.pulsar_video().get_wallpaper_avatar().ok()?;
            if !resp.ok || resp.detail.is_empty() {
                return None;
            }
            base64_decode(&resp.detail)
        }
    }
}

/// Persist a user-picked avatar image to `<app_data_dir>/avatar.jpg` so the
/// connect-time push (`avatar_mode="photo"`) and the Settings preview can read
/// it. `data` is base64 (a `data:` URL prefix, if present, is stripped). An empty
/// `data` deletes the stored image.
///
/// JS: `invoke('set_avatar_image', { data: '<base64>' })`
#[tauri::command]
pub async fn set_avatar_image<R: Runtime>(app: AppHandle<R>, data: String) -> Result<(), String> {
    let path = avatar_file_path(&app);
    if data.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    // Accept either raw base64 or a full `data:image/...;base64,XXXX` URL.
    let b64 = data.rsplit(',').next().unwrap_or(&data);
    let bytes = base64_decode(b64).ok_or_else(|| "invalid base64 image".to_string())?;
    if bytes.is_empty() {
        return Err("empty image".to_string());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, &bytes).map_err(|e| format!("write avatar failed: {e}"))
}

/// Return THIS device's avatar as a ready-to-render `data:` URL for the Settings
/// preview, or `""` when there is none (anonymous, or wallpaper unreadable).
///
/// `mode` overrides the persisted `avatar_mode` so the picker can preview a
/// choice before it is saved; omit it to use the stored config.
///
/// JS: `invoke('self_avatar', { mode: 'wallpaper' })` → `"data:image/jpeg;base64,…"` | `""`
#[tauri::command]
pub async fn self_avatar<R: Runtime>(app: AppHandle<R>, mode: Option<String>) -> Result<String, String> {
    let m = mode
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::config::load_config(&app).avatar_mode);
    Ok(resolve_self_avatar_png(&app, &m)
        .map(|png| avatar_data_url(&png))
        .unwrap_or_default())
}

// ── Route function ────────────────────────────────────────────────────────────

/// Decode `bytes` as a side-channel `DataMsg` and dispatch the matching event.
///
/// Called from the `client.rs` read loop after `media::parse` returns `None`.
pub fn route<R: Runtime>(app: &AppHandle<R>, slot: u8, bytes: &[u8]) {
    let msg = match decode_data(bytes) {
        Some(m) => m,
        None => return,
    };

    match msg {
        DataMsg::Clipboard(text) => {
            let _ = app.emit("clipboard-in", ClipboardInPayload { slot, text });
        }
        DataMsg::Chat(text) => {
            let _ = app.emit("chat-msg", ChatMsgPayload { slot, text });
        }
        DataMsg::PeerName(name) => {
            // The HOST's display name for this session (host→client push).
            let _ = app.emit("peer-name", PeerNamePayload { slot, name });
        }
        DataMsg::Avatar(png) => {
            // The HOST's identity image (host→client push). Base64 → data URL here.
            let _ = app.emit(
                "peer-avatar",
                PeerAvatarPayload { slot, data_url: avatar_data_url(&png) },
            );
        }
        DataMsg::FsEntries { path, entries } => {
            let dtos = entries
                .into_iter()
                .map(|e| FsEntryDto { name: e.name, dir: e.dir, size: e.size })
                .collect();
            let _ = app.emit("fs-entries", FsEntriesPayload { slot, path, entries: dtos });
        }
        DataMsg::FileBegin { id: xfer, name, size, chunks } => {
            // Emit file-begin so the JS transfer queue can show the entry.
            let sane = sanitize_filename(&name);
            let _ = app.emit(
                "file-begin",
                FileBeginPayload { slot, id: xfer, name: sane.clone(), size, chunks },
            );

            // Update the reassembler state.
            let xfers_state = app.state::<XferMap>();
            let mut outer = xfers_state.0.lock().unwrap();
            let slot_map = outer.entry(slot).or_default();

            // Idle sweep.
            let now = Instant::now();
            slot_map.retain(|_, r| now.duration_since(r.last_activity) < XFER_IDLE_TIMEOUT);

            // Concurrency cap: evict least-harmful entry if at the limit.
            if slot_map.len() >= MAX_CONCURRENT_XFERS && !slot_map.contains_key(&xfer) {
                let victim = slot_map
                    .iter()
                    .min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
                    .map(|(k, _)| *k);
                if let Some(v) = victim {
                    slot_map.remove(&v);
                }
            }

            // Merge into existing lazy entry (chunks arrived before FileBegin)
            // or insert a fresh one.
            if let Some(r) = slot_map.get_mut(&xfer) {
                r.name = sane;
                r.expected = Some(chunks);
                r.total_size = size;
                r.last_activity = now;
                // Prune out-of-range pre-buffered chunks.
                r.chunks.retain(|&idx, data| {
                    if idx < chunks {
                        true
                    } else {
                        r.received = r.received.saturating_sub(data.len() as u64);
                        false
                    }
                });
            } else {
                slot_map.insert(
                    xfer,
                    FileReasm {
                        name: sane,
                        expected: Some(chunks),
                        total_size: size,
                        chunks: BTreeMap::new(),
                        received: 0,
                        last_activity: now,
                    },
                );
            }
        }
        DataMsg::FileChunk { id: xfer, index, data } => {
            let xfers_state = app.state::<XferMap>();
            let mut outer = xfers_state.0.lock().unwrap();
            let slot_map = outer.entry(slot).or_default();

            // Lazy placeholder if FileBegin hasn't arrived yet (UDP reorder).
            if !slot_map.contains_key(&xfer) {
                let now = Instant::now();
                slot_map.retain(|_, r| now.duration_since(r.last_activity) < XFER_IDLE_TIMEOUT);
                if slot_map.len() >= MAX_CONCURRENT_XFERS {
                    let victim = slot_map
                        .iter()
                        .min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
                        .map(|(k, _)| *k);
                    if let Some(v) = victim {
                        slot_map.remove(&v);
                    }
                }
                slot_map.insert(
                    xfer,
                    FileReasm {
                        name: String::new(),
                        expected: None,
                        total_size: 0,
                        chunks: BTreeMap::new(),
                        received: 0,
                        last_activity: now,
                    },
                );
            }

            // Accumulate result in local vars so the mutable borrow on slot_map
            // is released before we emit (emitting takes &app, not the lock).
            enum ChunkResult {
                Stored { received: u64, total: u64 },
                Overflow { name: String },
                OutOfRange,
                NoEntry,
            }

            let result = if let Some(r) = slot_map.get_mut(&xfer) {
                r.last_activity = Instant::now();
                let in_range = r.expected.map_or(true, |e| index < e);
                if in_range {
                    let prev_len = r.chunks.get(&index).map(|p| p.len() as u64).unwrap_or(0);
                    let projected = r.received - prev_len + data.len() as u64;
                    if projected > MAX_XFER_BYTES {
                        let name = r.name.clone();
                        ChunkResult::Overflow { name }
                    } else {
                        r.chunks.insert(index, data);
                        r.received = projected;
                        ChunkResult::Stored { received: r.received, total: r.total_size }
                    }
                } else {
                    ChunkResult::OutOfRange
                }
            } else {
                ChunkResult::NoEntry
            };

            // Mutable borrow on slot_map has ended; we can now emit or mutate freely.
            match result {
                ChunkResult::Stored { received, total } => {
                    drop(outer);
                    let _ = app.emit(
                        "file-progress",
                        FileProgressPayload { slot, id: xfer, received, total },
                    );
                }
                ChunkResult::Overflow { name } => {
                    slot_map.remove(&xfer);
                    drop(outer);
                    let _ = app.emit(
                        "file-recv",
                        FileRecvPayload { slot, id: xfer, name, saved_path: String::new() },
                    );
                }
                ChunkResult::OutOfRange | ChunkResult::NoEntry => {
                    // Nothing to do; outer (mutex guard) drops at end of arm.
                }
            }
        }
        DataMsg::FileEnd { id: xfer } => {
            let xfers_state = app.state::<XferMap>();
            let mut outer = xfers_state.0.lock().unwrap();
            let slot_map = match outer.get_mut(&slot) {
                Some(m) => m,
                None => return,
            };

            let Some(r) = slot_map.remove(&xfer) else { return };

            let complete = r.expected.map_or(false, |e| {
                r.chunks.len() == e as usize
                    && (e == 0
                        || (r.chunks.contains_key(&0)
                            && r.chunks.contains_key(&(e - 1))
                            && (0..e).all(|i| r.chunks.contains_key(&i))))
            });

            drop(outer); // release the mutex before the blocking I/O

            // Clone what we need for the spawn_blocking closure.
            let app2 = app.clone();
            let dir = received_dir(app);

            tokio::task::spawn_blocking(move || {
                let saved_path = if complete {
                    save_chunks(&dir, &r.name, r.chunks.values())
                        .and_then(|p| p.to_str().map(str::to_string))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let _ = app2.emit(
                    "file-recv",
                    FileRecvPayload { slot, id: xfer, name: r.name, saved_path },
                );
            });
        }
        DataMsg::Rumble { large, small, .. } => {
            // The `slot` field in `Rumble` is the GAMEPAD slot on the host; the
            // outer `slot` arg is the SESSION slot — use the session slot for the
            // event so JS knows which session it came from.
            let _ = app.emit("rumble", RumblePayload { slot, large, small });
        }
        // All other variants are host-only or handled by another wave.
        _ => {}
    }
}

// ── Helper: get SessionSender for a slot ────────────────────────────────────

/// Look up the `SessionSender` for an active session `slot`.
///
/// Returns `Err` (surfaced to JS) if no session is active on `slot`.
fn get_sender<R: Runtime>(
    app: &AppHandle<R>,
    slot: u8,
) -> Result<pulsar_core::SessionSender, String> {
    app.state::<InputSenders>()
        .0
        .lock()
        .unwrap()
        .get(&slot)
        .cloned()
        .ok_or_else(|| format!("no active session on slot {slot}"))
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Send the local clipboard text to the host on session `slot`.
///
/// JS: `invoke('send_clipboard', { slot: 0, text: '…' })`
#[tauri::command]
pub async fn send_clipboard<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    text: String,
) -> Result<(), String> {
    let sender = get_sender(&app, slot)?;
    send_data_via(&sender, &DataMsg::Clipboard(text))
        .await
        .map_err(|e| format!("send_clipboard failed: {e:?}"))
}

/// Send a chat message to the host on session `slot`.
///
/// JS: `invoke('send_chat', { slot: 0, text: 'Merhaba!' })`
#[tauri::command]
pub async fn send_chat<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    text: String,
) -> Result<(), String> {
    let sender = get_sender(&app, slot)?;
    send_data_via(&sender, &DataMsg::Chat(text))
        .await
        .map_err(|e| format!("send_chat failed: {e:?}"))
}

/// Ask the host to list a remote directory. The reply arrives asynchronously as
/// an `fs-entries` event.
///
/// JS: `invoke('fs_list', { slot: 0, path: 'C:\\Users\\' })`
#[tauri::command]
pub async fn fs_list<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    path: String,
) -> Result<(), String> {
    let sender = get_sender(&app, slot)?;
    send_data_via(&sender, &DataMsg::FsList { path })
        .await
        .map_err(|e| format!("fs_list failed: {e:?}"))
}

/// Ask the host to stream a remote file to us. The file arrives asynchronously
/// via `file-begin`, `file-progress`, and `file-recv` events.
///
/// JS: `invoke('fs_get', { slot: 0, path: 'C:\\Users\\photo.png' })`
#[tauri::command]
pub async fn fs_get<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    path: String,
) -> Result<(), String> {
    let sender = get_sender(&app, slot)?;
    send_data_via(&sender, &DataMsg::FsGet { path })
        .await
        .map_err(|e| format!("fs_get failed: {e:?}"))
}

/// Upload a file to the host via `FileBegin/FileChunk/FileEnd` messages.
///
/// `bytes` is the file content encoded as **standard Base64** (no wrapping).
/// Returns `{ id }` where `id` is the transfer id used in the wire protocol.
///
/// JS: `invoke('send_file', { slot: 0, name: 'photo.png', bytes: '<base64>' })`
#[tauri::command]
pub async fn send_file<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    name: String,
    bytes: String, // base64-encoded file content
) -> Result<SendFileResult, String> {
    let raw = base64_decode(&bytes)
        .ok_or_else(|| "send_file: invalid base64 payload".to_string())?;

    let sender = get_sender(&app, slot)?;

    // Split into UPLOAD_CHUNK_BYTES chunks.
    let chunks_data: Vec<Vec<u8>> = raw.chunks(UPLOAD_CHUNK_BYTES).map(|c| c.to_vec()).collect();
    let chunk_count = chunks_data.len() as u32;
    let total_size = raw.len() as u64;

    // Use a simple incrementing id derived from the current time to be unique.
    // A real implementation would use a counter, but we need no shared state here.
    let id: u32 = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        t ^ (slot as u32).wrapping_shl(16)
    };

    // FileBegin
    send_data_via(
        &sender,
        &DataMsg::FileBegin {
            id,
            name: name.clone(),
            size: total_size,
            chunks: chunk_count,
        },
    )
    .await
    .map_err(|e| format!("send_file FileBegin failed: {e:?}"))?;

    // FileChunks
    for (index, data) in chunks_data.into_iter().enumerate() {
        send_data_via(&sender, &DataMsg::FileChunk { id, index: index as u32, data })
            .await
            .map_err(|e| format!("send_file FileChunk[{index}] failed: {e:?}"))?;
    }

    // FileEnd
    send_data_via(&sender, &DataMsg::FileEnd { id })
        .await
        .map_err(|e| format!("send_file FileEnd failed: {e:?}"))?;

    Ok(SendFileResult { id })
}

// ── Base64 decoder (no external dep — base64 is not yet in Cargo.toml) ───────

/// Decode standard Base64 (RFC 4648) without depending on an external crate.
/// Returns `None` on invalid input.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    // Strip padding; we'll handle trailing bytes manually.
    let clean: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in &clean {
        let v: u32 = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a') as u32 + 26,
            b'0'..=b'9' => (b - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None, // invalid character
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}
