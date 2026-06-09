//! Linux rkmpp video decode + zero-copy GL present, ported from `pulsar-vidsink.c`. Runs in the
//! SAME process/GL context as the egui overlay so the overlay is a child of the app window
//! (moves/clips/stacks with it — no separate top-level, no compositor desync). Decode on a
//! worker thread → DRM_PRIME mailbox; the main thread imports the newest frame as an
//! `EGL_LINUX_DMA_BUF_EXT` EGLImage → `GL_TEXTURE_EXTERNAL_OES` → draws a letterboxed quad.

use ffmpeg_sys_next as ff;
use std::ffi::{c_void, CString};
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

// --- EGL/GL extension constants (not in khronos-egl's typed API) ---
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const FD_EXT: [i32; 3] = [0x3272, 0x3275, 0x3278];
const OFFSET_EXT: [i32; 3] = [0x3273, 0x3276, 0x3279];
const PITCH_EXT: [i32; 3] = [0x3274, 0x3277, 0x327A];
const MOD_LO_EXT: [i32; 3] = [0x3443, 0x3445, 0x3447];
const MOD_HI_EXT: [i32; 3] = [0x3444, 0x3446, 0x3448];
const EGL_NONE_I: i32 = 0x3038;
pub const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

// --- DRM_PRIME frame queue (decode thread → render thread) ---
struct FramePtr(*mut ff::AVFrame);
unsafe impl Send for FramePtr {}

/// Max frames buffered between decode and present (env `PULSAR_QCAP`, default 3, min 2). With
/// pacing ON the present side drains this FIFO one frame per vsync, so a clumped/bursty arrival
/// (network/receive jitter, a periodic delivery gap) is spread smoothly over the next vsyncs
/// instead of collapsing to one update — Moonlight-style frame pacing. Costs ≤ QCAP-1 frames of
/// latency (~16 ms @120 fps at the default 3); raise it to absorb a larger arrival gap at a
/// proportional latency cost. Pacing OFF keeps only the newest frame each present (lowest latency).
static QCAP: AtomicUsize = AtomicUsize::new(3);

static MBX: Mutex<VecDeque<FramePtr>> = Mutex::new(VecDeque::new());
static STOP: AtomicBool = AtomicBool::new(false);
/// Frame pacing toggle. false = newest-wins (drain all-but-newest each present); true =
/// Moonlight per-vblank metering (present exactly ONE oldest frame per draw/vblank, hold the
/// last frame on underflow, adaptive depth ≤ PACE_CEIL). The startup default is ON (set via
/// `video::set_pace(true)` from linux.rs; `PULSAR_PACE=0` forces off). Flipped live by linux.rs's
/// stdin reader on a `pace 0|1` line.
static PACE: AtomicBool = AtomicBool::new(false);
/// Estimated source frame interval in microseconds (EMA over decoded-frame arrivals). DIAGNOSTIC
/// ONLY now — the Moonlight pacer drives cadence off the real vblank (one frame per draw), NOT
/// this timer (the old EMA-cadence pacer beat-drifted vs the panel). Kept for future HUD use.
static SRC_US: AtomicU64 = AtomicU64::new(16_666);
/// Demuxed video bytes received since the presenter last sampled it (received-stream bitrate,
/// matching mpv's `video-bitrate` notion). The decode thread adds each packet's size; the
/// presenter swaps it to 0 once a second and turns it into Mbit/s for the HUD. Atomic, so the
/// decode thread can poke it without taking the FPS lock the presenter owns.
static VBYTES: AtomicU64 = AtomicU64::new(0);
/// EMA of per-frame decode time in microseconds — how long `avcodec_receive_frame` takes to
/// hand us a decoded frame. Drives the overlay's "Çözme ms" tile (was hardcoded 0 before).
pub static DEC_US: AtomicU64 = AtomicU64::new(0);
pub static FPS: Mutex<[f32; 3]> = Mutex::new([0.0; 3]); // fps, mbit, ms (filled by main on present)

/// Live-toggle frame pacing (called from the stdin reader and the startup default).
pub fn set_pace(on: bool) {
    PACE.store(on, Ordering::Relaxed);
}

