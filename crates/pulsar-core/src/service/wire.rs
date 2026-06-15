//! Wire data types exchanged over a session: game listings, control input
//! events, stream requests, and the bidirectional side-channel `DataMsg`.

use serde::{Deserialize, Serialize};

use crate::audio::ChannelLayout;
use crate::input::GamepadState;

/// A game/app the host exposes to clients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameInfo {
	pub id: String,
	pub title: String,
	pub kind: String,
}

/// One control event a client sends to drive the host (mouse / keyboard / pad).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
	/// Controller state (game streaming).
	Gamepad(GamepadState),
	/// Absolute pointer position, normalized 0..1 within the streamed screen.
	PointerMotion { x: f64, y: f64 },
	/// Relative pointer movement (raw mouse deltas) — used by the native renderer,
	/// where there's no canvas to read absolute positions from (and what games want).
	PointerRelative { dx: f64, dy: f64 },
	/// Mouse button (0=left, 1=right, 2=middle) pressed/released.
	PointerButton { button: u8, down: bool },
	/// Smooth scroll delta.
	Scroll { dx: f64, dy: f64 },
	/// Keyboard evdev keycode pressed/released.
	Key { code: u32, down: bool },
	/// A resolved Unicode character to type verbatim (layout-independent / WYSIWYG). The
	/// client mapped a keypress through ITS OWN keyboard layout (xkb) to this exact codepoint,
	/// so the host inserts it regardless of the host's active layout (Windows KEYEVENTF_UNICODE).
	/// Sent for printable keys with no Ctrl/Alt/Win held; shortcuts (Ctrl+C) + non-text keys
	/// (Enter, arrows, F-keys, modifiers) still come as `Key` so VK-level semantics are preserved.
	Char(char),
}

/// How the client wants the host to bias the encode: minimize latency, balance,
/// or maximize quality. The UI exposes only `latency`/`quality`; `Balanced` is
/// the serde default so a request built by an older client (which omits the
/// field) folds into the host's existing `game_mode`-driven behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum QualityPref {
	/// Lowest latency (host uses its low-latency encode preset).
	Latency,
	/// Balanced — defers to the host's `game_mode` (no behavior change for old clients).
	#[default]
	Balanced,
	/// Highest quality (host disables the low-latency preset).
	Quality,
}

/// What a host can actually stream — the `QueryStreamCaps` reply. Both lists are
/// validated (one-frame probe) and preference-ordered; vocabularies match the UI/wire
/// strings (`h265`/`h264`/`av1`; `nvenc`/`qsv`/`vaapi`/`videotoolbox`/`amf`/
/// `mediafoundation`/`vulkan`/`software`). The client uses them to resolve its "auto"
/// codec and to disable session-menu options the host can't honor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamCaps {
	pub codecs: Vec<String>,
	pub encoders: Vec<String>,
	/// Transport features this host supports (see [`crate::service::media`]):
	/// `"mos"` = media-over-session (single-socket RTP), `"nack"` = it honors
	/// `MediaNack` retransmit requests. `#[serde(default)]` — an old host omits
	/// the field, so the client sees no features and uses the legacy direct flows.
	#[serde(default)]
	pub features: Vec<String>,
	/// Monitors the host can stream, best-first with the PRIMARY at index 0 (the
	/// default the client streams when it sends `StreamReq::display_idx = 0`). The
	/// session menu lists these so the user can pick another. `#[serde(default)]`
	/// (empty) — an old host omits it; the client then shows no monitor picker and
	/// streams the host default. The `idx` is what travels back in `display_idx`.
	#[serde(default)]
	pub displays: Vec<DisplayInfo>,
}

/// One host monitor advertised in [`StreamCaps::displays`]. `idx` is the selector the
/// client echoes in [`StreamReq::display_idx`] (0 = primary); `name` is human-facing
/// ("DISPLAY1" / "HDMI-1"); `width`/`height` are the monitor's pixel size; `primary`
/// marks the host's main display.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
	pub idx: u32,
	pub name: String,
	pub width: u32,
	pub height: u32,
	#[serde(default)]
	pub primary: bool,
}

