//! Per-session handler factories extracted from `go_online`'s serve loop. Each
//! `make_*` builds and returns the same inline closure the host wired before, so
//! behavior is identical; `spawn_loopback_audio` is the Windows WASAPI helper
//! `make_on_stream` calls within this module.

use super::*;

/// Windows: stream the host's system audio via **WASAPI loopback** (no
/// `virtual-audio-capturer` / Stereo Mix device required). Spawns the encoder ffmpeg
/// reading raw PCM on stdin (Opus/RTP to `dest`) plus a capture thread that feeds it
/// the default render endpoint's loopback. The ffmpeg is tracked in `procs`, so a
/// (re-)stream or session teardown kills it — which ends the capture thread via the
/// broken pipe. Returns whether it started (callers fall back to the dshow path).
#[cfg(windows)]
pub(super) fn spawn_loopback_audio(
	procs: &Arc<Mutex<Vec<Child>>>,
	ffmpeg: &str,
	dest: &str,
) -> bool {
	use std::process::Stdio;
	let fmt = match pulsar_core::audio::loopback_format() {
		Ok(f) => f,
		Err(_) => {
			return false;
		}
	};
	let mut args: Vec<String> = vec![
		"-hide_banner".into(),
		"-loglevel".into(),
		"error".into(),
		"-f".into(),
		fmt.ffmpeg_sample_fmt().into(),
		"-ar".into(),
		fmt.rate.to_string(),
		"-ac".into(),
		fmt.channels.to_string(),
		"-i".into(),
		"pipe:0".into(),
	];
	args.extend(pulsar_core::audio::opus_rtp_output(dest));
	let mut cmd = std::process::Command::new(ffmpeg);
	cmd.args(&args).stdin(Stdio::piped());
	no_window(&mut cmd);
	let mut child = match cmd.spawn() {
		Ok(c) => c,
		Err(_) => {
			return false;
		}
	};
	let stdin = match child.stdin.take() {
		Some(s) => s,
		None => {
			let _ = child.kill();
			return false;
		}
	};
	crate::job::assign(&child); // tie ffmpeg to Pulsar's lifetime (job.rs), like spawn_tracked
	procs.lock().unwrap().push(child);
	std::thread::spawn(move || {
		// Runs until the pipe breaks (ffmpeg killed on teardown) or WASAPI errors — both expected.
		let _ = pulsar_core::audio::run_loopback_capture(stdin);
	});
	true
}

/// Sessions (by sid) currently requesting the host's local output MUTED; mute is
/// applied while the set is non-empty. GLOBAL ownership, not a per-session flag:
/// a same-peer reconnect's new session must not be audibly un-muted by the OLD
/// session's delayed teardown (PEER_TIMEOUT ~6 s after a silent client death) —
/// and the stale per-session flag would then block re-muting on later re-streams.
static MUTE_OWNERS: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
	std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Record `sid`'s mute wish and (un)mute the host's local output when the owner
/// set flips between empty and non-empty.
fn set_mute_request(sid: u64, want: bool) {
	let mut owners = MUTE_OWNERS.lock().unwrap();
	let was = !owners.is_empty();
	if want {
		owners.insert(sid);
	} else {
		owners.remove(&sid);
	}
	let now = !owners.is_empty();
	tracing::info!(sid, want, owners = ?owners, "host mute request");
	if was != now {
		if let Err(e) = pulsar_core::audio::set_host_muted(now) {
			tracing::warn!("host mute toggle failed: {e}");
		}
	}
}

/// Teardown hook: drop `sid`'s mute request (un-mutes when it was the last owner).
pub(super) fn release_mute(sid: u64) {
	set_mute_request(sid, false);
}

/// go_online re-run hook: a reconnect aborts the accept loop but the per-session
/// tasks are independent spawns — one wedged mid-teardown can strand its
/// MUTE_OWNERS entry and leave the host audio muted forever. A fresh serve loop
/// has no live sessions yet, so clear the slate and audibly un-mute to a known-good
/// baseline (owner set empty + the saved-volume sentinel reset inside set_host_muted).
pub(super) fn reset_mute_all() {
	let mut owners = MUTE_OWNERS.lock().unwrap();
	if !owners.is_empty() {
		owners.clear();
		drop(owners);
		if let Err(e) = pulsar_core::audio::set_host_muted(false) {
			tracing::warn!("host unmute on go_online failed: {e}");
		}
	}
}

/// Startup crash-restore: a PRIOR process that silenced the host output and then
/// died abnormally (crash / taskkill / tray-quit) never ran its unmute, so the
/// machine is stuck at volume 0 with the in-memory saved level gone. The mute
/// backend persists the user's level to a marker file on the true mute transition
/// and restores it the first time the mute control is touched in a new process;
/// this hook makes that "first touch" happen at go_online. With the owner set empty
/// (a fresh serve loop), the unmute call is a harmless no-op for the volume but
/// triggers the one-time stale-marker recovery. Best-effort.
pub(super) fn restore_stale_host_mute() {
	// Triggers the backend's one-time stale-marker restore (mute.rs); a no-op for a
	// clean previous exit (no marker) and for the volume when nothing is muted.
	if let Err(e) = pulsar_core::audio::set_host_muted(false) {
		tracing::warn!("host mute crash-restore probe failed: {e}");
	}
}

/// Start the host→client audio stream (Opus/RTP) and apply the requested host-mute.
/// Shared by the X11/Windows fall-through path and the Wayland branch so a Wayland
/// host streams audio + honors game-mode mute exactly like the X11 path. Synchronous:
/// it only spawns tracked children (`spawn_tracked`/`spawn_loopback_audio`) and makes
/// the blocking `set_host_muted` call — it must run in the closure body, not the async
/// portal-capture task. Re-evaluated on every (re-)stream so live toggles take effect.
fn start_audio_and_mute(
	procs: &Arc<Mutex<Vec<Child>>>,
	ffmpeg: &str,
	app_h: &AppHandle,
	audio_dest: SocketAddr,
	req: &StreamReq,
	sid: u64,
) {
	// Audio: a second ffmpeg streams Opus/RTP to `audio_dest` — the client's audio
	// port directly (legacy), or the local media-over-session intake (the forwarder
	// ships it through the encrypted session). Transmit + host-mute are driven by
	// the session-menu toggles in the request (game mode defaults both on).
	let acfg = pulsar_core::Config::load(config_path(app_h));
	if req.transmit_audio && audio_dest.port() > 0 {
		let dest = format!("rtp://{audio_dest}");
		// Windows: prefer WASAPI loopback — it taps whatever is playing on the
		// default output, so it works with NO virtual-audio-capturer / Stereo
		// Mix device installed. Falls back to the dshow command if it can't
		// start or a specific capture device name was configured.
		#[cfg(windows)]
		let started_audio = acfg.audio_loopback() && spawn_loopback_audio(procs, ffmpeg, &dest);
		#[cfg(not(windows))]
		let started_audio = false;
		if !started_audio {
			let (_, aargs) = pulsar_core::audio::audio_command(&acfg.audio_input(), &dest);
			let _ = spawn_tracked(procs, ffmpeg, &aargs);
		}
	}
	// Apply the requested host-mute state through the global owner set
	// (re-evaluated on every (re-)stream so a live toggle takes effect).
	set_mute_request(sid, req.mute_host);
}

