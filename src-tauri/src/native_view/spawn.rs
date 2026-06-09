//! Process-spawning helpers for the native video path: pick a free UDP port, write the
//! tiny SDP describing the host's RTP stream, and spawn the platform renderer/audio player
//! (ffplay on Windows; the native vidsink/render or mpv on Linux/macOS) plus the Linux
//! native audio player. Each spawned child is wired to die with this process.

use std::path::PathBuf;
// `Path` is only referenced by the non-Windows `spawn_mpv`/vidsink/render helpers below.
#[cfg(not(windows))]
use std::path::Path;
use std::process::Child;

/// Make a spawned child process die when **this** (pulsar-tauri) process dies — the Linux
/// analogue of the Windows Job Object (`job.rs`). Without it, an abnormal pulsar-tauri exit
/// (crash, `kill`, a tray-quit, or the maintainer's relaunch-to-reconnect) ORPHANS the native
/// renderer: `pulsar-render` keeps its GL context, UDP port and `--wid` X11 child window alive
/// with no parent. Those orphans then ACCUMULATE one-per-reconnect and contend for the Mali
/// GPU + CPU → the streamed video sags below the panel rate and gains periodic ~100 ms hitches
/// (the "stutter after many reconnects" / "host stale" symptom — it was actually Pi-side orphan
/// pile-up, not the host). `PR_SET_PDEATHSIG(SIGKILL)` makes the kernel kill the child the moment
/// its parent goes away, covering even `SIGKILL` of the parent (an at-exit cleanup never would).
///
/// Caveat: `PR_SET_PDEATHSIG` fires on the parent *thread's* death; we spawn from a long-lived
/// tokio worker, and the `getppid()==1` re-check closes the fork/exec race, so the child can't
/// silently survive. No-op on macOS (no `PR_SET_PDEATHSIG`); Windows uses `job.rs` instead.
#[cfg(not(windows))]
fn die_with_parent(cmd: &mut std::process::Command) {
	#[cfg(target_os = "linux")]
	{
		use std::os::unix::process::CommandExt;
		// SAFETY: the closure runs in the forked child before exec and calls only
		// async-signal-safe libc functions (prctl/getppid/_exit).
		unsafe {
			cmd.pre_exec(|| {
				libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
				// If pulsar-tauri already died between fork and now, PR_SET_PDEATHSIG won't
				// fire — bail immediately so we never become an orphan.
				if libc::getppid() == 1 {
					libc::_exit(0);
				}
				Ok(())
			});
		}
	}
	#[cfg(not(target_os = "linux"))]
	let _ = cmd; // macOS: rely on explicit teardown (stop_stream) — no PR_SET_PDEATHSIG.
}

/// Grab a free UDP port for the native player to receive RTP on (bind then drop;
/// ffplay rebinds it immediately after). The tiny TOCTOU window is fine on LAN.
pub fn free_udp_port() -> Option<u16> {
	let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
	let port = sock.local_addr().ok()?.port();
	drop(sock);
	Some(port)
}

/// Write a minimal SDP describing the host's RTP video (payload type 96, 90 kHz) on
/// `port`, so the native player knows how to depacketize it. `codec` picks the rtpmap
/// (`h265`/`hevc` → H265, `av1` → AV1, else H264). Returns the file path.
pub fn write_sdp(port: u16, codec: &str) -> std::io::Result<PathBuf> {
	let (rtpmap, fmtp) = match codec {
		"h265" | "hevc" => ("H265/90000", ""),
		"av1" => ("AV1/90000", ""),
		_ => ("H264/90000", "a=fmtp:96 packetization-mode=1\r\n"),
	};
	let sdp = format!(
		"v=0\r\n\
		 o=- 0 0 IN IP4 127.0.0.1\r\n\
		 s=Pulsar\r\n\
		 c=IN IP4 0.0.0.0\r\n\
		 t=0 0\r\n\
		 m=video {port} RTP/AVP 96\r\n\
		 a=rtpmap:96 {rtpmap}\r\n{fmtp}"
	);
	let path = std::env::temp_dir().join(format!("pulsar-{port}.sdp"));
	std::fs::write(&path, sdp)?;
	Ok(path)
}

