//! Media/process layer: resolve the bundled ffmpeg, detect encoders, map UI strings
//! to pipeline enums, and spawn host processes (ffmpeg capture, shell commands,
//! launched games) without flashing a console window. Extracted from `lib.rs`
//! (see PENDING-WORK #9).

use std::process::Child;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use pulsar_core::pipeline::{self, CaptureMethod, HwEncoder, VCodec};

/// A host monitor with its capture geometry: `(name, x, y, w, h, primary)`. The
/// X/Y origin is in the virtual-desktop / X11 root space — what x11grab's
/// `-i :0.0+x,y` and gst `ximagesrc startx/starty` need to capture that monitor.
#[cfg(target_os = "linux")]
pub type LinuxDisplay = (String, i32, i32, u32, u32, bool);

/// Enumerate connected X11 outputs via `xrandr --query`, in xrandr order, returning
/// each monitor's name + geometry. Empty on Wayland / when xrandr is missing (the
/// caller then advertises no picker and captures the whole root as before).
#[cfg(target_os = "linux")]
pub fn linux_displays() -> Vec<LinuxDisplay> {
	if pulsar_core::capture::is_wayland() {
		// The XDG ScreenCast portal owns monitor choice via its own dialog; we don't
		// enumerate Wayland outputs here.
		return Vec::new();
	}
	let mut cmd = std::process::Command::new("xrandr");
	cmd.arg("--query");
	no_window(&mut cmd);
	let Ok(out) = cmd.output() else {
		return Vec::new();
	};
	let text = String::from_utf8_lossy(&out.stdout);
	let mut displays = Vec::new();
	for line in text.lines() {
		// Connected-output lines look like:
		//   HDMI-1 connected primary 1920x1080+0+0 (normal ...) 520mm x 290mm
		//   DP-2 connected 2560x1440+1920+0 (normal ...) ...
		if !line.contains(" connected") {
			continue;
		}
		let mut it = line.split_whitespace();
		let Some(name) = it.next() else { continue };
		let primary = line.contains(" primary ");
		// The geometry token is the first `WxH+X+Y`.
		let geom = line
			.split_whitespace()
			.find(|t| t.contains('x') && t.contains('+'));
		let Some(geom) = geom else { continue };
		// Parse WxH+X+Y.
		let parse = || -> Option<(u32, u32, i32, i32)> {
			let (wh, rest) = geom.split_once('+')?;
			let (w, h) = wh.split_once('x')?;
			let (x, y) = rest.split_once('+')?;
			Some((w.parse().ok()?, h.parse().ok()?, x.parse().ok()?, y.parse().ok()?))
		};
		if let Some((w, h, x, y)) = parse() {
			displays.push((name.to_string(), x, y, w, h, primary));
		}
	}
	// Primary first so it lands at idx 0 (the client's default), then the rest in
	// xrandr order. A stable order matters: the idx is what travels in display_idx.
	displays.sort_by_key(|d| !d.5);
	displays
}

/// Host monitors to advertise in `StreamCaps::displays`, primary at idx 0. Windows
/// uses the DXGI output list (same index the native capture takes); Linux/X11 uses
/// xrandr. Empty elsewhere / on Wayland — the client then shows no picker.
pub fn host_displays() -> Vec<pulsar_core::service::DisplayInfo> {
	use pulsar_core::service::DisplayInfo;
	#[cfg(windows)]
	{
		return pulsar_capture::list_displays()
			.into_iter()
			.map(|(idx, name, width, height, primary)| DisplayInfo {
				idx,
				name,
				width,
				height,
				primary,
			})
			.collect();
	}
	#[cfg(target_os = "linux")]
	{
		return linux_displays()
			.into_iter()
			.enumerate()
			.map(|(i, (name, _x, _y, w, h, primary))| DisplayInfo {
				idx: i as u32,
				name,
				width: w,
				height: h,
				primary,
			})
			.collect();
	}
	#[cfg(not(any(windows, target_os = "linux")))]
	{
		Vec::new()
	}
}

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
		// "quicksync" was the UI's old value for QSV — keep accepting persisted configs.
		"qsv" | "quicksync" => HwEncoder::Qsv,
		"videotoolbox" => HwEncoder::VideoToolbox,
		"vulkan" => HwEncoder::Vulkan,
		"mediafoundation" | "mf" => HwEncoder::MediaFoundation,
		"rkmpp" => HwEncoder::Rkmpp,
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
	probe_ok(cmd)
}

