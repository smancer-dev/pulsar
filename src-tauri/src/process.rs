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
		let raw = pulsar_capture::list_displays();
		tracing::info!(count = raw.len(), ?raw, "host_displays (DXGI)");
		return raw
			.into_iter()
			.map(|(idx, name, width, height, primary)| DisplayInfo {
				// `name` is the DXGI `DeviceName` (the GDI `\\.\DISPLAYn` trimmed of the
				// `\\.\` prefix), so it doubles as the EnumDisplaySettings device name — and
				// the iteration order matches `display_idx`. Advertise this monitor's available
				// resolutions for the session menu + the screen-adaptation best-fit picker.
				modes: enum_display_modes(&name),
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
				// X11 mode enumeration is not wired here; the client falls back to its fixed
				// resolution presets (and screen adaptation is Windows-only anyway).
				modes: Vec::new(),
			})
			.collect();
	}
	#[cfg(not(any(windows, target_os = "linux")))]
	{
		Vec::new()
	}
}

/// Enumerate a monitor's available resolutions via `EnumDisplaySettingsW` on its GDI device
/// name, returning distinct `(width, height)` pairs, largest-area first, capped to a sane count.
///
/// `dev_name` is the trimmed DXGI/GDI name (`DISPLAY1`); we re-prepend `\\.\` to form the full
/// `\\.\DISPLAYn` the API wants (same convention as `util::display_rotation_detect`). Only modes
/// at the monitor's CURRENT color depth are kept (so the same physical resolution doesn't appear
/// once per bit-depth), then deduped on `(w, h)` and sorted by descending area. The cap (24)
/// keeps the advertised list — which travels in `StreamCaps::displays[..].modes` — small on the
/// wire; a 4K panel typically exposes well under that anyway. Empty on any failure (the client
/// then shows only its fixed presets and screen adaptation simply has no candidates to pick).
#[cfg(windows)]
fn enum_display_modes(dev_name: &str) -> Vec<(u32, u32)> {
	use windows_sys::Win32::Graphics::Gdi::{
		EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
	};
	let wide: Vec<u16> = format!(r"\\.\{dev_name}")
		.encode_utf16()
		.chain(std::iter::once(0u16))
		.collect();
	let mut modes: Vec<(u32, u32)> = Vec::new();
	unsafe {
		// Current color depth: only enumerate modes matching it, so a single resolution isn't
		// listed once per supported bit depth (8/16/32-bit). Fall back to 0 (= "any") if the
		// current-settings query fails, which keeps the loop inclusive rather than empty.
		let cur_bpp = {
			let mut dm: DEVMODEW = std::mem::zeroed();
			dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
			if EnumDisplaySettingsW(wide.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
				dm.dmBitsPerPel
			} else {
				0
			}
		};
		let mut i = 0u32;
		loop {
			let mut dm: DEVMODEW = std::mem::zeroed();
			dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
			if EnumDisplaySettingsW(wide.as_ptr(), i, &mut dm) == 0 {
				break; // exhausted the driver's mode list
			}
			i += 1;
			if cur_bpp != 0 && dm.dmBitsPerPel != cur_bpp {
				continue;
			}
			let wh = (dm.dmPelsWidth, dm.dmPelsHeight);
			if wh.0 == 0 || wh.1 == 0 {
				continue;
			}
			if !modes.contains(&wh) {
				modes.push(wh);
			}
		}
	}
	// Largest area first (the UI shows the biggest resolutions on top, and the adaptation
	// best-fit's native-area bias assumes the list reflects real geometry, not driver order).
	modes.sort_by(|a, b| (b.0 as u64 * b.1 as u64).cmp(&(a.0 as u64 * a.1 as u64)));
	modes.truncate(24);
	modes
}