/// Spawn ffplay to render the SDP stream: hardware decode, low-latency, borderless
/// fullscreen, always-on-top, no audio (audio still plays via the webview path). The
/// returned `Child` is kept so the caller can kill it on session end. No console
/// window pops up. Returns None if ffplay can't be spawned.
pub fn spawn_ffplay(ffplay: &str, sdp: &PathBuf) -> Option<Child> {
	let mut cmd = std::process::Command::new(ffplay);
	cmd.args([
		"-hide_banner",
		"-loglevel",
		"error",
		"-protocol_whitelist",
		"file,rtp,udp",
		// Low-latency: no input buffering, drop late frames, tiny probe.
		"-fflags",
		"nobuffer",
		"-flags",
		"low_delay",
		"-framedrop",
		"-avioflags",
		"direct",
		"-probesize",
		"32",
		"-analyzeduration",
		"0",
		"-sync",
		"ext",
		// Hardware decode (d3d11va/dxva2/cuda) — auto-picks what the GPU supports.
		"-hwaccel",
		"auto",
		"-an", // audio comes through the webview WebAudio path, not ffplay
		"-noborder",
		"-fs",
		"-alwaysontop",
		"-autoexit",
		"-window_title",
		"Pulsar",
		"-i",
	]);
	cmd.arg(sdp);
	crate::no_window(&mut cmd);
	match cmd.spawn() {
		Ok(child) => {
			// Kill the fullscreen player too if Pulsar dies abnormally (see job.rs).
			#[cfg(windows)]
			crate::job::assign(&child);
			Some(child)
		}
		Err(_) => None,
	}
}

/// Spawn **Pulsar's native zero-copy video sink** (Linux/RK3588) — the renderer that
/// replaces mpv. It HW-decodes the SDP/RTP stream with `h264_rkmpp` (frames stay in GPU
/// memory as DRM_PRIME) and presents each frame straight from its dmabuf via an EGLImage
/// (`GL_TEXTURE_EXTERNAL_OES`) — NO GPU→CPU download, the path mpv 0.34's gpu VO can't do
/// (that download was the ~3 fps + multi-second-latency bug). Embedded in the Pulsar window
/// via `--wid` (a child X11 window), exactly like the old `mpv --wid`. Because it renders
/// far faster than the source it drains the UDP socket immediately → no backlog → Moonlight-
/// class low latency at native resolution. stdout carries `vidsink-fps <fps> <w>x<h>` lines
/// (piped) for the perf HUD. Returns None if the binary is missing (caller falls back to mpv).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn spawn_vidsink(bin: &str, sdp: &Path, wid: Option<u64>, rotate: u32) -> Option<Child> {
	let mut cmd = std::process::Command::new(bin);
	cmd.arg(sdp);
	if let Some(xid) = wid {
		cmd.arg("--wid").arg(format!("0x{xid:x}"));
	}
	// Rotate the displayed video to match the host's display orientation (auto-detected from the
	// host's DisplayRotation, or the PULSAR_ROTATE manual override). 0/90/180/270 CW.
	if rotate % 360 != 0 {
		cmd.arg("--rotate").arg(rotate.to_string());
	}
	cmd.arg("--stats");
	cmd.stdout(std::process::Stdio::piped());
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(child) => Some(child),
		Err(_) => None,
	}
}

/// Spawn the native **overlay** renderer (`pulsar-render`) for the Linux client. It is a
/// separate top-level override-redirect ARGB window positioned over the Pulsar window (`wid`):
/// the compositor blends its egui overlay over the video below. Hidden until SIGUSR1 (open) /
/// SIGUSR2 (close). stdin carries `stat <fps> <lat> <dec> <mbps>` lines (live HUD); stdout
/// carries `ov set <field> <val>` / `ov end` / `ov close` (user interaction). Returns None if
/// the binary is missing (the overlay is then simply unavailable; video is unaffected).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn spawn_render(bin: &str, sdp: &Path, wid: Option<u64>, game_mode: bool, pace_on: bool) -> Option<Child> {
	// Single-surface renderer: rkmpp video + egui overlay in ONE child window of `wid` (the
	// overlay moves/clips/stacks with the app). stdout carries `vidsink-fps …` (HUD) + `ov …`
	// (overlay interaction). SIGUSR1/2 toggle the overlay; video runs throughout.
	let mut cmd = std::process::Command::new(bin);
	cmd.arg(sdp);
	if let Some(xid) = wid {
		cmd.arg("--wid").arg(format!("0x{xid:x}"));
	}
	cmd.arg("--mode").arg(if game_mode { "game" } else { "remote" });
	cmd.arg("--pace").arg(if pace_on { "on" } else { "off" });
	// Pipe BOTH stdout (HUD `vidsink-fps` / `ov set` lines) AND stdin (live `pace 0|1` toggles
	// from the frontend); without stdin piped, set_frame_pacing can't reach the renderer.
	cmd.stdin(std::process::Stdio::piped());
	cmd.stdout(std::process::Stdio::piped());
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(child) => Some(child),
		Err(_) => None,
	}
}

