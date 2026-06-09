//! Wire data types exchanged over a session: game listings, control input
//! events, stream requests, and the bidirectional side-channel `DataMsg`.

use serde::{Deserialize, Serialize};

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
}

fn default_true() -> bool {
	true
}

/// A bidirectional data-channel message exchanged over a live session for the
/// session "side channels": clipboard sync, text chat, file transfer, and mic
/// audio. Either peer can send any of these. NOTE: the session transport is UDP
/// (unordered, lossy) — fine on LAN/loopback; file transfer tags chunks so the
/// receiver can detect a gap and report an incomplete transfer rather than
/// silently corrupting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DataMsg {
	/// Clipboard text pushed to the peer (peer sets its system clipboard).
	Clipboard(String),
	/// A chat line.
	Chat(String),
	/// Start of a file: name + total byte length + number of chunks to expect.
	FileBegin {
		name: String,
		size: u64,
		chunks: u32,
	},
	/// One file chunk with its 0-based index (for gap detection).
	FileChunk { index: u32, data: Vec<u8> },
	/// All chunks for the current file have been sent.
	FileEnd,
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
}