/// Retransmit ring depth for media-over-session video (packets ≈ a few hundred ms
/// at 60 fps / 15 Mbit; ~1.5 MB worst case).
const NACK_RING: usize = 1024;

/// Sane lower bounds + defaults for the resolved stream geometry. The client
/// request and the host config can BOTH be 0 (request unset → fall to cfg; cfg
/// never configured → 0), and a 0 here is poison downstream: `-r 0` / GStreamer
/// `framerate=0/1` make ffmpeg/gst error out, and a 0×0 size reaches the native
/// NVENC/DXGI capture on Windows (where the `display_size` clamp is compiled out
/// and only ever shrinks) → an init crash or a dead stream. Clamp every resolved
/// value to a usable floor on ALL platforms before it flows into an encoder.
const MIN_FPS: u32 = 15;
const DEFAULT_FPS: u32 = 60;
const MIN_DIM: u32 = 320;
const DEFAULT_W: u32 = 1280;
const DEFAULT_H: u32 = 720;

/// Clamp a resolved fps to the usable floor: a 0 (both request and config unset)
/// becomes a sensible default rather than `-r 0`/`framerate=0/1`.
fn clamp_fps(fps: u32) -> u32 {
	if fps == 0 {
		DEFAULT_FPS
	} else {
		fps.max(MIN_FPS)
	}
}

/// Clamp resolved width/height to a usable floor: a 0 (both request and config
/// unset) becomes a default size; anything positive is floored to `MIN_DIM` so a
/// tiny/degenerate request can't crash the encoder.
fn clamp_dims(w: u32, h: u32) -> (u32, u32) {
	let w = if w == 0 { DEFAULT_W } else { w.max(MIN_DIM) };
	let h = if h == 0 { DEFAULT_H } else { h.max(MIN_DIM) };
	(w, h)
}

/// Media-over-session: bind the two LOOPBACK intake sockets the encoders will
/// target, and spawn the forwarder tasks that ship every received RTP datagram
/// through the encrypted session (`[tag][rtp…]` frames). The video forwarder keeps
/// a retransmit ring and serves NACK requests (registered into `nack_slot`).
/// Returns the (video, audio) intake ports, or `None` if binding failed (caller
/// falls back to the legacy direct flows).
fn spawn_media_forwarders(
	media_tx: &pulsar_core::SessionSender,
	nack_slot: &Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>>,
	fwd_slot: &Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
) -> Option<(u16, u16)> {
	use pulsar_core::service::media;
	let bind = || -> Option<(tokio::net::UdpSocket, u16)> {
		// BIG receive buffer (like pulsar-core node.rs): the encoder bursts a whole
		// IDR into this loopback intake at once, and the OS default (64 KiB on
		// Windows) overflows instantly at high fps — at 1080p120 NVENC virtually
		// every packet was dropped here (the client saw a 1 fps green stream).
		let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).ok()?;
		let _ = s.set_recv_buffer_size(4 * 1024 * 1024);
		let _ = s.set_send_buffer_size(4 * 1024 * 1024);
		s.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
			.ok()?;
		let s: std::net::UdpSocket = s.into();
		let port = s.local_addr().ok()?.port();
		s.set_nonblocking(true).ok()?;
		Some((tokio::net::UdpSocket::from_std(s).ok()?, port))
	};
	let (vsock, vport) = bind()?;
	let (asock, aport) = bind()?;

	let (nack_tx, mut nack_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u16>>();
	*nack_slot.lock().unwrap() = Some(nack_tx);

	let vtx = media_tx.clone();
	let vh = tokio::spawn(async move {
		// seq → datagram retransmit ring (linear scan is fine at this size).
		let mut ring: std::collections::VecDeque<(u16, Vec<u8>)> =
			std::collections::VecDeque::with_capacity(NACK_RING);
		let mut buf = vec![0u8; 2048];
		// 1 Hz throughput meter (intake pkts/bytes + session send failures) — the
		// "video reaches the client mangled/not at all" debugging needs to know
		// WHERE the chain loses data, and this stage was silent.
		let (mut m_pkts, mut m_bytes, mut m_gaps) = (0u64, 0u64, 0u64);
		let mut m_last_seq: Option<u16> = None;
		let mut m_at = std::time::Instant::now();
		loop {
			tokio::select! {
				r = vsock.recv(&mut buf) => {
					let Ok(n) = r else { break };
					let rtp = &buf[..n];
					if let Some(seq) = media::rtp_seq(rtp) {
						if let Some(last) = m_last_seq {
							let d = media::seq_forward(last, seq);
							if d > 1 && d < 0x8000 {
								m_gaps += (d - 1) as u64;
							}
						}
						m_last_seq = Some(seq);
						if ring.len() == NACK_RING {
							ring.pop_front();
						}
						ring.push_back((seq, rtp.to_vec()));
					}
					m_pkts += 1;
					m_bytes += n as u64;
					if m_at.elapsed().as_secs() >= 1 {
						tracing::info!(
							pkts = m_pkts,
							mbit = (m_bytes * 8) / 1_000_000,
							gaps_before_intake = m_gaps,
							"mos video forwarder throughput"
						);
						(m_pkts, m_bytes, m_gaps) = (0, 0, 0);
						m_at = std::time::Instant::now();
					}
					if vtx.send(&media::frame(media::TAG_VIDEO, rtp)).await.is_err() {
						break; // session gone
					}
				}
				q = nack_rx.recv() => {
					let Some(seqs) = q else { break };
					for seq in seqs {
						if let Some((_, pkt)) = ring.iter().find(|(s, _)| *s == seq) {
							let _ = vtx.send(&media::frame(media::TAG_VIDEO, pkt)).await;
						}
					}
				}
			}
		}
	});
	let atx = media_tx.clone();
	let ah = tokio::spawn(async move {
		let mut buf = vec![0u8; 2048];
		while let Ok(n) = asock.recv(&mut buf).await {
			if atx
				.send(&media::frame(media::TAG_AUDIO, &buf[..n]))
				.await
				.is_err()
			{
				break;
			}
		}
	});
	fwd_slot.lock().unwrap().extend([vh, ah]);
	Some((vport, aport))
}