pub fn capture_from_str(s: &str) -> CaptureMethod {
	match s {
		"x11grab" => CaptureMethod::X11grab,
		// "kmsgrab" is intentionally NOT accepted here: it produces DRM_PRIME
		// hwframes and the encode_command pipeline has no hwdownload/hwmap stage
		// to bring those frames into a usable pixel format.  Every encoder would
		// receive an incompatible hwframe and the stream would be dead.  Fall
		// through to default_for_os() (x11grab on Linux) until a real
		// encoder-aware KMS filter chain exists.
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
	// Assign the probe child to the Windows Job Object so it dies with Pulsar on
	// abnormal exit (crash / taskkill), honouring job.rs's "assign every spawned
	// child" invariant.  spawn_tracked / spawn_tracked_enc_paced already do this;
	// probes previously did not, leaving an orphaned ffmpeg.exe on abnormal exit.
	#[cfg(windows)]
	crate::job::assign(&child);
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
	/// Optional cover image (data URL or http URL) sent to clients on the game card.
	#[serde(default)]
	pub image: String,
	#[serde(rename = "cmdStart", default)]
	pub cmd_start: String,
	#[allow(dead_code)]
	#[serde(rename = "cmdStop", default)]
	pub cmd_stop: String,
}

/// Grab ONE live frame of the host's screen as a small JPEG `data:` URL — the cover for
/// the built-in "Desktop" game card, so the client shows what it would stream right now.
/// Uses the bundled ffmpeg's screen-capture input per platform (gdigrab/x11grab/
/// avfoundation), scaled to 480px wide. Best-effort: returns `None` on any failure
/// (headless/no display/timeout) so the client just falls back to an icon. Bounded to a
/// few seconds so a wedged grab never stalls the games-list reply.
pub fn desktop_thumb_data_url(ffmpeg: &str) -> Option<String> {
	let tmp = std::env::temp_dir().join("pulsar-desk-thumb.jpg");
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
	#[cfg(windows)]
	cmd.args(["-f", "gdigrab", "-framerate", "1", "-i", "desktop"]);
	#[cfg(target_os = "linux")]
	{
		let disp = std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into());
		cmd.args(["-f", "x11grab", "-framerate", "1", "-i"]).arg(disp);
	}
	#[cfg(target_os = "macos")]
	cmd.args(["-f", "avfoundation", "-framerate", "1", "-i", "1"]);
	cmd.args(["-frames:v", "1", "-vf", "scale=480:-1", "-q:v", "6"])
		.arg(&tmp);
	no_window(&mut cmd);
	cmd.stdout(std::process::Stdio::null());
	cmd.stderr(std::process::Stdio::null());
	let mut child = cmd.spawn().ok()?;
	#[cfg(windows)]
	crate::job::assign(&child);
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
	let ok = loop {
		match child.try_wait() {
			Ok(Some(st)) => break st.success(),
			Ok(None) => {
				if std::time::Instant::now() >= deadline {
					let _ = child.kill();
					let _ = child.wait();
					break false;
				}
				std::thread::sleep(std::time::Duration::from_millis(40));
			}
			Err(_) => break false,
		}
	};
	let out = if ok {
		std::fs::read(&tmp).ok().map(|jpg| crate::avatar::data_url(&jpg))
	} else {
		None
	};
	let _ = std::fs::remove_file(&tmp);
	out
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

/// Launch a host game: its start hook, then the program/command itself. Returns the PID of
/// the launched process so the caller can resolve its top-level window for per-window (WGC)
/// capture in game mode (`None` when nothing was launched — e.g. the "Desktop" entry, which
/// streams the whole desktop). The launched process is tracked in the Windows Job Object so
/// it dies with Pulsar, exactly like before; capturing the `Child` (instead of the old
/// fire-and-forget `spawn_shell`) is what lets us read its `id()`.
///
/// For `program` we spawn the executable DIRECTLY (so the returned PID is the game's own,
/// not a `cmd.exe` wrapper's). For `command` we spawn through the shell and return the
/// shell's PID — the real program is a DESCENDANT, which `resolve_launched_window` matches
/// (it walks child PIDs). The launcher case (Steam/Epic) is also covered by that descendant
/// walk + the retry budget, since those re-parent the game into a separate child process.
pub fn launch_host_game(g: &HostGame) -> Option<u32> {
	spawn_shell(&g.cmd_start);
	match g.kind.as_str() {
		"program" if !g.path.is_empty() => spawn_program_pid(&g.path, &g.args),
		"command" if !g.command.is_empty() => spawn_shell_pid(&g.command),
		_ => None,
	}
}

/// Spawn an executable directly (no shell), no console window, tracked in the Job Object.
/// Returns its PID. `args` is a single user-entered argument string, split on whitespace
/// (best-effort — matches the old `spawn_shell(format!("\"{path}\" {args}"))` behavior
/// without a shell in the middle, so the PID we get is the program's). Fire-and-forget: the
/// `Child` handle is dropped (the Job Object owns lifetime).
fn spawn_program_pid(path: &str, args: &str) -> Option<u32> {
	let mut cmd = std::process::Command::new(path);
	for a in args.split_whitespace() {
		cmd.arg(a);
	}
	no_window(&mut cmd);
	cmd.stdout(std::process::Stdio::null());
	cmd.stderr(std::process::Stdio::null());
	let child = cmd.spawn().ok()?;
	#[cfg(windows)]
	crate::job::assign(&child);
	let pid = child.id();
	// Drop the handle but keep the process running (Job Object owns teardown).
	std::mem::forget(child);
	Some(pid)
}

/// Like [`spawn_shell`] but returns the shell process's PID (the launched command runs as a
/// descendant). Used for the `command` game kind, where the user's command may be a script /
/// pipeline that needs a shell. The game's window is found via the descendant-PID walk.
fn spawn_shell_pid(cmd_str: &str) -> Option<u32> {
	let cmd_str = cmd_str.trim();
	if cmd_str.is_empty() {
		return None;
	}
	#[cfg(windows)]
	let mut c = {
		let mut c = std::process::Command::new("cmd");
		c.args(["/C", cmd_str]);
		c
	};
	#[cfg(not(windows))]
	let mut c = {
		let mut c = std::process::Command::new("sh");
		c.args(["-c", cmd_str]);
		c
	};
	no_window(&mut c);
	let child = c.spawn().ok()?;
	#[cfg(windows)]
	crate::job::assign(&child);
	let pid = child.id();
	std::mem::forget(child);
	Some(pid)
}

/// Poll for the top-level window of a launched game/app (Phase 2b game-mode WGC capture).
/// Games create their window asynchronously (and launchers re-parent into a child process),
/// so this retries [`pulsar_capture::find_window_for_launch`] — which matches the launched
/// PID *and its descendants* — for up to ~10 s (250 ms apart). Returns the resolved HWND as
/// an `i64` (wire/`StreamReq::window_hwnd` form) or `None` if no window appeared in time
/// (the caller then falls back to whole-desktop/monitor capture). Blocking — run it off the
/// session task (e.g. in a spawned thread that stashes the result into a per-session slot).
///
/// ## Reliability caveats
/// - Borderless-fullscreen games are found fine; a few anti-cheat / DRM titles create their
///   render window in a protected process whose windows EnumWindows can't see — those fall
///   back to monitor capture.
/// - A launcher that hands off to an ALREADY-RUNNING game instance (e.g. Steam reusing a live
///   client) spawns no new descendant, so the new window isn't tied to our PID tree → not
///   found (fallback). This is the documented honest ceiling; the explicit window-list pick
///   (`host_window_list`) is the robust path for those.
#[cfg(windows)]
pub fn resolve_launched_window(pid: u32) -> Option<i64> {
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
	loop {
		if let Some(hwnd) = pulsar_capture::find_window_for_launch(pid) {
			return Some(hwnd as i64);
		}
		if std::time::Instant::now() >= deadline {
			return None;
		}
		std::thread::sleep(std::time::Duration::from_millis(250));
	}
}

/// Non-Windows stub (see [`resolve_launched_window`]) — per-window capture is Windows-only.
#[cfg(not(windows))]
pub fn resolve_launched_window(_pid: u32) -> Option<i64> {
	None
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

/// Make a `pulsar-render` child resolve the HOST's system ffmpeg instead of the one
/// bundled in the AppImage, when the bundle's ffmpeg lacks the SoC hardware decoder
/// the system has. Apply to EVERY `pulsar-render` invocation on Linux — the real
/// renderer AND the `--probe` (so capability negotiation + the overlay HUD report the
/// decoder that will actually run).
///
/// Why: the Linux AppImage bundles a GENERIC ffmpeg (no `h264_rkmpp`/`hevc_rkmpp`) and
/// `AppRun` puts its libs first on `LD_LIBRARY_PATH`, shadowing the Pi's Rockchip
/// ffmpeg. `pulsar-render`'s tiered decoder selection then finds NO DRM_PRIME hardware
/// decoder among the bundled libs and falls to Tier-2 software decode — the RK3588
/// appliance silently software-decodes H.265 (high CPU, capped fps). Prepending the
/// host's multiarch lib dir makes the loader resolve the system `libav*` (which pull in
/// `librockchip_mpp`/`librga`) ahead of the bundled set, restoring zero-copy `rkmpp`.
///
/// Gated so it can only ever help: auto-on only when running inside an AppImage
/// (`APPDIR` set) AND a Rockchip decode device (`/dev/mpp_service`) is present AND the
/// host actually ships a matching `libavcodec.so.58`. `PULSAR_PREFER_SYSTEM_FFMPEG=1`
/// forces it on (other SoCs); `=0` forces it off. No-op on non-Linux and on x86
/// desktops without the device, so behaviour is unchanged everywhere else.
pub fn apply_render_lib_env(cmd: &mut std::process::Command) {
	#[cfg(target_os = "linux")]
	{
		let on = match std::env::var("PULSAR_PREFER_SYSTEM_FFMPEG").as_deref() {
			Ok("0") | Ok("off") | Ok("false") => return,
			Ok("1") | Ok("on") | Ok("true") => true,
			// Auto: the documented RK3588 appliance case — inside an AppImage, on a
			// Rockchip SoC, where the bundled ffmpeg is the rkmpp-less generic build.
			_ => {
				std::env::var_os("APPDIR").is_some()
					&& std::path::Path::new("/dev/mpp_service").exists()
			}
		};
		if !on {
			return;
		}
		// Debian/Ubuntu multiarch layout. The soname (`.58`) must match the ffmpeg major
		// `pulsar-render` is linked against (ffmpeg-sys-next); if the host has a different
		// major the guard fails and this stays a safe no-op (bundled libs are kept).
		let sysdir = format!("/usr/lib/{}-linux-gnu", std::env::consts::ARCH);
		if !std::path::Path::new(&format!("{sysdir}/libavcodec.so.58")).exists() {
			return;
		}
		let new_val = match std::env::var("LD_LIBRARY_PATH") {
			Ok(cur) if !cur.is_empty() => format!("{sysdir}:{cur}"),
			_ => sysdir,
		};
		cmd.env("LD_LIBRARY_PATH", new_val);
	}
	#[cfg(not(target_os = "linux"))]
	{
		let _ = cmd;
	}
}

/// The main Tauri window's native HWND (Windows) as a u64, so `pulsar-render` can create its
/// D3D11 child window under it via `--wid` (the Win32 analogue of Linux's `render::window_xid`).
/// None before the window exists.
#[cfg(target_os = "windows")]
pub fn window_hwnd(app: &AppHandle) -> Option<u64> {
	let w = app.get_webview_window("main")?;
	w.hwnd().ok().map(|h| h.0 as u64)
}
