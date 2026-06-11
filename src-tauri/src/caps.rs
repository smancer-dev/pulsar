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
			let codecs = if e == pulsar_core::pipeline::HwEncoder::Software {
				// Software always works (libx264 needs no probe); offer its full set.
				vec!["h264".to_string(), "h265".to_string(), "av1".to_string()]
			} else {
				crate::process::validated_codecs(&ffmpeg, e, &vaapi)
					.into_iter()
					.map(|c| codec_id(c).to_string())
					.collect()
			};
			EncoderCap {
				id: crate::process::encoder_wire_id(e).to_string(),
				backend: "ffmpeg".to_string(),
				codecs,
			}
		})
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

fn probe_decoders(app: &AppHandle, platform: &str) -> Vec<DecoderCap> {
	// macOS: the client is system mpv (no probeable native backend yet) — assume the
	// universal software pair. Windows: the MF probe isn't implemented yet — assume
	// MediaFoundation H.264/HEVC (HW-or-SW MFT always exists for those).
	if platform == "macos" {
		return ["h264", "h265"]
			.iter()
			.map(|c| DecoderCap {
				codec: c.to_string(),
				ok: true,
				name: "mpv".into(),
				hw: false,
				tier: "software".into(),
			})
			.collect();
	}
	if platform == "windows" {
		return ["h264", "h265"]
			.iter()
			.map(|c| DecoderCap {
				codec: c.to_string(),
				ok: true,
				name: "mediafoundation".into(),
				hw: true,
				tier: "hwaccel".into(),
			})
			.collect();
	}
	let render = crate::process::render_bin(app);
	let out = std::process::Command::new(&render)
		.arg("--probe")
		.stderr(std::process::Stdio::null())
		.output();
	let Ok(out) = out else {
		// Renderer missing: software ffmpeg decode still exists inside it when present;
		// report nothing rather than guessing.
		return Vec::new();
	};
	let text = String::from_utf8_lossy(&out.stdout);
	let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
		return Vec::new();
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
					})
				})
				.collect()
		})
		.unwrap_or_default()
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