/// Build the per-session `on_stream` handler. A (re-)stream request restarts
/// capture: it kills any ffmpeg/native capture already running for this session,
/// then spawns the new encode (native DXGI+NVENC on Windows, else ffmpeg), pushes
/// the encode summary + display rotation to the client, starts the audio stream,
/// and applies the requested host-mute. On Wayland it routes through the portals.
#[allow(clippy::too_many_arguments)]
pub(super) fn make_on_stream(
	stream_cfg: Arc<Mutex<crate::state::StreamCfg>>,
	procs: Arc<Mutex<Vec<Child>>>,
	active: Arc<Mutex<std::collections::HashMap<String, crate::state::ConnInfo>>>,
	incoming: Arc<Mutex<std::collections::HashMap<String, (u64, oneshot::Sender<()>)>>>,
	host_out: Arc<
		Mutex<std::collections::HashMap<String, (u64, tokio::sync::mpsc::Sender<DataMsg>)>>,
	>,
	stop_tx: oneshot::Sender<()>,
	out_tx: tokio::sync::mpsc::Sender<DataMsg>,
	since_ms: u64,
	sid: u64,
	self_name: String,
	#[cfg(windows)] native_slot: Arc<Mutex<Option<pulsar_capture::CaptureHandle>>>,
	stats_out: tokio::sync::mpsc::Sender<DataMsg>,
	app_h: AppHandle,
	peer: String,
	media_tx: pulsar_core::SessionSender,
	nack_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u16>>>>>,
	fwd_slot: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
	#[cfg(target_os = "linux")] restore_token: Arc<Mutex<Option<String>>>,
	#[cfg(target_os = "linux")] cap_slot: Arc<Mutex<Option<pulsar_core::capture::WaylandCapture>>>,
	#[cfg(target_os = "linux")] cap_gen: Arc<std::sync::atomic::AtomicU64>,
) -> impl FnMut(StreamReq, SocketAddr) + Send + 'static {
	let mut announced = false;
	let mut stop_tx = Some(stop_tx);
	// Cursor side-channel poller's liveness flag (Linux KMS path). Held across re-streams so a
	// re-stream can stop the prior poller before (maybe) starting a new one — avoids stacking
	// pollers when a session re-requests while still on the cursorless KMS capture.
	#[cfg(target_os = "linux")]
	let mut cursor_alive: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> = None;
	move |req: StreamReq, addr: SocketAddr| {
		let cfg = stream_cfg.lock().unwrap().clone();
		// A (re-)stream supersedes any prior cursor poller; the (maybe) new KMS branch below
		// starts a fresh one. Stopping here keeps it at most one per session.
		#[cfg(target_os = "linux")]
		if let Some(flag) = cursor_alive.take() {
			flag.store(false, std::sync::atomic::Ordering::SeqCst);
		}

		// Media destinations. Legacy: plain UDP straight to the client's ports.
		// Media-over-session (client opted in + this host advertised `mos`): the
		// encoders target LOCAL loopback intakes whose forwarders ship each datagram
		// through the encrypted session — ONE external socket total. A re-stream
		// replaces the forwarders (fresh retransmit ring / NACK channel).
		for h in fwd_slot.lock().unwrap().drain(..) {
			h.abort();
		}
		*nack_slot.lock().unwrap() = None;
		let lo = std::net::IpAddr::from([127, 0, 0, 1]);
		let (vdest, adest) = if req.media_over_session {
			match spawn_media_forwarders(&media_tx, &nack_slot, &fwd_slot) {
				Some((vp, ap)) => (
					SocketAddr::new(lo, vp),
					SocketAddr::new(lo, if req.audio_port > 0 { ap } else { 0 }),
				),
				None => (
					SocketAddr::new(addr.ip(), req.port),
					SocketAddr::new(addr.ip(), req.audio_port),
				),
			}
		} else {
			(
				SocketAddr::new(addr.ip(), req.port),
				SocketAddr::new(addr.ip(), req.audio_port),
			)
		};

		// First stream request reveals this connection's mode: register it and open the
		// dedicated connections window — brought forward for a Remote connection, opened
		// hidden for a Game one (so it doesn't disrupt / leak into the streamed game).
		// Done once (not on re-streams) so a live resolution change doesn't re-pop it.
		if !announced {
			announced = true;
			let mode = if req.game_mode {
				crate::state::ConnMode::Game
			} else {
				crate::state::ConnMode::Remote
			};
			// Registration happens HERE, not at accept (see go_online): a control
			// session that never streams must not clobber a live same-peer session's
			// entries — overwriting `incoming` drops the live stop_tx and instantly
			// tears its stream down. A second STREAMING session still takes over
			// (the overwritten stop_tx drop ends the old session cleanly).
			active.lock().unwrap().insert(
				peer.clone(),
				crate::state::ConnInfo {
					sid,
					since_ms,
					mode,
					view_only: false,
				},
			);
			if let Some(tx) = stop_tx.take() {
				incoming.lock().unwrap().insert(peer.clone(), (sid, tx));
			}
			host_out
				.lock()
				.unwrap()
				.insert(peer.clone(), (sid, out_tx.clone()));
			crate::connections::open_or_update(&app_h, !req.game_mode);
			// Identity push (host → client) is ALSO deferred to here: at accept time
			// the client is still inside query_stream_caps' wait loop, which discards
			// every non-StreamCaps frame — a PeerName/Avatar queued there never
			// reached its UI. By the first stream request the client's hold loop owns
			// the read side. Avatar resolve may decode a full-size wallpaper → too
			// slow for this closure, so it runs on a blocking thread; honors the
			// avatar_mode setting (anonymous = nothing sent); best-effort.
			let _ = stats_out.try_send(DataMsg::PeerName(self_name.clone()));
			let av_tx = stats_out.clone();
			let av_app = app_h.clone();
			let av_mode = tauri::Manager::state::<AppState>(&app_h)
				.config
				.lock()
				.unwrap()
				.avatar_mode
				.clone();
			tokio::task::spawn_blocking(move || {
				if let Some(png) = crate::avatar::avatar_png(&av_app, &av_mode) {
					let _ = av_tx.try_send(DataMsg::Avatar(png));
				}
			});
		}

		// Wayland: x11grab of rootless Xwayland is black, so capture the
		// real screen (and inject input) through the desktop portals.
		#[cfg(target_os = "linux")]
		if pulsar_core::capture::is_wayland() {
			// A (re-)stream restarts capture: kill any audio ffmpeg already running for
			// this session before spawning the new one, so a live re-stream (resolution/
			// codec/fps/audio-toggle change) doesn't stack audio encoders. (The video
			// capture is restarted separately via `cap_slot` in the async task below.)
			for mut child in procs.lock().unwrap().drain(..) {
				let _ = child.kill();
				let _ = child.wait();
			}
			let ip = vdest.ip().to_string();
			let port = vdest.port();
			// Client-requested bitrate wins; 0 falls back to the host config.
			let eff_bitrate = if req.bitrate_kbps > 0 {
				req.bitrate_kbps
			} else {
				cfg.bitrate_kbps
			};
			let req_fps = if req.fps > 0 { req.fps } else { cfg.fps };
			// Negotiate against the host panel (see the main path below for the rationale).
			let panel_hz = crate::util::host_panel_hz();
			let eff_fps = match panel_hz {
				Some(hz) => req_fps.min(hz),
				None => req_fps,
			};
			// Floor it: req_fps/cfg.fps can both be 0 → GStreamer framerate=0/1 errors.
			let eff_fps = clamp_fps(eff_fps);
			tracing::info!(
				req_fps,
				cfg_fps = cfg.fps,
				panel_hz = panel_hz.unwrap_or(0),
				eff_fps,
				"host stream fps resolved (wayland)"
			);
			let (bitrate, fps) = (eff_bitrate, eff_fps);
			// Pick the gst encoder family + codec from what THIS box validated
			// (mpp/vaapi/nv hardware first, x264 terminal) honoring the request.
			// Falls back to plain x264/H.264 when nothing probed (gst missing —
			// the spawn then fails visibly, same as the old hardcoded pipeline).
			let enc_pref = if req.encoder.is_empty() {
				cfg.encoder.clone()
			} else {
				req.encoder.clone()
			};
			let validated = crate::process::validated_gst_encoders();
			let (genc, gcodec) = crate::process::pick_gst(&validated, &enc_pref, &req.codec)
				.unwrap_or((pipeline::gst::GstEncoder::X264, pipeline::VCodec::H264));
			let fragment = pipeline::gst::encoder_fragment(genc, gcodec, bitrate, fps)
				.unwrap_or_else(|| {
					pipeline::gst::encoder_fragment(
						pipeline::gst::GstEncoder::X264,
						pipeline::VCodec::H264,
						bitrate,
						fps,
					)
					.expect("x264/h264 fragment always builds")
				});
			// Encode summary for the client's stats panel (the Wayland path never sent
			// one before, so the panel showed nothing).
			let fps_part = if eff_fps != req_fps {
				format!("{}fps ({} {})", fps, crate::i18n::t("stream.fpsRequested"), req_fps)
			} else {
				format!("{}fps", fps)
			};
			let _ = stats_out.try_send(DataMsg::Stats(format!(
				"{} · {} · — · {} · {} {}",
				vcodec_label(gcodec),
				genc.label(),
				fps_part,
				(bitrate as f32 / 1000.0).round() as u32,
				crate::i18n::t("stream.mbitTarget")
			)));
			let token = restore_token.lock().unwrap().clone();
			let restore_token = restore_token.clone();
			let cap_slot = cap_slot.clone();
			// This (re-)stream's generation: capture::start can sit in the portal
			// dialog for seconds; teardown and any newer re-stream bump the counter,
			// telling a stale task to discard its fresh capture (see go_online).
			let gen = cap_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
			let cap_gen = cap_gen.clone();
			// Clone for the spawned capture task; the param `app_h` stays owned so the
			// synchronous audio+host-mute below (and FnMut re-calls) can still use it.
			let app_h_task = app_h.clone();
			let peer = peer.clone();
			tokio::spawn(async move {
				use std::sync::atomic::Ordering;
				// Already superseded before we even started (overlapping re-streams):
				// leave the prior capture to the newer task.
				if cap_gen.load(Ordering::SeqCst) != gen {
					return;
				}
				// A (re-)stream restarts capture: stop the prior WaylandCapture FIRST —
				// kill its gst-launch AND close the portal session. WaylandCapture has no
				// Drop and the gst child has PR_SET_PDEATHSIG, so a bare overwrite would
				// neither kill it nor stop the duplicate RTP (and the compositor would keep
				// showing "screen is being shared"). Take the Option in its own statement so
				// the MutexGuard is dropped before the `.await` (std guards are !Send).
				let prev = cap_slot.lock().unwrap().take();
				if let Some(p) = prev {
					p.stop().await;
				}
				match pulsar_core::capture::start(&ip, port, &fragment, token).await {
					Ok((cap, new_token)) => {
						if let Some(t) = new_token {
							*restore_token.lock().unwrap() = Some(t);
						}
						// Store under the slot lock ONLY while still current: a session
						// that tore down (or re-streamed) during the portal dialog gets
						// its fresh capture STOPPED instead of stored into a dead slot
						// (orphaned "screen is being shared" + gst until app exit).
						let stale = {
							let mut slot = cap_slot.lock().unwrap();
							if cap_gen.load(Ordering::SeqCst) == gen {
								*slot = Some(cap);
								None
							} else {
								Some(cap)
							}
						};
						if let Some(c) = stale {
							c.stop().await;
							return;
						}
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: "Wayland · ekran + kontrol".into(),
							},
						);
					}
					Err(e) => {
						let _ = app_h_task.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer,
								detail: format!("Wayland yakalama başarısız: {e}"),
							},
						);
					}
				}
			});
			// Audio + host-mute on Wayland too (the early return above used to skip both,
			// so a Wayland host streamed silent video and never muted in game mode). Runs
			// synchronously in the closure body, like the X11 path; the Pulse `.monitor`
			// ffmpeg is tracked in `procs` and killed on (re-)stream/teardown.
			let ffmpeg = ffmpeg_bin(&app_h);
			start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
			return;
		}

		// A (re-)stream request restarts capture: kill any ffmpeg already
		// running for this session before spawning the new one (this is how
		// a live resolution change from the client takes effect).
		for mut child in procs.lock().unwrap().drain(..) {
			let _ = child.kill();
			let _ = child.wait();
		}
		// Same for the native capture thread, if the prior (re)stream used it.
		#[cfg(windows)]
		if let Some(h) = native_slot.lock().unwrap().take() {
			h.stop();
		}
		// Client-requested size/fps/bitrate win; 0 falls back to the host config.
		let eff_w = if req.width > 0 { req.width } else { cfg.width };
		let eff_h = if req.height > 0 {
			req.height
		} else {
			cfg.height
		};
		let req_fps = if req.fps > 0 { req.fps } else { cfg.fps };
		// Negotiate against the host panel: encoding above the host's own refresh just
		// produces duplicate frames at extra cost (the user-visible "120 seçtim,
		// değişmiyor" was a 120-req on a slower panel), so clamp to it when known.
		let panel_hz = crate::util::host_panel_hz();
		let eff_fps = match panel_hz {
			Some(hz) => req_fps.min(hz),
			None => req_fps,
		};
		// Diagnostic ceiling (`PULSAR_MAX_FPS`): bisect client-decoder fps limits live.
		let eff_fps = match std::env::var("PULSAR_MAX_FPS")
			.ok()
			.and_then(|v| v.parse::<u32>().ok())
		{
			Some(m) => eff_fps.min(m),
			None => eff_fps,
		};
		// Floor it AFTER every adjustment: req_fps, cfg.fps, the panel clamp and the
		// diagnostic ceiling can all drive this to 0 → `-r 0` / native NVENC with a
		// 0 fps. A 0 here is poison on every path, so clamp last.
		let eff_fps = clamp_fps(eff_fps);
		tracing::info!(
			req_fps,
			cfg_fps = cfg.fps,
			panel_hz = panel_hz.unwrap_or(0),
			eff_fps,
			"host stream fps resolved"
		);
		let eff_bitrate = if req.bitrate_kbps > 0 {
			req.bitrate_kbps
		} else {
			cfg.bitrate_kbps
		};
		// Clamp the capture resolution to the host's actual screen. ffmpeg's x11grab/gdigrab grab a
		// REGION of size `-video_size`, which must be ≤ the screen or ffmpeg dies ("Capture area …
		// outside the screen size") and streams NO video — hit when a 1440p-configured stream
		// targets a 1080p host (e.g. an Orange Pi acting as host). Windows captures via the native
		// DXGI path (scales internally), so this only guards the ffmpeg capture path.
		#[cfg(not(windows))]
		let (eff_w, eff_h) = match crate::util::display_size(&cfg.display) {
			Some((sw, sh)) if eff_w > sw || eff_h > sh => (sw, sh),
			_ => (eff_w, eff_h),
		};
		// Floor the resolved size on ALL platforms: req+cfg can both be 0 (0×0 reaches
		// native NVENC/DXGI on Windows, where the clamp above is compiled out and only
		// shrinks anyway) → encoder init crash / dead stream. Apply after the Unix
		// screen-clamp so a legitimately-clamped size is preserved, only floored.
		let (eff_w, eff_h) = clamp_dims(eff_w, eff_h);
		let ffmpeg = ffmpeg_bin(&app_h);
		// The viewer picks the encoder live from the session menu (sent in the
		// stream request); an empty request falls back to the host's own setting.
		// `resolve` still degrades gracefully if this host lacks that encoder.
		let enc_pref = if req.encoder.is_empty() {
			cfg.encoder.clone()
		} else {
			req.encoder.clone()
		};
		// Probe the bundled ffmpeg ONCE: which backends exist, and (per backend) which
		// codecs. `resolve` degrades the encoder; `resolve_codec` then degrades the codec
		// to what that encoder can actually emit (requested → H.264 → first available), so a
		// HEVC/AV1 request on a build lacking it falls back instead of failing.
		let enc_text = crate::process::encoders_text(&ffmpeg);
		let encoder = pipeline::resolve(encoder_from_str(&enc_pref), &pipeline::detect(&enc_text));
		// Off-Windows, ffmpeg is the ONLY encode path, so an encoder ffmpeg merely *lists* (a generic
		// build lists h264_nvenc even with no NVIDIA GPU) but can't initialize here must be dropped,
		// not used — else it fails at spawn and sends no video (the Orange-Pi-as-host case:
		// h264_nvenc → "Cannot load libcuda.so.1"). Validate + degrade to a working encoder
		// (ultimately libx264). Windows keeps its native-NVENC path + hybrid guard (compiled out here).
		#[cfg(not(windows))]
		let encoder = crate::process::resolve_encoder_validated(
			&ffmpeg,
			encoder,
			&enc_text,
			&cfg.vaapi_device,
		);
		// Validate the codec with a real one-frame encode probe (cached) — catches "listed
		// but the GPU/driver can't init it" (e.g. av1_nvenc on Ampere), degrading to a codec
		// that actually works rather than producing a dead stream.
		let codec = crate::process::resolve_codec_validated(
			&ffmpeg,
			encoder,
			codec_from_str(&req.codec),
			&cfg.vaapi_device,
		);
		// Clamp to what the CLIENT can decode (its startup probe travels in the
		// request): never stream a codec the other side can't show. H.264 software
		// decode exists everywhere, so it is the universal meeting point.
		let codec = if !req.decode_codecs.is_empty()
			&& !req.decode_codecs.iter().any(|c| codec_from_str(c) == codec)
		{
			pipeline::VCodec::H264
		} else {
			codec
		};
		// Linux X11: when ffmpeg has no working HW encoder (terminal Software — the
		// Orange Pi 5 case: rkmpp encode exists only as GStreamer mpph26Xenc) or the
		// client explicitly asked for "rkmpp", route the encode through a gst
		// `ximagesrc → mpp/vaapi/nv → RTP` pipeline instead of ffmpeg+libx264.
		// gst x264 is NOT used here — ffmpeg's own libx264 path has more knobs.
		#[cfg(target_os = "linux")]
		{
			let want_gst = enc_pref == "rkmpp" || encoder == HwEncoder::Software;
			if want_gst {
				let hw: Vec<_> = crate::process::validated_gst_encoders()
					.into_iter()
					.filter(|(e, _)| *e != pipeline::gst::GstEncoder::X264)
					.collect();
				tracing::info!(
					families = hw.len(),
					%enc_pref,
					req_codec = %req.codec,
					"x11 gst hw-encode candidates"
				);
				if let Some((genc, gcodec)) = crate::process::pick_gst(&hw, &enc_pref, &req.codec) {
					if let Some(fragment) =
						pipeline::gst::encoder_fragment(genc, gcodec, eff_bitrate, eff_fps)
					{
						// Encode-pace meter: an identity right AFTER the encoder (the
						// fragment's first ` ! ` joins encoder→parse; props carry no `!`).
						let metered =
							fragment.replacen(" ! ", " ! identity name=encpace silent=false ! ", 1);
						// Zero-copy KMS capture (scanout DMABuf → MPP): game mode only — the
						// X HW cursor is NOT in the KMS frame (own DRM plane), fine in-game,
						// unusable for remote desktop. Probed, never assumed.
						// `PULSAR_KMS`: 0 = never (bisect back to ximagesrc), 1 = force even
						// for remote sessions (testing / hosts running a software cursor),
						// unset = game-mode-gated default — EXTENDED so a remote session can
						// also use KMS when the client draws the cursor itself
						// (`cursor_external`): the missing hardware cursor is then supplied
						// out-of-band (see `cursor.rs`), which was the only thing that pinned
						// KMS to game mode. The cursor side-channel + PULSAR_KMS=1 stay the
						// safety net (side-channel down / explicit force → old behavior).
						let kms_mode = match std::env::var("PULSAR_KMS").as_deref() {
							Ok("0") => false,
							Ok("1") => true,
							_ => req.game_mode || req.cursor_external,
						};
						let kms = kms_mode
							&& genc == pipeline::gst::GstEncoder::Mpp
							&& crate::process::kms_encode_ok(genc, gcodec);
						// Cursor side-channel: the KMS scan-out frame has NO hardware cursor
						// (own DRM plane), so when the client asked to draw it itself
						// (`cursor_external`) start the X pointer poller that streams the cursor
						// position+shape out-of-band. The poller stops when `stats_out` closes
						// (session teardown) — a re-stream that drops KMS simply doesn't start a
						// new one and the old one self-stops with the prior session.
						if kms && req.cursor_external {
							let flag =
								std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
							cursor_alive = Some(flag.clone());
							crate::host::cursor::spawn(stats_out.clone(), flag);
						}
						// MPP eats BGRx via the RGA blitter — skip the CPU convert.
						let direct_bgrx = genc == pipeline::gst::GstEncoder::Mpp;
						let pipeline_str = if kms {
							pipeline::gst::kms_pipeline(
								eff_fps,
								&metered,
								&vdest.ip().to_string(),
								vdest.port(),
							)
						} else {
							pipeline::gst::x11_pipeline(
								&cfg.display,
								eff_fps,
								&metered,
								&vdest.ip().to_string(),
								vdest.port(),
								direct_bgrx,
							)
						};
						let fps_part = if eff_fps != req_fps {
							format!(
								"{}fps ({} {})",
								eff_fps,
								crate::i18n::t("stream.fpsRequested"),
								req_fps
							)
						} else {
							format!("{}fps", eff_fps)
						};
						// Resolution part: surface a host-clamped request ("1080p (istenen
						// 1440p)") so the overlay res "Aktif:" line reads as a negotiation —
						// same pattern as `fps_part`. eff_h was clamped to the host screen above.
						let res_part = if req.height > 0 && req.height != eff_h {
							format!(
								"{}p ({} {}p)",
								eff_h,
								crate::i18n::t("stream.fpsRequested"),
								req.height
							)
						} else {
							format!("{}p", eff_h)
						};
						let base_label = format!(
							"{} · {}{} · {} · {} · {} {}",
							vcodec_label(gcodec),
							genc.label(),
							if kms { " (KMS)" } else { "" },
							res_part,
							fps_part,
							(eff_bitrate as f32 / 1000.0).round() as u32,
							crate::i18n::t("stream.mbitTarget")
						);
						let stats_enc = stats_out.clone();
						let label_enc = base_label.clone();
						// `PULSAR_GST_METER=0` bypasses the pace meter (plain -q spawn) —
						// the regression-bisect knob for the encode-ms instrumentation.
						let started = if std::env::var("PULSAR_GST_METER").as_deref() == Ok("0") {
							spawn_gst_tracked(&procs, &pipeline_str).is_ok()
						} else {
							match spawn_gst_paced(&procs, &pipeline_str, move |ms| {
								let _ = stats_enc.try_send(DataMsg::Stats(format!(
									"{label_enc} · {ms:.1} {}",
									crate::i18n::t("stream.msEncode")
								)));
							}) {
								Ok((pid, ticked)) => {
									if !kms && direct_bgrx {
										// RGA >4G watchdog. The BGRx-direct path hands
										// ximagesrc's malloc'd pool straight to the RGA
										// blitter, and the RGA2 core can't map memory
										// above 4 GiB phys (dmesg: "RGA_MMU unsupported
										// Memory larger than 4G!"). The pool is allocated
										// once per spawn, so it's allocation luck: same
										// pipeline sometimes runs at 76 fps, sometimes
										// never emits a single frame (black screen). No
										// first frame within 2.6 s → kill + respawn with
										// the CPU-convert (I420) variant, which feeds the
										// encoder RGA-safe buffers.
										let fb_pipeline = pipeline::gst::x11_pipeline(
											&cfg.display,
											eff_fps,
											&metered,
											&vdest.ip().to_string(),
											vdest.port(),
											false,
										);
										let procs_fb = procs.clone();
										let stats_fb = stats_out.clone();
										let label_fb = base_label.clone();
										std::thread::spawn(move || {
											std::thread::sleep(std::time::Duration::from_millis(
												2600,
											));
											if ticked.load(std::sync::atomic::Ordering::Relaxed) {
												return;
											}
											// The whole kill-and-respawn runs under the procs lock,
											// keyed to the ORIGINAL child: teardown and a re-stream
											// drain `procs` under this same lock, so once our child
											// is gone from it the session moved on — a fallback
											// spawned past that point would run as an untracked
											// orphan (or duplicate the new encode's RTP).
											let mut g = procs_fb.lock().unwrap();
											let Some(idx) = g.iter().position(|c| c.id() == pid)
											else {
												return;
											};
											tracing::warn!(
												pid,
												"gst BGRx-direct produced no frames in 2.6 s (RGA >4G map failure) — respawning with CPU convert"
											);
											let mut old = g.remove(idx);
											let _ = old.kill();
											let _ = old.wait();
											let _ = spawn_gst_paced_locked(
												&mut g,
												&fb_pipeline,
												move |ms| {
													let _ = stats_fb.try_send(DataMsg::Stats(
														format!("{label_fb} · {ms:.1} {}", crate::i18n::t("stream.msEncode")),
													));
												},
											);
										});
									}
									true
								}
								Err(_) => false,
							}
						};
						tracing::info!(encoder = ?genc, codec = ?gcodec, started, "x11 gst encode spawned");
						let _ = stats_out.try_send(DataMsg::Stats(base_label));
						let _ = stats_out.try_send(DataMsg::DisplayRotation(display_rotation()));
						start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
						let _ = app_h.emit(
							"session",
							SessionEvent {
								kind: "stream".into(),
								peer: peer.clone(),
								detail: format!("{} · {}p", genc.label(), eff_h)
									+ if started { "" } else { crate::i18n::t("host.gstFailed") },
							},
						);
						return;
					}
				}
			}
		}
		let capture = capture_from_str(&cfg.capture);
		// NVENC + ddagrab: probe ONCE whether the fully zero-copy
		// D3D11→CUDA→NVENC path works (display on the NVIDIA GPU). On a
		// hybrid box it doesn't, and we use the GPU-scale path instead.
		let gpu_zerocopy = if encoder == HwEncoder::Nvenc && capture == CaptureMethod::Ddagrab {
			let ff = ffmpeg.clone();
			*DDAGRAB_ZEROCOPY.get_or_init(|| probe_ddagrab_zerocopy(&ff))
		} else {
			false
		};
		let plan = StreamPlan {
			encoder,
			codec,
			width: eff_w,
			height: eff_h,
			fps: eff_fps,
			bitrate_kbps: eff_bitrate,
			capture,
			display: cfg.display.clone(),
			vaapi_device: cfg.vaapi_device.clone(),
			dest: format!("rtp://{vdest}"),
			// Quality bias: explicit client preference wins; `Balanced` defers to
			// game_mode (no regression — game mode → lowest latency, remote → quality).
			low_latency: match req.quality {
				QualityPref::Quality => false,
				QualityPref::Latency => true,
				QualityPref::Balanced => req.game_mode,
			},
			gpu_zerocopy,
			hdr: req.hdr,
			yuv444: req.yuv444,
		};
		// NATIVE WINDOWS path: DXGI Desktop Duplication + NVENC SDK → RTP
		// (Sunshine-technique, steady client-fps). Used for NVENC on Windows
		// unless PULSAR_FFMPEG_CAPTURE=1. Init happens inside the capture thread
		// and is reported back synchronously — Ok ⇒ streaming started; Err ⇒ fall
		// back to ffmpeg with zero behaviour change.
		#[cfg(windows)]
		let native_started = if encoder == HwEncoder::Nvenc
			&& capture == CaptureMethod::Ddagrab
			&& !req.hdr
			&& !req.yuv444
			&& std::env::var_os("PULSAR_FFMPEG_CAPTURE").is_none()
		{
			let ncodec = match codec {
				pipeline::VCodec::H264 => pulsar_capture::Codec::H264,
				pipeline::VCodec::H265 => pulsar_capture::Codec::H265,
				pipeline::VCodec::Av1 => pulsar_capture::Codec::Av1,
			};
			match pulsar_capture::start_capture_encode(pulsar_capture::CaptureConfig {
				width: eff_w,
				height: eff_h,
				fps: eff_fps,
				bitrate_kbps: eff_bitrate,
				dest: format!("rtp://{vdest}"),
				codec: ncodec,
				output_idx: 0,
				low_latency: plan.low_latency,
				draw_mouse: true,
			}) {
				Ok(h) => {
					*native_slot.lock().unwrap() = Some(h);
					true
				}
				Err(_) => false,
			}
		} else {
			false
		};
		#[cfg(not(windows))]
		let native_started = false;

		// Encode summary (codec · encoder · res · fps · bitrate target) — the base the
		// client's stats panel shows; the ffmpeg path appends a live "… ms kodlama"
		// part from the encode-pace meter below.
		// Reflect the RESOLVED codec (after `resolve_codec` fallback), not the request —
		// the client uses this to pick its decoder, so it must match what we actually send.
		// The fps part is the overlay's FPS-combo "Aktif:" truth line (overlay.rs act(3)).
		// When the host clamped the request to its panel, surface BOTH so "120 seçtim,
		// değişmiyor" reads as a negotiation, not a bug: "60fps (istenen 120)".
		let fps_part = if eff_fps != req_fps {
			format!("{}fps ({} {})", eff_fps, crate::i18n::t("stream.fpsRequested"), req_fps)
		} else {
			format!("{}fps", eff_fps)
		};
		// Resolution part: when the client asked for a height the host couldn't honor
		// (clamped to the host screen / config), surface BOTH so the overlay's res
		// "Aktif:" line reads as a negotiation ("1080p (istenen 1440p)"), not a bug —
		// same pattern as `fps_part` above.
		let res_part = if req.height > 0 && req.height != eff_h {
			format!("{}p ({} {}p)", eff_h, crate::i18n::t("stream.fpsRequested"), req.height)
		} else {
			format!("{}p", eff_h)
		};
		let base_label = format!(
			"{} · {} · {} · {} · {} {}",
			vcodec_label(codec),
			encoder.label(),
			res_part,
			fps_part,
			(eff_bitrate as f32 / 1000.0).round() as u32,
			crate::i18n::t("stream.mbitTarget")
		);
		// encode_command always yields ("ffmpeg", args); run the bundled
		// ffmpeg binary directly rather than relying on a system ffmpeg.
		let started = if native_started {
			true
		} else {
			let (_, args) = pipeline::encode_command(&plan);
			// Encode-pace meter: ffmpeg `-progress` ticks → per-frame wall ms → re-push
			// the Stats label with a live "kodlama" part (~2 Hz, tiny control message).
			let stats_enc = stats_out.clone();
			let label_enc = base_label.clone();
			crate::process::spawn_tracked_enc_paced(&procs, &ffmpeg, &args, move |ms| {
				let _ =
					stats_enc.try_send(DataMsg::Stats(format!("{label_enc} · {ms:.1} {}", crate::i18n::t("stream.msEncode"))));
			})
			.is_ok()
		};
		let _ = stats_out.try_send(DataMsg::Stats(base_label));
		// Tell the client our display orientation so it can render the video upright even if
		// this host's screen is rotated (e.g. a tent-mode laptop). The NATIVE capture path
		// (pulsar-capture) already BAKES the rotation into the encoded stream via the
		// VideoProcessor Blt, so we report 0 then (avoids double-rotation); the ffmpeg fallback
		// path does NOT rotate, so it reports the real rotation for the client to apply.
		let reported_rotation = if native_started {
			0
		} else {
			display_rotation()
		};
		let _ = stats_out.try_send(DataMsg::DisplayRotation(reported_rotation));
		start_audio_and_mute(&procs, &ffmpeg, &app_h, adest, &req, sid);
		let _ = app_h.emit(
			"session",
			SessionEvent {
				kind: "stream".into(),
				peer: peer.clone(),
				detail: format!("{} · {}p", encoder.label(), eff_h)
					+ if started { "" } else { crate::i18n::t("host.ffmpegFailed") },
			},
		);
	}
}

