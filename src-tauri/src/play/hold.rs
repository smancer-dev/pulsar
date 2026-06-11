//! The client's full-duplex hold-loop, extracted verbatim from `start_remote_play`.
//!
//! `hold_session` owns the control `Session` for the lifetime of a remote-play tab:
//! it forwards local input + side-channel data to the host, keepalives every ~2s
//! (UDP has no disconnect signal), receives the host's chat/clipboard/stat pushes
//! (surfacing them to the UI), watchdogs host silence, and re-requests the stream
//! when the session menu changes resolution/encoder. On exit it tells the frontend
//! (`play-ended`) and sends a clean `Bye`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pulsar_core::service::{
	decode_data, media, request_stream, send_data, send_input, send_keepalive, DataMsg, InputEvent,
	QualityPref, StreamReq,
};
use pulsar_core::Session;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::Receiver;

use crate::events::{
	AvatarPayload, DataPayload, FilePayload, FsEntriesPayload, PlayRtt, PlayStats,
};
use crate::state::Restream;

/// Adaptive-bitrate floor (kbit/s) — never step below a usable desktop stream.
const ADAPT_MIN_KBPS: u32 = 2_000;
/// Sustained-loss threshold that triggers a step DOWN (per 2 s keepalive window).
const ADAPT_LOSS_DOWN: f32 = 0.03;
/// "Clean" threshold; this many consecutive clean windows step back UP.
const ADAPT_LOSS_CLEAN: f32 = 0.005;
const ADAPT_CLEAN_WINDOWS: u32 = 10; // ×2 s = 20 s stable before raising
/// Don't re-step within this many seconds (each step restarts the host encoder).
const ADAPT_COOLDOWN_S: u64 = 5;

