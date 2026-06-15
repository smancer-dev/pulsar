//! Shared application state and the long-lived value types it holds.
//!
//! `AppState` is the Tauri-managed state; `PlaySession` / `Restream` / `StreamCfg`
//! are the per-session bookkeeping types referenced across the command modules.

use std::collections::HashMap;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use pulsar_core::config::Config;
use pulsar_core::service::{DataMsg, InputEvent, QualityPref};
use pulsar_core::Discovery;
use pulsar_core::Node;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::process::HostGame;
use crate::viewer;

#[derive(Default)]
pub(crate) struct AppState {
	pub(crate) node: Mutex<Option<Arc<Node>>>,
	pub(crate) config: Mutex<Config>,
	/// LAN auto-discovery beacon (announces this device + collects peers on the
	/// local network). Started on `go_online`, replaced on reconnect.
	pub(crate) discovery: Mutex<Option<Arc<Discovery>>>,
	/// Games this host publishes to clients (set from the UI via `publish_games`).
	pub(crate) games: Arc<Mutex<Vec<HostGame>>>,
	/// Host stream settings (resolution/fps/bitrate/encoder/display).
	pub(crate) stream_cfg: Arc<Mutex<StreamCfg>>,
	/// Saved windowed geometry while fullscreen, restored on exit.
	pub(crate) fs_geom: Mutex<Option<(tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>)>>,
	/// Startup-probed local capabilities (encoders + decoders); None until the
	/// background probe finishes. Re-probed on every launch (Moonlight model).
	pub(crate) local_caps: Mutex<Option<crate::caps::LocalCaps>>,
	/// One-time password a client must present to connect (shown in the host UI).
	/// Generated on `go_online`; empty means "not online yet".
	pub(crate) password: Arc<Mutex<String>>,
	/// Active outbound remote-play sessions, keyed by play id (this client can be
	/// connected to several hosts at once — one per tab).
	pub(crate) plays: Arc<Mutex<HashMap<u64, PlaySession>>>,
	/// Play ids whose in-session overlay is currently OPEN. The evdev capture's
	/// SUSPENDED latch is global while the overlay is per-tab, so the latch is
	/// derived from this set (suspend ⇔ non-empty): a tab closed with its overlay
	/// open, or a second tab opened next to it, can no longer strand the capture
	/// in "suspended" forever (= connected but uncontrollable — seen live Pi→PC).
	pub(crate) overlay_open: Arc<Mutex<std::collections::HashSet<u64>>>,
	/// Monotonic id for play sessions.
	pub(crate) next_play: Arc<AtomicU64>,
	/// Pending Allow/Deny approval requests (request id → decision sender),
	/// resolved by the approval popup via `respond_request`.
	pub(crate) pending: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
	/// Monotonic id for approval requests / popup windows.
	pub(crate) next_req: Arc<AtomicU64>,
	/// Pending client-side password prompts (req id → password sender), resolved by
	/// the UI via `submit_password`. `None` payload means the user cancelled.
	pub(crate) pw_pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
	/// Monotonic id for client password prompts.
	pub(crate) next_auth: Arc<AtomicU64>,
	/// Incoming (host-side) sessions, keyed by the connected peer's id → its session
	/// id + a signal to kick them. The session id lets a stale session's teardown
	/// avoid evicting a same-peer reconnection that already replaced this entry.
	pub(crate) incoming: Arc<Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>>,
	/// Host → client side-channel senders, keyed by the connected peer's id (paired
	/// with the session id, as above). Lets the host push chat replies / clipboard to
	/// a connected client from the UI.
	pub(crate) host_out: Arc<Mutex<HashMap<String, (u64, tokio::sync::mpsc::Sender<DataMsg>)>>>,
	/// Restore token for the Wayland ScreenCast portal, so the "share your screen"
	/// dialog only appears the first time.
	#[cfg(target_os = "linux")]
	pub(crate) restore_token: Arc<Mutex<Option<String>>>,
	/// The serve loop (accept-incoming task) spawned by `go_online`. Aborted at the
	/// top of each `go_online` so a reconnect/settings change doesn't leak the prior
	/// node + its serve loop (which keeps an Arc<Node> alive forever).
	pub(crate) serve_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
	/// The independently-spawned per-session tasks (one per accepted incoming
	/// connection). Each owns a strong `Arc<Node>` (its `Session` + `SessionSender`),
	/// so aborting only `serve_task` would leave a live session pinning the old node
	/// alive — its UDP socket stays bound (a re-bind to a pinned port then fails) and
	/// its relay heartbeat keeps pinging. `go_online` aborts these before dropping
	/// `state.node` so the old node reaches strong-count 0 and its loops exit.
	pub(crate) session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
	/// Active inbound connections, keyed by the connected peer's id. Drives the
	/// dedicated "connections" management window (`connections.rs`): the window lists
	/// these, and the per-connection `mode` decides whether a new connection brings the
	/// window forward (Remote) or opens it hidden (Game — don't disrupt the streamed
	/// fullscreen game / leak into its stream). `sid` is the session id, used so a stale
	/// session's teardown doesn't evict a same-peer reconnection's newer entry.
	pub(crate) active: Arc<Mutex<HashMap<String, ConnInfo>>>,
	/// Peer identity decorations pushed over sessions (`DataMsg::PeerName`/`Avatar`),
	/// keyed by peer id: (display name, avatar data-URL). The connections window's
	/// snapshot reads this so rows opened AFTER the push still show who connected.
	pub(crate) peer_meta: Arc<Mutex<HashMap<String, (Option<String>, Option<String>)>>>,
	/// Host-side chat log (peer id, text, me?) for the connections window's message
	/// modal: events broadcast only to LIVE windows, so a message arriving while the
	/// window is closed would otherwise vanish — the modal seeds from this backlog.
	pub(crate) chat_log: Arc<Mutex<Vec<(String, String, bool)>>>,
	/// The node's ACTUAL bound UDP port (0 = not online yet). Set on `go_online`
	/// (also emitted as the `node-port` event); the Home screen shows it next to the
	/// local IP so a copy-able direct `ip:port` target is always visible.
	pub(crate) node_port: std::sync::atomic::AtomicU16,
	/// When `true` the main window QUITS on close instead of hiding to the tray.
	/// Synced from the UI's `ui.tray` setting via `set_tray` (tray_disabled = !ui.tray).
	/// Default `false` (tray enabled) — preserves the existing behavior on first launch.
	pub(crate) tray_disabled: AtomicBool,
}