/// Short human label for a codec (stats panel strings).
fn vcodec_label(c: pipeline::VCodec) -> &'static str {
	match c {
		pipeline::VCodec::H265 => "H.265",
		pipeline::VCodec::Av1 => "AV1",
		pipeline::VCodec::H264 => "H.264",
	}
}

/// Like `spawn_gst_tracked`, but with the ENCODE-PACE meter: the pipeline carries an
/// `identity name=encpace silent=false` right after the encoder, and `-v` makes
/// gst-launch print one `last-message = chain …` line per encoded frame. The parser
/// thread times the line gaps (Δwall per frame ≈ encode pace, same semantics as the
/// ffmpeg `-progress` meter) and calls `on_ms` about once a second — so a gst host
/// (Pi MPP) finally feeds the client's "Kodlama ms" tile instead of "—".
/// Returns the child's pid + a flag that flips on the FIRST encoded frame — the
/// caller's no-output watchdog reads it (see the BGRx-direct RGA fallback).
#[cfg(target_os = "linux")]
fn spawn_gst_paced(
	procs: &Arc<Mutex<Vec<Child>>>,
	pipeline: &str,
	on_ms: impl Fn(f32) + Send + 'static,
) -> Result<(u32, Arc<std::sync::atomic::AtomicBool>), String> {
	let mut g = procs.lock().unwrap();
	spawn_gst_paced_locked(&mut g, pipeline, on_ms)
}

