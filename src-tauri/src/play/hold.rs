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
	cursor_external: bool,
	req_hdr: bool,
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
	// One-shot diagnostic: did host media actually reach this client + get forwarded?
	let mut first_video_logged = false;
	// Video RTP gap tracking → NACK retransmit requests + the loss% the adaptive
	// bitrate controller runs on. `missing` holds requested-but-not-yet-seen seqs.
	let mut last_vseq: Option<u16> = None;
	let mut missing: std::collections::HashMap<u16, std::time::Instant> =
		std::collections::HashMap::new();
	let (mut win_recv, mut win_lost) = (0u32, 0u32);
	let mut clean_windows = 0u32;
	let mut last_step = std::time::Instant::now();
	// When the last menu re-request (esp. a monitor switch) fired. The host rebuilds its encoder
	// for one → a brief packet-loss gap that is NOT congestion; the adaptive controller must
	// ignore it for a moment, or it steps the bitrate (a full-path restream that churns + slows
	// the very switch in progress). Seeded far in the past so startup isn't suppressed.
	let mut last_switch_at = std::time::Instant::now() - std::time::Duration::from_secs(60);
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
	// Host-silent intent: game mode requests it by default (seeds the initial play.rs request);
	// the host honors it by REDIRECTING its default output to a virtual sink (never muting the
	// captured endpoint — see play.rs). Preserved across every re-request; the user can flip it
	// mid-session from the overlay (sets cur_mute below).
	let mut cur_mute = game_mode;
	// Host monitor index (0 = primary). Changed live by the session menu's monitor
	// picker (Restream::Display); preserved across every re-request below.
	let mut cur_display: u32 = 0;
	// Requested audio channel layout. Stereo today (the client default in play.rs — the
	// audio paths expect opus/48000/2 and the host negotiates surround down anyway);
	// kept as state so it's preserved across every re-request and a future surround
	// picker can flip it without re-plumbing each StreamReq.
	let cur_audio_layout = pulsar_core::audio::ChannelLayout::Stereo;
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
	// HDR preference from the UI toggle (Settings → Display). The PULSAR_HDR env var
	// is a debug override: if set it wins regardless of the UI value.
	let cur_hdr = req_hdr || std::env::var_os("PULSAR_HDR").is_some();
	// Inbound file reassembly (file-manager downloads — the host streams an FsGet
	// back as FileBegin/Chunk/End): Begin → state, Chunk → store BY INDEX,
	// End → save under "Pulsar Alınanlar". The session transport is unordered UDP,
	// so chunks can arrive reordered or duplicated; keying by index (not appending
	// in arrival order) lets reorders/dups self-heal — only a genuinely lost chunk
	// fails the transfer. Each transfer carries a per-stream `id`, so concurrent
	// downloads keep separate reassembly state keyed by that id and interleaved
	// messages no longer clobber each other. Mirrors the host's make_on_file.
	/// Idle transfers older than this are swept from the map on the next FileBegin.
	const F_XFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
	/// Maximum concurrent in-flight transfers. If a new FileBegin would push us over
	/// this limit we first evict a headless entry (`expected == None` — FileBegin was
	/// lost, can never complete) and only fall back to oldest-by-last_activity among
	/// active entries, to avoid silently killing a slow-but-legitimate transfer.
	const F_MAX_CONCURRENT_XFERS: usize = 8;
	struct FileReasm {
		/// Set by FileBegin. `None` means chunks arrived before FileBegin (UDP
		/// reorder); the entry was lazily created to buffer them. A FileEnd with
		/// `expected == None` is treated as incomplete (Begin never arrived).
		name: String,
		expected: Option<u32>,
		chunks: std::collections::BTreeMap<u32, Vec<u8>>,
		// Running total of buffered bytes (sum of every stored chunk's len, dup-safe).
		// Capped against MAX_XFER_BYTES so a peer can't OOM us by announcing a huge
		// chunk count and streaming distinct-index chunks without ever sending FileEnd.
		received: u64,
		/// Updated on FileBegin and every FileChunk; entries idle beyond
		/// F_XFER_IDLE_TIMEOUT are swept on the next FileBegin so a lost FileEnd
		/// (UDP, no retransmit) can't leak buffered bytes for the session lifetime.
		last_activity: std::time::Instant,
	}
	let mut f_xfers: std::collections::HashMap<u32, FileReasm> = std::collections::HashMap::new();
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
					// Media-over-session fast path: [tag][rtp…] frames → forward the raw
					// RTP datagram to its local consumer (renderer / audio viewer port).
					// Checked FIRST — these are the highest-rate payloads on the session.
					if let Some((tag, rtp)) = media::parse(&bytes) {
						if let Some(s) = fwd_sock.as_ref() {
							let dest = if tag == media::TAG_VIDEO { v_dest } else { a_dest };
							if tag == media::TAG_VIDEO && !first_video_logged {
								first_video_logged = true;
								tracing::info!(%dest, "first video RTP datagram forwarded to renderer");
							}
							let _ = s.send_to(rtp, dest).await;
						} else if !first_video_logged {
							first_video_logged = true;
							tracing::warn!("media arrived but fwd_sock is None — nothing forwarded");
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
									if d == 0 {
										// Duplicate / retransmit of the last seq — ignore.
									} else if d < 0x8000 && d <= 128 {
										// Small forward gap → NACK the missing seqs for retransmit.
										if d > 1 {
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
										}
										last_vseq = Some(seq);
									} else if d < 0x8000 {
										// Big forward gap (d > 128 but < 0x8000) — encoder restarted
										// with a fresh RTP sequence base (every monitor/codec switch
										// rebuilds it). Resync cleanly to the new base.
										missing.clear();
										last_vseq = Some(seq);
									} else {
										// Backward jump (d >= 0x8000). Compute the backward distance:
										// how far behind seq is relative to last.
										let back = 0x10000u32 - d as u32;
										if back > 128 {
											// Large backward jump = the host's encoder RESTARTED with a
											// new sequence base that happens to be numerically lower
											// (roughly half of random bases land here). The old code
											// skipped these — last_vseq stayed STUCK, so every later
											// packet re-tripped the discontinuity and NACK-flooded the
											// session. Resync cleanly to the new base instead.
											missing.clear();
											last_vseq = Some(seq);
										}
										// else: small backward distance (≤128 seqs behind) = an
										// ordinary stale reorder or duplicate that arrived late.
										// Leave last_vseq unchanged and do NOT NACK or count loss.
									}
								} else {
									last_vseq = Some(seq);
								}
							}
						}
						continue;
					}
						// Reached only by NON-media (control) datagrams -- the media fast-path
						// continue-d above. Reset the grab-release watchdog HERE so it tracks CONTROL
						// liveness (keepalive Pong + any DataMsg), not the video flow: a dead control
						// link with live video must still trip the timeout below and tear the session
						// down instead of leaving it frozen-but-grabbed.
						last_inbound = std::time::Instant::now();
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
							DataMsg::CursorPos { x, y } => {
								// Cursor side-channel: the host captured WITHOUT a hardware cursor
								// in the frame (KMS zero-copy) and streams the pointer position
								// out-of-band. Push it to the native renderer so it draws the cursor
								// over the video (Moonlight model). Tiny payload, ~60 Hz.
								use std::io::Write as _;
								if let Some(si) = render_stdin.lock().unwrap().as_mut() {
									let _ = writeln!(si, "cursor {x:.5} {y:.5}");
								}
							}
							DataMsg::CursorShape { w, h, hot_x, hot_y, rgba_png } => {
								// The host pointer's bitmap changed (caret/resize transitions).
								// Base64 the PNG onto one stdin line so the renderer swaps the drawn
								// cursor image + hotspot.
								use base64::Engine as _;
								use std::io::Write as _;
								let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba_png);
								if let Some(si) = render_stdin.lock().unwrap().as_mut() {
									let _ = writeln!(si, "cursorimg {w} {h} {hot_x} {hot_y} {b64}");
								}
							}
							DataMsg::CursorHidden => {
								// The host pointer is hidden / left the screen — stop drawing it.
								use std::io::Write as _;
								if let Some(si) = render_stdin.lock().unwrap().as_mut() {
									let _ = writeln!(si, "cursorhide");
								}
							}
							DataMsg::FsEntries { path, entries } => {
								// A host directory listing for the file panel's remote pane.
								let _ = app_ev.emit("fs-entries", FsEntriesPayload { id, path, entries });
							}
							DataMsg::FileBegin { id: xfer, name, size: _, chunks } => {
								// Evict stale entries (lost FileEnd / lost middle chunk) so a
								// long-lived session over a lossy link can't accumulate unbounded
								// dead reassemblers.
								let now = std::time::Instant::now();
								f_xfers.retain(|_, r| now.duration_since(r.last_activity) < F_XFER_IDLE_TIMEOUT);
								// Hard cap: if still at the concurrent limit, evict the
								// least-harmful entry — but not the lazy placeholder for THIS
								// id (don't evict the entry we're about to complete).
								// Prefer headless entries (expected == None; their FileBegin
								// was lost so they can never complete) over active ones;
								// tie-break by oldest last_activity.
								if f_xfers.len() >= F_MAX_CONCURRENT_XFERS && !f_xfers.contains_key(&xfer) {
									let victim = f_xfers
										.iter()
										.min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
										.map(|(k, _)| *k);
									if let Some(vid) = victim {
										f_xfers.remove(&vid);
									}
								}
								// If early FileChunks created a lazy entry (UDP reorder), merge
								// the name + expected into it to preserve buffered chunks.
								// Otherwise insert a fresh entry.
								if let Some(r) = f_xfers.get_mut(&xfer) {
									r.name = crate::files::sanitize_filename(&name);
									r.expected = Some(chunks);
									r.last_activity = now;
									// Prune any pre-buffered chunks whose index is now >= chunks.
									// They arrived before FileBegin (expected was None) so the
									// in_range guard passed them all through; now that we know the
									// count, out-of-range indices must be removed so they cannot
									// substitute for a genuinely lost in-range chunk.
									r.chunks.retain(|&idx, data| {
										if idx < chunks {
											true
										} else {
											r.received = r.received.saturating_sub(data.len() as u64);
											false
										}
									});
								} else {
									f_xfers.insert(xfer, FileReasm {
										name: crate::files::sanitize_filename(&name),
										expected: Some(chunks),
										chunks: std::collections::BTreeMap::new(),
										received: 0,
										last_activity: now,
									});
								}
							}
							DataMsg::FileChunk { id: xfer, index, data } => {
								// If no entry exists yet (FileBegin hasn't arrived — UDP reorder),
								// create a lazy placeholder so the chunk is buffered rather than
								// dropped. FileBegin will fill in `name` and `expected` when it
								// arrives, keeping the already-stored chunks intact.
								if !f_xfers.contains_key(&xfer) {
									// Mirror the FileBegin guard: sweep idle entries and enforce
									// the concurrent cap before inserting a new headless entry.
									// Without this a chunk-only flood bypasses both the retain()
									// and the cap, allowing unbounded HashMap growth.
									let now = std::time::Instant::now();
									f_xfers.retain(|_, r| now.duration_since(r.last_activity) < F_XFER_IDLE_TIMEOUT);
									if f_xfers.len() >= F_MAX_CONCURRENT_XFERS {
										let victim = f_xfers
											.iter()
											.min_by_key(|(_, r)| (r.expected.is_some() as u8, r.last_activity))
											.map(|(k, _)| *k);
										if let Some(vid) = victim {
											f_xfers.remove(&vid);
										}
									}
									f_xfers.insert(xfer, FileReasm {
										name: String::new(),
										expected: None,
										chunks: std::collections::BTreeMap::new(),
										received: 0,
										last_activity: now,
									});
								}
								// Ignore an index past the announced count (bogus); a re-sent
								// index just overwrites with the identical bytes. Before FileBegin
								// (expected == None) we buffer all indices — the overflow check
								// runs once expected is known.
								let overflow = if let Some(r) = f_xfers.get_mut(&xfer) {
									r.last_activity = std::time::Instant::now();
									let in_range = r.expected.map_or(true, |e| index < e);
									if in_range {
										let prev_len = r.chunks.get(&index).map(|p| p.len() as u64).unwrap_or(0);
										let projected = r.received - prev_len + data.len() as u64;
										if projected > crate::files::MAX_XFER_BYTES {
											true
										} else {
											r.chunks.insert(index, data);
											r.received = projected;
											false
										}
									} else {
										false
									}
								} else {
									false
								};
								if overflow {
									// Peer is overshooting the sane transfer ceiling — drop the
									// whole transfer (further chunks for this id find no entry →
									// ignored, a later FileEnd is a no-op) so the buffer can't
									// grow unbounded.
									f_xfers.remove(&xfer);
								}
							}
							DataMsg::FileEnd { id: xfer } => {
								// End the transfer: a repeated/stray FileEnd (no matching
								// Begin) must not re-save — `remove` drops the state so a
								// second End for the same id is a no-op.
								let Some(r) = f_xfers.remove(&xfer) else { continue };
								// Save only complete transfers (unordered UDP — a missing
								// index means a lost chunk; report a failed transfer instead
								// of corrupting). Reorders/dups already self-healed above.
								// `expected == Some(0)` is a legitimate empty file. If
								// `expected` is still `None`, FileBegin never arrived (lost
								// or extremely delayed); treat that as failed.
								// Contiguous check: len == e is not sufficient — an
								// out-of-range pre-Begin chunk (now pruned on FileBegin)
								// could otherwise substitute for a lost in-range chunk.
								let complete = r.expected.map_or(false, |e| {
									r.chunks.len() == e as usize
										&& (e == 0 || r.chunks.contains_key(&0) && r.chunks.contains_key(&(e - 1)))
										&& (0..e).all(|i| r.chunks.contains_key(&i))
								});
								// Write chunks directly to disk without building a contiguous
								// intermediate Vec — avoids ~2x peak memory at the MAX_XFER_BYTES
								// ceiling (C24 fix).
								let saved = if complete {
									crate::files::save_received_file_chunks(
										&r.name,
										r.chunks.values(),
										r.received,
									)
								} else {
									None
								};
								let written = saved.as_ref().map(|(_, b)| *b).unwrap_or(0);
								let _ = app_ev.emit("file-recv", FilePayload {
									peer: id.to_string(),
									name: r.name.clone(),
									bytes: written,
									ok: saved.is_some(),
								});
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
				if last_switch_at.elapsed() < std::time::Duration::from_millis(2500) {
					// A recent menu switch caused an encoder-rebuild gap — skip the adaptive step so
					// that loss isn't mistaken for congestion (win_recv/win_lost are zeroed at the
					// end of this tick anyway); also don't let it count toward a clean-stretch creep.
					clean_windows = 0;
				} else if adapt_enabled && !manual_bitrate {
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
							hdr: cur_hdr,
							yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
							decode_codecs: decode_codecs.clone(),
							media_over_session: mos,
							cursor_external,
							display_idx: cur_display,
							audio_layout: cur_audio_layout,
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
					// A menu re-request (esp. a monitor switch) → the host rebuilds its encoder
					// (a brief gap). Mark it so the adaptive controller ignores that gap's loss.
					last_switch_at = std::time::Instant::now();
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
						Restream::Audio(t, m) => {
							tracing::info!(transmit = t, mute = m, "audio restream requested");
							cur_transmit = t;
							cur_mute = m;
						}
						Restream::Display(d) => {
							tracing::info!(display = d, "host monitor switch requested");
							cur_display = d;
						}
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
						// Preserve the HDR/4:4:4 preference across live re-requests.
						hdr: cur_hdr,
						yuv444: std::env::var_os("PULSAR_YUV444").is_some(),
						decode_codecs: decode_codecs.clone(),
						media_over_session: mos,
						cursor_external,
						display_idx: cur_display,
						audio_layout: cur_audio_layout,
					};
					let rs = request_stream(&mut sess, &req).await;
					tracing::info!(display_idx = cur_display, ok = rs.is_ok(), "restream request_stream sent");
					if rs.is_err() {
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
	// The session's file-manager window (if open) dies with the session.
	crate::files_window::close(&app_ev, id);
	let _ = pulsar_core::service::send_bye(&mut sess).await;
	drop(sess);
}