/// Current pacing state — the overlay reads this each frame so its toggle highlight tracks
/// whatever set it last (the egui click OR a `pace 0|1` line pushed from the frontend).
pub fn pace_on() -> bool {
    PACE.load(Ordering::Relaxed)
}

/// Soft adaptive pacing ceiling: the MOST frames the Moonlight pacer will buffer when the
/// queue has recently drained (so it can absorb the next burst). Within the QCAP hard cap.
/// Set from `--mode` at startup — game=2 (min latency), remote=3 (smoother). The pacer trims
/// toward 1 frame at steady state and only fills toward this ceiling after a recent drain.
static PACE_CEIL: AtomicUsize = AtomicUsize::new(3);

/// Set the adaptive pacing ceiling (clamped ≥1). Called from linux.rs once `--mode` is known.
pub fn set_pace_ceiling(n: usize) {
    PACE_CEIL.store(n.max(1), Ordering::Relaxed);
}

unsafe extern "C" fn get_drm_prime(_c: *mut ff::AVCodecContext, fmts: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    let mut p = fmts;
    while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME {
            return ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME;
        }
        p = p.add(1);
    }
    *fmts
}

/// Open the SDP (RTP/H.264, H.265 or AV1 — codec read from the SDP), decode with the matching
/// rkmpp HW decoder (or a software fallback for AV1) into DRM_PRIME, publish newest to the mailbox.
pub fn start_decode(sdp_path: &str) {
    if let Some(n) = std::env::var("PULSAR_QCAP").ok().and_then(|v| v.parse::<usize>().ok()) {
        QCAP.store(n.max(2), Ordering::Relaxed);
    }
    let sdp = CString::new(sdp_path).unwrap();
    std::thread::spawn(move || unsafe {
        let mut fmt: *mut ff::AVFormatContext = ptr::null_mut();
        let mut opts: *mut ff::AVDictionary = ptr::null_mut();
        let set = |o: &mut *mut ff::AVDictionary, k: &str, v: &str| {
            let k = CString::new(k).unwrap();
            let v = CString::new(v).unwrap();
            ff::av_dict_set(o, k.as_ptr(), v.as_ptr(), 0);
        };
        set(&mut opts, "protocol_whitelist", "file,rtp,udp");
        set(&mut opts, "fflags", "nobuffer+discardcorrupt");
        set(&mut opts, "flags", "low_delay");
        // RTP reorder window (Moonlight's RtpVideoQueue reassembles fragments in ascending seq
        // before depacketizing). 0 = ZERO tolerance: one reordered UDP fragment corrupts the whole
        // access unit → discardcorrupt drops it → the P-frame reference chain breaks → rkmpp can't
        // decode the next ~10-12 frames until a keyframe → a ~200 ms freeze (the cursor/typing
        // "teleport"). On a 1440p stream each AU spans many fragments, so even a little LAN reorder
        // hits often (measured ~1 lost AU/s). 16 packets of reorder tolerance fixes it with
        // negligible added latency at 60-120 fps. (env PULSAR_REORDER overrides.)
        let reorder = std::env::var("PULSAR_REORDER").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "16".into());
        set(&mut opts, "reorder_queue_size", &reorder);
        // UDP socket SO_RCVBUF headroom. NOT a latency delay-line: with nobuffer+low_delay+
        // max_delay=0 the decode thread reads packets immediately, so the socket stays drained at
        // steady state (RecvQ≈0); this only absorbs transient bursts (an IDR spread over many
        // packets). 4 MiB (env PULSAR_BUFSZ); Pi rmem_max is 16 MiB (sysctl).
        let bufsz = std::env::var("PULSAR_BUFSZ").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "4194304".into());
        set(&mut opts, "buffer_size", &bufsz);
        set(&mut opts, "max_delay", "0");
        if ff::avformat_open_input(&mut fmt, sdp.as_ptr(), ptr::null_mut(), &mut opts) < 0 {
            eprintln!("pulsar-render: avformat_open_input failed");
            return;
        }
        ff::avformat_find_stream_info(fmt, ptr::null_mut());
        let vs = ff::av_find_best_stream(fmt, ff::AVMediaType::AVMEDIA_TYPE_VIDEO, -1, -1, ptr::null_mut(), 0);
        if vs < 0 {
            eprintln!("pulsar-render: no video stream");
            return;
        }
        // Codec-aware decoder selection. The SDP (written by spawn.rs::write_sdp from the active
        // codec) sets the stream's codec_id, so we don't need a separate --codec arg: read it and
        // pick the matching Rockchip MPP hardware decoder. RK3588's MPP does H.264 + HEVC in HW
        // (h264_rkmpp / hevc_rkmpp) but has NO AV1 path, so AV1 falls back to a generic/software
        // decoder (libdav1d if built in, else the native AV1 decoder) — same DRM_PRIME→GL present
        // either way (a non-DRM_PRIME frame is skipped in the receive loop below).
        let st = *(*fmt).streams.add(vs as usize);
        let codec_id = (*(*st).codecpar).codec_id;
        let hw_name = match codec_id {
            ff::AVCodecID::AV_CODEC_ID_HEVC => Some("hevc_rkmpp"),
            ff::AVCodecID::AV_CODEC_ID_AV1 => None, // rkmpp lacks AV1 on RK3588 → software fallback
            _ => Some("h264_rkmpp"), // H.264 (and anything else MPP can take)
        };
        let mut dec = if let Some(name) = hw_name {
            let rk = CString::new(name).unwrap();
            ff::avcodec_find_decoder_by_name(rk.as_ptr())
        } else {
            ptr::null()
        };
        if dec.is_null() {
            // No HW decoder (AV1, or the rkmpp variant isn't present) → generic/software decoder.
            dec = ff::avcodec_find_decoder(codec_id);
        }
        let dc = ff::avcodec_alloc_context3(dec);
        ff::avcodec_parameters_to_context(dc, (*st).codecpar);
        (*dc).get_format = Some(get_drm_prime);
        (*dc).extra_hw_frames = 8;
        if ff::avcodec_open2(dc, dec, ptr::null_mut()) < 0 {
            eprintln!("pulsar-render: avcodec_open2 failed");
            return;
        }
        eprintln!("pulsar-render: decoder={}", std::ffi::CStr::from_ptr((*dec).name).to_string_lossy());
        let pkt = ff::av_packet_alloc();
        let frame = ff::av_frame_alloc();
        let mut last_pub_pace = std::time::Instant::now();
        while !STOP.load(Ordering::Relaxed) {
            let r = ff::av_read_frame(fmt, pkt);
            if r == ff::AVERROR_EOF {
                break;
            }
            if r < 0 {
                ff::av_packet_unref(pkt);
                continue;
            }
            // Time the decode work for this packet (send + frame drain), EMA'd per frame into the
            // overlay's "Çözme ms" tile. rkmpp is async HW: timing avcodec_receive_frame alone
            // reads ~0 (the frame is already decoded), so span the whole send→drain instead.
            let dec_t0 = if (*pkt).stream_index == vs {
                // Tally received video bytes for the HUD bitrate (presenter reads this 1×/s).
                VBYTES.fetch_add((*pkt).size.max(0) as u64, Ordering::Relaxed);
                let t0 = std::time::Instant::now();
                ff::avcodec_send_packet(dc, pkt);
                Some(t0)
            } else {
                None
            };
            ff::av_packet_unref(pkt);
            let mut got_frames: u64 = 0;
            while ff::avcodec_receive_frame(dc, frame) == 0 {
                got_frames += 1;
                if (*frame).format != ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int {
                    ff::av_frame_unref(frame);
                    continue;
                }
                let nf = ff::av_frame_alloc();
                ff::av_frame_move_ref(nf, frame);
                {
                    let mut q = MBX.lock().unwrap();
                    q.push_back(FramePtr(nf));
                    // Bound the queue: if the consumer falls behind, drop the OLDEST frame so
                    // latency stays capped (pacing ON) and newest-wins still works (pacing OFF).
                    while q.len() > QCAP.load(Ordering::Relaxed) {
                        if let Some(old) = q.pop_front() {
                            let mut o = old.0;
                            ff::av_frame_free(&mut o);
                        }
                    }
                }
                // Update the source-rate estimate (EMA) so the pacer presents at the host's
                // frame rate, not the display refresh rate.
                {
                    let pubt = std::time::Instant::now();
                    let dt = pubt.duration_since(last_pub_pace).as_micros() as u64;
                    if (1_000..1_000_000).contains(&dt) {
                        let prev = SRC_US.load(Ordering::Relaxed);
                        SRC_US.store((prev * 7 + dt) / 8, Ordering::Relaxed);
                    }
                    last_pub_pace = pubt;
                }
            }
            if let Some(t0) = dec_t0 {
                if got_frames > 0 {
                    let us = t0.elapsed().as_micros() as u64 / got_frames;
                    let prev = DEC_US.load(Ordering::Relaxed);
                    DEC_US.store(if prev == 0 { us } else { (prev * 7 + us) / 8 }, Ordering::Relaxed);
                }
            }
        }
        let mut pkt = pkt;
        let mut frame = frame;
        ff::av_packet_free(&mut pkt);
        ff::av_frame_free(&mut frame);
    });
}