/// `spawn_gst_paced` with the `procs` lock already held: the BGRx-direct RGA
/// watchdog needs its find-dead → kill → respawn → push sequence atomic against
/// the teardown/re-stream drains (which run under this same lock).
#[cfg(target_os = "linux")]
fn spawn_gst_paced_locked(
	procs: &mut Vec<Child>,
	pipeline: &str,
	on_ms: impl Fn(f32) + Send + 'static,
) -> Result<(u32, Arc<std::sync::atomic::AtomicBool>), String> {
	use std::os::unix::process::CommandExt;
	let mut cmd = std::process::Command::new(crate::process::gst_launch_bin());
	cmd.arg("-v").args(pipeline.split_whitespace());
	cmd.stdout(std::process::Stdio::piped());
	// stderr to a per-host scratch file (truncated each spawn): gst dying at start
	// is otherwise invisible — this is the only channel that says WHY.
	let errlog = std::fs::File::create(std::env::temp_dir().join("pulsar-gst-stderr.log")).ok();
	match errlog {
		Some(f) => {
			cmd.stderr(f);
		}
		None => {
			cmd.stderr(std::process::Stdio::null());
		}
	}
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
			if libc::getppid() == 1 {
				libc::_exit(0);
			}
			Ok(())
		});
	}
	let ticked = Arc::new(std::sync::atomic::AtomicBool::new(false));
	match cmd.spawn() {
		Ok(mut child) => {
			if let Some(mut stdout) = child.stdout.take() {
				let ticked = ticked.clone();
				std::thread::spawn(move || {
					use std::io::Read;
					// BYTE-safe line scan — `BufRead::lines()` returns Err on any
					// non-UTF-8 byte in the `-v` dump and a `break` there DROPS the
					// pipe: gst-launch then dies of SIGPIPE on its next print (the
					// "video never starts" regression). This reader only ends at EOF,
					// i.e. when gst itself exited.
					let mut buf = [0u8; 8192];
					let mut line: Vec<u8> = Vec::with_capacity(256);
					let mut last: Option<std::time::Instant> = None;
					let mut ema_ms: f32 = 0.0;
					let mut last_push = std::time::Instant::now();
					loop {
						let n = match stdout.read(&mut buf) {
							Ok(0) | Err(_) => break,
							Ok(n) => n,
						};
						for &b in &buf[..n] {
							if b != b'\n' {
								// Cap pathological unterminated lines; the marker fits well within.
								if line.len() < 4096 {
									line.push(b);
								}
								continue;
							}
							let is_tick = {
								let s = String::from_utf8_lossy(&line);
								s.contains("encpace") && s.contains("last-message")
							};
							line.clear();
							if !is_tick {
								continue;
							}
							ticked.store(true, std::sync::atomic::Ordering::Relaxed);
							let now = std::time::Instant::now();
							if let Some(t0) = last {
								let ms = now.duration_since(t0).as_secs_f32() * 1000.0;
								ema_ms = if ema_ms == 0.0 {
									ms
								} else {
									ema_ms * 0.9 + ms * 0.1
								};
								if last_push.elapsed().as_millis() >= 1000 {
									on_ms(ema_ms);
									last_push = now;
								}
							}
							last = Some(now);
						}
					}
				});
			}
			let pid = child.id();
			procs.push(child);
			Ok((pid, ticked))
		}
		Err(e) => Err(format!("gst-launch-1.0 {}: {e}", crate::i18n::t("err.spawn"))),
	}
}