/// Spawn the Windows native renderer (`pulsar-render`) embedded in the Tauri window via
/// `--wid <hwnd>` — the Win32 analogue of the Linux `spawn_render`. It receives the host's
/// RTP on the SDP's UDP port, HW-decodes with Media Foundation (DXVA), and presents NV12→RGB
/// on a D3D11 swapchain inside a child HWND of the app window (replacing the webview WebCodecs
/// path). stdin carries `stat …` / `open|close` / `pace 0|1`; stdout carries `vidsink-fps …`
/// / `ov …`. Tied to the Job Object so it dies with Pulsar (job.rs). None on spawn failure.
#[cfg(windows)]
pub fn spawn_render_win(bin: &str, sdp: &PathBuf, hwnd: u64, game_mode: bool, pace_on: bool) -> Option<Child> {
	let mut cmd = std::process::Command::new(bin);
	cmd.arg(sdp);
	cmd.arg("--wid").arg(format!("0x{hwnd:x}"));
	cmd.arg("--mode").arg(if game_mode { "game" } else { "remote" });
	cmd.arg("--pace").arg(if pace_on { "on" } else { "off" });
	cmd.stdin(std::process::Stdio::piped());
	cmd.stdout(std::process::Stdio::piped());
	crate::no_window(&mut cmd);
	match cmd.spawn() {
		Ok(child) => {
			crate::job::assign(&child);
			Some(child)
		}
		Err(_) => None,
	}
}

/// Spawn a native **audio** player for the Linux client. WebKitGTK can't decode the host's
/// Opus/RTP audio via WebCodecs (its webview audio path is silent), so — like the native
/// `vidsink` for video — we decode + play it natively. ffmpeg receives the Opus RTP on
/// `127.0.0.1:loopback_port` (an SDP describes it; `Viewer::forward_audio_to_loopback` pumps
/// the host's audio datagrams there) and plays it to PulseAudio. Returns None on spawn failure.
#[cfg(target_os = "linux")]
pub fn spawn_native_audio(ffmpeg: &str, loopback_port: u16) -> Option<Child> {
	// Matches the host encoder (pulsar_core::audio::opus_rtp_output): Opus, 48 kHz, stereo,
	// RTP payload type 97. The viewer forwards the host's datagrams to this loopback port.
	let sdp = format!(
		"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=PulsarAudio\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
		 m=audio {loopback_port} RTP/AVP 97\r\na=rtpmap:97 opus/48000/2\r\n"
	);
	let path = std::env::temp_dir().join(format!("pulsar-audio-{loopback_port}.sdp"));
	if std::fs::write(&path, sdp).is_err() {
		return None;
	}
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args([
		"-hide_banner", "-loglevel", "error",
		"-protocol_whitelist", "file,udp,rtp",
		"-fflags", "nobuffer", "-flags", "low_delay",
		"-i",
	])
	.arg(&path)
	.args(["-f", "pulse", "default"]);
	cmd.stdin(std::process::Stdio::null());
	cmd.stdout(std::process::Stdio::null());
	cmd.stderr(std::process::Stdio::null());
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(child) => Some(child),
		Err(_) => None,
	}
}

