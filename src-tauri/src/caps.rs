//! Startup capability detection (Moonlight model: re-probed on EVERY launch, no
//! persistence — the splash stays up while it runs).
//!
//! One background task probes BOTH directions with the real machinery:
//! - **Encode** (host role): the cached one-frame ffmpeg probes
//!   (`process::validated_encoders`/`validated_codecs`) plus the GStreamer families
//!   (`process::validated_gst_encoders` — Rockchip MPP etc.).
//! - **Decode** (client role): `pulsar-render --probe`, which runs the tiered
//!   decoder chain (zero-copy SoC → hwaccel → software) against canned keyframes.
//!
//! The result lands in `AppState.local_caps` and is pushed to the webview as a
//! `local-caps` event; the Settings UI disables what isn't available, and the
//! host's `QueryStreamCaps` reply reads it for an instant first answer.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EncoderCap {
	/// Wire/UI id (`nvenc`/`vaapi`/`rkmpp`/…/`software`).
	pub id: String,
	/// Which backend serves it (`ffmpeg` or `gst`).
	pub backend: String,
	/// Validated codecs (`h264`/`h265`/`av1`).
	pub codecs: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DecoderCap {
	pub codec: String,
	pub ok: bool,
	pub name: String,
	pub hw: bool,
	pub tier: String,
	/// Encoder families whose real bitstream this decoder can't decode even though the
	/// codec validates against a conformant sample (e.g. `["nvenc"]` for rkmpp HEVC on
	/// RK3588). The negotiator drops a host-encoder × this-decoder combo that lands here.
	#[serde(default)]
	pub incompatible_with: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LocalCaps {
	pub platform: String,
	pub encoders: Vec<EncoderCap>,
	pub decoders: Vec<DecoderCap>,
}

fn codec_id(c: pulsar_core::pipeline::VCodec) -> &'static str {
	match c {
		pulsar_core::pipeline::VCodec::H264 => "h264",
		pulsar_core::pipeline::VCodec::H265 => "h265",
		pulsar_core::pipeline::VCodec::Av1 => "av1",
	}
}

/// Run the full probe (blocking — call on a background thread).
pub(crate) fn probe_all(app: &AppHandle) -> LocalCaps {
	let platform = if cfg!(windows) {
		"windows"
	} else if cfg!(target_os = "macos") {
		"macos"
	} else {
		"linux"
	}
	.to_string();

	// --- Encode: ffmpeg families (probe-validated), then gst families merged in. ---
	let ffmpeg = crate::process::ffmpeg_bin(app);
	let vaapi = app
		.state::<crate::state::AppState>()
		.stream_cfg
		.lock()
		.unwrap()
		.vaapi_device
		.clone();
	let mut encoders: Vec<EncoderCap> = crate::process::validated_encoders(&ffmpeg, &vaapi)
		.into_iter()
		.map(|e| {
			#[allow(unused_mut)]
			let mut codecs = if e == pulsar_core::pipeline::HwEncoder::Software {
				// Software always works (libx264 needs no probe); offer its full set.
				vec!["h264".to_string(), "h265".to_string(), "av1".to_string()]
			} else {
				crate::process::validated_codecs(&ffmpeg, e, &vaapi)
					.into_iter()
					.map(|c| codec_id(c).to_string())
					.collect()
			};
			// Windows NVENC: the ffmpeg one-frame probe is the wrong oracle — it targets the
			// display GPU (fails on hybrid laptops where NVENC works via the native SDK) and
			// its name listing passes on GPUs with no NVENC silicon at all (GP108 MX-series).
			// Ask the SDK itself which codecs this GPU's NVENC encodes (empty = no NVENC, so
			// the family is dropped below and Auto picks a working one). Per-codec on purpose:
			// a Kepler/GM107 card does H.264 only, and advertising HEVC there would put HEVC in
			// the client's SDP while the host degrades the wire to H.264 → permanent black.
			#[cfg(windows)]
			if e == pulsar_core::pipeline::HwEncoder::Nvenc {
				codecs = pulsar_capture::nvenc_codecs()
					.iter()
					.map(|c| match c {
						pulsar_capture::Codec::H264 => "h264".to_string(),
						pulsar_capture::Codec::H265 => "h265".to_string(),
						pulsar_capture::Codec::Av1 => "av1".to_string(),
					})
					.collect();
			}
			EncoderCap {
				id: crate::process::encoder_wire_id(e).to_string(),
				backend: "ffmpeg".to_string(),
				codecs,
			}
		})
		// An encoder family with ZERO validated codecs cannot produce a stream on this
		// machine — advertising it (the old Windows behavior: trust the name listing,
		// vaapi included) made clients and the Auto pick select encoders that die at
		// init with no error surfaced anywhere. Drop them from the caps entirely.
		.filter(|e| !e.codecs.is_empty())
		.collect();
	#[cfg(target_os = "linux")]
	for (genc, codecs) in crate::process::validated_gst_encoders() {
		let id = genc.wire_id().to_string();
		let codecs: Vec<String> = codecs
			.into_iter()
			.map(|c| codec_id(c).to_string())
			.collect();
		if let Some(existing) = encoders.iter_mut().find(|e| e.id == id) {
			for c in codecs {
				if !existing.codecs.contains(&c) {
					existing.codecs.push(c);
				}
			}
		} else {
			// HW families ahead of the terminal software entry.
			let pos = encoders
				.iter()
				.position(|e| e.id == "software")
				.unwrap_or(encoders.len());
			encoders.insert(
				pos,
				EncoderCap {
					id,
					backend: "gst".to_string(),
					codecs,
				},
			);
		}
	}

	// --- Decode: the renderer's own tiered probe (real canned-frame decodes). ---
	let decoders = probe_decoders(app, &platform);

	LocalCaps {
		platform,
		encoders,
		decoders,
	}
}

