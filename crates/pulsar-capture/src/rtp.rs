//! Minimal RTP/H.264 packetizer (RFC 6184) — byte-for-byte compatible with the
//! Pulsar client depacketizer (`desktop-app/src/lib/h264.ts`) and ffmpeg's RTP
//! demuxer (the Pi native client). This is the pure-Rust replacement for the
//! ffmpeg `-f rtp -payload_type 96` muxer used on the NVENC fast path; emitting
//! it ourselves lets the native capture pipeline skip libavformat for transport.
//!
//! Wire contract (exactly what the client parses — see `h264.ts::push`):
//!
//! * **RTP header** — 12 bytes, no CSRCs, no extension, no padding:
//!   ```text
//!    0                   1                   2                   3
//!    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//!   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!   |V=2|P|X|  CC   |M|     PT=96   |       sequence number         |
//!   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!   |                           timestamp (90 kHz)                  |
//!   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!   |                            SSRC (random)                      |
//!   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!   ```
//!   byte0 = `0x80` (V=2, P=0, X=0, CC=0); byte1 = `M<<7 | 96`. Sequence,
//!   timestamp and SSRC are big-endian. The client masks X (`0x10`) and CC
//!   (`0x0f`) out of byte0, M (`0x80`) out of byte1, so we keep both zero.
//!
//! * **Single-NAL packet** (NAL ≤ MTU): the payload IS the NAL (header byte
//!   `[F|NRI|type]` followed by the RBSP). Client: `t = pl[0] & 0x1f` in 1..=23.
//!
//! * **FU-A** (NAL > MTU, type 28): each fragment is
//!   `[FU indicator][FU header][fragment bytes]` where
//!   - FU indicator = `(orig_hdr & 0xE0) | 28` — carries the original F+NRI bits.
//!   - FU header    = `(S<<7) | (E<<6) | (orig_hdr & 0x1f)` — S on the first
//!     fragment, E on the last, original NAL type in the low 5 bits.
//!   The client rebuilds the NAL header as `(pl[0] & 0x60) | (pl[1] & 0x1f)`, so
//!   the NRI MUST live in the indicator's top bits (it does) — see `h264.ts`.
//!
//! * **Marker bit** is set on the LAST RTP packet of an access unit (the client
//!   calls `emitAU()` only when M=1). Sequence number increments by one per
//!   packet (wraps at 2^16); the 90 kHz timestamp is constant across every
//!   packet of one access unit. PT=96, 90 kHz, SSRC random per sender — matching
//!   the ffmpeg muxer the client was verified against.
//!
//! SPS/PPS are emitted in-band as their own single-NAL packets ahead of the IDR
//! (the host's `dump_extra` already prepends them to the Annex-B keyframe), so
//! this packetizer needs no special parameter-set handling — they fall out of
//! the generic NAL split below.