/// Spawn **mpv** to render the SDP/RTP stream on Linux/macOS with hardware decode and
/// zero-copy GL output. This is the path that actually works on Rockchip RK3588:
/// `--hwdec=auto` selects the `h264_rkmpp`/`hevc_rkmpp` hardware decoder (whose frames
/// are DRM_PRIME — ffplay/SDL can't show those, but mpv imports them via EGL with no CPU
/// copy, the same shape Moonlight uses). On non-Rockchip Linux `--hwdec=auto` falls back
/// to VAAPI/NVDEC.
///
/// When `wid` is `Some(xid)` mpv is **embedded INSIDE the given X11 window** (the Pulsar
/// app window) via `--wid`, so the video renders *in-app* (like the WebCodecs canvas on
/// Windows) instead of a separate top-level window — the goal on Linux/X11 where the
/// webview can't hardware-decode. When `None` it falls back to a borderless fullscreen
/// window (legacy / non-X11).
///
/// The demuxer opts give it a large UDP receive buffer and make it **survive overruns and
/// drop corrupt access units** (self-healing on the next keyframe) instead of freezing on
/// a lost RTP burst — the receiver-side half of the packet-loss fix (the device's
/// `net.core.rmem_max` must also be raised, see the Pi setup). Returns None if mpv isn't
/// installed (caller then falls back to the webview).
///
/// `ipc` is the JSON-IPC socket path mpv listens on (`--input-ipc-server`); the caller
/// (`lib.rs`) derives a deterministic per-session path and keeps it in
/// `PlaySession.mpv_ipc` so it can later `mpv_set_pause` it (gaming-overlay toggle, Faz 3)
/// and poll real fps/drops/bitrate/vo-delay over it (perf HUD, Faz 4). It does not conflict
/// with `--no-input-default-bindings`/`--input-vo-keyboard=no`/`--input-cursor=no` (those
/// gate VO input, not this command channel) and works on macOS too (AF_UNIX).
#[cfg(not(windows))]
pub fn spawn_mpv(sdp: &PathBuf, wid: Option<u64>, ipc: &Path) -> Option<Child> {
	let mut cmd = std::process::Command::new("mpv");
	cmd.args([
		"--hwdec=auto", // RK3588 → rkmpp; otherwise vaapi/nvdec
		"--vo=gpu",
		"--profile=low-latency",
		"--cache=no",
		"--demuxer-readahead-secs=0",
		"--vd-lavc-threads=1",
		"--framedrop=decoder",
		// RTP has no usable PTS → without these mpv paces to a made-up 30fps (adds latency
		// + wrong fps). Show each frame the instant it's decoded = lowest glass-to-glass.
		"--untimed",
		"--no-correct-pts",
		"--video-sync=desync",
		"--demuxer-lavf-probe-info=no",
		"--audio=no", // audio is handled separately (webview WebAudio / a second stream)
		"--no-osc",
		"--no-config",
		// Don't let mpv consume keyboard/mouse/cursor — control is captured + forwarded to
		// the host separately; the embedded video must never steal the local input.
		"--input-vo-keyboard=no",
		"--no-input-default-bindings",
		"--input-cursor=no",
		"--cursor-autohide=always",
	]);
	// libavformat options. The RTP/UDP protocol whitelist is REQUIRED to open the SDP; mpv's
	// key-value-list needs the comma-containing value length-escaped as `%N%…` (29 =
	// len("file,udp,rtp,rtcp,crypto,data")). mpv has no `--protocol-whitelist`. `+discardcorrupt`
	// drops a damaged AU cleanly so rkmpp recovers on the next IDR.
	//
	// `buffer_size` = the UDP socket SO_RCVBUF, and it is THE latency knob: mpv reads at the
	// display rate (`--demuxer-readahead-secs=0`) and never drains ahead, so any RTP that piles
	// up in this socket buffer becomes a fixed glass-to-glass delay (fill ÷ bitrate). The old
	// 8 MiB let ~10 s of video queue on the Pi (mpv 0.34 here can't HW-decode→present fast
	// enough to keep the buffer empty). A small buffer caps that delay (and drops the oldest
	// burst instead of playing it seconds late). Env-tunable via PULSAR_BUFSZ; 256 KiB default
	// (~0.5 s cap at the default bitrate) trades a little burst tolerance for low latency —
	// raise it if heavy motion glitches, lower it for less lag.
	let bufsz: u32 = std::env::var("PULSAR_BUFSZ")
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(262144);
	cmd.arg(format!(
		"--demuxer-lavf-o=protocol_whitelist=%29%file,udp,rtp,rtcp,crypto,data,buffer_size={bufsz},fifo_size=1000000,overrun_nonfatal=1,fflags=+nobuffer+discardcorrupt,probesize=32,analyzeduration=0,max_delay=0"
	));
	// JSON-IPC channel: lets us pause/resume (gaming-overlay toggle) and poll real
	// fps/drops/bitrate/vo-delay for the perf HUD. Distinct from VO input (above) — this is
	// the command socket. Path is owned by lib.rs (deterministic per session id).
	cmd.arg(format!("--input-ipc-server={}", ipc.display()));
	// The perf overlay now lives in the Pulsar HUD (stats come back over the IPC socket, not
	// mpv's OSD). Keep the OSD text only behind PULSAR_OSD=1 for native debugging.
	if std::env::var_os("PULSAR_OSD").is_some() {
		cmd.args([
			"--osd-level=3",
			"--osd-align-x=right",
			"--osd-align-y=top",
			"--osd-font-size=22",
			"--osd-back-color=#80000000",
			"--osd-status-msg=Pulsar   FPS ${estimated-vf-fps}   drops ${decoder-frame-drop-count}   ${video-bitrate}",
		]);
	}
	match wid {
		Some(xid) => {
			// Embed inside the Pulsar app window (X11 only). `x11egl` gives the zero-copy
			// DRM_PRIME→EGL present path on Panfrost (RK3588). No --fullscreen/--title:
			// mpv parents a child window filling the app window and moves/closes with it.
			cmd.arg(format!("--wid=0x{xid:x}"));
			cmd.arg("--gpu-context=x11egl");
		}
		None => {
			cmd.args(["--fullscreen", "--no-border", "--title=Pulsar"]);
		}
	}
	cmd.arg(sdp);
	die_with_parent(&mut cmd);
	match cmd.spawn() {
		Ok(child) => Some(child),
		Err(_) => None,
	}
}