/// A client's request to start receiving a video stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamReq {
	/// UDP port the client is listening on for the media stream.
	pub port: u16,
	/// Codec the client prefers (`h264` / `h265` / `av1`).
	pub codec: String,
	/// Encoder hint for the host (`auto` / `nvenc` / `vaapi` / …).
	pub encoder: String,
	/// Desired stream resolution; `0` means "use the host's configured size". Lets
	/// the client pick quality from the session menu and change it live by
	/// re-requesting the stream.
	pub width: u32,
	pub height: u32,
	/// Desired frame rate; `0` means "use the host's configured fps". Like resolution,
	/// the client can pick it from the session menu and change it live.
	#[serde(default)]
	pub fps: u32,
	/// UDP port the client is listening on for the **audio** (Opus RTP) stream.
	/// `0` means the client doesn't want audio. `#[serde(default)]` so a client
	/// built before audio existed still deserializes on a newer host.
	#[serde(default)]
	pub audio_port: u16,
	/// Whether the host should transmit its audio to the client (session-menu toggle).
	/// Defaults true so a client that omits it still gets audio.
	#[serde(default = "default_true")]
	pub transmit_audio: bool,
	/// Whether the host should mute its own local speakers while streaming (session-menu
	/// toggle; game mode defaults this on so the sound moves entirely to the player).
	#[serde(default)]
	pub mute_host: bool,
	/// The client entered **game mode** for this session, which makes the host move
	/// audio entirely to the player (see [`crate::audio::AudioSettings::policy`]).
	#[serde(default)]
	pub game_mode: bool,
	/// Desired bitrate in kbit/s; `0` means "use the host's configured bitrate".
	/// Like resolution/fps, the client can pick it from the session menu and
	/// change it live by re-requesting the stream. Appended (with `#[serde(default)]`)
	/// for additive wire compat — old clients deserialize to `0`.
	#[serde(default)]
	pub bitrate_kbps: u32,
	/// How the host should bias the encode (latency vs. quality). Appended with a
	/// serde default of [`QualityPref::Balanced`], so an older client that omits it
	/// keeps today's `game_mode`-driven behavior on the host.
	#[serde(default)]
	pub quality: QualityPref,
	/// Request 10-bit **HDR** encode (BT.2020/PQ). `#[serde(default)]` (false) for wire compat
	/// — only honored when the chosen encoder+codec actually validates for it.
	#[serde(default)]
	pub hdr: bool,
	/// Request **4:4:4** chroma (no subsampling; sharper text for remote desktop).
	#[serde(default)]
	pub yuv444: bool,
	/// Codecs this CLIENT can decode (startup-probe result, best-first), already
	/// PRUNED of any codec whose decoder is incompatible with an encoder family the
	/// host actually has (e.g. rkmpp-HEVC × native-NVENC). The host clamps its codec
	/// fallback to this set so it never streams something the client can't show — its
	/// auto-degradation can't land on a codec the client deliberately excluded.
	/// `#[serde(default)]` (empty = unknown/old client → no clamping).
	#[serde(default)]
	pub decode_codecs: Vec<String>,
	/// Carry the RTP media INSIDE this session (single external socket) instead of
	/// separate plain-UDP flows to `port`/`audio_port` — see [`crate::service::media`].
	/// Only set when the host advertised the `mos` feature; `#[serde(default)]`
	/// (false) keeps the legacy direct flows for old peers.
	#[serde(default)]
	pub media_over_session: bool,
	/// The client can draw the host pointer ITSELF from the cursor side-channel
	/// ([`DataMsg::CursorPos`]/[`CursorShape`]) — so the host may capture without a
	/// hardware cursor in the frame (the KMS zero-copy path) and stream the pointer
	/// out-of-band. Only the native renderer sets this; the webview client leaves it
	/// false (its host bakes the cursor into the video). `#[serde(default)]` (false)
	/// keeps the embedded-cursor behavior for old peers.
	#[serde(default)]
	pub cursor_external: bool,
	/// Which host monitor to capture, as an index into [`StreamCaps::displays`]
	/// (`0` = the host's primary/default monitor). The client picks it from the
	/// session menu and changes it live by re-requesting the stream — exactly like
	/// resolution/fps. `#[serde(default)]` (0) keeps the primary for old clients.
	#[serde(default)]
	pub display_idx: u32,
	/// The audio **channel layout** the client requests (Stereo / 5.1 / 7.1). The host
	/// negotiates it against its own configured/capturable layout (it never emits more
	/// channels than it actually captures) and echoes the resolved layout back via the
	/// encode stats. On Windows host-silent this also drives the virtual sink's device
	/// format (so the redirected loopback opens at the right channel count). Appended
	/// with a serde default of [`ChannelLayout::Stereo`] for additive wire compat — an
	/// older client that omits it negotiates stereo, the universally-decodable default.
	#[serde(default)]
	pub audio_layout: ChannelLayout,
}