/// The renderer probe couldn't run (binary missing / unparseable output). Windows keeps a
/// minimal H.264 claim: MF's inbox H.264 decoder ships with every Windows edition that has
/// Media Foundation, and reporting NOTHING would grey out every codec in Settings on a box
/// that can in fact stream. HEVC is deliberately NOT claimed here — it needs the (optional)
/// HEVC Video Extensions, so an unprobed guess is exactly what steered negotiation into a
/// black screen. Elsewhere, report nothing rather than guessing.
fn probe_unavailable(platform: &str) -> Vec<DecoderCap> {
	if platform != "windows" {
		return Vec::new();
	}
	vec![DecoderCap {
		codec: "h264".into(),
		ok: true,
		name: "mediafoundation".into(),
		hw: true,
		tier: "hwaccel".into(),
		incompatible_with: Vec::new(),
	}]
}

fn probe_decoders(app: &AppHandle, platform: &str) -> Vec<DecoderCap> {
	// macOS: the client is system mpv (no probeable native backend yet) — assume the
	// universal software pair. Windows falls through to the real renderer probe below
	// (`pulsar-render --probe`, MFT enumeration): the old hardcoded "MF decodes
	// h264+h265, always" claim advertised HEVC on machines without the HEVC Video
	// Extensions / with async-only MFTs, steering negotiation into a black screen.
	if platform == "macos" {
		return ["h264", "h265"]
			.iter()
			.map(|c| DecoderCap {
				codec: c.to_string(),
				ok: true,
				name: "mpv".into(),
				hw: false,
				tier: "software".into(),
				incompatible_with: Vec::new(),
			})
			.collect();
	}
	// macOS returned above (mpv client, no probeable backend) — `render_bin` is
	// configured out there, so the renderer probe only compiles where it exists.
	#[cfg(not(target_os = "macos"))]
	{
		let render = crate::process::render_bin(app);
		let mut cmd = std::process::Command::new(&render);
		cmd.arg("--probe").stderr(std::process::Stdio::null());
		// Probe with the SAME lib resolution the real renderer uses, so the reported
		// decoder/tier match what will actually run (rkmpp HW on RK3588, not software).
		crate::process::apply_render_lib_env(&mut cmd);
		// Windows: the probe must never flash a console window (GUI host).
		crate::process::no_window(&mut cmd);
		let out = cmd.output();
		let Ok(out) = out else {
			// Renderer missing: software ffmpeg decode still exists inside it when present;
			// report nothing rather than guessing.
			return probe_unavailable(platform);
		};
		let text = String::from_utf8_lossy(&out.stdout);
		let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
			return probe_unavailable(platform);
		};
		parsed
			.as_array()
			.map(|arr| {
				arr.iter()
					.filter_map(|e| {
						Some(DecoderCap {
							codec: e.get("codec")?.as_str()?.to_string(),
							ok: e.get("ok")?.as_bool()?,
							name: e
								.get("decoder")
								.and_then(|v| v.as_str())
								.unwrap_or("")
								.to_string(),
							hw: e.get("hw").and_then(|v| v.as_bool()).unwrap_or(false),
							tier: e
								.get("tier")
								.and_then(|v| v.as_str())
								.unwrap_or("")
								.to_string(),
							incompatible_with: e
								.get("incompatible_with")
								.and_then(|v| v.as_array())
								.map(|a| {
									a.iter()
										.filter_map(|x| x.as_str().map(|s| s.to_string()))
										.collect()
								})
								.unwrap_or_default(),
						})
					})
					.collect()
			})
			.unwrap_or_default()
	}
	#[cfg(target_os = "macos")]
	Vec::new()
}

/// Spawn the startup probe: runs in the background, stores the result in AppState and
/// pushes it to the webview (`local-caps`). The splash waits for that event (with a
/// safety cap) so the UI never shows un-gated options.
pub(crate) fn spawn_startup_probe(app: AppHandle) {
	std::thread::spawn(move || {
		let t0 = std::time::Instant::now();
		let caps = probe_all(&app);
		tracing::info!(
			elapsed_ms = t0.elapsed().as_millis() as u64,
			encoders = caps.encoders.len(),
			decoders = caps.decoders.len(),
			"local caps probed"
		);
		*app.state::<crate::state::AppState>()
			.local_caps
			.lock()
			.unwrap() = Some(caps.clone());
		let _ = app.emit("local-caps", caps);
	});
}

/// Tauri command: the probed caps (None while the startup probe is still running —
/// the frontend also listens for the `local-caps` event).
#[tauri::command]
pub(crate) fn local_caps(state: tauri::State<'_, crate::state::AppState>) -> Option<LocalCaps> {
	state.local_caps.lock().unwrap().clone()
}
