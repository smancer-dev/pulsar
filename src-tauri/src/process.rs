//! Media/process layer: resolve the bundled ffmpeg, detect encoders, map UI strings
//! to pipeline enums, and spawn host processes (ffmpeg capture, shell commands,
//! launched games) without flashing a console window. Extracted from `lib.rs`
//! (see PENDING-WORK #9).

use std::process::Child;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use pulsar_core::pipeline::{self, CaptureMethod, HwEncoder, VCodec};

pub fn capture_from_str(s: &str) -> CaptureMethod {
	match s {
		"x11grab" => CaptureMethod::X11grab,
		"kmsgrab" => CaptureMethod::Kmsgrab,
		"gdigrab" => CaptureMethod::Gdigrab,
		"ddagrab" => CaptureMethod::Ddagrab,
		"avfoundation" => CaptureMethod::AvFoundation,
		_ => CaptureMethod::default_for_os(),
	}
}

pub fn encoder_from_str(s: &str) -> HwEncoder {
	match s {
		"nvenc" => HwEncoder::Nvenc,
		"amf" => HwEncoder::Amf,
		"vaapi" => HwEncoder::Vaapi,
		"qsv" => HwEncoder::Qsv,
		"videotoolbox" => HwEncoder::VideoToolbox,
		"vulkan" => HwEncoder::Vulkan,
		"mediafoundation" | "mf" => HwEncoder::MediaFoundation,
		"software" => HwEncoder::Software,
		_ => HwEncoder::Auto,
	}
}

pub fn codec_from_str(s: &str) -> VCodec {
	match s {
		"h265" => VCodec::H265,
		"av1" => VCodec::Av1,
		_ => VCodec::H264,
	}
}

/// Probe whether the fully zero-copy ddagrab→CUDA→NVENC path works on THIS box —
/// it only does when the display adapter IS the NVIDIA GPU. On a hybrid box (iGPU
/// display + dGPU encode) `hwmap=derive_device=cuda` fails, and we must use the
/// GPU-scale-with-CPU-bounce path instead. Runs ffmpeg for one frame to null.
pub fn probe_ddagrab_zerocopy(ffmpeg: &str) -> bool {
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args([
		"-hide_banner",
		"-loglevel",
		"error",
		"-filter_complex",
		"ddagrab=output_idx=0:framerate=30,hwmap=derive_device=cuda,scale_cuda=64:64:format=nv12",
		"-frames:v",
		"1",
		"-f",
		"null",
		"-",
	]);
	no_window(&mut cmd);
	cmd.stdout(std::process::Stdio::null());
	cmd.stderr(std::process::Stdio::null());
	matches!(cmd.status().map(|st| st.success()), Ok(true))
}

/// Raw `ffmpeg -encoders` stdout (bundled binary). Empty on failure. Used both to detect
/// the available backends AND to resolve which codecs each backend can emit.
pub fn encoders_text(ffmpeg: &str) -> String {
	let mut probe = std::process::Command::new(ffmpeg);
	probe.args(["-hide_banner", "-encoders"]);
	no_window(&mut probe);
	match probe.output() {
		Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
		Err(_) => String::new(),
	}
}

/// Run `ffmpeg -encoders` (the bundled binary) and return the hardware encoders available.
pub fn detect_encoders(ffmpeg: &str) -> Vec<HwEncoder> {
	pipeline::detect(&encoders_text(ffmpeg))
}

/// Sunshine-style runtime VALIDATION of one encoder+codec: actually encode a single
/// synthetic frame and check ffmpeg exits 0. Catches the cases name-presence detection
/// misses — encoder listed but the GPU/driver can't init it (e.g. `av1_nvenc` on an Ampere
/// card, `h264_qsv` with no Intel GPU). Results are cached per (encoder, codec) for the
/// process lifetime so we probe at most once each.
fn probe_encoder_codec(ffmpeg: &str, encoder: HwEncoder, codec: VCodec, vaapi_device: &str) -> bool {
	use std::collections::HashMap;
	use std::sync::{Mutex, OnceLock};
	static CACHE: OnceLock<Mutex<HashMap<(HwEncoder, VCodec), bool>>> = OnceLock::new();
	let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	if let Some(&hit) = cache.lock().unwrap().get(&(encoder, codec)) {
		return hit;
	}
	let ok = match pipeline::probe_command(encoder, codec, vaapi_device) {
		Some((_, args)) => {
			let mut cmd = std::process::Command::new(ffmpeg);
			cmd.args(&args);
			no_window(&mut cmd);
			cmd.stdout(std::process::Stdio::null());
			cmd.stderr(std::process::Stdio::null());
			matches!(cmd.status().map(|st| st.success()), Ok(true))
		}
		None => false,
	};
	cache.lock().unwrap().insert((encoder, codec), ok);
	ok
}