/// Run a one-frame probe command, treating "still running after 15s" as a failure:
/// a probe ffmpeg that crashed into a (suppressed) WER report or wedged a broken
/// driver would otherwise block startup caps probing forever.
fn probe_ok(mut cmd: std::process::Command) -> bool {
	let Ok(mut child) = cmd.spawn() else {
		return false;
	};
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
	loop {
		match child.try_wait() {
			Ok(Some(st)) => return st.success(),
			Ok(None) => {
				if std::time::Instant::now() >= deadline {
					let _ = child.kill();
					let _ = child.wait();
					return false;
				}
				std::thread::sleep(std::time::Duration::from_millis(50));
			}
			Err(_) => return false,
		}
	}
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

/// UI/wire id for an encoder backend (the `StreamCaps.encoders` / settings vocabulary).
pub fn encoder_wire_id(e: HwEncoder) -> &'static str {
	match e {
		HwEncoder::Auto => "auto",
		HwEncoder::Nvenc => "nvenc",
		HwEncoder::Amf => "amf",
		HwEncoder::Vaapi => "vaapi",
		HwEncoder::Qsv => "qsv",
		HwEncoder::VideoToolbox => "videotoolbox",
		HwEncoder::Vulkan => "vulkan",
		HwEncoder::MediaFoundation => "mediafoundation",
		HwEncoder::Rkmpp => "rkmpp",
		HwEncoder::Software => "software",
	}
}

/// Encoder backends that ACTUALLY work on this machine, preference-ordered, always
/// ending with Software (which needs no probe). Off-Windows each candidate must pass
/// the cached one-frame probe (a generic ffmpeg build LISTS h264_nvenc on a GPU-less
/// SBC); on Windows ffmpeg probes are unreliable on hybrid boxes and the native NVENC
/// SDK path exists, so the name-detected list is trusted as-is.
pub fn validated_encoders(ffmpeg: &str, vaapi_device: &str) -> Vec<HwEncoder> {
	let mut out: Vec<HwEncoder> = detect_encoders(ffmpeg)
		.into_iter()
		.filter(|&e| e != HwEncoder::Software)
		.filter(|&e| {
			#[cfg(windows)]
			{
				let _ = (&e, vaapi_device);
				true
			}
			#[cfg(not(windows))]
			{
				!validated_codecs(ffmpeg, e, vaapi_device).is_empty()
			}
		})
		.collect();
	out.push(HwEncoder::Software);
	out
}

/// GStreamer encode support (Linux): which gst encoder families ACTUALLY work here,
/// each validated by launching a one-frame `videotestsrc → fragment → fakesink`
/// pipeline (the gst analog of `probe_encoder_codec`). Hardware families only get in
/// when both the element exists (`gst-inspect-1.0 --exists`) and the probe exits 0;
/// X264 is included whenever gst itself is present. Cached per (family, codec) for
/// the process lifetime. Empty when gst-launch isn't installed.
#[cfg(target_os = "linux")]
pub fn validated_gst_encoders() -> Vec<(pipeline::gst::GstEncoder, Vec<VCodec>)> {
	use pipeline::gst::{self, GstEncoder};
	use std::collections::HashMap;
	use std::sync::{Mutex, OnceLock};
	static CACHE: OnceLock<Mutex<HashMap<(GstEncoder, VCodec), bool>>> = OnceLock::new();
	if !gst_available() {
		return Vec::new();
	}
	let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	let mut out = Vec::new();
	for enc in GstEncoder::PRIORITY {
		let mut codecs = Vec::new();
		for &codec in enc.codecs() {
			if let Some(&hit) = cache.lock().unwrap().get(&(enc, codec)) {
				if hit {
					codecs.push(codec);
				}
				continue;
			}
			let ok = enc.element(codec).is_some_and(|el| {
				gst_element_exists(el)
					&& gst::encoder_fragment(enc, codec, 4000, 30)
						.is_some_and(|frag| probe_gst_pipeline(&gst::probe_pipeline(&frag)))
			});
			cache.lock().unwrap().insert((enc, codec), ok);
			if ok {
				codecs.push(codec);
			}
		}
		if !codecs.is_empty() {
			out.push((enc, codecs));
		}
	}
	out
}

