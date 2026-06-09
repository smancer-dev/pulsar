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
	decode_data, request_stream, send_data, send_input, send_keepalive, DataMsg, InputEvent,
	QualityPref, StreamReq,
};
use pulsar_core::Session;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::Receiver;

use crate::events::{DataPayload, PlayRtt, PlayStats};
use crate::state::Restream;

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
) {
	let mut keep = tokio::time::interval(std::time::Duration::from_secs(2));
	keep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
	// When the last keepalive Ping was sent, to time the host's Pong (RTT).
	let mut ping_at: Option<std::time::Instant> = None;
	// Watchdog: a host-side kick / network drop doesn't always send a clean close over UDP,
	// so track the last time we heard ANYTHING from the host (incl. keepalive Pongs). If it
	// goes silent past the timeout, end the session — this fires `play-ended` so the client
	// tears down (releases the input grab + drops the tab) instead of freezing on the last
	// frame with the keyboard/mouse still captured.
	let mut last_inbound = std::time::Instant::now();
	// Current stream state; a session-menu change updates one field and re-requests.
	let (mut cur_w, mut cur_h) = (0u32, 0u32);
	let mut cur_encoder = encoder_h;
	let mut cur_codec = codec_h;
	let mut cur_fps = 0u32;
	let mut cur_transmit = true;
	let mut cur_mute = game_mode;
	// 0 = host default. Quality bias defaults to the mode's natural side (game → low
	// latency, remote → quality); a live `set_play_quality` overrides it.
	let mut cur_bitrate = 0u32;
	let mut cur_quality = if game_mode {
		QualityPref::Latency
	} else {
		QualityPref::Quality
	};
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
				Some(dm) => if send_data(&sess, &dm).await.is_err() { break },
				None => {}
			},
			inbound = sess.recv() => match inbound {
				Some(bytes) => {
					last_inbound = std::time::Instant::now();
						if pulsar_core::service::is_pong(&bytes) {
						if let Some(t0) = ping_at.take() {
							let _ = app_ev.emit("play-rtt", PlayRtt { id, rtt: t0.elapsed().as_secs_f64() * 1000.0 });
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
						Restream::Bitrate(b) => { cur_bitrate = b; }
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
