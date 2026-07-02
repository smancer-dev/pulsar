//! Serde-(de)serializable payload + event types passed between the SvelteKit UI
//! and the Tauri command layer (command args/returns and `emit`-ed events).

use serde::Serialize;

/// Auto-connect target handed to the frontend (id/ip + optional one-time password,
/// plus the session mode and the host app/game to launch). `mode` is `game` or
/// `remote` (default `remote`); `app` is the host app/game id-or-name to launch
/// in game mode (empty / `Desktop` = stream the whole desktop, launch nothing).
#[derive(Clone, Serialize)]
pub(crate) struct AutoConnect {
	pub(crate) id: String,
	pub(crate) pw: String,
	pub(crate) mode: String,
	pub(crate) app: String,
	/// `--nofullscreen`: a `--connect` kiosk launch normally goes fullscreen so the
	/// host fills the screen with no chrome. With this flag the auto-connect session
	/// starts windowed instead (debugging / dev test loops / embedded use).
	pub(crate) nofullscreen: bool,
}

#[derive(Serialize)]
pub(crate) struct ConnInfo {
	pub(crate) transport: String,
	pub(crate) peer: String,
}

#[derive(Serialize)]
pub(crate) struct ControllerInfo {
	/// Positional index in the backend list (stable within a session).
	pub(crate) index: u32,
	/// Stable device key: gilrs uuid bytes as a lowercase hex string.
	/// Used as the key in the UI's `controllerOrder` player-slot permutation.
	pub(crate) uuid: String,
	/// OS/driver-reported name, e.g. "Wireless Controller".
	pub(crate) name: String,
	/// Detected family as a stable tag, e.g. "Ds4" / "Xbox".
	pub(crate) kind: String,
	/// Human label, e.g. "DualShock 4".
	pub(crate) label: String,
	/// Connected + forwardable right now.
	pub(crate) connected: bool,
	/// Battery charge 0..100, or `None` for a wired pad / unknown.
	pub(crate) battery: Option<u8>,
}

/// An event about a client session, emitted to the host UI as `session`.
///
/// `sid` is the SESSION id (the same key the host's `active`/`incoming` maps use):
/// the connections window keys its rows by sid so one client DEVICE can hold several
/// concurrent sessions (couch co-op / split panes) — `peer` groups them for display.
/// 0 for the pre-accept `rejected` events (no session id assigned yet there).
#[derive(Clone, Serialize)]
pub(crate) struct SessionEvent {
	pub(crate) kind: String,
	pub(crate) peer: String,
	pub(crate) sid: u64,
	pub(crate) detail: String,
}

/// A side-channel text payload (clipboard / chat) tagged with the peer it came
/// from. Emitted to the host UI (`clipboard` / `host-chat`) or, with `peer`
/// holding the play id, to the client UI (`data-clip` / `chat-msg`).
#[derive(Clone, Serialize)]
pub(crate) struct DataPayload {
	pub(crate) peer: String,
	pub(crate) text: String,
}

/// A peer's identity image, emitted as `peer-avatar`. Mirrors [`DataPayload`]'s
/// addressing: `peer` is the connection's peer id on the host side, or the play id
/// (as a string) on the client side. `data_url` is a ready-to-render
/// `data:image/png;base64,…` URL (built in Rust so the webview never touches raw bytes).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AvatarPayload {
	pub(crate) peer: String,
	pub(crate) data_url: String,
}

/// Emitted to the host UI (`file-recv`) when a file transfer from a client
/// finishes (or fails the gap check). Also emitted to the CLIENT UI when a
/// file-manager download lands (with `peer` holding the play id, like
/// [`DataPayload`]'s client-side addressing).
/// `xfer_id` is the host-assigned transfer id (u32 from `next_transfer_id`),
/// serialised as `xferId` so the client can key pending completions by transfer
/// id rather than by filename — preventing a timed-out same-name download from
/// draining a different in-flight download's concurrency slot (C21).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FilePayload {
	pub(crate) peer: String,
	pub(crate) name: String,
	pub(crate) bytes: u64,
	pub(crate) ok: bool,
	pub(crate) xfer_id: u32,
}

/// Emitted to the CLIENT UI (`file-begin`) when a `FileBegin` datagram arrives
/// for a file-manager download — signals that the host has started streaming the
/// file and the concurrency slot should not be released on the short wall-clock
/// timeout (the transfer is legitimately in flight). `peer` = play id as string.
/// `xfer_id` is the host-assigned transfer id so the client can map this
/// `file-begin` to the queued `download()` call for that filename (C21 fix).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileBeginPayload {
	pub(crate) peer: String,
	pub(crate) name: String,
	pub(crate) xfer_id: u32,
}