use rand::Rng;
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Dynamic payload type for H.264 (matches the ffmpeg `-payload_type 96` path
/// and `h264.ts`, which assumes PT 96 / 90 kHz without inspecting the PT field).
const PAYLOAD_TYPE: u8 = 96;

/// RTP fixed header length (no CSRCs, no extension).
const RTP_HEADER_LEN: usize = 12;

/// FU-A NAL unit type (RFC 6184 §5.8).
const NAL_FU_A: u8 = 28;

/// Per-packet UDP payload budget. 1200 keeps a whole RTP packet inside a typical
/// 1500-byte Ethernet MTU with comfortable room for IPv4/IPv6 + UDP headers (the
/// usual streaming default, same ballpark ffmpeg uses). A NAL whose RTP packet
/// would exceed this is FU-A fragmented.
const MTU: usize = 1200;

/// Largest single-NAL payload that still fits the MTU once the RTP header is added.
const MAX_SINGLE_NAL: usize = MTU - RTP_HEADER_LEN;

/// Per-FU-A-fragment NAL payload budget: MTU minus the RTP header (12) minus the
/// 2-byte FU indicator + FU header.
const MAX_FU_PAYLOAD: usize = MTU - RTP_HEADER_LEN - 2;

/// An RTP/H.264 sender bound to one UDP socket, `connect()`ed to the client so
/// `send()` needs no destination per call. Holds the rolling sequence number and
/// the random SSRC for the lifetime of the stream.
pub struct RtpSender {
    socket: UdpSocket,
    ssrc: u32,
    seq: u16,
    /// Scratch buffer reused for every packet to avoid per-packet allocation.
    buf: Vec<u8>,
    // --- Stage-1 packet pacing (inert until `enable_pacing`) ---
    /// When true, spread an AU's packets over ~one frame interval (Sunshine stream.cpp:1407-1572)
    /// so a big IDR doesn't burst-saturate the link (the ~142ms/GOP delivery gap). false = burst.
    pace: bool,
    fps: u32,
    /// Live target bitrate (kbps), shared with the encoder so pacing tracks the (adaptive) bitrate.
    bitrate_kbps: Arc<AtomicU32>,
    /// Carry the send schedule across AUs (Sunshine ratecontrol_next_frame_start).
    next_frame_start: Option<Instant>,
    /// Per-AU pacing state, (re)computed at the top of `send_access_unit`.
    pace_frame_start: Instant,
    pace_per_ms: f64,
    pace_pkts: u32,
    /// High-res sleep for the pacing gate (Windows only; the native sender is Windows-only).
    #[cfg(windows)]
    timer: Option<crate::dxgi::platform::HiResTimer>,
}

impl RtpSender {
    /// Bind an ephemeral UDP socket and connect it to `dest`. After this, every
    /// datagram goes to `dest` (the client's RTP port). A random non-zero SSRC and
    /// a random initial sequence number are chosen per RFC 3550 §5.1.
    pub fn new(dest: SocketAddr) -> io::Result<Self> {
        // Bind a wildcard address of the same family as the destination so the
        // socket can reach it (an IPv4 dest needs an IPv4-bound socket).
        let bind_addr: SocketAddr = if dest.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let socket = UdpSocket::bind(bind_addr)?;
        socket.connect(dest)?;

        // FIX B: enlarge the UDP send buffer. A keyframe/IDR access unit FU-A-fragments into
        // hundreds of ~1200-byte packets; on the default (~64 KiB) send buffer the back-to-back
        // `send()` loop BLOCKS once the buffer fills, stalling the (synchronous) capture+encode
        // thread for ~110 ms every GOP — which reached the Pi as a periodic delivery gap (decode
        // max_gap spikes, Recv-Q~0, 0 loss). A 4 MiB send buffer absorbs the whole burst so the
        // sends return immediately and the kernel drains to the NIC without stalling capture.
        // Best-effort: the OS may clamp the request; never fail construction over a socket tweak.
        let _ = socket2::SockRef::from(&socket).set_send_buffer_size(4 * 1024 * 1024);

        let mut rng = rand::thread_rng();
        // SSRC is any 32-bit value; the client doesn't demux by it but uses it as
        // an opaque per-stream id. Avoid 0 (some stacks treat it specially).
        let ssrc = loop {
            let s: u32 = rng.gen();
            if s != 0 {
                break s;
            }
        };
        // RFC 3550 recommends a random starting sequence number.
        let seq: u16 = rng.gen();

        Ok(Self {
            socket,
            ssrc,
            seq,
            buf: Vec::with_capacity(MTU),
            pace: false,
            fps: 60,
            bitrate_kbps: Arc::new(AtomicU32::new(20_000)),
            next_frame_start: None,
            pace_frame_start: Instant::now(),
            pace_per_ms: 0.0,
            pace_pkts: 0,
            #[cfg(windows)]
            timer: None,
        })
    }

    /// Enable Stage-1 packet pacing (called by `RtpEgress` on the host). `bitrate_kbps` is shared
    /// with the encoder so pacing tracks the live (adaptive) bitrate. Creates the high-res sleep
    /// timer on Windows.
    pub fn enable_pacing(&mut self, fps: u32, bitrate_kbps: Arc<AtomicU32>) {
        self.pace = true;
        self.fps = fps.max(1);
        self.bitrate_kbps = bitrate_kbps;
        #[cfg(windows)]
        {
            self.timer = unsafe { crate::dxgi::platform::HiResTimer::new().ok() };
        }
    }

    /// Pacing gate — called right before each packet's `send`. At every BATCH-th packet of the
    /// current AU, sleep until that packet's scheduled due time so the AU's packets spread over
    /// ~one frame interval instead of bursting. No-op when pacing is off.
    fn pace_gate(&mut self) {
        if !self.pace {
            return;
        }
        const BATCH: u32 = 48; // ~57 KB at MTU 1200 — Sunshine's 64-packet/64 KB send granularity
        if self.pace_pkts > 0 && self.pace_pkts % BATCH == 0 && self.pace_per_ms > 0.0 {
            let due = self.pace_frame_start
                + Duration::from_secs_f64(self.pace_pkts as f64 / self.pace_per_ms / 1000.0);
            let now = Instant::now();
            if now < due {
                let d = due - now;
                #[cfg(windows)]
                {
                    if let Some(t) = self.timer.as_ref() {
                        unsafe { t.sleep_for(d) };
                    }
                }
                #[cfg(not(windows))]
                {
                    std::thread::sleep(d);
                }
            }
        }
        self.pace_pkts = self.pace_pkts.wrapping_add(1);
    }

    /// The SSRC chosen for this stream (exposed for SDP/diagnostics).
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Packetize and send one full access unit. `annexb` is the Annex-B byte
    /// stream for the AU (a `00 00 00 01` / `00 00 01` start code before each
    /// NAL — exactly what the host's `dump_extra`'d encoder emits, with SPS/PPS
    /// preceding IDRs). `pts_90k` is the presentation timestamp in 90 kHz ticks,
    /// the same value for every packet of this AU.
    ///
    /// Each NAL becomes one single-NAL RTP packet if it fits the MTU, else a run
    /// of FU-A fragments. The marker bit is set on the very last packet so the
    /// client decodes the AU as a unit. Returns the number of RTP packets sent.
    pub fn send_access_unit(&mut self, annexb: &[u8], pts_90k: u32) -> io::Result<usize> {
        // Collect the NAL byte-ranges first so we know which one is last (the
        // marker bit must land on the final RTP packet of the AU).
        let nals = split_nals(annexb);
        if nals.is_empty() {
            return Ok(0);
        }
        // Stage-1 pacing setup: rate = max(bitrate-derived, total/frame-interval). The floor
        // (total * fps / 1000 packets-per-ms) caps the spread of any AU at ~one frame interval,
        // so a big IDR is dripped over one frame's air-time (no link saturation) while small
        // P-frames go at the stream rate — and nothing ever adds more than ~1 frame of latency.
        if self.pace {
            let total = count_packets(&nals).max(1);
            let now = Instant::now();
            self.pace_frame_start = self.next_frame_start.map_or(now, |t| t.max(now));
            let bps = (self.bitrate_kbps.load(Ordering::Relaxed).max(1) as f64) * 1000.0;
            let bitrate_per_ms = bps * 1.1 / (MTU as f64 * 8.0) / 1000.0;
            let floor_per_ms = total as f64 * self.fps.max(1) as f64 / 1000.0;
            self.pace_per_ms = bitrate_per_ms.max(floor_per_ms).max(0.001);
            self.pace_pkts = 0;
        }
        let last_idx = nals.len() - 1;

        let mut sent = 0;
        for (i, nal) in nals.iter().enumerate() {
            if nal.is_empty() {
                continue;
            }
            let is_last_nal = i == last_idx;
            if nal.len() <= MAX_SINGLE_NAL {
                // Single-NAL: payload is the NAL verbatim. Marker on the AU's last NAL.
                self.send_packet(nal, pts_90k, is_last_nal)?;
                sent += 1;
            } else {
                sent += self.send_fu_a(nal, pts_90k, is_last_nal)?;
            }
        }
        // Carry the schedule into the next AU so back-to-back frames keep a smooth wire cadence
        // (Sunshine ratecontrol_next_frame_start, stream.cpp:1570-1572).
        if self.pace && self.pace_per_ms > 0.0 {
            self.next_frame_start = Some(
                self.pace_frame_start
                    + Duration::from_secs_f64(self.pace_pkts as f64 / self.pace_per_ms / 1000.0),
            );
        }
        Ok(sent)
    }

    /// FU-A fragment a single NAL that exceeds the MTU. The original NAL header
    /// byte (`nal[0]`) supplies F+NRI (top 3 bits) and the type (low 5 bits); the
    /// RBSP `nal[1..]` is sliced into MTU-sized fragments. Marker on the final
    /// fragment only if this is the AU's last NAL.
    fn send_fu_a(&mut self, nal: &[u8], pts_90k: u32, is_last_nal: bool) -> io::Result<usize> {
        let hdr = nal[0];
        let fu_indicator = (hdr & 0xE0) | NAL_FU_A; // F + NRI + type=28
        let nal_type = hdr & 0x1F;
        let rbsp = &nal[1..]; // FU-A does NOT resend the original header byte

        let total = rbsp.len();
        let mut off = 0;
        let mut sent = 0;
        while off < total {
            let end = (off + MAX_FU_PAYLOAD).min(total);
            let is_first_frag = off == 0;
            let is_last_frag = end == total;

            let mut fu_header = nal_type;
            if is_first_frag {
                fu_header |= 0x80; // S bit
            }
            if is_last_frag {
                fu_header |= 0x40; // E bit
            }

            // Marker only on the last packet of the AU = last fragment of last NAL.
            let marker = is_last_nal && is_last_frag;

            self.buf.clear();
            self.write_rtp_header(pts_90k, marker);
            self.buf.push(fu_indicator);
            self.buf.push(fu_header);
            self.buf.extend_from_slice(&rbsp[off..end]);
            self.pace_gate();
            self.socket.send(&self.buf)?;
            self.seq = self.seq.wrapping_add(1);

            off = end;
            sent += 1;
        }
        Ok(sent)
    }

    /// Emit one single-NAL RTP packet whose payload is `payload` (a whole NAL).
    fn send_packet(&mut self, payload: &[u8], pts_90k: u32, marker: bool) -> io::Result<()> {
        self.buf.clear();
        self.write_rtp_header(pts_90k, marker);
        self.buf.extend_from_slice(payload);
        self.pace_gate();
        self.socket.send(&self.buf)?;
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }

    /// Append the 12-byte RTP fixed header to `self.buf`. V=2, P=0, X=0, CC=0;
    /// marker + PT in byte 1; big-endian sequence, timestamp and SSRC.
    fn write_rtp_header(&mut self, pts_90k: u32, marker: bool) {
        let byte0 = 0x80; // version 2, no padding/extension/CSRC
        let byte1 = if marker {
            0x80 | PAYLOAD_TYPE
        } else {
            PAYLOAD_TYPE
        };
        self.buf.push(byte0);
        self.buf.push(byte1);
        self.buf.extend_from_slice(&self.seq.to_be_bytes());
        self.buf.extend_from_slice(&pts_90k.to_be_bytes());
        self.buf.extend_from_slice(&self.ssrc.to_be_bytes());
    }
}

// ===========================================================================
// RtpEgress — decouple the blocking UDP send from the capture+encode thread.
// ===========================================================================
//
// PROBLEM (the opi5 "periodic ~110 ms freeze → jump"): `Encoder::submit` used to call
// `RtpSender::send_access_unit` INLINE on the capture+encode thread. An IDR/keyframe AU
// FU-A-fragments into hundreds of ~1200-byte packets; the back-to-back blocking
// `UdpSocket::send` loop wedges that thread until the kernel send buffer drains (~110 ms
// every GOP, and the OS may clamp SO_SNDBUF so the 4 MiB request doesn't always help), so
// frame PRODUCTION halts. The Pi sees a periodic delivery gap (decode max_gap spikes, 0
// loss, empty recv-Q) and its newest-wins presenter collapses the post-gap catch-up burst
// into one visual jump (the cursor teleport / typing-in-bursts).
//
// FIX (Sunshine's model — `videoBroadcastThread` + `safe::queue_t`, _ref/sunshine
// stream.cpp:1272/1297 + thread_safe.h:249-273): the encode thread only ENQUEUES the owned
// Annex-B AU (a memcpy + a brief lock) and returns; a dedicated `pulsar-rtp-send` thread is
// the SOLE owner of the `RtpSender` and runs the blocking send loop. A slow/wedged send can
// no longer back-pressure NVENC. The bounded mailbox drops the stale backlog on overflow
// (newest-wins — exactly Sunshine's `safe::queue_t` clear-on-overflow), so the producer
// never blocks and the queue never grows unbounded; a dropped AU recovers on the next frame
// / the multi-second safety IDR. A single consumer over a FIFO keeps the wire sequence
// numbers monotonic, and the bytes are produced by the SAME unchanged `RtpSender`, so the
// client depacketizer (`src/lib/h264.ts`, the Pi ffmpeg demuxer) is untouched.

/// One encoded access unit handed encode-thread → sender-thread (owned copy of the bytes,
/// since the NVENC locked bitstream is only valid until Unlock).
struct AuMsg {
    annexb: Vec<u8>,
    pts_90k: u32,
}

/// Bounded producer→consumer mailbox (mutex + condvar + `VecDeque`), mirroring Sunshine's
/// `safe::queue_t`: enqueue only emplaces (never waits on the network), and on overflow it
/// drops the stale backlog and keeps the newest AU.
struct AuMailbox {
    state: Mutex<MailState>,
    cv: Condvar,
    cap: usize,
}
struct MailState {
    q: VecDeque<AuMsg>,
    stop: bool,
}

/// Decoupled RTP egress: owns the dedicated `pulsar-rtp-send` thread that is the sole owner
/// of the inner `RtpSender`. Replaces the inline `RtpSender` on `Encoder` so the per-GOP
/// ~110 ms encode-thread stall is gone. `Drop` closes the mailbox and joins the thread so the
/// socket + thread never orphan.
pub struct RtpEgress {
    mb: Arc<AuMailbox>,
    ssrc: u32,
    join: Option<JoinHandle<()>>,
    /// A/B knob: `PULSAR_RTP_INLINE=1` keeps the OLD behavior (send on the calling/encode
    /// thread) to reproduce the stall on demand. `Some` ⇒ inline, no sender thread spawned.
    inline: Option<Mutex<RtpSender>>,
}

impl RtpEgress {
    /// Bind the RTP socket (unchanged `RtpSender::new`) and, unless `PULSAR_RTP_INLINE=1`,
    /// spawn the sender thread that drains the mailbox. `PULSAR_RTP_QCAP` overrides the
    /// mailbox depth (default 16 access units).
    pub fn spawn(dest: SocketAddr, fps: u32, bitrate_kbps: Arc<AtomicU32>) -> io::Result<Self> {
        let mut sender = RtpSender::new(dest)?;
        // Bound a blocked send so a wedged socket can never hang teardown: the sender thread
        // checks `stop` between AUs, and the write timeout caps any in-flight send. 250 ms is
        // far longer than any healthy AU send yet imperceptible on stop. On timeout the rest of
        // that AU is dropped (returned as an Err the sender loop ignores).
        let _ = sender
            .socket
            .set_write_timeout(Some(Duration::from_millis(250)));
        let ssrc = sender.ssrc();

        // A/B: inline mode reproduces the old inline-send stall (no sender thread).
        if std::env::var("PULSAR_RTP_INLINE")
            .map(|v| v == "1" || v == "on" || v == "true")
            .unwrap_or(false)
        {
            return Ok(Self {
                mb: Arc::new(AuMailbox {
                    state: Mutex::new(MailState {
                        q: VecDeque::new(),
                        stop: false,
                    }),
                    cv: Condvar::new(),
                    cap: 1,
                }),
                ssrc,
                join: None,
                inline: Some(Mutex::new(sender)),
            });
        }

        // Stage-1 packet pacing: ON by default on the threaded path (PULSAR_RTP_PACE=0 disables
        // for A/B). Spreads each AU's packets so a big IDR doesn't burst-saturate the link.
        if std::env::var("PULSAR_RTP_PACE")
            .map(|v| v != "0")
            .unwrap_or(true)
        {
            sender.enable_pacing(fps, bitrate_kbps);
        }
        let cap = std::env::var("PULSAR_RTP_QCAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(16);
        let mb = Arc::new(AuMailbox {
            state: Mutex::new(MailState {
                q: VecDeque::with_capacity(cap),
                stop: false,
            }),
            cv: Condvar::new(),
            cap,
        });
        let mb_thread = mb.clone();
        let join = std::thread::Builder::new()
            .name("pulsar-rtp-send".into())
            .spawn(move || {
                let mut sender = sender;
                loop {
                    // Wait for the next AU (or a stop). Holding the lock only across the
                    // pop/wait — never across the send below — so the producer never blocks
                    // on the network.
                    let msg = {
                        let mut st = mb_thread.state.lock().unwrap();
                        loop {
                            if let Some(m) = st.q.pop_front() {
                                break Some(m);
                            }
                            if st.stop {
                                break None;
                            }
                            st = mb_thread.cv.wait(st).unwrap();
                        }
                    };
                    match msg {
                        // An Err (incl. a write-timeout on a saturated socket) just drops the
                        // rest of THIS AU and moves on — never stalls, never reorders.
                        Some(m) => {
                            let _ = sender.send_access_unit(&m.annexb, m.pts_90k);
                        }
                        None => break,
                    }
                }
            })?;
        Ok(Self {
            mb,
            ssrc,
            join: Some(join),
            inline: None,
        })
    }

    /// The stream SSRC (cached at construction so SDP/diagnostics need not cross the thread).
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Hand one access unit to the sender thread. NON-BLOCKING: copies `annexb` (valid only
    /// until the caller unlocks the NVENC bitstream) into an owned `Vec`, enqueues it, and
    /// returns. On overflow the stale backlog is dropped (newest-wins) so the encode thread is
    /// never back-pressured. In `PULSAR_RTP_INLINE=1` mode it sends synchronously here (the old
    /// stall-prone path, for A/B).
    pub fn send_access_unit(&self, annexb: &[u8], pts_90k: u32) {
        if let Some(inline) = &self.inline {
            let mut s = inline.lock().unwrap();
            let _ = s.send_access_unit(annexb, pts_90k);
            return;
        }
        let msg = AuMsg {
            annexb: annexb.to_vec(),
            pts_90k,
        };
        {
            let mut st = self.mb.state.lock().unwrap();
            if st.q.len() >= self.mb.cap {
                // Sunshine `safe::queue_t` overflow policy: drop the stale backlog, keep newest.
                st.q.clear();
            }
            st.q.push_back(msg);
        }
        self.mb.cv.notify_one();
    }
}

impl Drop for RtpEgress {
    fn drop(&mut self) {
        // Wake the sender thread (if any) and join it so the socket + thread never orphan.
        {
            let mut st = self.mb.state.lock().unwrap();
            st.stop = true;
        }
        self.mb.cv.notify_all();
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// Count how many RTP packets `send_access_unit` will emit for these NALs (single-NAL = 1 each,
/// else `ceil(rbsp_len / MAX_FU_PAYLOAD)` FU-A fragments). Used to clamp the pacing rate so an AU
/// is never spread beyond ~one frame interval.
fn count_packets(nals: &[&[u8]]) -> u32 {
    nals.iter()
        .map(|nal| {
            if nal.is_empty() {
                0
            } else if nal.len() <= MAX_SINGLE_NAL {
                1
            } else {
                // FU-A fragments the RBSP (nal[1..]); ceil-divide by the per-fragment budget.
                ((nal.len() - 1 + MAX_FU_PAYLOAD - 1) / MAX_FU_PAYLOAD) as u32
            }
        })
        .sum()
}

/// Split an Annex-B byte stream into its NAL units (without the start codes).
/// Recognizes both 4-byte (`00 00 00 01`) and 3-byte (`00 00 01`) start codes,
/// as emitted by the encoder/`dump_extra`. Trailing zero bytes that some encoders
/// append are trimmed off each NAL.
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let n = data.len();

    // Find the offset of the first start code; bytes before it (if any) are skipped.
    let mut i = match find_start_code(data, 0) {
        Some((sc_start, _)) if sc_start == 0 => 0,
        Some((_, sc_end)) => sc_end,
        None => return nals, // no start code at all → nothing to send
    };
    // `i` now points at (or just before) the first start code. Normalize: advance
    // past the leading start code to the first NAL byte.
    if let Some((sc_start, sc_end)) = find_start_code(data, i) {
        if sc_start == i {
            i = sc_end;
        }
    }

    while i < n {
        // The NAL runs from `i` up to the next start code (or end of buffer).
        let (nal_end, next) = match find_start_code(data, i) {
            Some((sc_start, sc_end)) => (sc_start, sc_end),
            None => (n, n),
        };
        // Trim trailing zero bytes that belong to the next start code's run-in.
        let mut end = nal_end;
        while end > i && data[end - 1] == 0x00 {
            end -= 1;
        }
        if end > i {
            nals.push(&data[i..end]);
        }
        i = next;
    }
    nals
}

/// Find the next Annex-B start code at or after `from`, returning
/// `(start_index, byte_after_start_code)`. Matches `00 00 01` and `00 00 00 01`;
/// a 4-byte code is reported with its full length consumed.
fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let n = data.len();
    let mut i = from;
    while i + 3 <= n {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            // Prefer the 4-byte form if preceded by an extra zero.
            if i > 0 && data[i - 1] == 0 {
                return Some((i - 1, i + 3));
            }
            return Some((i, i + 3));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the Annex-B for a NAL of `type`/`nri` with `len` RBSP bytes, prefixed
    /// by a 4-byte start code.
    fn annexb_nal(nal_type: u8, nri: u8, rbsp_len: usize) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1];
        v.push((nri << 5) | nal_type); // [F=0|NRI|type]
        v.extend(std::iter::repeat(0xAB).take(rbsp_len));
        v
    }

    fn parse_header(pkt: &[u8]) -> (bool, u8, u16, u32, u32) {
        let marker = pkt[1] & 0x80 != 0;
        let pt = pkt[1] & 0x7f;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let ts = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
        let ssrc = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);
        (marker, pt, seq, ts, ssrc)
    }

    /// Reassemble the NALs a depacketizer (mirroring `h264.ts`) would produce from
    /// a list of RTP packets, asserting marker/PT/timestamp invariants.
    fn depacketize(pkts: &[Vec<u8>], expect_ts: u32) -> Vec<Vec<u8>> {
        let mut nals: Vec<Vec<u8>> = Vec::new();
        let mut fu: Option<Vec<u8>> = None;
        let mut last_seq: Option<u16> = None;
        let mut saw_marker = false;
        for pkt in pkts {
            assert!(pkt.len() >= RTP_HEADER_LEN);
            let (marker, pt, seq, ts, _ssrc) = parse_header(pkt);
            assert_eq!(pt, PAYLOAD_TYPE, "payload type must be 96");
            assert_eq!(ts, expect_ts, "timestamp constant across the AU");
            if let Some(prev) = last_seq {
                assert_eq!(seq, prev.wrapping_add(1), "sequence increments by 1");
            }
            last_seq = Some(seq);
            if marker {
                saw_marker = true;
            }

            let pl = &pkt[RTP_HEADER_LEN..];
            let t = pl[0] & 0x1f;
            if (1..=23).contains(&t) {
                nals.push(pl.to_vec());
            } else if t == NAL_FU_A {
                let fh = pl[1];
                let start = fh & 0x80 != 0;
                let endb = fh & 0x40 != 0;
                let orig_type = fh & 0x1f;
                let nri = pl[0] & 0x60;
                if start {
                    fu = Some(vec![nri | orig_type]); // reconstruct NAL header
                }
                if let Some(acc) = fu.as_mut() {
                    acc.extend_from_slice(&pl[2..]);
                    if endb {
                        nals.push(fu.take().unwrap());
                    }
                }
            }
        }
        assert!(saw_marker, "exactly one packet must carry the marker bit");
        // Marker must be on the LAST packet only.
        for (i, pkt) in pkts.iter().enumerate() {
            let m = pkt[1] & 0x80 != 0;
            assert_eq!(m, i == pkts.len() - 1, "marker only on the final packet");
        }
        nals
    }

    /// Capture what `send_access_unit` puts on the wire by reading from a paired
    /// loopback socket.
    fn roundtrip(annexb: &[u8], pts: u32) -> Vec<Vec<u8>> {
        let recv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let dest = recv.local_addr().unwrap();
        let mut sender = RtpSender::new(dest).unwrap();
        let sent = sender.send_access_unit(annexb, pts).unwrap();

        let mut pkts = Vec::new();
        let mut buf = [0u8; 2048];
        recv.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        for _ in 0..sent {
            let n = recv.recv(&mut buf).unwrap();
            pkts.push(buf[..n].to_vec());
        }
        pkts
    }

    #[test]
    fn single_nal_one_packet() {
        // A small NAL (type 1, NRI 2) → exactly one single-NAL packet, marker set.
        let au = annexb_nal(1, 2, 100);
        let pkts = roundtrip(&au, 90_000);
        assert_eq!(pkts.len(), 1);
        let nals = depacketize(&pkts, 90_000);
        assert_eq!(nals.len(), 1);
        // Reassembled NAL equals the original (header + RBSP).
        assert_eq!(nals[0], &au[4..]);
        // Header byte preserved exactly: F=0, NRI=2, type=1.
        assert_eq!(nals[0][0], (2 << 5) | 1);
    }

    #[test]
    fn large_nal_fragments_fu_a() {
        // A NAL bigger than the MTU (type 5 IDR, NRI 3) → multiple FU-A fragments.
        let au = annexb_nal(5, 3, MTU * 3);
        let pkts = roundtrip(&au, 12345);
        assert!(pkts.len() > 3, "should fragment into several packets");
        let nals = depacketize(&pkts, 12345);
        assert_eq!(nals.len(), 1);
        // FU-A reconstruction restores the exact original NAL (header + RBSP).
        assert_eq!(nals[0], &au[4..]);
        assert_eq!(nals[0][0], (3 << 5) | 5); // NRI=3, type=5 survived round-trip
    }

    #[test]
    fn multi_nal_access_unit_marker_on_last() {
        // SPS(7) + PPS(8) + IDR(5, large) — the keyframe AU shape `dump_extra` emits.
        let mut au = annexb_nal(7, 3, 20);
        au.extend(annexb_nal(8, 3, 8));
        au.extend(annexb_nal(5, 3, MTU * 2));
        let pkts = roundtrip(&au, 7777);
        let nals = depacketize(&pkts, 7777);
        assert_eq!(nals.len(), 3, "SPS, PPS, IDR all delivered");
        assert_eq!(nals[0][0] & 0x1f, 7);
        assert_eq!(nals[1][0] & 0x1f, 8);
        assert_eq!(nals[2][0] & 0x1f, 5);
    }

    #[test]
    fn three_byte_start_code_is_split() {
        // Mixed 4-byte then 3-byte start codes.
        let mut au = vec![0, 0, 0, 1, (3 << 5) | 7, 0xAA, 0xBB];
        au.extend_from_slice(&[0, 0, 1, (2 << 5) | 1, 0xCC]);
        let pkts = roundtrip(&au, 1);
        let nals = depacketize(&pkts, 1);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[(3 << 5) | 7, 0xAA, 0xBB]);
        assert_eq!(nals[1], &[(2 << 5) | 1, 0xCC]);
    }

    #[test]
    fn ssrc_is_random_nonzero_and_stable() {
        let recv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut s = RtpSender::new(recv.local_addr().unwrap()).unwrap();
        let ssrc = s.ssrc();
        assert_ne!(ssrc, 0);
        // SSRC is constant across packets in a stream.
        s.send_access_unit(&annexb_nal(1, 2, 10), 0).unwrap();
        let mut buf = [0u8; 2048];
        let n = recv.recv(&mut buf).unwrap();
        let (_, _, _, _, wire_ssrc) = parse_header(&buf[..n]);
        assert_eq!(wire_ssrc, ssrc);
    }

    #[test]
    fn egress_thread_delivers_identical_bytes() {
        // The decoupled `RtpEgress` (sender thread) must put the SAME bytes on the wire as a
        // direct `RtpSender` — the wire contract is unchanged, only WHERE the send runs moved.
        let recv = UdpSocket::bind("127.0.0.1:0").unwrap();
        recv.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let dest = recv.local_addr().unwrap();
        let au = annexb_nal(1, 2, 100); // single-NAL → exactly one packet, marker set
        let egress = RtpEgress::spawn(dest, 60, Arc::new(AtomicU32::new(20_000))).unwrap();
        egress.send_access_unit(&au, 4242);
        let mut buf = [0u8; 2048];
        let m = recv.recv(&mut buf).unwrap(); // blocks until the sender thread delivers it
        let nals = depacketize(&[buf[..m].to_vec()], 4242);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], &au[4..]);
        drop(egress); // closes the mailbox + joins the sender thread (no orphan)
    }
}