/// Spawn a `gst-launch-1.0` encode pipeline tracked in `procs` (killed on
/// (re-)stream/teardown like the ffmpeg children). `PR_SET_PDEATHSIG` mirrors the
/// Wayland capture spawn so an app crash can never orphan a streaming encoder.
#[cfg(target_os = "linux")]
fn spawn_gst_tracked(procs: &Arc<Mutex<Vec<Child>>>, pipeline: &str) -> Result<(), String> {
	use std::os::unix::process::CommandExt;
	let mut cmd = std::process::Command::new(crate::process::gst_launch_bin());
	cmd.arg("-q").args(pipeline.split_whitespace());
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
			if libc::getppid() == 1 {
				libc::_exit(0);
			}
			Ok(())
		});
	}
	match cmd.spawn() {
		Ok(child) => {
			procs.lock().unwrap().push(child);
			Ok(())
		}
		Err(e) => Err(format!("gst-launch-1.0 {}: {e}", crate::i18n::t("err.spawn"))),
	}
}

/// Build the per-session `on_file` handler: reassemble an inbound file transfer
/// (Begin → buffer, Chunk → append + detect gaps, End → save) and surface the
/// result to the host UI.
pub(super) fn make_on_file(app_h: AppHandle, peer: String) -> impl FnMut(DataMsg) + Send + 'static {
	// Reassemble: Begin → buffer, Chunk → append (detect gaps), End → save.
	let mut name = String::new();
	let mut buf: Vec<u8> = Vec::new();
	let mut next = 0u32;
	let mut expected = 0u32;
	let mut gap = false;
	move |m: DataMsg| match m {
		DataMsg::FileBegin {
			name: n,
			size,
			chunks,
		} => {
			name = sanitize_filename(&n);
			// `size` is peer-controlled: clamp the PRE-allocation (an absurd value
			// would panic with "capacity overflow", a merely huge one reserves GBs
			// up front) — extend_from_slice still grows past the clamp if a
			// legitimately larger file actually arrives.
			buf = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
			next = 0;
			expected = chunks;
			gap = false;
		}
		DataMsg::FileChunk { index, data } => {
			if index != next {
				gap = true;
			}
			next = index.wrapping_add(1);
			buf.extend_from_slice(&data);
		}
		DataMsg::FileEnd => {
			let complete = !gap && next == expected;
			let saved = if complete {
				save_received_file(&name, &buf)
			} else {
				None
			};
			let ok = saved.is_some();
			let _ = app_h.emit(
				"file-recv",
				FilePayload {
					peer: peer.clone(),
					name: name.clone(),
					bytes: buf.len() as u64,
					ok,
				},
			);
			if ok {
				let _ = app_h.emit(
					"session",
					SessionEvent {
						kind: "file".into(),
						peer: peer.clone(),
						detail: format!("{} · {} B", name, buf.len()),
					},
				);
			}
			buf = Vec::new();
		}
		_ => {}
	}
}

