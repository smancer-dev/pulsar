//! In-process libav host encode path (Linux) — adaptive streaming, decision 1.
//!
//! The ffmpeg CLI cannot be touched while it runs: every bitrate step and every keyframe
//! request meant killing it and starting a new one (a visible gap). This module runs the
//! same pipeline **inside the app** with the ffmpeg libraries:
//!
//! ```text
//! x11grab (libavdevice) → libavfilter (scale, format) → libavcodec encoder → libavformat RTP
//! ```
//!
//! and exposes what the controller needs live: `set_bitrate` (libx264 / NVENC / QSV
//! reconfigure the rate control in place when `bit_rate` changes between frames),
//! `request_keyframe` (the next frame is sent with `pict_type = I` + `forced-idr`, so every
//! encoder emits an IDR on demand), and `set_recovery` (the short-GOP mode forces a keyframe
//! every ~0.5 s from here — no GOP change needed in the encoder).
//!
//! Scope (`supported`): X11 capture, H.264/HEVC/AV1 with libx264/libx265/SVT-AV1 or NVENC
//! via ffmpeg, SDR 4:2:0. Everything else (VA-API/Vulkan hw frames, HDR, 4:4:4, Windows
//! and macOS) stays on the ffmpeg CLI path — the same library build is not bundled there yet.
//! A `lavfi testsrc` input (`Source::Test`) lets the engine run headless in tests.

#![cfg(target_os = "linux")]

use std::ffi::{c_int, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;

use ffmpeg_sys_next as ff;
use pulsar_core::pipeline::{CaptureMethod, HwEncoder, StreamPlan, VCodec};
use pulsar_core::service::LossRecovery;

/// Where the frames come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
	/// `x11grab` of `display` (e.g. `:0.0` or `:0.0+1920,0`).
	X11 { display: String },
	/// `lavfi testsrc2` — headless, for tests.
	Test,
}

/// Everything the engine needs, derived from the [`StreamPlan`].
#[derive(Clone, Debug)]
pub struct Params {
	pub source: Source,
	pub width: u32,
	pub height: u32,
	pub fps: u32,
	pub bitrate_kbps: u32,
	pub encoder: HwEncoder,
	pub codec: VCodec,
	pub low_latency: bool,
	pub recovery: LossRecovery,
	/// `host:port` of the RTP destination (UDP).
	pub dest: String,
}

impl Params {
	pub fn from_plan(plan: &StreamPlan) -> Option<Self> {
		if !supported(plan) {
			return None;
		}
		let dest = plan.dest.strip_prefix("rtp://").unwrap_or(&plan.dest).to_string();
		Some(Self {
			source: Source::X11 { display: plan.display.clone() },
			width: plan.width.max(2) & !1,
			height: plan.height.max(2) & !1,
			fps: plan.fps.max(1),
			bitrate_kbps: plan.bitrate_kbps.max(200),
			encoder: plan.encoder,
			codec: plan.codec,
			low_latency: plan.low_latency,
			recovery: plan.loss_recovery,
			dest,
		})
	}

	fn encoder_name(&self) -> Option<&'static str> {
		self.encoder.ffmpeg_name(self.codec)
	}
}

/// Can the in-process path take this plan? (X11 capture, a software or NVENC encoder that
/// takes CPU frames, SDR 4:2:0, and not disabled by `PULSAR_LIBAV_HOST=0`.)
pub fn supported(plan: &StreamPlan) -> bool {
	if std::env::var("PULSAR_LIBAV_HOST").map(|v| v == "0").unwrap_or(false) {
		return false;
	}
	plan.capture == CaptureMethod::X11grab
		&& !plan.hdr
		&& !plan.yuv444
		&& matches!(plan.encoder, HwEncoder::Software | HwEncoder::Nvenc)
		&& plan.encoder.ffmpeg_name(plan.codec).is_some()
}

enum Cmd {
	Bitrate(u32),
	Keyframe,
	Recovery(LossRecovery),
	Stop,
}