/// Whether an inbound connection is a remote-desktop or a game-streaming session.
/// Set from `StreamReq.game_mode` once the client requests its stream.
#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConnMode {
	Remote,
	Game,
}

/// Per-connection bookkeeping for the connections window.
pub(crate) struct ConnInfo {
	/// This session's id (matches the `incoming`/`host_out` sid) for teardown guarding.
	pub(crate) sid: u64,
	/// Connection start, epoch milliseconds (host clock == the window's `Date.now()`).
	pub(crate) since_ms: u64,
	pub(crate) mode: ConnMode,
	/// "Sadece izleme": the host user revoked this client's CONTROL — its input is
	/// dropped before injection while the stream keeps running (AnyDesk-style).
	pub(crate) view_only: bool,
}

/// A live stream-reconfiguration command from the session menu. Each one merges into
/// the session's current stream state and triggers a re-request (host restarts ffmpeg).
pub(crate) enum Restream {
	/// New resolution (width, height); 0×0 = host default.
	Resolution(u32, u32),
	/// New host encoder ("auto"/"nvenc"/"amf"/"qsv"/"vaapi"/"videotoolbox"/"software").
	Encoder(String),
	/// New video codec ("h264"/"h265"/"av1").
	Codec(String),
	/// New frame rate; 0 = host default.
	Fps(u32),
	/// New target bitrate in kbit/s; 0 = host default.
	Bitrate(u32),
	/// New quality/perf bias (latency ↔ quality).
	Quality(QualityPref),
	/// Audio toggles: (transmit_audio, mute_host).
	Audio(bool, bool),
	/// New host monitor to capture (index into `StreamCaps::displays`, 0 = primary).
	Display(u32),
}

/// Stdin-only renderer state re-pushed after a codec-switch renderer respawn: a
/// fresh `pulsar-render` process starts from its built-in defaults, so everything
/// previously configured over stdin must be replayed (the `caps …` line is kept
/// separately in [`PlaySession::caps_line`]). `None` = never set → nothing to replay.
#[derive(Clone, Default)]
pub(crate) struct RenderSeed {
	/// Parsec-style overlay-open button visibility ("ovbtn 0|1").
	pub(crate) ovbtn: Option<bool>,
	/// …and its dragged position in egui points ("ovbtnpos <x> <y>").
	pub(crate) ovbtn_pos: Option<(f32, f32)>,
	/// Always-on mini stats HUD ("statshud 0|1").
	pub(crate) statshud: Option<bool>,
	/// Frame pacing ("pace 0|1") — `--pace` only covers the spawn-time default; a
	/// live toggle after spawn is recorded here.
	pub(crate) pace: Option<bool>,
	/// View-fit mode ("fit <fit|stretch|original>"). The renderer owns fit changes
	/// (`ov set fit` flows outward only), so this has no app-side writer yet; the
	/// respawn re-push is wired for when one mirrors it back.
	pub(crate) fit: Option<String>,
	/// Audio truth: (transmit, mute_host, mic_on) → "audio tx=… mute=… mic=…".
	pub(crate) audio: Option<(bool, bool, bool)>,
}