/// Async-signal-safe stop request: ONLY stores the STOP atomic (no lock, no libav free), so it
/// can be called from a SIGINT/SIGTERM handler. The decode loop checks STOP and exits; the actual
/// drain+free (`stop_decode`) must be done from the main thread after the render loop ends, where
/// MBX is not held — calling stop_decode() from the signal handler can deadlock on the MBX mutex
/// the main thread holds every frame in `Presenter::draw`.
pub fn signal_stop() {
    STOP.store(true, Ordering::Relaxed);
}

pub fn stop_decode() {
    STOP.store(true, Ordering::Relaxed);
    // Drain + free any queued frames so none leak on teardown.
    let mut q = MBX.lock().unwrap();
    while let Some(f) = q.pop_front() {
        let mut o = f.0;
        unsafe { ff::av_frame_free(&mut o) };
    }
}

// --- GL video presenter (runs on the main/GL thread) ---
type EglCreateImage = unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImage = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type GlEglImageTargetTexture2DOES = unsafe extern "C" fn(u32, *mut c_void);

pub struct Presenter {
    dpy: *mut c_void,
    create_image: EglCreateImage,
    destroy_image: EglDestroyImage,
    image_target: GlEglImageTargetTexture2DOES,
    prog: glow::Program,
    vbo: glow::Buffer,
    tex: glow::Texture,
    last: *mut ff::AVFrame, // most-recent frame, RE-PRESENTED every vsync (no flicker on empty)
    last_t: f64,
    frames: u64,
    last_frames: u64,
    last_fresh_t: f64, // time of last FRESH-frame take (present-side cadence probe)
    max_gap_ms: f64,   // worst gap between fresh presents in the current window
    // Moonlight-style adaptive pacer: a rolling window of recent MBX depth samples (one per
    // draw/vblank). The pacer trims the backlog toward 1 frame but tolerates up to PACE_CEIL
    // when the recent min depth shows the queue keeps draining (so it can absorb the next burst).
    depth_hist: [u16; 32],
    depth_idx: usize,
}