/// A host directory listing (the file panel's right pane), emitted to the client
/// UI as `fs-entries` — addressed by play id like the other client-side payloads.
#[derive(Clone, Serialize)]
pub(crate) struct FsEntriesPayload {
	pub(crate) id: u64,
	pub(crate) path: String,
	pub(crate) entries: Vec<pulsar_core::service::FsEntry>,
}

/// Emitted to the CLIENT UI (`auth-prompt`) when a host asks for a password — the
/// UI shows a prompt and replies via `submit_password(req, ...)`.
#[derive(Clone, Serialize)]
pub(crate) struct AuthPrompt {
	pub(crate) req: u64,
	pub(crate) peer: String,
}

/// Returned to the UI after a successful connect: the play id (for this tab), how
/// the link was made, and the loopback WebSocket port the webview renders from.
#[derive(Serialize)]
pub(crate) struct PlayInfo {
	pub(crate) id: u64,
	pub(crate) transport: String,
	pub(crate) ws_port: u16,
	/// Loopback WebSocket port the webview reads **audio** (Opus) from; 0 when audio
	/// isn't being received.
	pub(crate) audio_ws_port: u16,
	/// True when the host is this same machine (loopback P2P) — control would be a
	/// cursor feedback loop, so the UI disables it.
	pub(crate) local: bool,
	/// True when the native ffplay renderer was launched (the webview canvas is not
	/// the video surface for this session).
	pub(crate) native: bool,
	/// True when the Linux single-surface renderer is active: video is in a GtkGLArea
	/// *behind* this same webview, so the session screen must be transparent to show it.
	pub(crate) embedded: bool,
	/// The HOST's validated stream caps (QueryStreamCaps): codecs + encoder backends it
	/// can really emit. The session menu disables/hides options outside these lists.
	/// Empty = unknown (old host / timeout) — the UI then only trusts "auto".
	pub(crate) host_codecs: Vec<String>,
	pub(crate) host_encoders: Vec<String>,
	/// The host's streamable monitors (QueryStreamCaps): primary at index 0. The
	/// session menu lists these so the user can pick which screen to view. Empty =
	/// the host advertised none (old host / Wayland / single-monitor) → no picker.
	pub(crate) host_displays: Vec<pulsar_core::service::DisplayInfo>,
	/// This client's own decodable codecs (probe), for completeness/diagnostics.
	pub(crate) client_codecs: Vec<String>,
}

/// A real connection milestone for the Connecting screen, emitted as `conn-phase`
/// (keyed by the target so the right tab picks it up).
#[derive(Clone, Serialize)]
pub(crate) struct ConnPhase {
	pub(crate) target: String,
	pub(crate) transport: String,
}

/// Round-trip time (ms) for a play session, emitted as the `play-rtt` event.
#[derive(Clone, Serialize)]
pub(crate) struct PlayRtt {
	pub(crate) id: u64,
	pub(crate) rtt: f64,
}

/// The host's encode summary for a session, emitted as the `host-stats` event.
#[derive(Clone, Serialize)]
pub(crate) struct PlayStats {
	pub(crate) id: u64,
	pub(crate) label: String,
}

/// A reverse-direction request from a controlling peer, emitted as `reverse-request`
/// (the host UI connects back to `id` to swap roles).
#[derive(Clone, Serialize)]
pub(crate) struct ReverseReq {
	pub(crate) id: String,
}

/// Real client-side video stats for the perf panel, sourced from mpv (the native path has
/// no WebCodecs sink to read fps/decode-ms from).
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
// Constructed only by the Linux mpv stats pollers; unused on other targets.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct PlayVStats {
	pub(crate) id: u64,
	pub(crate) fps: f64,
	pub(crate) drops: i64,
	pub(crate) mbps: f64,
	/// Output/render latency (ms): real `render_ms()` on the single-surface path, the
	/// mpv `vo-delay` proxy on the `--wid` path, or 0 when unavailable (never fabricated).
	/// Serializes as `decodeMs` for the UI.
	pub(crate) decode_ms: f64,
}

/// A Pulsar device found on the local network (via the multicast beacon).
#[derive(Serialize)]
pub(crate) struct LanDevice {
	/// Grouped relay id (e.g. `482 913 056`), or empty if the peer is relay-less.
	pub(crate) id: String,
	/// Whether `id` is usable to connect via the normal flow.
	pub(crate) has_id: bool,
	pub(crate) name: String,
	/// `ip:port` the peer announced.
	pub(crate) addr: String,
	/// `windows` / `linux` / `macos`.
	pub(crate) platform: String,
}

#[derive(Serialize)]
pub(crate) struct ScannedApp {
	pub(crate) name: String,
	pub(crate) path: String,
}