fn default_true() -> bool {
	true
}

/// One entry of a host directory listing (the `FsEntries` reply): name only (no
/// path components), whether it's a directory, and the byte size for files
/// (0 for directories). Listings are sorted dirs-first, then alphabetically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FsEntry {
	pub name: String,
	pub dir: bool,
	pub size: u64,
}

// ---------------------------------------------------------------------------
// FileEnd cross-version compatibility helper
// ---------------------------------------------------------------------------
// `FileEnd` was originally a UNIT variant (`"FileEnd"` on the wire).  The
// round-1 multi-transfer patch promoted it to a STRUCT variant
// (`{"FileEnd":{"id":0}}`).  serde_json cannot decode the bare-string form
// into a struct variant and vice-versa, so a new peer receiving an old
// `"FileEnd"` silently drops it (dec() → None) and the transfer never
// completes.
//
// Fix: keep the struct variant for Serialize (so new→new transfers carry the
// id), but implement a custom Deserialize on DataMsg (via the shadow enum
// DataMsgWire + From conversion) that handles the FileEnd variant to accept
// BOTH the legacy unit form ("FileEnd") and the new struct form
// ({"FileEnd":{"id":N}}).
//
// serde_json's UnitVariantAccess (for bare-string variants) returns an error
// from every method except unit_variant().  So we cannot use the standard
// EnumAccess / VariantAccess visitor approach to handle both forms in one
// visitor -- the unit and struct paths are driven by serde_json before we can
// inspect them.  Instead the DataMsg::Deserialize impl captures the raw JSON
// token cheaply via serde_json::value::RawValue, checks whether it is the
// bare string "FileEnd", and short-circuits that to DataMsg::FileEnd{id:0}
// directly.  For every other variant it forwards to DataMsgWire which is
// deserialized in a single pass (no intermediate Value, no per-byte allocation
// for Vec<u8> fields — critical for the hot mic-Audio path at ~50 fps).

/// Internal shadow used only for deserialization.  Identical to `DataMsg`
/// except `FileEnd` is also a struct variant with `id` defaulting to 0
/// (no special compat needed here -- compat is handled in DataMsg::Deserialize
/// before the shadow enum is deserialized).
///
/// # IMPORTANT — keep in sync with [`DataMsg`]
///
/// This enum and the [`From`]`<DataMsgWire> for DataMsg` impl below are the
/// ONLY place that governs which variants are actually decodable at runtime.
/// They are NOT checked by the compiler against [`DataMsg`]: adding a new
/// [`DataMsg`] variant without mirroring it here compiles cleanly but
/// silently drops that variant on every receiver (the `serde_json::from_str`
/// below returns an error which `dec()` swallows as `None`).
///
/// After adding a variant here, also add a case to the
/// `data_msg_all_variants_roundtrip` test in `wire.rs` so CI catches any
/// future drift.
#[derive(Deserialize)]
enum DataMsgWire {
	Clipboard(String),
	Chat(String),
	FileBegin {
		#[serde(default)]
		id: u32,
		name: String,
		size: u64,
		chunks: u32,
	},
	FileChunk {
		#[serde(default)]
		id: u32,
		index: u32,
		data: Vec<u8>,
	},
	/// Carries the resolved id (0 when the legacy unit form was decoded).
	FileEnd {
		#[serde(default)]
		id: u32,
	},
	Audio(Vec<u8>),
	AudioEnd,
	Stats(String),
	ReverseRequest(String),
	DisplayRotation(u32),
	MediaNack(Vec<u16>),
	Avatar(Vec<u8>),
	FsList { path: String },
	FsEntries { path: String, entries: Vec<FsEntry> },
	FsGet { path: String },
	PeerName(String),
	CursorPos { x: f32, y: f32 },
	CursorShape {
		w: u32,
		h: u32,
		hot_x: u32,
		hot_y: u32,
		rgba_png: Vec<u8>,
	},
	CursorHidden,
}