const VERT: &str = "attribute vec2 pos;\nattribute vec2 uvin;\nvarying vec2 uv;\nvoid main(){ uv=uvin; gl_Position=vec4(pos,0.0,1.0); }\n";
const FRAG: &str = "#extension GL_OES_EGL_image_external : require\nprecision mediump float;\nvarying vec2 uv;\nuniform samplerExternalOES tex;\nvoid main(){ gl_FragColor=vec4(texture2D(tex,uv).rgb,1.0); }\n";

impl Presenter {
    /// `get_proc` resolves EGL/GL functions (egl.get_proc_address). `dpy` = raw EGLDisplay.
    pub unsafe fn new(gl: &glow::Context, dpy: *mut c_void, get_proc: &dyn Fn(&str) -> *const c_void) -> Self {
        use glow::HasContext;
        let create_image: EglCreateImage = std::mem::transmute(get_proc("eglCreateImageKHR"));
        let destroy_image: EglDestroyImage = std::mem::transmute(get_proc("eglDestroyImageKHR"));
        let image_target: GlEglImageTargetTexture2DOES = std::mem::transmute(get_proc("glEGLImageTargetTexture2DOES"));

        let prog = gl.create_program().unwrap();
        for (ty, src) in [(glow::VERTEX_SHADER, VERT), (glow::FRAGMENT_SHADER, FRAG)] {
            let s = gl.create_shader(ty).unwrap();
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                eprintln!("pulsar-render: video shader: {}", gl.get_shader_info_log(s));
            }
            gl.attach_shader(prog, s);
        }
        gl.bind_attrib_location(prog, 0, "pos");
        gl.bind_attrib_location(prog, 1, "uvin");
        gl.link_program(prog);