/// One active outbound remote-play session (one connected-host tab): the local
/// video relay, the input forwarding channel, and a flag held open until stopped.
pub(crate) struct PlaySession {
	pub(crate) viewer: viewer::Viewer,
	pub(crate) input_tx: tokio::sync::mpsc::Sender<InputEvent>,
	/// Side-channel sender (clipboard / chat / file / mic audio → host).
	pub(crate) data_tx: tokio::sync::mpsc::Sender<DataMsg>,
	/// Running mic recorder (`parecord`), if the user enabled the microphone.
	pub(crate) mic: Arc<Mutex<Option<Child>>>,
	pub(crate) running: Arc<AtomicBool>,
	/// Live stream re-configuration from the session menu (resolution / encoder): each
	/// message re-requests the stream so the host restarts ffmpeg with the new setting.
	pub(crate) restream_tx: tokio::sync::mpsc::Sender<Restream>,
	/// Native renderer (ffplay) child, when the native player is in use — killed on
	/// stop so the fullscreen window closes.
	pub(crate) ffplay: Option<Child>,
	/// Native audio player (ffmpeg→PulseAudio) child on Linux, where WebKitGTK can't play the
	/// Opus stream via WebCodecs. `None` on Windows/macOS (webview audio) — killed on stop.
	pub(crate) audio_native: Option<Child>,
	/// Unix socket of the embedded `--wid` mpv's JSON IPC server (deterministic per-id
	/// path, `temp_dir/pulsar-mpv-<id>.sock`). Drives BOTH the overlay pause (Faz 3) and
	/// the stats poller (Faz 4). `None` on Windows (ffplay) and the single-surface path.
	#[allow(dead_code)]
	pub(crate) mpv_ipc: Option<std::path::PathBuf>,
	/// Every SDP temp file this session has written (`write_sdp` for video,
	/// `spawn_native_audio` for audio). The port-based filenames are essentially unique
	/// per session (ephemeral ports) and a fresh one is written on every live codec /
	/// monitor switch, so they would otherwise pile up in `temp_dir` for the life of the
	/// machine. `stop_stream` `remove_file`s each on teardown.
	pub(crate) sdp_files: Arc<Mutex<Vec<std::path::PathBuf>>>,
	/// Faz 3 overlay (Linux `--wid` path): the SDP + window id needed to respawn the mpv
	/// child after the overlay killed it. Killing mpv on overlay-open destroys its window so
	/// the webview menu (which mpv otherwise composites over) becomes visible; respawning on
	/// close resumes the stream (the host keeps sending to the same port). `None` elsewhere.
	#[allow(dead_code)]
	pub(crate) mpv_sdp: Option<std::path::PathBuf>,
	#[allow(dead_code)]
	pub(crate) mpv_wid: Option<u64>,
	/// Native zero-copy renderer (`pulsar-vidsink`) binary path, when it — not mpv — is the
	/// active Linux renderer. `set_overlay` respawns this on overlay-close (instead of mpv);
	/// `None` → mpv/ffplay path.
	#[allow(dead_code)]
	pub(crate) vidsink_bin: Option<String>,
	/// Current vidsink display rotation (degrees CW) — set from the host's DisplayRotation
	/// (or the PULSAR_ROTATE override); used to respawn the vidsink (overlay close / rotation
	/// change) at the same orientation.
	#[allow(dead_code)]
	pub(crate) vidsink_rotate: u32,
	/// Native overlay renderer (`pulsar-render`) child — the egui overlay drawn over the video
	/// (Linux). `set_overlay` signals it SIGUSR1 (open) / SIGUSR2 (close); killed on stop.
	#[allow(dead_code)]
	pub(crate) render_child: Option<Child>,
	/// `pulsar-render`'s stdin, shared with the vidsink-stats thread which writes live
	/// `stat <fps> <lat> <dec> <mbps>` lines for the overlay HUD. `None` → no overlay process.
	#[allow(dead_code)]
	pub(crate) render_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
	/// The renderer's RTP video port (its SDP points here) — needed to rewrite the
	/// SDP + respawn the renderer on a live codec switch.
	pub(crate) video_port: u16,
	/// Whether this play session runs in game mode (renderer --mode on respawn).
	pub(crate) game_mode: bool,
	/// The last `caps …` stdin line sent to the renderer (re-sent after a respawn so
	/// the egui overlay keeps its filtered lists + seeded selections).
	pub(crate) caps_line: Arc<Mutex<String>>,
	/// Stdin-only renderer state (see [`RenderSeed`]) — re-sent after a codec-switch
	/// respawn, which otherwise resets the fresh renderer to its defaults.
	pub(crate) render_seed: Arc<Mutex<RenderSeed>>,
}

/// Host-side stream settings pushed from the UI.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub(crate) struct StreamCfg {
	pub(crate) width: u32,
	pub(crate) height: u32,
	pub(crate) fps: u32,
	pub(crate) bitrate_kbps: u32,
	/// `auto` / `nvenc` / `vaapi` / `qsv` / `videotoolbox` / `software`
	pub(crate) encoder: String,
	/// `auto` / `x11grab` / `kmsgrab` / `gdigrab` / `avfoundation`
	pub(crate) capture: String,
	pub(crate) display: String,
	pub(crate) vaapi_device: String,
}

impl Default for StreamCfg {
	fn default() -> Self {
		Self {
			width: 1920,
			height: 1080,
			fps: 60,
			bitrate_kbps: 30_000,
			encoder: "auto".into(),
			capture: "auto".into(),
			display: std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into()),
			vaapi_device: "/dev/dri/renderD128".into(),
		}
	}
}