impl From<DataMsgWire> for DataMsg {
	fn from(w: DataMsgWire) -> Self {
		match w {
			DataMsgWire::Clipboard(s) => DataMsg::Clipboard(s),
			DataMsgWire::Chat(s) => DataMsg::Chat(s),
			DataMsgWire::FileBegin { id, name, size, chunks } => {
				DataMsg::FileBegin { id, name, size, chunks }
			}
			DataMsgWire::FileChunk { id, index, data } => DataMsg::FileChunk { id, index, data },
			DataMsgWire::FileEnd { id } => DataMsg::FileEnd { id },
			DataMsgWire::Audio(v) => DataMsg::Audio(v),
			DataMsgWire::AudioEnd => DataMsg::AudioEnd,
			DataMsgWire::Stats(s) => DataMsg::Stats(s),
			DataMsgWire::ReverseRequest(s) => DataMsg::ReverseRequest(s),
			DataMsgWire::DisplayRotation(n) => DataMsg::DisplayRotation(n),
			DataMsgWire::MediaNack(v) => DataMsg::MediaNack(v),
			DataMsgWire::Avatar(v) => DataMsg::Avatar(v),
			DataMsgWire::FsList { path } => DataMsg::FsList { path },
			DataMsgWire::FsEntries { path, entries } => DataMsg::FsEntries { path, entries },
			DataMsgWire::FsGet { path } => DataMsg::FsGet { path },
			DataMsgWire::PeerName(s) => DataMsg::PeerName(s),
			DataMsgWire::CursorPos { x, y } => DataMsg::CursorPos { x, y },
			DataMsgWire::CursorShape { w, h, hot_x, hot_y, rgba_png } => {
				DataMsg::CursorShape { w, h, hot_x, hot_y, rgba_png }
			}
			DataMsgWire::CursorHidden => DataMsg::CursorHidden,
		}
	}
}

/// Return true if the raw JSON token is the bare string `"FileEnd"` — the
/// legacy unit-variant form emitted by pre-patch peers.  Checked on the raw
/// JSON bytes so no allocation is needed.
fn is_legacy_file_end(raw: &str) -> bool {
	raw == "\"FileEnd\""
}