        let vbo = gl.create_buffer().unwrap();
        let tex = gl.create_texture().unwrap();
        gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(tex));
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

        Presenter { dpy, create_image, destroy_image, image_target, prog, vbo, tex, last: ptr::null_mut(), last_t: now_s(), frames: 0, last_frames: 0, last_fresh_t: now_s(), max_gap_ms: 0.0, depth_hist: [0; 32], depth_idx: 0 }
    }

    /// Push one MBX-depth sample (one per vblank) into the rolling window and return the MIN
    /// depth over the window — the Moonlight adaptive-drop signal (pacer.cpp:210-242). A recent
    /// min ≤ 1 means the queue keeps draining (jitter / source ≈ display), so the pacer may
    /// buffer up to PACE_CEIL to absorb the next burst; a recent min > 1 means a sustained
    /// backlog (source faster than present), so it trims hard toward 1 frame. The 32-element
    /// scan is O(1)-bounded and runs while the MBX lock is held — kept trivial on purpose.
    fn push_depth(&mut self, depth: usize) -> usize {
        let n = self.depth_hist.len();
        self.depth_hist[self.depth_idx % n] = depth.min(u16::MAX as usize) as u16;
        self.depth_idx = self.depth_idx.wrapping_add(1);
        self.depth_hist.iter().copied().min().unwrap_or(0) as usize
    }

    /// Draw the newest decoded frame letterboxed into a `w`x`h` viewport. Returns true if a frame
    /// was present (so the caller knows video is live).
    pub unsafe fn draw(&mut self, gl: &glow::Context, w: i32, h: i32) -> bool {
        use glow::HasContext;
        // Take the newest decoded frame if there is one; else RE-PRESENT the last frame. The
        // render loop runs at vsync (~display rate) but video arrives slower (e.g. 30 fps), so most
        // presents have no new frame — without re-presenting the last one those frames would clear
        // to black → flicker. A new frame bumps the fps counter; a re-present does not.
        let fresh = {
            let mut q = MBX.lock().unwrap();
            if PACE.load(Ordering::Relaxed) {
                // Pacing ON — Moonlight per-vblank metering (pacer.cpp:201-260). draw() IS the
                // per-vblank tick (egl.swap_buffers + swap_interval=1 in linux.rs blocks to
                // vblank), so present EXACTLY ONE frame per call: a clumped/bursty arrival is
                // metered out one step per refresh instead of collapsing to a single update (the
                // "teleport"). An empty queue returns None → the re-present path below HOLDS the
                // last frame (no black, no jump), bridging a delivery gap smoothly.
                //
                // Cadence is the REAL vblank, not an SRC_US wall-clock timer (the old pacer beat-
                // drifted against the panel and paid a fixed multi-frame prebuffer). Adaptive
                // depth: trim the backlog toward 1 frame (low latency) but tolerate up to
                // PACE_CEIL when the recent min depth shows the queue keeps draining, so the next
                // burst is absorbed without dropping mid-burst.
                let recent_min = self.push_depth(q.len());
                let target = if recent_min <= 1 {
                    PACE_CEIL.load(Ordering::Relaxed).max(1)
                } else {
                    1
                };
                // Catch-up: shed the OLDEST down to `target` BEFORE presenting (free each shed
                // frame) so steady-state latency self-corrects to ~1 frame.
                while q.len() > target {
                    if let Some(old) = q.pop_front() {
                        let mut o = old.0;
                        ff::av_frame_free(&mut o);
                    }
                }
                q.pop_front() // exactly one (oldest) frame this vblank; None ⇒ underflow HOLD
            } else {
                // Pacing OFF: newest-wins — keep only the most recent frame, free the skipped.
                let mut newest = None;
                while let Some(f) = q.pop_front() {
                    if let Some(old) = newest.replace(f) {
                        let mut o = old.0;
                        ff::av_frame_free(&mut o);
                    }
                }
                newest
            }
        };
        if let Some(FramePtr(nf)) = fresh {
            if !self.last.is_null() {
                let mut old = self.last;
                ff::av_frame_free(&mut old);
            }
            self.last = nf;
            self.frames += 1;
            let t = now_s();
            let gap = (t - self.last_fresh_t) * 1000.0;
            if gap > self.max_gap_ms {
                self.max_gap_ms = gap;
            }
            self.last_fresh_t = t;
        }
        let cur = self.last;
        if cur.is_null() {
            return false;
        }

        let vw = (*cur).width;
        let vh = (*cur).height;
        let d = (*cur).data[0] as *const ff::AVDRMFrameDescriptor;
        let layer = &(*d).layers[0];

        let mut a: Vec<i32> = Vec::with_capacity(64);
        a.push(EGL_LINUX_DRM_FOURCC_EXT); a.push(layer.format as i32);
        a.push(EGL_WIDTH); a.push(vw);
        a.push(EGL_HEIGHT); a.push(vh);
        for i in 0..layer.nb_planes as usize {
            let p = &layer.planes[i];
            let o = &(*d).objects[p.object_index as usize];
            a.push(FD_EXT[i]); a.push(o.fd);
            a.push(OFFSET_EXT[i]); a.push(p.offset as i32);
            a.push(PITCH_EXT[i]); a.push(p.pitch as i32);
            if o.format_modifier != DRM_FORMAT_MOD_INVALID {
                a.push(MOD_LO_EXT[i]); a.push((o.format_modifier & 0xFFFF_FFFF) as i32);
                a.push(MOD_HI_EXT[i]); a.push((o.format_modifier >> 32) as i32);
            }
        }
        a.push(EGL_NONE_I);

        let img = (self.create_image)(self.dpy, ptr::null_mut(), EGL_LINUX_DMA_BUF_EXT, ptr::null_mut(), a.as_ptr());
        if img.is_null() {
            let mut cur = cur; // cur is *mut, already a copy of self.last
            ff::av_frame_free(&mut cur);
            self.last = ptr::null_mut(); // critical: drop the dangling pointer so it can't be re-freed/derefed next frame
            return false;
        }

        // Letterbox.
        let rw;
        let rh;
        let r2 = (w as i64 * vh as i64 / vw as i64) as i32;
        if r2 > h {
            rh = h;
            rw = (h as i64 * vw as i64 / vh as i64) as i32;
        } else {
            rw = w;
            rh = r2;
        }
        gl.viewport((w - rw) / 2, (h - rh) / 2, rw, rh);
        gl.use_program(Some(self.prog));
        let quad: [f32; 24] = [
            -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0,
            -1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 0.0,
        ];
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_cast(&quad), glow::STREAM_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
        gl.enable_vertex_attrib_array(0);
        gl.enable_vertex_attrib_array(1);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.tex));
        (self.image_target)(GL_TEXTURE_EXTERNAL_OES, img);
        gl.draw_arrays(glow::TRIANGLES, 0, 6);

        (self.destroy_image)(self.dpy, img);

        let t = now_s();
        if t - self.last_t >= 1.0 {
            let win = t - self.last_t;
            let f = (self.frames - self.last_frames) as f32 / win as f32;
            // Received-stream bitrate: drain the decode thread's byte tally over this window.
            let bytes = VBYTES.swap(0, Ordering::Relaxed);
            let mbit = (bytes as f64 * 8.0 / win / 1e6) as f32;
            *FPS.lock().unwrap() = [f, mbit, self.max_gap_ms as f32];
            self.last_t = t;
            self.last_frames = self.frames;
            self.max_gap_ms = 0.0;
        }
        true
    }
}

fn now_s() -> f64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    t.as_secs_f64()
}

fn bytemuck_cast(f: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(f.as_ptr() as *const u8, f.len() * 4) }
}