/// Pick a (family, codec) from the validated gst set: an explicit wire-id request
/// ("rkmpp"/"vaapi"/"nvenc"/"software") binds the family; "auto"/unknown takes the
/// first validated (PRIORITY order = hardware first). Codec preference degrades
/// requested → H.264 → first available, mirroring `resolve_codec`.
#[cfg(target_os = "linux")]
pub fn pick_gst(
	validated: &[(pipeline::gst::GstEncoder, Vec<VCodec>)],
	enc_pref: &str,
	codec_pref: &str,
) -> Option<(pipeline::gst::GstEncoder, VCodec)> {
	let want = codec_from_str(codec_pref);
	let pick_codec = |codecs: &[VCodec]| -> Option<VCodec> {
		if codecs.contains(&want) {
			Some(want)
		} else if codecs.contains(&VCodec::H264) {
			Some(VCodec::H264)
		} else {
			codecs.first().copied()
		}
	};
	if let Some(explicit) = pipeline::gst::from_wire_id(enc_pref) {
		if let Some((enc, codecs)) = validated.iter().find(|(e, _)| *e == explicit) {
			return pick_codec(codecs).map(|c| (*enc, c));
		}
	}
	validated
		.first()
		.and_then(|(enc, codecs)| pick_codec(codecs).map(|c| (*enc, c)))
}

/// The gst-launch binary all gst pipelines (probe AND runtime) spawn through.
/// `~/pulsar-gst-launch` — a user-made copy of gst-launch-1.0 carrying
/// `CAP_SYS_ADMIN` (file capability, granted once via `sudo setcap`) — is
/// preferred when present: DRM `GETFB2` only hands FB handles to privileged
/// callers, so the zero-copy `kmssrc` path needs it (ffmpeg's `kmsgrab` has the
/// same requirement). Plain `gst-launch-1.0` otherwise; every non-KMS pipeline
/// behaves identically under both.
#[cfg(target_os = "linux")]
pub fn gst_launch_bin() -> std::path::PathBuf {
	if let Some(home) = std::env::var_os("HOME") {
		let cap = std::path::Path::new(&home).join("pulsar-gst-launch");
		if cap.is_file() {
			return cap;
		}
	}
	std::path::PathBuf::from("gst-launch-1.0")
}

/// Whether the zero-copy KMS capture→encode path works for this (family, codec):
/// runs `kms_probe_pipeline` (2 real scanout frames through the encoder) via
/// `gst_launch_bin`. Cached per pair — the stack (plugins, capability, DRM)
/// doesn't change within a process lifetime.
#[cfg(target_os = "linux")]
pub fn kms_encode_ok(enc: pipeline::gst::GstEncoder, codec: VCodec) -> bool {
	use std::collections::HashMap;
	use std::sync::{Mutex, OnceLock};
	static CACHE: OnceLock<Mutex<HashMap<(pipeline::gst::GstEncoder, VCodec), bool>>> =
		OnceLock::new();
	let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	if let Some(&hit) = cache.lock().unwrap().get(&(enc, codec)) {
		return hit;
	}
	let ok = pipeline::gst::encoder_fragment(enc, codec, 4000, 30)
		.is_some_and(|frag| probe_gst_pipeline(&pipeline::gst::kms_probe_pipeline(&frag)));
	tracing::info!(?enc, ?codec, ok, "kms zero-copy probe");
	cache.lock().unwrap().insert((enc, codec), ok);
	ok
}