/// Hold the control session open full-duplex: forward input + side-channel data,
/// keepalive every ~2s (UDP has no disconnect signal), and receive the host's
/// chat/clipboard pushes — surfacing them to the UI.
#[allow(clippy::too_many_arguments)]
pub(super) async fn hold_session(
	mut sess: Session,
	app_ev: AppHandle,
	send_flag: Arc<AtomicBool>,
	mut input_rx: Receiver<InputEvent>,
	mut data_rx: Receiver<DataMsg>,
	mut restream_rx: Receiver<Restream>,
	id: u64,
	video_port: u16,
	audio_port: u16,
	encoder_h: String,
	codec_h: String,
	game_mode: bool,
	decode_codecs: Vec<String>,
	render_stdin: std::sync::Arc<std::sync::Mutex<Option<std::process::ChildStdin>>>,
	mos: bool,
	host_nack: bool,
	req_w: u32,
	req_h: u32,
	req_fps: u32,
	base_kbps: u32,
) {
	let mut keep = tokio::time::interval(std::time::Duration::from_secs(2));
	keep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
	// ---- Media-over-session demux state (single-socket transport) ----
	// Media frames arrive INSIDE this session ([tag][rtp…]); forward each datagram to
	// the local consumer: the renderer's RTP port (video) / the viewer's audio port.
	let fwd_sock = if mos {
		tokio::net::UdpSocket::bind("127.0.0.1:0").await.ok()
	} else {
		None
	};
	let v_dest = std::net::SocketAddr::from(([127, 0, 0, 1], video_port));
	let a_dest = std::net::SocketAddr::from(([127, 0, 0, 1], audio_port));
	// Video RTP gap tracking → NACK retransmit requests + the loss% the adaptive
	// bitrate controller runs on. `missing` holds requested-but-not-yet-seen seqs.
	let mut last_vseq: Option<u16> = None;
	let mut missing: std::collections::HashMap<u16, std::time::Instant> =
		std::collections::HashMap::new();
	let (mut win_recv, mut win_lost) = (0u32, 0u32);
	let mut clean_windows = 0u32;
	let mut last_step = std::time::Instant::now();
	// When the last keepalive Ping was sent, to time the host's Pong (RTT).
	let mut ping_at: Option<std::time::Instant> = None;
	// Watchdog: a host-side kick / network drop doesn't always send a clean close over UDP,
	// so track the last time we heard ANYTHING from the host (incl. keepalive Pongs). If it
	// goes silent past the timeout, end the session — this fires `play-ended` so the client
	// tears down (releases the input grab + drops the tab) instead of freezing on the last
	// frame with the keyboard/mouse still captured.
	let mut last_inbound = std::time::Instant::now();
	// Current stream state; a session-menu change updates one field and re-requests.
	// Geometry/fps seed from the INITIAL request (the Linux native path negotiates an
	// explicit startup size — e.g. the deliberate 960x540@30 mpv cap): a re-request
	// (adaptive-bitrate step / menu change) must preserve it. Seeding 0 instead meant
	// "host config default" (1080p60), flipping a capped stream to full size exactly
	// when the link was already lossy.
	let (mut cur_w, mut cur_h) = (req_w, req_h);
	let mut cur_encoder = encoder_h;
	let mut cur_codec = codec_h;
	let mut cur_fps = req_fps;
	let mut cur_transmit = true;
	let mut cur_mute = game_mode;
	// Starts at the INITIAL request's bitrate (0 = host default) so a menu-driven
	// re-request doesn't silently reset an explicit starting target; the adaptive
	// controller moves it within [ADAPT_MIN_KBPS, base_kbps].
	let mut cur_bitrate = base_kbps;
	// A user-picked limit (session menu / overlay) disables the adaptive controller;
	// picking "Otomatik" (0) re-enables it.
	let mut manual_bitrate = false;
	let adapt_enabled = mos && base_kbps > 0;
	let mut cur_quality = if game_mode {
		QualityPref::Latency
	} else {
		QualityPref::Quality
	};
	// Inbound file reassembly (file-manager downloads — the host streams an FsGet
	// back as FileBegin/Chunk/End): Begin → buffer, Chunk → append + gap detection,
	// End → save under "Pulsar Alınanlar". Mirrors the host's make_on_file.
	let mut f_name = String::new();
	let mut f_buf: Vec<u8> = Vec::new();
	let mut f_next = 0u32;
	let mut f_expected = 0u32;
	let mut f_gap = false;
	loop {
		if !send_flag.load(Ordering::SeqCst) {
			break;
		}
		tokio::select! {
			ev = input_rx.recv() => match ev {
				Some(ev) => if send_input(&mut sess, &ev).await.is_err() { break },
				None => break,
			},
			d = data_rx.recv() => match d {
				// A side-channel send failure must NOT kill the session: an oversized
				// payload (e.g. an avatar past the datagram budget → EMSGSIZE) is a
				// one-message problem, and a genuinely dead link is already caught by
				// the keepalive watchdog below. Breaking here tore the whole session
				// down 0.7 s into the stream ("connected but no video").
				Some(dm) => if let Err(e) = send_data(&sess, &dm).await {
					tracing::warn!(%e, "side-channel send failed (message dropped)");
				},
				None => {}
			},
			inbound = sess.recv() => match inbound {
				Some(bytes) => {
					last_inbound = std::time::Instant::now();
					// Media-over-session fast path: [tag][rtp…] frames → forward the raw
					// RTP datagram to its local consumer (renderer / audio viewer port).
					// Checked FIRST — these are the highest-rate payloads on the session.
					if let Some((tag, rtp)) = media::parse(&bytes) {
						if let Some(s) = fwd_sock.as_ref() {
							let dest = if tag == media::TAG_VIDEO { v_dest } else { a_dest };
							let _ = s.send_to(rtp, dest).await;
						}
						// Video gap tracking: count loss for the adaptive controller and
						// NACK freshly-missing seqs so the host retransmits them.
						if tag == media::TAG_VIDEO {
							if let Some(seq) = media::rtp_seq(rtp) {
								win_recv += 1;
								if missing.remove(&seq).is_some() {
									// A requested retransmit (or late reorder) arrived — not lost.
									win_lost = win_lost.saturating_sub(1);
								} else if let Some(last) = last_vseq {
									let d = media::seq_forward(last, seq);
									if d > 0 && d < 0x8000 {
										if d > 1 && d <= 128 {
											let now = std::time::Instant::now();
											let mut nacks = Vec::with_capacity((d - 1) as usize);
											for i in 1..d {
												let m = last.wrapping_add(i);
												missing.insert(m, now);
												nacks.push(m);
												win_lost += 1;
											}
											if host_nack {
												let _ = send_data(&sess, &DataMsg::MediaNack(nacks)).await;
											}
										} else if d > 128 {
											// Huge jump = encoder restart / long stall: resync.
											missing.clear();
										}
										last_vseq = Some(seq);
									}
								} else {
									last_vseq = Some(seq);
								}
							}
						}
						continue;
					}
						if pulsar_core::service::is_pong(&bytes) {
						if let Some(t0) = ping_at.take() {
							let rtt = t0.elapsed().as_secs_f64() * 1000.0;
							let _ = app_ev.emit("play-rtt", PlayRtt { id, rtt });
							// Feed the native overlay's "Gecikme" tile the real NETWORK
							// latency (it used to show the render present-gap).
							{
								use std::io::Write as _;
								if let Some(si) = render_stdin.lock().unwrap().as_mut() {
									let _ = writeln!(si, "rtt {rtt:.1}");
								}
							}
						}
					} else if let Some(dm) = decode_data(&bytes) {
						match dm {
							DataMsg::Clipboard(text) => {
								let _ = app_ev.emit("data-clip", DataPayload { peer: id.to_string(), text });
							}
							DataMsg::Chat(text) => {
								let _ = app_ev.emit("chat-msg", DataPayload { peer: id.to_string(), text });
							}
							DataMsg::Stats(label) => {
								// Mirror the host's ACTIVE encode summary into the native
								// overlay ("hostenc <label>" stdin line) so it can show what's
								// really in use under the selectors.
								{
									use std::io::Write as _;
									if let Some(si) = render_stdin.lock().unwrap().as_mut() {
										let _ = writeln!(si, "hostenc {label}");
									}
								}
								let _ = app_ev.emit("host-stats", PlayStats { id, label });
							}
							DataMsg::DisplayRotation(deg) => {
								// Render the video upright for a rotated host display (vidsink
								// path only; the webview path can't rotate yet).
								#[cfg(all(unix, not(target_os = "macos")))]
								crate::render::apply_vidsink_rotation(&app_ev, id, deg);
								#[cfg(not(all(unix, not(target_os = "macos"))))]
								let _ = deg;
							}
							DataMsg::Avatar(png) => {
								// The host's identity image for this session, addressed by play
								// id like the other client-side payloads.
								let _ = app_ev.emit("peer-avatar", AvatarPayload {
									peer: id.to_string(),
									data_url: crate::avatar::data_url(&png),
								});
							}
							DataMsg::PeerName(name) => {
								// The host's display name — the UI updates the session target
								// chip and the recents cache.
								let _ = app_ev.emit("peer-name", (id.to_string(), name));
							}
							DataMsg::FsEntries { path, entries } => {
								// A host directory listing for the file panel's remote pane.
								let _ = app_ev.emit("fs-entries", FsEntriesPayload { id, path, entries });
							}
							DataMsg::FileBegin { name, size, chunks } => {
								f_name = crate::files::sanitize_filename(&name);
								// `size` is peer-controlled: clamp the pre-allocation so a
								// bogus huge value can't reserve gigabytes (or panic with
								// "capacity overflow" near usize::MAX) and tear the session
								// down; extend_from_slice grows past this if a legitimately
								// larger file actually arrives.
								f_buf = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
								f_next = 0;
								f_expected = chunks;
								f_gap = false;
							}
							DataMsg::FileChunk { index, data } => {
								if index != f_next {
									f_gap = true;
								}
								f_next = index.wrapping_add(1);
								f_buf.extend_from_slice(&data);
							}
							DataMsg::FileEnd => {
								// Save only complete transfers (UDP transport — a gap means a
								// lost chunk; report a failed transfer instead of corrupting).
								let complete = !f_gap && f_next == f_expected;
								let saved = if complete {
									crate::files::save_received_file(&f_name, &f_buf)
								} else {
									None
								};
								let _ = app_ev.emit("file-recv", FilePayload {
									peer: id.to_string(),
									name: f_name.clone(),
									bytes: f_buf.len() as u64,
									ok: saved.is_some(),
								});
								f_buf = Vec::new();
							}
							_ => {}
						}
					}
				}
				None => break, // host closed the session
			},
			_ = keep.tick() => {
				if send_keepalive(&mut sess).await.is_err() { break }
				ping_at = Some(std::time::Instant::now());
					// Host silent too long (a kick / drop without a clean UDP close) → end the
					// session so the client tears down instead of freezing with input grabbed.
					if last_inbound.elapsed() > std::time::Duration::from_secs(5) {
						break;
					}
				// Retransmits that never arrived stay counted as lost; stop tracking them.
				missing.retain(|_, t| t.elapsed() < std::time::Duration::from_millis(300));
				// ---- Adaptive bitrate (auto mode only): step DOWN on sustained loss,
				// creep back UP after a long clean stretch. Each step re-requests the
				// stream (the host restarts its encoder — brief, so steps are damped).
				if adapt_enabled && !manual_bitrate {
					let total = win_recv + win_lost;
					let loss = if total > 0 { win_lost as f32 / total as f32 } else { 0.0 };
					let mut new_kbps = None;
					if total > 100
						&& loss > ADAPT_LOSS_DOWN
						&& last_step.elapsed().as_secs() >= ADAPT_COOLDOWN_S
						&& cur_bitrate > ADAPT_MIN_KBPS
					{
						new_kbps = Some((cur_bitrate * 7 / 10).max(ADAPT_MIN_KBPS));
						clean_windows = 0;
					} else if loss < ADAPT_LOSS_CLEAN && total > 0 {
						clean_windows += 1;
						if clean_windows >= ADAPT_CLEAN_WINDOWS && cur_bitrate < base_kbps {
							new_kbps = Some((cur_bitrate * 5 / 4).min(base_kbps));
							clean_windows = 0;
						}
					} else {
						clean_windows = 0;
					}
					if let Some(kbps) = new_kbps {
						tracing::info!(loss_pct = loss * 100.0, from = cur_bitrate, to = kbps, "adaptive bitrate step");
						cur_bitrate = kbps;
						last_step = std::time::Instant::now();
						let req = StreamReq {
							port: video_port,
							codec: cur_codec.clone(),
							encoder: cur_encoder.clone(),
							width: cur_w,
							height: cur_h,
							fps: cur_fps,
							audio_port,
							transmit_audio: cur_transmit,
							mute_host: cur_mute,
							game_mode,
							bitrate_kbps: cur_bitrate,
							quality: cur_quality,
							hdr: std::env::var_os("PULSAR_HDR").is_some(),
							yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
							decode_codecs: decode_codecs.clone(),
							media_over_session: mos,
						};
						if request_stream(&mut sess, &req).await.is_err() {
							break;
						}
					}
				}
				(win_recv, win_lost) = (0, 0);
			},
			q = restream_rx.recv() => match q {
				// Session menu changed the resolution or encoder: merge it into the
				// current state and re-request — the host kills the old ffmpeg and
				// restarts capture/encode with the new setting.
				Some(cmd) => {
					match cmd {
						Restream::Resolution(w, h) => { cur_w = w; cur_h = h; }
						Restream::Encoder(e) => { cur_encoder = e; }
						Restream::Codec(c) => { cur_codec = c; }
						Restream::Fps(f) => { cur_fps = f; }
						// A user-picked limit (b > 0) takes over and PAUSES the adaptive
						// controller; "Otomatik" (0) hands control back to it.
						Restream::Bitrate(b) => {
							manual_bitrate = b > 0;
							cur_bitrate = if b > 0 { b } else { base_kbps };
						}
						Restream::Quality(q) => { cur_quality = q; }
						Restream::Audio(t, m) => { cur_transmit = t; cur_mute = m; }
					}
					let req = StreamReq {
						port: video_port,
						codec: cur_codec.clone(),
						encoder: cur_encoder.clone(),
						width: cur_w,
						height: cur_h,
						fps: cur_fps,
						audio_port,
						transmit_audio: cur_transmit,
						mute_host: cur_mute,
						game_mode,
						bitrate_kbps: cur_bitrate,
						quality: cur_quality,
						// Preserve the HDR/4:4:4 preference across live re-requests (same env
						// source as the initial request in play.rs).
						hdr: std::env::var_os("PULSAR_HDR").is_some(),
						yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
						decode_codecs: decode_codecs.clone(),
						media_over_session: mos,
					};
					if request_stream(&mut sess, &req).await.is_err() {
						break;
					}
				}
				None => {}
			},
		}
	}
	// Tell the host we're leaving so it tears down immediately (kills ffmpeg +
	// releases held input) instead of waiting out PEER_TIMEOUT.
	// Hold-loop ended (host closed the session, a network error, or we're leaving): tell the
	// frontend so it tears the session down — release the evdev/input grab + drop the tab —
	// instead of freezing on mpv's last frame with the keyboard/mouse still captured.
	let _ = app_ev.emit("play-ended", id);
	let _ = pulsar_core::service::send_bye(&mut sess).await;
	drop(sess);
}