/// Which codecs an encoder can ACTUALLY encode on this machine, VALIDATED by a real
/// one-frame probe (not just ffmpeg's `-encoders` listing). Falls back to the name-presence
/// set if probing is somehow unavailable.
pub fn validated_codecs(ffmpeg: &str, encoder: HwEncoder, vaapi_device: &str) -> Vec<VCodec> {
	let listed = encoder.available_codecs(&encoders_text(ffmpeg));
	listed
		.into_iter()
		.filter(|&c| probe_encoder_codec(ffmpeg, encoder, c, vaapi_device))
		.collect()
}

/// Resolve the requested codec against the VALIDATED codec set for `encoder` (probe-backed):
/// honor the request if it truly works, else prefer H.264, else the first validated codec,
/// else fall back to the name-presence resolution.
pub fn resolve_codec_validated(
	ffmpeg: &str,
	encoder: HwEncoder,
	requested: VCodec,
	vaapi_device: &str,
) -> VCodec {
	let text = encoders_text(ffmpeg);
	let avail = validated_codecs(ffmpeg, encoder, vaapi_device);
	// UNRELIABLE-PROBE GUARD: if H.264 is listed for this encoder but fails to validate, the
	// probe is testing the wrong thing — classic on a HYBRID laptop, where `ffmpeg -c:v
	// h264_nvenc` reports "no encode device" because ffmpeg targets the AMD display GPU, not
	// the NVIDIA dGPU (the native SDK path uses the explicit dGPU device and works fine). In
	// that case trust the name-based listing instead of spuriously downgrading the codec.
	let h264_listed = encoder.available_codecs(&text).contains(&VCodec::H264);
	if h264_listed && !avail.contains(&VCodec::H264) {
		return pipeline::resolve_codec(encoder, requested, &text);
	}
	if avail.contains(&requested) {
		requested
	} else if avail.contains(&VCodec::H264) {
		VCodec::H264
	} else if let Some(&c) = avail.first() {
		c
	} else {
		// Nothing validated (probe failed across the board) — fall back to the listing.
		pipeline::resolve_codec(encoder, requested, &text)
	}
}

/// Validate that `chosen` (already name-resolved by `pipeline::resolve`) can ACTUALLY initialize
/// an encode on this host (Sunshine-style one-frame probe), degrading to the next available
/// encoder if it can't — ending at `Software` (libx264), which always works. ffmpeg merely
/// *lists* an encoder if it's compiled in (a generic build lists `h264_nvenc` even with no
/// NVIDIA GPU), so name detection alone picks unusable encoders on machines like an ARM SBC host
/// (`h264_nvenc` → "Cannot load libcuda.so.1" → no video). This is **off-Windows only**: there
/// ffmpeg is the sole encode path, so a failed probe is authoritative. Windows keeps its native
/// NVENC SDK path + the hybrid-laptop probe guard (`resolve_codec_validated`) untouched.
#[cfg(not(windows))]
pub fn resolve_encoder_validated(
	ffmpeg: &str,
	chosen: HwEncoder,
	enc_text: &str,
	vaapi_device: &str,
) -> HwEncoder {
	let mut available = pipeline::detect(enc_text);
	let mut cur = chosen;
	loop {
		// libx264 is always usable — accept it without a probe (and as the terminal fallback).
		if cur == HwEncoder::Software {
			return cur;
		}
		if !validated_codecs(ffmpeg, cur, vaapi_device).is_empty() {
			return cur;
		}
		available.retain(|&e| e != cur);
		let next = pipeline::resolve(HwEncoder::Auto, &available);
		if next == cur {
			return HwEncoder::Software;
		}
		cur = next;
	}
}