/// Audio player child killed AND reaped on drop. The closure itself dropping
/// (client vanished mid-mic) must reap too — the player would otherwise exit on
/// stdin EOF and linger as a zombie for the host app's lifetime, one per
/// mic stop/disconnect.
struct PlayerGuard(Child);
impl Drop for PlayerGuard {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

/// Build the per-session `on_audio` handler: lazily spawn an audio player and
/// pipe received PCM frames to it, tearing it down on `AudioEnd` / write error.
pub(super) fn make_on_audio() -> impl FnMut(DataMsg) + Send + 'static {
	use std::io::Write;
	// Lazily spawn an audio player and pipe received PCM frames to it.
	let mut sink: Option<std::process::ChildStdin> = None;
	let mut player: Option<PlayerGuard> = None;
	move |m: DataMsg| match m {
		DataMsg::Audio(frame) => {
			if sink.is_none() {
				if let Some((c, s)) = spawn_audio_player() {
					player = Some(PlayerGuard(c));
					sink = Some(s);
				}
			}
			if let Some(s) = sink.as_mut() {
				if s.write_all(&frame).is_err() {
					sink = None;
					player = None; // PlayerGuard::drop kills + reaps
				}
			}
		}
		DataMsg::AudioEnd => {
			sink = None;
			player = None; // PlayerGuard::drop kills + reaps
		}
		_ => {}
	}
}