/// A bidirectional data-channel message exchanged over a live session for the
/// session "side channels": clipboard sync, text chat, file transfer, and mic
/// audio. Either peer can send any of these. NOTE: the session transport is UDP
/// (unordered, lossy) — fine on LAN/loopback; file transfer tags chunks so the
/// receiver can detect a gap and report an incomplete transfer rather than
/// silently corrupting.
///
/// `Serialize` is derived normally (produces the struct-variant form for
/// `FileEnd`).  `Deserialize` is implemented manually: it captures the raw JSON
/// token via `serde_json::value::RawValue` (zero allocation), detects the legacy
/// bare-string `"FileEnd"` unit-variant form, and for all other variants forwards
/// directly to the derived `DataMsgWire` deserializer (one parse pass, no
/// `Value` intermediate and no per-byte allocation for `Vec<u8>` fields).
///
/// # IMPORTANT — shadow enum must stay in sync
///
/// Deserialization goes through the **private [`DataMsgWire`] shadow enum**
/// (defined just above in `wire.rs`) plus a hand-written
/// [`From`]`<DataMsgWire>` conversion.  Adding a new variant here WITHOUT also
/// adding it to `DataMsgWire` + its `From` impl compiles cleanly but **silently
/// drops** that variant at runtime (the receiver's `dec()` returns `None`).
/// Guard against this by adding the new variant to the
/// `data_msg_all_variants_roundtrip` test — CI will then catch any future drift.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum DataMsg {
	/// Clipboard text pushed to the peer (peer sets its system clipboard).
	Clipboard(String),
	/// A chat line.
	Chat(String),
	/// Start of a file: a per-transfer `id` (lets concurrent transfers on the same
	/// session interleave on the wire without their reassembly state colliding),
	/// name + total byte length + number of chunks to expect. `id` defaults to 0 so
	/// an old peer that omits it still decodes (its single-transfer behavior).
	FileBegin {
		#[serde(default)]
		id: u32,
		name: String,
		size: u64,
		chunks: u32,
	},
	/// One file chunk: the transfer `id` it belongs to + its 0-based index (for gap
	/// detection / index-keyed reassembly).
	FileChunk {
		#[serde(default)]
		id: u32,
		index: u32,
		data: Vec<u8>,
	},
	/// All chunks for the transfer `id` have been sent.  Serializes as the new
	/// struct-variant form `{"FileEnd":{"id":N}}`.  Deserialized via
	/// [`DataMsgWire`] which also accepts the legacy unit-variant form `"FileEnd"`
	/// (id defaults to 0) so new ↔ old peer file transfers still complete even
	/// when PROTOCOL_VERSION was not bumped.
	FileEnd {
		#[serde(default)]
		id: u32,
	},
	/// One frame of raw PCM mic audio (s16le, 48kHz mono).
	Audio(Vec<u8>),
	/// The mic stream stopped.
	AudioEnd,
	/// Host → client: a short encode summary for the perf tooltip (e.g.
	/// "NVENC · 1080p · 60fps").
	Stats(String),
	/// Client → host: "reverse the direction" — the controlled peer should connect
	/// back to us (the payload is the requester's connect id) so roles swap. The
	/// requester must be online/serving for the reverse connect to land.
	ReverseRequest(String),
	/// Host → client: the host's display orientation in degrees (0/90/180/270). The
	/// captured framebuffer carries this rotation, so the client un-rotates the rendered
	/// video by the inverse → it shows upright regardless of how the host screen is mounted
	/// (e.g. a laptop configured upside-down sends 180). Sent once at stream start.
	DisplayRotation(u32),
	/// Client → host: RTP **video** sequence numbers the client detected as missing
	/// (media-over-session only). The host re-sends them from its retransmit ring if
	/// they're still buffered — cheap loss recovery on LAN/Wi-Fi without waiting for
	/// the next keyframe. An old host fails to decode the unknown variant and ignores it.
	MediaNack(Vec<u16>),
	/// The sender's identity image (PNG bytes, **≤ 64 KB** — a small center-cropped
	/// avatar), pushed once right after a session is up, in both directions: client →
	/// host so the host's connections list can show *who* connected, host → client for
	/// the session UI. Best-effort decoration: the session transport is one datagram
	/// per message, so senders must keep it small; an old peer fails to decode the
	/// unknown (appended) variant and ignores it.
	Avatar(Vec<u8>),
	/// Client → host: list a directory of the host's filesystem (the file-manager
	/// panel). `path` is **relative to the host user's HOME** with `/` separators
	/// ("" = HOME itself) — the host canonicalizes and refuses anything that
	/// escapes HOME. Appended for additive wire compat (old hosts ignore it).
	FsList { path: String },
	/// Host → client: the `FsList` reply — the echoed request path + its entries
	/// (dirs first, alphabetical). A rejected/unreadable path replies with an
	/// empty `entries` so the client always gets an answer.
	FsEntries { path: String, entries: Vec<FsEntry> },
	/// Client → host: stream the file at this HOME-relative path back to the
	/// client through the existing `FileBegin`/`FileChunk`/`FileEnd` flow (the
	/// file-manager "indir" action). Same HOME jail as `FsList`.
	FsGet { path: String },
	/// The sender's display NAME (the device/OS-user name), pushed once right after
	/// a session is up alongside [`DataMsg::Avatar`] — the receiving side shows it in
	/// its connections list / session UI and caches it for recents. Appended for
	/// additive wire compat (old peers ignore it).
	PeerName(String),
	/// Host → client: the host pointer position, normalized 0..1 within the streamed
	/// screen (Moonlight-style cursor side-channel). Sent at ~60 Hz when the host's
	/// captured framebuffer does NOT carry the hardware cursor (the KMS zero-copy path
	/// scans out without the X cursor plane) so the client can draw it over the video
	/// itself. Tiny (two f32) — well within the datagram budget. Appended for additive
	/// wire compat (an old peer ignores the unknown variant).
	CursorPos { x: f32, y: f32 },
	/// Host → client: the host pointer SHAPE, sent only when the cursor image changes
	/// (rarely — text-caret / resize-arrow transitions). RGBA pixels are PNG-encoded
	/// to stay small (cursors are 32–64 px → well under the avatar budget); `hot_x`/
	/// `hot_y` are the click hotspot in pixels so the client offsets the drawn bitmap
	/// exactly like the host. Pairs with [`DataMsg::CursorPos`]. Appended for wire compat.
	CursorShape {
		w: u32,
		h: u32,
		hot_x: u32,
		hot_y: u32,
		rgba_png: Vec<u8>,
	},
	/// Host → client: the host pointer is currently HIDDEN (e.g. a full-screen game
	/// hid it, or it left the captured screen). The client stops drawing the
	/// side-channel cursor until the next [`DataMsg::CursorPos`]. Appended for wire compat.
	CursorHidden,
}