#[cfg(target_os = "linux")]
fn gst_available() -> bool {
	std::process::Command::new("gst-launch-1.0")
		.arg("--version")
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status()
		.map(|st| st.success())
		.unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn gst_element_exists(element: &str) -> bool {
	// Plain inspect, NOT `--exists`: on Ubuntu 22.04's gst 1.20 `--exists` exits 1
	// even for elements that ARE present (verified live on the Orange Pi — mpph264enc
	// and x264enc both "missing" per --exists, both exit 0 via plain inspect).
	std::process::Command::new("gst-inspect-1.0")
		.arg(element)
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status()
		.map(|st| st.success())
		.unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn probe_gst_pipeline(pipeline: &str) -> bool {
	let mut cmd = std::process::Command::new(gst_launch_bin());
	cmd.arg("-q").args(pipeline.split_whitespace());
	cmd.stdout(std::process::Stdio::null());
	cmd.stderr(std::process::Stdio::null());
	cmd.status().map(|st| st.success()).unwrap_or(false)
}

/// Sunshine-style runtime VALIDATION of one encoder+codec: actually encode a single
/// synthetic frame and check ffmpeg exits 0. Catches the cases name-presence detection
/// misses — encoder listed but the GPU/driver can't init it (e.g. `av1_nvenc` on an Ampere
/// card, `h264_qsv` with no Intel GPU). Results are cached per (encoder, codec) for the
/// process lifetime so we probe at most once each.
fn probe_encoder_codec(
	ffmpeg: &str,
	encoder: HwEncoder,
	codec: VCodec,
	vaapi_device: &str,
) -> bool {
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
			probe_ok(cmd)
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

/// Linux: tie a child to Pulsar's lifetime via `PR_SET_PDEATHSIG` (mirrors the gst
/// spawns in host/handlers.rs) — a Pulsar crash/SIGKILL must never leave an ffmpeg
/// capturing and streaming the host's screen/audio to the last client. Windows gets
/// the equivalent through job.rs; macOS has no prctl (accepted gap).
#[cfg(target_os = "linux")]
fn die_with_parent(cmd: &mut std::process::Command) {
	use std::os::unix::process::CommandExt;
	unsafe {
		cmd.pre_exec(|| {
			// SAFETY: async-signal-safe libc calls only.
			libc::prctl(
				libc::PR_SET_PDEATHSIG,
				libc::SIGKILL as libc::c_ulong,
				0,
				0,
				0,
			);
			// Parent died before the prctl took effect — don't outlive it.
			if libc::getppid() == 1 {
				libc::_exit(0);
			}
			Ok(())
		});
	}
}

/// Spawn a process and remember it (in `procs`) so it can be stopped later.
pub fn spawn_tracked(
	procs: &Arc<Mutex<Vec<Child>>>,
	program: &str,
	args: &[String],
) -> Result<(), String> {
	let mut cmd = std::process::Command::new(program);
	cmd.args(args);
	no_window(&mut cmd); // never pop up a console window for ffmpeg
	#[cfg(target_os = "linux")]
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(child) => {
			// Tie it to Pulsar's lifetime so a crash/taskkill/tray-quit can't leave the
			// encoder running and pegging NVENC (see job.rs).
			#[cfg(windows)]
			crate::job::assign(&child);
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("{program} {}: {e}", crate::i18n::t("err.spawn"))),
	}
}

/// Spawn the stream-encode ffmpeg like `spawn_tracked`, but with `-nostats -progress
/// pipe:2` injected and stderr piped to a parser thread that measures the ENCODE PACE:
/// per-frame wall time between progress ticks (Δt/Δframes, ms). Realtime capture pins
/// this near the frame budget while the encoder keeps up; it RISES when encoding falls
/// behind — the host-side number the client's "Kodlama ms" tile shows. The thread calls
/// `on_ms` (~2 Hz) and exits when ffmpeg does.
pub fn spawn_tracked_enc_paced(
	procs: &Arc<Mutex<Vec<Child>>>,
	program: &str,
	args: &[String],
	on_ms: impl Fn(f32) + Send + 'static,
) -> Result<(), String> {
	let mut cmd = std::process::Command::new(program);
	// Global options — must precede everything else on the command line.
	cmd.args(["-nostats", "-progress", "pipe:2"]);
	cmd.args(args);
	cmd.stderr(std::process::Stdio::piped());
	no_window(&mut cmd);
	#[cfg(target_os = "linux")]
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(mut child) => {
			#[cfg(windows)]
			crate::job::assign(&child);
			if let Some(stderr) = child.stderr.take() {
				std::thread::spawn(move || {
					use std::io::BufRead;
					let reader = std::io::BufReader::new(stderr);
					let mut last: Option<(u64, std::time::Instant)> = None;
					for line in reader.lines() {
						let Ok(line) = line else { break };
						let Some(v) = line.strip_prefix("frame=") else {
							continue;
						};
						let Ok(frame) = v.trim().parse::<u64>() else {
							continue;
						};
						let now = std::time::Instant::now();
						if let Some((f0, t0)) = last {
							let df = frame.saturating_sub(f0);
							if df > 0 {
								let ms = now.duration_since(t0).as_secs_f32() * 1000.0 / df as f32;
								on_ms(ms);
							}
						}
						last = Some((frame, now));
					}
				});
			}
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("{program} {}: {e}", crate::i18n::t("err.spawn"))),
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
/// style, replacing the webview WebCodecs path). On macOS: an OVERLAY-ONLY eframe window (the
/// video is the separate native mpv child until the Metal renderer phase) — same egui overlay.
/// A workspace crate, dropped next to the app exe. Resolution is path-based (bundled resources
/// dir, then next to the exe), so it is platform-agnostic.
#[cfg(any(unix, target_os = "windows"))]
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