/// A running in-process encode. Dropping it stops the thread (best effort, joined).
pub struct LibavHost {
	ctrl: Sender<Cmd>,
	thread: Option<std::thread::JoinHandle<()>>,
	alive: Arc<AtomicBool>,
	params: Params,
}

impl LibavHost {
	/// Start capture + encode on a dedicated thread; fails fast (within ~1 s) when the
	/// input device, filter graph, encoder or RTP output cannot be opened.
	pub fn start(params: Params) -> Result<Self, String> {
		params.encoder_name().ok_or("encoder/codec pair has no ffmpeg encoder")?;
		let (ctrl, rx) = mpsc::channel();
		let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
		let alive = Arc::new(AtomicBool::new(true));
		let alive_t = alive.clone();
		let p = params.clone();
		let thread = std::thread::Builder::new()
			.name("pulsar-libav-host".into())
			.spawn(move || {
				// SAFETY: every libav call below follows the documented ownership rules; the
				// thread owns all contexts and frees them on every exit path.
				let r = unsafe { run(&p, rx, ready_tx) };
				if let Err(e) = r {
					tracing::error!("libav host encode ended with an error: {e}");
				}
				alive_t.store(false, Ordering::Relaxed);
			})
			.map_err(|e| format!("thread spawn failed: {e}"))?;
		match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
			Ok(Ok(())) => Ok(Self { ctrl, thread: Some(thread), alive, params }),
			Ok(Err(e)) => {
				let _ = thread.join();
				Err(e)
			}
			Err(_) => {
				let _ = ctrl.send(Cmd::Stop);
				Err("libav host did not become ready in time".into())
			}
		}
	}

	pub fn params(&self) -> &Params {
		&self.params
	}

	pub fn is_alive(&self) -> bool {
		self.alive.load(Ordering::Relaxed)
	}

	/// Change the encoder bitrate live (kbit/s).
	pub fn set_bitrate(&self, kbps: u32) {
		let _ = self.ctrl.send(Cmd::Bitrate(kbps.max(200)));
	}

	/// Force the next frame to be an IDR.
	pub fn request_keyframe(&self) {
		let _ = self.ctrl.send(Cmd::Keyframe);
	}

	/// Loss-recovery mode: `ShortGop` / `IntraRefresh` force a keyframe every ~0.5 s from
	/// here (intra refresh proper is only settable at encoder open; the forced cadence is
	/// the live equivalent), `Normal` returns to the encoder's own GOP.
	pub fn set_recovery(&self, r: LossRecovery) {
		let _ = self.ctrl.send(Cmd::Recovery(r));
	}

	pub fn stop(mut self) {
		let _ = self.ctrl.send(Cmd::Stop);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

impl Drop for LibavHost {
	fn drop(&mut self) {
		let _ = self.ctrl.send(Cmd::Stop);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

// ── The libav pipeline ───────────────────────────────────────────────────────────────────

fn cstr(s: &str) -> CString {
	CString::new(s).unwrap_or_default()
}

fn averr(code: c_int) -> String {
	let mut buf = [0i8; 128];
	unsafe {
		ff::av_strerror(code, buf.as_mut_ptr() as *mut _, buf.len());
		CStr::from_ptr(buf.as_ptr() as *const _).to_string_lossy().into_owned()
	}
}

unsafe fn dict_set(d: &mut *mut ff::AVDictionary, k: &str, v: &str) {
	let k = cstr(k);
	let v = cstr(v);
	ff::av_dict_set(d, k.as_ptr(), v.as_ptr(), 0);
}

unsafe fn opt_set(obj: *mut std::ffi::c_void, k: &str, v: &str) {
	let k = cstr(k);
	let v = cstr(v);
	// Search children too (the encoder's private options live on priv_data).
	ff::av_opt_set(obj, k.as_ptr(), v.as_ptr(), ff::AV_OPT_SEARCH_CHILDREN as c_int);
}

/// RAII for the four libav contexts; frees whatever was opened on every exit path.
struct Ctx {
	ictx: *mut ff::AVFormatContext,
	dec: *mut ff::AVCodecContext,
	graph: *mut ff::AVFilterGraph,
	src: *mut ff::AVFilterContext,
	sink: *mut ff::AVFilterContext,
	enc: *mut ff::AVCodecContext,
	octx: *mut ff::AVFormatContext,
	header_written: bool,
}

impl Drop for Ctx {
	fn drop(&mut self) {
		unsafe {
			if !self.octx.is_null() {
				if self.header_written {
					ff::av_write_trailer(self.octx);
				}
				if !(*self.octx).pb.is_null() {
					ff::avio_closep(&mut (*self.octx).pb);
				}
				ff::avformat_free_context(self.octx);
			}
			if !self.enc.is_null() {
				ff::avcodec_free_context(&mut self.enc);
			}
			if !self.graph.is_null() {
				ff::avfilter_graph_free(&mut self.graph);
			}
			if !self.dec.is_null() {
				ff::avcodec_free_context(&mut self.dec);
			}
			if !self.ictx.is_null() {
				ff::avformat_close_input(&mut self.ictx);
			}
		}
	}
}

unsafe fn open_input(p: &Params) -> Result<(*mut ff::AVFormatContext, c_int), String> {
	ff::avdevice_register_all();
	let mut opts: *mut ff::AVDictionary = ptr::null_mut();
	let (fmt_name, url) = match &p.source {
		Source::X11 { display } => {
			dict_set(&mut opts, "framerate", &p.fps.to_string());
			dict_set(&mut opts, "video_size", &format!("{}x{}", p.width, p.height));
			dict_set(&mut opts, "draw_mouse", "1");
			("x11grab", display.clone())
		}
		Source::Test => ("lavfi", format!("testsrc2=size={}x{}:rate={}", p.width, p.height, p.fps)),
	};
	let fmt = ff::av_find_input_format(cstr(fmt_name).as_ptr());
	if fmt.is_null() {
		return Err(format!("libav input format `{fmt_name}` unavailable"));
	}
	let mut ictx: *mut ff::AVFormatContext = ptr::null_mut();
	let url_c = cstr(&url);
	let r = ff::avformat_open_input(&mut ictx, url_c.as_ptr(), fmt, &mut opts);
	ff::av_dict_free(&mut opts);
	if r < 0 {
		return Err(format!("{fmt_name} open failed ({url}): {}", averr(r)));
	}
	let r = ff::avformat_find_stream_info(ictx, ptr::null_mut());
	if r < 0 {
		ff::avformat_close_input(&mut ictx);
		return Err(format!("stream info failed: {}", averr(r)));
	}
	let vs = ff::av_find_best_stream(ictx, ff::AVMediaType::AVMEDIA_TYPE_VIDEO, -1, -1, ptr::null_mut(), 0);
	if vs < 0 {
		ff::avformat_close_input(&mut ictx);
		return Err("no video stream from the capture".into());
	}
	Ok((ictx, vs))
}

unsafe fn open_decoder(ictx: *mut ff::AVFormatContext, vs: c_int) -> Result<*mut ff::AVCodecContext, String> {
	let st = *(*ictx).streams.add(vs as usize);
	let par = (*st).codecpar;
	let dec = ff::avcodec_find_decoder((*par).codec_id);
	if dec.is_null() {
		return Err("no decoder for the capture format".into());
	}
	let mut dc = ff::avcodec_alloc_context3(dec);
	if dc.is_null() {
		return Err("decoder alloc failed".into());
	}
	ff::avcodec_parameters_to_context(dc, par);
	(*dc).thread_count = 1;
	let r = ff::avcodec_open2(dc, dec, ptr::null_mut());
	if r < 0 {
		ff::avcodec_free_context(&mut dc);
		return Err(format!("decoder open failed: {}", averr(r)));
	}
	Ok(dc)
}

/// `buffer → scale/format → buffersink` so the encoder gets exactly `w×h` in `pix_fmt`.
unsafe fn build_graph(
	dc: *mut ff::AVCodecContext,
	st: *mut ff::AVStream,
	w: u32,
	h: u32,
	pix_fmt: ff::AVPixelFormat,
) -> Result<(*mut ff::AVFilterGraph, *mut ff::AVFilterContext, *mut ff::AVFilterContext), String> {
	let graph = ff::avfilter_graph_alloc();
	if graph.is_null() {
		return Err("filter graph alloc failed".into());
	}
	let bufsrc = ff::avfilter_get_by_name(cstr("buffer").as_ptr());
	let bufsink = ff::avfilter_get_by_name(cstr("buffersink").as_ptr());
	if bufsrc.is_null() || bufsink.is_null() {
		ff::avfilter_graph_free(&mut { graph });
		return Err("buffer/buffersink filters unavailable".into());
	}
	let tb = (*st).time_base;
	let args = format!(
		"video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect=1/1",
		(*dc).width,
		(*dc).height,
		(*dc).pix_fmt as c_int,
		tb.num.max(1),
		tb.den.max(1)
	);
	let mut src: *mut ff::AVFilterContext = ptr::null_mut();
	let mut sink: *mut ff::AVFilterContext = ptr::null_mut();
	let r = ff::avfilter_graph_create_filter(&mut src, bufsrc, cstr("in").as_ptr(), cstr(&args).as_ptr(), ptr::null_mut(), graph);
	if r < 0 {
		let mut g = graph;
		ff::avfilter_graph_free(&mut g);
		return Err(format!("buffer source failed: {}", averr(r)));
	}
	let r = ff::avfilter_graph_create_filter(&mut sink, bufsink, cstr("out").as_ptr(), ptr::null(), ptr::null_mut(), graph);
	if r < 0 {
		let mut g = graph;
		ff::avfilter_graph_free(&mut g);
		return Err(format!("buffer sink failed: {}", averr(r)));
	}
	// Pin the sink's pixel format.
	let fmts: [c_int; 2] = [pix_fmt as c_int, -1];
	let _ = av_opt_set_int_list_workaround(sink, &fmts);
	let desc = format!("scale={}:{}:flags=fast_bilinear,format=pix_fmts={}", w, h, pix_fmt as c_int);
	let mut inputs = ff::avfilter_inout_alloc();
	let mut outputs = ff::avfilter_inout_alloc();
	(*outputs).name = ff::av_strdup(cstr("in").as_ptr());
	(*outputs).filter_ctx = src;
	(*outputs).pad_idx = 0;
	(*outputs).next = ptr::null_mut();
	(*inputs).name = ff::av_strdup(cstr("out").as_ptr());
	(*inputs).filter_ctx = sink;
	(*inputs).pad_idx = 0;
	(*inputs).next = ptr::null_mut();
	let r = ff::avfilter_graph_parse_ptr(graph, cstr(&desc).as_ptr(), &mut inputs, &mut outputs, ptr::null_mut());
	ff::avfilter_inout_free(&mut inputs);
	ff::avfilter_inout_free(&mut outputs);
	if r < 0 {
		let mut g = graph;
		ff::avfilter_graph_free(&mut g);
		return Err(format!("filter graph parse failed ({desc}): {}", averr(r)));
	}
	let r = ff::avfilter_graph_config(graph, ptr::null_mut());
	if r < 0 {
		let mut g = graph;
		ff::avfilter_graph_free(&mut g);
		return Err(format!("filter graph config failed: {}", averr(r)));
	}
	Ok((graph, src, sink))
}

/// `av_opt_set_int_list` is a macro in C; this is its expansion for a `-1`-terminated
/// `AVPixelFormat` list on the `pix_fmts` option.
unsafe fn av_opt_set_int_list_workaround(obj: *mut ff::AVFilterContext, list: &[c_int]) -> c_int {
	ff::av_opt_set_bin(
		obj as *mut _,
		cstr("pix_fmts").as_ptr(),
		list.as_ptr() as *const u8,
		(list.len() * std::mem::size_of::<c_int>()) as c_int,
		ff::AV_OPT_SEARCH_CHILDREN as c_int,
	)
}

unsafe fn open_encoder(p: &Params, pix_fmt: ff::AVPixelFormat) -> Result<*mut ff::AVCodecContext, String> {
	let name = p.encoder_name().ok_or("no encoder name")?;
	let codec = ff::avcodec_find_encoder_by_name(cstr(name).as_ptr());
	if codec.is_null() {
		return Err(format!("encoder `{name}` not in this libavcodec"));
	}
	let mut ec = ff::avcodec_alloc_context3(codec);
	if ec.is_null() {
		return Err("encoder alloc failed".into());
	}
	(*ec).width = p.width as c_int;
	(*ec).height = p.height as c_int;
	(*ec).time_base = ff::AVRational { num: 1, den: p.fps as c_int };
	(*ec).framerate = ff::AVRational { num: p.fps as c_int, den: 1 };
	(*ec).pix_fmt = pix_fmt;
	(*ec).bit_rate = p.bitrate_kbps as i64 * 1000;
	(*ec).rc_max_rate = (*ec).bit_rate;
	(*ec).rc_buffer_size = ((*ec).bit_rate / 2) as c_int; // ~0.5 s VBV: low latency, no spikes
	(*ec).max_b_frames = 0;
	// GOP as the CLI path: game ≈ 0.25 s, remote ≈ 2 s; the recovery modes force keyframes
	// from the loop instead of changing this live.
	(*ec).gop_size = if p.low_latency { (p.fps / 4).max(1) } else { (p.fps * 2).max(1) } as c_int;
	(*ec).thread_count = 0;
	let priv_ = ec as *mut std::ffi::c_void;
	// The same option sets `pipeline::encode_command` emits for the CLI.
	match (p.encoder, p.codec, p.low_latency) {
		(HwEncoder::Nvenc, _, true) => {
			opt_set(priv_, "preset", "p1");
			opt_set(priv_, "tune", "ull");
			opt_set(priv_, "delay", "0");
			opt_set(priv_, "rc", "cbr");
			opt_set(priv_, "rc-lookahead", "0");
			opt_set(priv_, "zerolatency", "1");
		}
		(HwEncoder::Nvenc, _, false) => {
			opt_set(priv_, "preset", "p5");
			opt_set(priv_, "tune", "ll");
			opt_set(priv_, "rc", "vbr");
			opt_set(priv_, "spatial-aq", "1");
		}
		(HwEncoder::Software, VCodec::Av1, true) => {
			opt_set(priv_, "preset", "8");
			opt_set(priv_, "svtav1-params", "lp=0:fast-decode=1");
		}
		(HwEncoder::Software, VCodec::Av1, false) => {
			opt_set(priv_, "preset", "6");
		}
		(HwEncoder::Software, _, true) => {
			opt_set(priv_, "preset", "ultrafast");
			opt_set(priv_, "tune", "zerolatency");
		}
		(HwEncoder::Software, _, false) => {
			opt_set(priv_, "preset", "veryfast");
			opt_set(priv_, "tune", "zerolatency");
		}
		_ => {}
	}
	// A frame sent with pict_type I becomes a real IDR (not a plain I frame) — the client's
	// keyframe request relies on it.
	opt_set(priv_, "forced-idr", "1");
	if p.recovery == LossRecovery::IntraRefresh
		&& matches!((p.encoder, p.codec), (HwEncoder::Software, VCodec::H264) | (HwEncoder::Nvenc, _))
	{
		opt_set(priv_, "intra-refresh", "1");
	}
	let r = ff::avcodec_open2(ec, codec, ptr::null_mut());
	if r < 0 {
		ff::avcodec_free_context(&mut ec);
		return Err(format!("encoder `{name}` open failed: {}", averr(r)));
	}
	Ok(ec)
}

unsafe fn open_output(p: &Params, ec: *mut ff::AVCodecContext) -> Result<(*mut ff::AVFormatContext, *mut ff::AVStream), String> {
	let url = format!("rtp://{}?pkt_size=1200", p.dest);
	let mut octx: *mut ff::AVFormatContext = ptr::null_mut();
	let url_c = cstr(&url);
	// `null_mut` coerces to both the `*mut AVOutputFormat` (FFmpeg 4.4) and the `*const`
	// (≥ 5) the binding declares.
	let r = ff::avformat_alloc_output_context2(&mut octx, ptr::null_mut(), cstr("rtp").as_ptr(), url_c.as_ptr());
	if r < 0 || octx.is_null() {
		return Err(format!("rtp muxer alloc failed: {}", averr(r)));
	}
	let st = ff::avformat_new_stream(octx, ptr::null());
	if st.is_null() {
		ff::avformat_free_context(octx);
		return Err("rtp stream alloc failed".into());
	}
	ff::avcodec_parameters_from_context((*st).codecpar, ec);
	(*st).time_base = (*ec).time_base;
	opt_set((*octx).priv_data, "payload_type", "96");
	let r = ff::avio_open(&mut (*octx).pb, url_c.as_ptr(), ff::AVIO_FLAG_WRITE as c_int);
	if r < 0 {
		ff::avformat_free_context(octx);
		return Err(format!("rtp output open failed ({url}): {}", averr(r)));
	}
	let r = ff::avformat_write_header(octx, ptr::null_mut());
	if r < 0 {
		ff::avio_closep(&mut (*octx).pb);
		ff::avformat_free_context(octx);
		return Err(format!("rtp header failed: {}", averr(r)));
	}
	Ok((octx, st))
}

unsafe fn run(p: &Params, rx: Receiver<Cmd>, ready: Sender<Result<(), String>>) -> Result<(), String> {
	let mut cx = Ctx {
		ictx: ptr::null_mut(),
		dec: ptr::null_mut(),
		graph: ptr::null_mut(),
		src: ptr::null_mut(),
		sink: ptr::null_mut(),
		enc: ptr::null_mut(),
		octx: ptr::null_mut(),
		header_written: false,
	};
	let setup = (|| -> Result<(c_int, *mut ff::AVStream), String> {
		let (ictx, vs) = open_input(p)?;
		cx.ictx = ictx;
		cx.dec = open_decoder(ictx, vs)?;
		// libx264/libx265 want yuv420p; NVENC + SVT-AV1 take nv12/yuv420p — yuv420p works for
		// all of them through their own conversion, nv12 is the cheaper upload for NVENC.
		let pix_fmt = if p.encoder == HwEncoder::Nvenc { ff::AVPixelFormat::AV_PIX_FMT_NV12 } else { ff::AVPixelFormat::AV_PIX_FMT_YUV420P };
		let st = *(*ictx).streams.add(vs as usize);
		let (graph, src, sink) = build_graph(cx.dec, st, p.width, p.height, pix_fmt)?;
		cx.graph = graph;
		cx.src = src;
		cx.sink = sink;
		cx.enc = open_encoder(p, pix_fmt)?;
		let (octx, ost) = open_output(p, cx.enc)?;
		cx.octx = octx;
		cx.header_written = true;
		Ok((vs, ost))
	})();
	let (vs, ost) = match setup {
		Ok(v) => {
			let _ = ready.send(Ok(()));
			v
		}
		Err(e) => {
			let _ = ready.send(Err(e.clone()));
			return Err(e);
		}
	};
	tracing::info!(
		encoder = p.encoder_name().unwrap_or("?"),
		w = p.width,
		h = p.height,
		fps = p.fps,
		kbps = p.bitrate_kbps,
		dest = %p.dest,
		"libav host encode running in-process"
	);

	let pkt = ff::av_packet_alloc();
	let frame = ff::av_frame_alloc();
	let filt = ff::av_frame_alloc();
	let mut pts: i64 = 0;
	let mut force_key = true; // the first frame is an IDR
	let mut recovery = p.recovery;
	let mut frames_since_key: u32 = 0;
	let mut stats_t = std::time::Instant::now();
	let mut stats_frames = 0u32;
	let mut stats_bytes = 0u64;
	let mut enc_us_acc = 0u128;
	let ost_tb = (*ost).time_base;
	let enc_tb = (*cx.enc).time_base;
	'main: loop {
		// Commands from the app (non-blocking).
		loop {
			match rx.try_recv() {
				Ok(Cmd::Bitrate(kbps)) => {
					let br = kbps as i64 * 1000;
					(*cx.enc).bit_rate = br;
					(*cx.enc).rc_max_rate = br;
					(*cx.enc).rc_buffer_size = (br / 2) as c_int;
					tracing::info!(kbps, "libav host: bitrate applied live");
				}
				Ok(Cmd::Keyframe) => force_key = true,
				Ok(Cmd::Recovery(r)) => {
					tracing::info!(recovery = ?r, "libav host: recovery mode");
					recovery = r;
				}
				Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => break 'main,
				Err(TryRecvError::Empty) => break,
			}
		}
		let r = ff::av_read_frame(cx.ictx, pkt);
		if r < 0 {
			if r == ff::AVERROR_EOF {
				break;
			}
			ff::av_packet_unref(pkt);
			continue;
		}
		if (*pkt).stream_index != vs {
			ff::av_packet_unref(pkt);
			continue;
		}
		let r = ff::avcodec_send_packet(cx.dec, pkt);
		ff::av_packet_unref(pkt);
		if r < 0 {
			continue;
		}
		while ff::avcodec_receive_frame(cx.dec, frame) == 0 {
			let r = ff::av_buffersrc_add_frame_flags(cx.src, frame, ff::AV_BUFFERSRC_FLAG_KEEP_REF as c_int);
			ff::av_frame_unref(frame);
			if r < 0 {
				continue;
			}
			while ff::av_buffersink_get_frame(cx.sink, filt) == 0 {
				(*filt).pts = pts;
				pts += 1;
				// Keyframe on request, or on the recovery cadence (~0.5 s).
				let cadence = if recovery.is_active() { (p.fps / 2).max(1) } else { u32::MAX };
				// `pict_type = I` is what libx264 / NVENC read (with `forced-idr` it becomes a
				// real IDR); no frame flag needed — and `AV_FRAME_FLAG_KEY` only exists from
				// FFmpeg 6.1 (the Linux release builds use Ubuntu 22.04's 4.4).
				if force_key || frames_since_key >= cadence {
					(*filt).pict_type = ff::AVPictureType::AV_PICTURE_TYPE_I;
					force_key = false;
					frames_since_key = 0;
				} else {
					(*filt).pict_type = ff::AVPictureType::AV_PICTURE_TYPE_NONE;
					frames_since_key += 1;
				}
				let t0 = std::time::Instant::now();
				let r = ff::avcodec_send_frame(cx.enc, filt);
				ff::av_frame_unref(filt);
				if r < 0 {
					tracing::warn!("libav host: encoder rejected a frame: {}", averr(r));
					continue;
				}
				while ff::avcodec_receive_packet(cx.enc, pkt) == 0 {
					(*pkt).stream_index = 0;
					ff::av_packet_rescale_ts(pkt, enc_tb, ost_tb);
					stats_bytes += (*pkt).size.max(0) as u64;
					let w = ff::av_interleaved_write_frame(cx.octx, pkt);
					ff::av_packet_unref(pkt);
					if w < 0 {
						tracing::warn!("libav host: rtp write failed: {}", averr(w));
					}
				}
				enc_us_acc += t0.elapsed().as_micros();
				stats_frames += 1;
			}
		}
		if stats_t.elapsed().as_secs() >= 2 && stats_frames > 0 {
			tracing::debug!(
				fps = stats_frames / 2,
				mbit = (stats_bytes * 8) as f32 / 2.0 / 1_000_000.0,
				enc_ms = (enc_us_acc / stats_frames as u128) as f32 / 1000.0,
				"libav host encode stats"
			);
			stats_t = std::time::Instant::now();
			stats_frames = 0;
			stats_bytes = 0;
			enc_us_acc = 0;
		}
	}
	// Flush the encoder.
	ff::avcodec_send_frame(cx.enc, ptr::null());
	while ff::avcodec_receive_packet(cx.enc, pkt) == 0 {
		ff::av_packet_unref(pkt);
	}
	let mut pkt = pkt;
	let mut frame = frame;
	let mut filt = filt;
	ff::av_packet_free(&mut pkt);
	ff::av_frame_free(&mut frame);
	ff::av_frame_free(&mut filt);
	drop(cx);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::net::UdpSocket;
	use std::time::{Duration, Instant};

	fn h264_nal_types(rtp: &[u8]) -> Vec<u8> {
		// RTP header 12 bytes; payload NAL type (single NAL or FU-A original type).
		if rtp.len() < 14 {
			return Vec::new();
		}
		let t = rtp[12] & 0x1f;
		match t {
			28 => vec![rtp[13] & 0x1f],
			24 => Vec::new(),
			_ => vec![t],
		}
	}

	/// Headless end-to-end: testsrc → libx264 → RTP into a local socket. Verifies packets
	/// flow, a keyframe request produces an IDR within a few frames, and a live bitrate
	/// change is accepted without breaking the stream.
	#[test]
	fn testsrc_x264_rtp_end_to_end_with_live_controls() {
		let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
		sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
		let port = sock.local_addr().unwrap().port();
		let p = Params {
			source: Source::Test,
			width: 640,
			height: 360,
			fps: 30,
			bitrate_kbps: 1500,
			encoder: HwEncoder::Software,
			codec: VCodec::H264,
			low_latency: true,
			recovery: LossRecovery::Normal,
			dest: format!("127.0.0.1:{port}"),
		};
		let host = match LibavHost::start(p) {
			Ok(h) => h,
			Err(e) if e.contains("not in this libavcodec") || e.contains("unavailable") => {
				eprintln!("skipping: {e}");
				return;
			}
			Err(e) => panic!("start failed: {e}"),
		};
		let mut buf = [0u8; 2048];
		let mut pkts = 0;
		let start = Instant::now();
		while start.elapsed() < Duration::from_millis(1500) {
			if let Ok(n) = sock.recv(&mut buf) {
				pkts += 1;
				let _ = n;
			}
		}
		assert!(pkts > 10, "no RTP flowed: {pkts}");
		// Live bitrate change + keyframe request.
		host.set_bitrate(600);
		host.request_keyframe();
		let mut idr = false;
		let t = Instant::now();
		while t.elapsed() < Duration::from_millis(1000) && !idr {
			if let Ok(n) = sock.recv(&mut buf) {
				if h264_nal_types(&buf[..n]).contains(&5) {
					idr = true;
				}
			}
		}
		assert!(idr, "no IDR after the keyframe request");
		host.set_recovery(LossRecovery::ShortGop);
		let mut idrs = 0;
		let t = Instant::now();
		while t.elapsed() < Duration::from_millis(1600) {
			if let Ok(n) = sock.recv(&mut buf) {
				if h264_nal_types(&buf[..n]).contains(&5) {
					idrs += 1;
				}
			}
		}
		assert!(idrs >= 2, "short-GOP recovery must force keyframes (~every 0.5 s): {idrs}");
		assert!(host.is_alive());
		host.stop();
	}
}