impl<'de> Deserialize<'de> for DataMsg {
	fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
		// Capture the raw JSON token without materialising a serde_json::Value
		// tree.  This avoids the hot-path overhead of the previous approach, which
		// decoded every DataMsg through Value::deserialize — for byte-array
		// variants (Audio, Avatar, FileChunk.data, CursorShape.rgba_png) that
		// allocates one Value::Number per byte (~3 840 boxed numbers per 20 ms mic
		// frame at 50 fps, megabytes/sec of transient allocation on the host's
		// single session-driving task).
		//
		// We only need the Value round-trip to handle the ONE legacy compat case:
		// a pre-patch peer that sent `"FileEnd"` as a bare unit-variant string
		// (serde_json cannot decode a bare string into a struct variant).  We
		// detect that case cheaply by comparing the raw token, short-circuit it,
		// and for every other variant forward directly to the derived DataMsgWire
		// deserializer which parses Vec<u8> in one pass (no intermediate Value).
		let raw = Box::<serde_json::value::RawValue>::deserialize(de)
			.map_err(serde::de::Error::custom)?;
		if is_legacy_file_end(raw.get()) {
			// Legacy unit form `"FileEnd"` from a pre-patch peer: treat as id=0.
			return Ok(DataMsg::FileEnd { id: 0 });
		}
		serde_json::from_str::<DataMsgWire>(raw.get())
			.map(DataMsg::from)
			.map_err(serde::de::Error::custom)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn streamreq_missing_bitrate_and_quality_defaults() {
		// A request built before the bitrate/quality fields existed omits both. The
		// `#[serde(default)]` fields (appended at the end) must still deserialize:
		// bitrate falls back to 0 ("use host default") and quality to Balanced ("defer
		// to game_mode"). This locks the additive wire contract.
		let json = r#"{
			"port": 5000,
			"codec": "h265",
			"encoder": "auto",
			"width": 0,
			"height": 0,
			"fps": 60,
			"game_mode": true
		}"#;
		let req: StreamReq = serde_json::from_str(json).expect("old StreamReq must deserialize");
		assert_eq!(req.bitrate_kbps, 0, "missing bitrate_kbps defaults to 0");
		assert_eq!(
			req.quality,
			QualityPref::Balanced,
			"missing quality defaults to Balanced"
		);
		assert!(
			req.decode_codecs.is_empty(),
			"missing decode_codecs defaults to empty (= unknown → host doesn't clamp)"
		);
		assert!(
			!req.media_over_session,
			"missing media_over_session defaults to false (legacy direct flows)"
		);
		assert_eq!(req.display_idx, 0, "missing display_idx defaults to 0 (host primary)");
		assert_eq!(
			req.audio_layout,
			ChannelLayout::Stereo,
			"missing audio_layout defaults to Stereo (the universally-decodable layout)"
		);
	}

	#[test]
	fn stream_caps_roundtrip_and_features_default() {
		let caps = StreamCaps {
			codecs: vec!["av1".into(), "h265".into(), "h264".into()],
			encoders: vec!["rkmpp".into(), "software".into()],
			features: vec!["mos".into(), "nack".into()],
			displays: vec![DisplayInfo {
				idx: 0,
				name: "DISPLAY1".into(),
				width: 2560,
				height: 1440,
				primary: true,
			}],
		};
		let json = serde_json::to_string(&caps).unwrap();
		let back: StreamCaps = serde_json::from_str(&json).unwrap();
		assert_eq!(caps, back);
		// An OLD host's reply has no `features`/`displays` — must deserialize to empty.
		let old = r#"{"codecs":["h264"],"encoders":["software"]}"#;
		let caps: StreamCaps = serde_json::from_str(old).unwrap();
		assert!(caps.features.is_empty());
		assert!(caps.displays.is_empty());
	}

	#[test]
	fn media_nack_roundtrip() {
		let m = DataMsg::MediaNack(vec![1, 65535, 42]);
		let json = serde_json::to_string(&m).unwrap();
		assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
	}

	#[test]
	fn avatar_roundtrip() {
		// Locks the additive wire contract for the appended Avatar variant: raw PNG
		// bytes (incl. the 0x89 signature byte) survive the JSON roundtrip intact.
		let m = DataMsg::Avatar(vec![0x89, b'P', b'N', b'G', 0, 255, 13, 10]);
		let json = serde_json::to_string(&m).unwrap();
		assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
	}

	#[test]
	fn peer_name_roundtrip() {
		// The pushed display name (Unicode-safe) survives the JSON roundtrip.
		let m = DataMsg::PeerName("Ahmet Enes Dürüer".into());
		let json = serde_json::to_string(&m).unwrap();
		assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
	}

	#[test]
	fn fs_list_and_get_roundtrip() {
		// Locks the additive wire contract for the file-manager request variants:
		// HOME-relative paths (incl. "" = HOME and non-ASCII names) survive intact.
		for m in [
			DataMsg::FsList {
				path: String::new(),
			},
			DataMsg::FsList {
				path: "Belgeler/Çalışma".into(),
			},
			DataMsg::FsGet {
				path: "Belgeler/rapor.pdf".into(),
			},
		] {
			let json = serde_json::to_string(&m).unwrap();
			assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
		}
	}

	#[test]
	fn fs_entries_roundtrip() {
		// The listing reply: echoed path + entries (dirs first, alphabetical — the
		// producer sorts; the wire just carries the order through).
		let m = DataMsg::FsEntries {
			path: "Belgeler".into(),
			entries: vec![
				FsEntry {
					name: "Projeler".into(),
					dir: true,
					size: 0,
				},
				FsEntry {
					name: "not.txt".into(),
					dir: false,
					size: 1234,
				},
			],
		};
		let json = serde_json::to_string(&m).unwrap();
		assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
		// A rejected path replies with empty entries — must roundtrip too.
		let empty = DataMsg::FsEntries {
			path: "../etc".into(),
			entries: Vec::new(),
		};
		let json = serde_json::to_string(&empty).unwrap();
		assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), empty);
	}

	#[test]
	fn cursor_side_channel_roundtrip() {
		// Locks the additive wire contract for the cursor side-channel variants:
		// position (two f32), a PNG shape (incl. the 0x89 signature byte + hotspot),
		// and the hidden marker all survive the JSON roundtrip intact.
		for m in [
			DataMsg::CursorPos { x: 0.5, y: 0.25 },
			DataMsg::CursorShape {
				w: 32,
				h: 32,
				hot_x: 4,
				hot_y: 2,
				rgba_png: vec![0x89, b'P', b'N', b'G', 0, 255, 13, 10],
			},
			DataMsg::CursorHidden,
		] {
			let json = serde_json::to_string(&m).unwrap();
			assert_eq!(serde_json::from_str::<DataMsg>(&json).unwrap(), m);
		}
	}

	#[test]
	fn streamreq_missing_cursor_external_defaults_false() {
		// A request built before the cursor side-channel existed omits the field; the
		// appended `#[serde(default)]` must deserialize it to false (host bakes the
		// cursor into the video, as before).
		let json = r#"{
			"port": 5000,
			"codec": "h265",
			"encoder": "auto",
			"width": 0,
			"height": 0
		}"#;
		let req: StreamReq = serde_json::from_str(json).expect("old StreamReq must deserialize");
		assert!(
			!req.cursor_external,
			"missing cursor_external defaults to false (embedded cursor)"
		);
	}

	/// Regression test for C2: `FileEnd` unit→struct variant shape change breaks
	/// cross-version file transfer.  Decodes the LITERAL old-form bytes produced
	/// by a pre-patch peer (bare string `"FileEnd"` wrapped in a `DataMsg`
	/// envelope), NOT just a fresh roundtrip — a same-build roundtrip can never
	/// catch a unit↔struct mismatch.
	#[test]
	fn file_end_legacy_unit_form_decodes() {
		// Old peers wrap the DataMsg in a Msg::Data envelope; the service layer
		// calls dec(bytes) which runs serde_json::from_slice.  The relevant inner
		// JSON for a bare FileEnd as emitted by an old build is:
		//   {"Data":"FileEnd"}
		// But what we test here is the DataMsg layer in isolation:
		let old_form = r#""FileEnd""#;
		let got: DataMsg = serde_json::from_str(old_form)
			.expect("legacy unit-form \"FileEnd\" must decode (C2 regression)");
		assert_eq!(
			got,
			DataMsg::FileEnd { id: 0 },
			"legacy FileEnd must decode with id=0"
		);
	}

	#[test]
	fn file_end_new_struct_form_roundtrips() {
		// New peers produce {"FileEnd":{"id":N}}; must survive roundtrip AND
		// decode from a hardcoded literal (guards against a future regression
		// where we accidentally re-emit the old unit form).
		let m = DataMsg::FileEnd { id: 7 };
		let json = serde_json::to_string(&m).unwrap();
		// Verify the serialized form is the new struct shape, not the legacy
		// bare string.  An old peer (unit form) would produce `"FileEnd"`;
		// if we see that here the Serialize impl regressed.
		assert!(
			json.contains(r#"{"FileEnd":"#),
			"FileEnd must serialize as struct variant {{\"FileEnd\":...}}, got: {json}"
		);
		let back: DataMsg = serde_json::from_str(&json)
			.expect("new struct-form FileEnd must decode");
		assert_eq!(back, m, "FileEnd id=7 must roundtrip");
	}

	#[test]
	fn file_end_new_struct_form_literal_decodes() {
		// Literal bytes a new peer produces — ensures new↔new still works.
		let new_form = r#"{"FileEnd":{"id":3}}"#;
		let got: DataMsg = serde_json::from_str(new_form)
			.expect("new struct-form FileEnd literal must decode");
		assert_eq!(got, DataMsg::FileEnd { id: 3 });
	}

	#[test]
	fn file_end_new_struct_form_missing_id_defaults_zero() {
		// New peer omits id (e.g. a single-transfer build that doesn't set it).
		let new_form_no_id = r#"{"FileEnd":{}}"#;
		let got: DataMsg = serde_json::from_str(new_form_no_id)
			.expect("FileEnd with empty map must decode with id=0");
		assert_eq!(got, DataMsg::FileEnd { id: 0 });
	}

	#[test]
	fn quality_pref_serde_is_lowercase() {
		// The wire form is lowercase so the JS/Rust string mapping stays in sync.
		assert_eq!(
			serde_json::to_string(&QualityPref::Latency).unwrap(),
			"\"latency\""
		);
		assert_eq!(
			serde_json::to_string(&QualityPref::Quality).unwrap(),
			"\"quality\""
		);
		assert_eq!(
			serde_json::from_str::<QualityPref>("\"balanced\"").unwrap(),
			QualityPref::Balanced
		);
		assert_eq!(QualityPref::default(), QualityPref::Balanced);
	}

	/// Guard against the DataMsgWire shadow-enum drift trap (bug C21).
	///
	/// Every `DataMsg` variant must survive a serialize→deserialize roundtrip
	/// through the custom `Deserialize` impl (which delegates to `DataMsgWire`).
	/// If a variant is present in `DataMsg` but missing from `DataMsgWire` / its
	/// `From` impl, the `from_str` inside `DataMsg::deserialize` returns an error
	/// and the assertion here panics — catching the omission in CI before it
	/// silently drops messages at runtime.
	///
	/// When you add a new `DataMsg` variant you MUST add a corresponding entry to
	/// this test (and mirror the variant in `DataMsgWire` + its `From` impl).
	#[test]
	fn data_msg_all_variants_roundtrip() {
		let cases: &[DataMsg] = &[
			DataMsg::Clipboard("hello".into()),
			DataMsg::Chat("world".into()),
			DataMsg::FileBegin { id: 1, name: "foo.bin".into(), size: 1024, chunks: 4 },
			DataMsg::FileChunk { id: 1, index: 2, data: vec![0xDE, 0xAD, 0xBE, 0xEF] },
			DataMsg::FileEnd { id: 1 },
			DataMsg::Audio(vec![0x01, 0x02, 0x03]),
			DataMsg::AudioEnd,
			DataMsg::Stats("NVENC · 1080p · 60fps".into()),
			DataMsg::ReverseRequest("123456789".into()),
			DataMsg::DisplayRotation(180),
			DataMsg::MediaNack(vec![1, 2, 65535]),
			DataMsg::Avatar(vec![0x89, b'P', b'N', b'G']),
			DataMsg::FsList { path: "Belgeler".into() },
			DataMsg::FsEntries {
				path: "Belgeler".into(),
				entries: vec![FsEntry { name: "a.txt".into(), dir: false, size: 42 }],
			},
			DataMsg::FsGet { path: "Belgeler/a.txt".into() },
			DataMsg::PeerName("Ahmet".into()),
			DataMsg::CursorPos { x: 0.5, y: 0.25 },
			DataMsg::CursorShape {
				w: 32,
				h: 32,
				hot_x: 1,
				hot_y: 1,
				rgba_png: vec![0x89, b'P', b'N', b'G'],
			},
			DataMsg::CursorHidden,
		];

		for msg in cases {
			let json = serde_json::to_string(msg)
				.unwrap_or_else(|e| panic!("DataMsg::{msg:?} failed to serialize: {e}"));
			let back: DataMsg = serde_json::from_str(&json).unwrap_or_else(|e| {
				panic!(
					"DataMsg::{msg:?} failed to deserialize (missing from DataMsgWire?): {e}\n  json={json}"
				)
			});
			assert_eq!(
				&back, msg,
				"DataMsg::{msg:?} roundtrip mismatch (From<DataMsgWire> bug?)"
			);
		}
	}
}