/// On Windows, stop a spawned child (ffmpeg etc.) from flashing up a console window
/// — the user must never see it, and closing it would kill the stream. No-op
/// elsewhere.
#[cfg(windows)]
pub fn no_window(cmd: &mut std::process::Command) {
	use std::os::windows::process::CommandExt;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
pub fn no_window(_cmd: &mut std::process::Command) {}

/// Spawn a process and remember it (in `procs`) so it can be stopped later.
pub fn spawn_tracked(
	procs: &Arc<Mutex<Vec<Child>>>,
	program: &str,
	args: &[String],
) -> Result<(), String> {
	let mut cmd = std::process::Command::new(program);
	cmd.args(args);
	no_window(&mut cmd); // never pop up a console window for ffmpeg
	match cmd.spawn() {
		Ok(child) => {
			// Tie it to Pulsar's lifetime so a crash/taskkill/tray-quit can't leave the
			// encoder running and pegging NVENC (see job.rs).
			#[cfg(windows)]
			crate::job::assign(&child);
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("{program} başlatılamadı: {e}")),
	}
}

/// A host game/app, as sent from the UI's games store.
#[derive(Clone, Deserialize)]
pub struct HostGame {
	pub id: String,
	pub title: String,
	#[serde(rename = "type")]
	pub kind: String,
	#[serde(default)]
	pub path: String,
	#[serde(default)]
	pub args: String,
	#[serde(default)]
	pub command: String,
	#[serde(rename = "cmdStart", default)]
	pub cmd_start: String,
	#[allow(dead_code)]
	#[serde(rename = "cmdStop", default)]
	pub cmd_stop: String,
}

/// Run a command through the platform shell (fire-and-forget), no console window.
pub fn spawn_shell(cmd: &str) {
	let cmd = cmd.trim();
	if cmd.is_empty() {
		return;
	}
	#[cfg(windows)]
	{
		let mut c = std::process::Command::new("cmd");
		c.args(["/C", cmd]);
		no_window(&mut c);
		let _ = c.spawn();
	}
	#[cfg(not(windows))]
	let _ = std::process::Command::new("sh").args(["-c", cmd]).spawn();
}

/// Launch a host game: its start hook, then the program/command itself.
pub fn launch_host_game(g: &HostGame) {
	spawn_shell(&g.cmd_start);
	match g.kind.as_str() {
		"program" if !g.path.is_empty() => spawn_shell(&format!("\"{}\" {}", g.path, g.args)),
		"command" if !g.command.is_empty() => spawn_shell(&g.command),
		_ => {}
	}
}

/// Resolve a bundled binary by name (`ffmpeg.exe` / `ffplay.exe`), preferring the
/// installed app's resource dir, then next to the executable (portable/`tauri dev`),
/// falling back to the bare name on PATH.
pub fn bundled_bin(app: &AppHandle, base: &str) -> String {
	let name = if cfg!(windows) {
		format!("{base}.exe")
	} else {
		base.to_string()
	};
	if let Ok(dir) = app.path().resource_dir() {
		for cand in [dir.join(&name), dir.join("resources").join(&name)] {
			if cand.is_file() {
				return cand.to_string_lossy().into_owned();
			}
		}
	}
	if let Ok(exe) = std::env::current_exe() {
		if let Some(p) = exe.parent().map(|d| d.join(&name)) {
			if p.is_file() {
				return p.to_string_lossy().into_owned();
			}
		}
	}
	base.to_string()
}

/// The bundled ffmpeg (host capture+encode). Pulsar bundles it so streaming works
/// out of the box.
pub fn ffmpeg_bin(app: &AppHandle) -> String {
	bundled_bin(app, "ffmpeg")
}

/// The bundled ffplay (native renderer — HW-decoded fullscreen client playback).
pub fn ffplay_bin(app: &AppHandle) -> String {
	bundled_bin(app, "ffplay")
}

/// Pulsar's native zero-copy video sink (Linux/RK3588 client renderer that replaces
/// mpv): rkmpp → DRM_PRIME → EGL, embedded via `--wid`. Built from
/// `scripts/pulsar-vidsink.c`; resolved next to the exe / in resources.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn vidsink_bin(app: &AppHandle) -> String {
	bundled_bin(app, "pulsar-vidsink")
}

/// Pulsar's native renderer (`pulsar-render`). On Linux: rkmpp video + egui overlay in a child
/// X11 window. On Windows: Media Foundation decode + D3D11 present in a child HWND (Moonlight-
/// style, replacing the webview WebCodecs path). A workspace crate, dropped next to the app exe.
#[cfg(any(all(unix, not(target_os = "macos")), target_os = "windows"))]
pub fn render_bin(app: &AppHandle) -> String {
	bundled_bin(app, "pulsar-render")
}

/// The main Tauri window's native HWND (Windows) as a u64, so `pulsar-render` can create its
/// D3D11 child window under it via `--wid` (the Win32 analogue of Linux's `render::window_xid`).
/// None before the window exists.
#[cfg(target_os = "windows")]
pub fn window_hwnd(app: &AppHandle) -> Option<u64> {
	let w = app.get_webview_window("main")?;
	w.hwnd().ok().map(|h| h.0 as u64)
}
