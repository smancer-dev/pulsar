//! Minimal RTP video depacketizer (H.264 / H.265 / AV1) → decoder access units.
//!
//! pulsar-core carries video as RTP datagrams (RFC 6184 for H.264, RFC 7798 for
//! H.265, RFC 9043 for AV1), either as a direct UDP flow or wrapped in the encrypted
//! session (`service::media`, tag `TAG_VIDEO`). The desktop client depacketizes in JS
//! (`src/lib/h264.ts`) or lets ffmpeg/mpv do it; the mobile client has neither, so
//! it reassembles NAL units / OBUs here and feeds the resulting access units to an
//! Android `MediaCodec`.
//!
//! H.264/H.265: single NAL, STAP-A (H.264 agg) / AP (H.265 agg), FU-A (H.264 frag)
//! / FU (H.265 frag). DONL is assumed absent (the host's ffmpeg RTP muxer uses
//! `sprop-max-don-diff=0`).
//!
//! AV1: RFC 9043 OBU depacketizer. The 1-byte aggregation header encodes Z (first OBU
//! is a continuation fragment), Y (last OBU is fragmented, continues in next packet),
//! W (3-bit count of OBU elements; 0 means the number is not given / last OBU runs to
//! end), N (new coded video sequence), and reserved bits. Each OBU element is
//! length-prefixed with a variable-length LEB128 integer *except* the last one in a
//! packet when W≠0 (its length is inferred from the remaining bytes). The reassembled
//! output is a sequence of *open_bitstream_unit()* bytes (OBU header preserved, no
//! additional framing) concatenated into one temporal unit, suitable for feeding to an
//! Android `MediaCodec` configured with `"video/av01"`.
//!
//! Access-unit (temporal-unit) boundaries for all codecs: the RTP marker bit signals
//! the last packet of a frame/TU; a defensive timestamp-change flush fires when the
//! marker was lost.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Annex-B start code prepended to every emitted H.264/H.265 NAL unit.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    /// AV1 per RFC 9043. Output is a raw OBU byte-stream temporal unit.
    Av1,
}

/// Return the RTP payload (past the 12-byte header + CSRC list + extension, minus
/// any padding), e.g. the Opus packet for an audio datagram. `None` if the packet
/// is malformed or not RTP v2.
///
/// Strictness matters here: one structurally-invalid "opus packet" reaching Android's
/// `c2.android.opus.decoder` is a FATAL codec error (OPUS_INVALID_PACKET → C2 err 14),
/// so padding bytes must be stripped and non-RTP datagrams rejected.
pub fn rtp_payload(pkt: &[u8]) -> Option<&[u8]> {
    if pkt.len() < 12 {
        return None;
    }
    if pkt[0] >> 6 != 2 {
        return None; // not RTP version 2
    }
    let cc = (pkt[0] & 0x0F) as usize;
    let ext = pkt[0] & 0x10 != 0;
    let mut off = 12 + cc * 4;
    if ext {
        if pkt.len() < off + 4 {
            return None;
        }
        let words = u16::from_be_bytes([pkt[off + 2], pkt[off + 3]]) as usize;
        off += 4 + words * 4;
    }
    // Padding (P bit): the last byte is the pad count, including itself.
    let mut end = pkt.len();
    if pkt[0] & 0x20 != 0 {
        let pad = pkt[end - 1] as usize;
        if pad == 0 || pad > end.saturating_sub(off) {
            return None;
        }
        end -= pad;
    }
    if end <= off {
        return None;
    }
    Some(&pkt[off..end])
}

/// Reassembles RTP video payloads into whole access units.
///
/// - H.264/H.265: outputs an Annex-B byte stream (each NAL prefixed with `[0,0,0,1]`).
/// - AV1: outputs a raw OBU temporal-unit byte stream (concatenated whole OBUs as
///   produced by the encoder, no additional framing). Android `MediaCodec` with
///   `"video/av01"` accepts this directly.
pub struct Depacketizer {
    codec: Codec,
    /// NAL units / OBUs of the access unit being built.
    ///
    /// H.264/H.265: each unit is start-code prefixed.
    /// AV1: raw OBU bytes concatenated; whole OBUs only (fragments reassembled first).
    au: Vec<u8>,
    /// In-progress fragmented NAL (FU-A / FU) without its start code yet.
    /// Also used for the AV1 leading-fragment OBU body while Z or Y is in flight.
    fu: Option<Vec<u8>>,
    last_ts: Option<u32>,
    // ---- AV1 state (RFC 9043) ----
    /// Header byte of the OBU currently being fragment-reassembled (for AV1 Z/Y path).
    /// Stores the original OBU header byte so we can prepend it to the reassembled body.
    av1_frag_obu_hdr: u8,
    /// When true the *next* packet's first OBU element is the continuation of `fu`.
    av1_frag_pending: bool,
    /// A24: a recycled access-unit buffer swapped in on emit (instead of allocating
    /// a fresh empty `Vec`) so the next AU reuses the allocation. Empty until a
    /// caller hands a finished AU back via [`Depacketizer::recycle_au`].
    spare: Vec<u8>,
}

impl Depacketizer {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            au: Vec::new(),
            fu: None,
            last_ts: None,
            av1_frag_obu_hdr: 0,
            av1_frag_pending: false,
            spare: Vec::new(),
        }
    }

    /// Take the finished access unit, swapping in a recycled spare buffer instead of
    /// a fresh empty `Vec` (A24) so the next AU reuses the allocation. Behaves
    /// exactly like `mem::take(&mut self.au)` when no spare has been recycled yet.
    fn take_au(&mut self) -> Vec<u8> {
        std::mem::replace(&mut self.au, std::mem::take(&mut self.spare))
    }

    /// Return a consumed access-unit buffer for reuse as the next AU's backing store
    /// (A24). Cleared and kept as the spare. Optional: callers that don't recycle
    /// simply pay one allocation per AU (the previous behaviour).
    pub fn recycle_au(&mut self, mut au: Vec<u8>) {
        au.clear();
        self.spare = au;
    }

    /// Feed one full RTP datagram (12-byte header included). Returns a complete
    /// Annex-B access unit when a frame boundary is reached (marker bit, or a
    /// timestamp change that flushes the previous frame), else `None`.
    pub fn push(&mut self, pkt: &[u8]) -> Option<Vec<u8>> {
        if pkt.len() < 12 {
            return None;
        }
        if pkt[0] >> 6 != 2 {
            return None; // not RTP v2 — a stray datagram, not our video stream
        }
        let cc = (pkt[0] & 0x0F) as usize;
        let ext = pkt[0] & 0x10 != 0;
        let marker = pkt[1] & 0x80 != 0;
        let ts = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);

        // Stale-retransmit guard: a NACK-retransmitted packet arrives ≥1 RTT late —
        // long after its frame was flushed (a 60 fps frame lives ~16 ms). Feeding it
        // anyway bounced `last_ts` backward, so the defensive ts-flush TORE both the
        // in-progress AU and the retransmit itself into partial AUs (observed live:
        // ~2-3x inflated AU counts + heavy visible corruption whenever the link had
        // loss). Drop anything older than the newest timestamp; a huge backward jump
        // (> ~10 s of the 90 kHz clock) is an encoder restart on a fresh random
        // timestamp base → accept and resync instead.
        if let Some(prev) = self.last_ts {
            let d = ts.wrapping_sub(prev) as i32;
            if d < 0 && d > -900_000 {
                return None;
            }
        }

        let mut off = 12 + cc * 4;
        if ext {
            if pkt.len() < off + 4 {
                return None;
            }
            let words = u16::from_be_bytes([pkt[off + 2], pkt[off + 3]]) as usize;
            off += 4 + words * 4;
        }
        // Padding (P bit): the last byte is the pad count, including itself.
        let mut end = pkt.len();
        if pkt[0] & 0x20 != 0 {
            let pad = pkt[end - 1] as usize;
            if pad == 0 || pad > end.saturating_sub(off) {
                return None;
            }
            end -= pad;
        }
        if end <= off {
            return None;
        }
        let payload = &pkt[off..end];

        // Defensive: a new timestamp with an unfinished AU means the previous
        // frame's marker was lost — flush it before starting the new frame.
        let mut out = None;
        if self.last_ts.is_some_and(|prev| prev != ts) && !self.au.is_empty() {
            out = Some(self.take_au()); // A24: reuse a recycled buffer
            self.fu = None;
        }
        self.last_ts = Some(ts);

        match self.codec {
            Codec::H264 => self.process_h264(payload),
            Codec::H265 => self.process_h265(payload),
            Codec::Av1 => self.process_av1(payload),
        }

        if marker && !self.au.is_empty() && out.is_none() {
            out = Some(self.take_au()); // A24: reuse a recycled buffer
        }
        out
    }

    fn emit_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        self.au.extend_from_slice(&START_CODE);
        self.au.extend_from_slice(nal);
    }

    fn process_h264(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        match payload[0] & 0x1F {
            1..=23 => self.emit_nal(payload), // single NAL unit
            24 => {
                // STAP-A: [NAL hdr][ (size:u16)(NAL) ]*
                let mut i = 1;
                while i + 2 <= payload.len() {
                    let size = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                    i += 2;
                    if i + size > payload.len() {
                        break;
                    }
                    let nal = payload[i..i + size].to_vec();
                    self.emit_nal(&nal);
                    i += size;
                }
            }
            28 => {
                // FU-A: [FU indicator][FU header][fragment]
                if payload.len() < 2 {
                    return;
                }
                let (ind, hdr) = (payload[0], payload[1]);
                let frag = &payload[2..];
                if hdr & 0x80 != 0 {
                    // start: rebuild NAL header = F|NRI from indicator + type from FU header
                    let mut nal = Vec::with_capacity(1 + frag.len());
                    nal.push((ind & 0xE0) | (hdr & 0x1F));
                    nal.extend_from_slice(frag);
                    self.fu = Some(nal);
                } else if let Some(buf) = self.fu.as_mut() {
                    buf.extend_from_slice(frag);
                }
                if hdr & 0x40 != 0 {
                    if let Some(nal) = self.fu.take() {
                        self.emit_nal(&nal);
                    }
                }
            }
            _ => {} // STAP-B / MTAP / FU-B: unused by the host's ffmpeg muxer
        }
    }

    // -------------------------------------------------------------------------
    // AV1 — RFC 9043
    // -------------------------------------------------------------------------
    //
    // RTP payload layout (§4):
    //
    //   +-+-+-+-+-+-+-+-+
    //   |Z|Y|W W W|N|V|0|   ← 1-byte aggregation header
    //   +-+-+-+-+-+-+-+-+
    //   followed by one or more OBU elements, each:
    //     [ leb128_length ] [ OBU bytes ]
    //   …except the very last element in the packet when W ≠ 0, whose length is
    //   omitted (inferred from remaining bytes).
    //
    //   Z = 1 → the first OBU element is a continuation fragment of the OBU
    //            that ended the previous packet (i.e. reassemble with `fu`).
    //   Y = 1 → the last OBU element is fragmented; its tail arrives in the
    //            next packet as a Z=1 element.
    //   W[2:0] → number of OBU elements in this packet (0 = not signalled;
    //             treat each element as length-prefixed until end of payload).
    //   N = 1 → new coded video sequence starts here (random access point /
    //            key-frame; carries the sequence header OBU).
    //   V = 1 → reserved (was previously "video layer ID present"); IGNORE.
    //   bit 0 → always 0 (reserved).
    //
    // The output for Android MediaCodec ("video/av01") is a temporal unit:
    // consecutive whole OBU bytes, no additional framing. We therefore append
    // each reassembled OBU to `self.au` and emit on the RTP marker bit.

    fn process_av1(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        let agg_hdr = payload[0];
        let z = agg_hdr & 0x80 != 0; // first element is a fragment continuation
        let y = agg_hdr & 0x40 != 0; // last element is fragmented (continues next pkt)
        let w = ((agg_hdr >> 3) & 0x07) as usize; // OBU element count (0 = unknown)
        // N (new sequence) and V (reserved) bits are informational for us — no action needed.

        let mut pos = 1usize; // cursor into payload (past the aggregation header)
        let mut elem_idx = 0usize; // how many OBU elements we have processed

        while pos < payload.len() {
            // Determine whether this element's length is length-prefixed.
            // The last element in the packet is NOT length-prefixed when W ≠ 0.
            let is_last_signalled = w != 0 && elem_idx == w.saturating_sub(1);

            let obu_len = if is_last_signalled {
                // Last element: runs to end of payload.
                payload.len() - pos
            } else {
                // Read LEB128 length.
                match read_leb128(&payload[pos..]) {
                    Some((len, consumed)) => {
                        pos += consumed;
                        len
                    }
                    None => return, // malformed
                }
            };

            if pos + obu_len > payload.len() {
                return; // malformed / truncated
            }

            let obu_bytes = &payload[pos..pos + obu_len];
            pos += obu_len;

            let is_first_elem = elem_idx == 0;
            // Determine if this element is the last one in the packet.
            let is_last_elem = if w != 0 {
                elem_idx == w.saturating_sub(1)
            } else {
                pos >= payload.len()
            };

            if is_first_elem && z {
                // Continuation fragment: append to the in-progress OBU body in `fu`.
                if let Some(buf) = self.fu.as_mut() {
                    buf.extend_from_slice(obu_bytes);
                }
                // If this is also the last element and Y=0, the OBU is now complete.
                if is_last_elem && !y {
                    if let Some(body) = self.fu.take() {
                        // Prepend the saved OBU header byte.
                        self.au.push(self.av1_frag_obu_hdr);
                        self.au.extend_from_slice(&body);
                    }
                    self.av1_frag_pending = false;
                }
                // If Z=1 and Y=1 and this is the only element, the OBU fragment
                // runs across both the Z-continuation and Y-fragmentation: keep
                // accumulating into `fu` (av1_frag_pending stays true).
            } else if is_last_elem && y {
                // This OBU element is fragmented; its tail arrives next packet.
                // Save the OBU header byte and start a new fragment buffer.
                if obu_bytes.is_empty() {
                    elem_idx += 1;
                    continue;
                }
                self.av1_frag_obu_hdr = obu_bytes[0];
                let body = obu_bytes[1..].to_vec();
                self.fu = Some(body);
                self.av1_frag_pending = true;
            } else {
                // Whole OBU: append directly.
                // Any previously pending fragment is implicitly abandoned (shouldn't
                // happen in a well-behaved stream; discard defensively).
                if self.av1_frag_pending {
                    self.fu = None;
                    self.av1_frag_pending = false;
                }
                self.au.extend_from_slice(obu_bytes);
            }

            elem_idx += 1;
        }
    }

    fn process_h265(&mut self, payload: &[u8]) {
        if payload.len() < 2 {
            return;
        }
        match (payload[0] >> 1) & 0x3F {
            0..=47 => self.emit_nal(payload), // single NAL unit
            48 => {
                // AP (aggregation), DONL absent: [2-byte NAL hdr][ (size:u16)(NAL) ]*
                let mut i = 2;
                while i + 2 <= payload.len() {
                    let size = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                    i += 2;
                    if i + size > payload.len() {
                        break;
                    }
                    let nal = payload[i..i + size].to_vec();
                    self.emit_nal(&nal);
                    i += size;
                }
            }
            49 => {
                // FU, DONL absent: [2-byte NAL hdr][FU header][fragment]
                if payload.len() < 3 {
                    return;
                }
                let (b0, b1, hdr) = (payload[0], payload[1], payload[2]);
                let frag = &payload[3..];
                if hdr & 0x80 != 0 {
                    // start: rebuild 2-byte NAL header, replacing the 6-bit type
                    let mut nal = Vec::with_capacity(2 + frag.len());
                    nal.push((b0 & 0x81) | ((hdr & 0x3F) << 1));
                    nal.push(b1);
                    nal.extend_from_slice(frag);
                    self.fu = Some(nal);
                } else if let Some(buf) = self.fu.as_mut() {
                    buf.extend_from_slice(frag);
                }
                if hdr & 0x40 != 0 {
                    if let Some(nal) = self.fu.take() {
                        self.emit_nal(&nal);
                    }
                }
            }
            _ => {}
        }
    }
}

// -------------------------------------------------------------------------
// LEB128 unsigned integer decoder (used by the AV1 OBU element length field,
// RFC 9043 §3.2 / AV1 spec §4.10.5).
//
// Returns `(value, bytes_consumed)` or `None` if the input is empty or the
// integer requires more than 8 bytes (64-bit overflow guard).
// -------------------------------------------------------------------------
fn read_leb128(buf: &[u8]) -> Option<(usize, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in buf.iter().enumerate() {
        if shift >= 64 {
            return None; // overflow
        }
        value |= ((byte & 0x7F) as u64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            // No more bytes; check the result fits in usize.
            return usize::try_from(value).ok().map(|v| (v, i + 1));
        }
    }
    None // truncated
}

// =========================================================================
// A7 — RTP reorder / jitter buffer (THE spike fix)
// =========================================================================
//
// Sits between `service::media::parse` and the `Depacketizer` in the client read
// loop. A single late or reordered datagram otherwise trips the depacketizer's
// gap handling and corrupts the in-flight access unit, producing a visible decode
// spike. This buffer holds out-of-order packets for a brief, RTT-bounded window
// and releases them in sequence order. In-order packets (the steady-state common
// case) fall straight through with no added latency.
//
// Packets are keyed by a 64-bit *extended* sequence number reconstructed from the
// 16-bit RTP seq (so the map orders correctly across the 16-bit wrap). On a stream
// discontinuity (encoder restart with a fresh RTP base — every codec / monitor
// switch rebuilds it) the buffer resyncs to the new base instead of stalling.

/// Max buffered packets before we force a flush (overflow guard).
const REORDER_CAPACITY: usize = 128;

/// Seq distance beyond which a jump is a stream restart (fresh RTP base) rather
/// than ordinary loss/reorder — resync to it instead of NACK-flooding / stalling.
const REORDER_RESYNC_DISTANCE: i32 = 256;

pub struct ReorderBuffer {
    /// Out-of-order packets awaiting release, keyed by extended seq. Owned copies
    /// (the source recv datagram is reused by the batch). Recycled via `recycle`.
    buf: BTreeMap<u64, Vec<u8>>,
    /// Extended seq of the next packet to release in order (`None` until primed).
    next: Option<u64>,
    /// Highest extended seq observed — used to spot new holes to NACK.
    seen_hi: Option<u64>,
    /// Outstanding holes already NACKed (so we don't re-NACK the same seq).
    missing: BTreeSet<u64>,
    /// Max time the head-of-line packet may wait for a missing predecessor before
    /// we flush past the hole (a "declared gap"). clamp(2.5*RTT, 30, 120) ms.
    max_delay: Duration,
    /// When the current head-of-line block started waiting.
    blocked_since: Option<Instant>,
    /// Set by `pop_ready` when it skipped past a missing packet; read+cleared by the
    /// caller (`take_gap_declared`) to engage the awaiting-IDR gate + keyframe NACK.
    gap_declared: bool,
    /// Window counters for the ABR controller's *true* loss (declared gaps only).
    win_recv: u32,
    win_lost: u32,
    /// Free list of packet buffers so the hot path stays allocation-free.
    pool: Vec<Vec<u8>>,
}

impl Default for ReorderBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ReorderBuffer {
    pub fn new() -> Self {
        Self {
            buf: BTreeMap::new(),
            next: None,
            seen_hi: None,
            missing: BTreeSet::new(),
            max_delay: Duration::from_millis(40),
            blocked_since: None,
            gap_declared: false,
            win_recv: 0,
            win_lost: 0,
            pool: Vec::new(),
        }
    }

    /// Drop all buffered state — called on restream, where the host restarts its
    /// RTP sequence base (and the depacketizer is rebuilt).
    pub fn reset(&mut self) {
        let drained: Vec<Vec<u8>> = self.buf.values_mut().map(std::mem::take).collect();
        self.buf.clear();
        for v in drained {
            self.recycle(v);
        }
        self.next = None;
        self.seen_hi = None;
        self.missing.clear();
        self.blocked_since = None;
        self.gap_declared = false;
        self.win_recv = 0;
        self.win_lost = 0;
    }

    /// Size the hold window from the measured network RTT: clamp(2.5*RTT, 30, 120) ms.
    pub fn set_rtt_ms(&mut self, rtt_ms: f64) {
        let ms = (rtt_ms * 2.5).clamp(30.0, 120.0);
        self.max_delay = Duration::from_millis(ms as u64);
    }

    fn take_buf(&mut self, data: &[u8]) -> Vec<u8> {
        let mut v = self.pool.pop().unwrap_or_default();
        v.clear();
        v.extend_from_slice(data);
        v
    }

    /// Return a released buffer to the free list for reuse.
    pub fn recycle(&mut self, buf: Vec<u8>) {
        if self.pool.len() < REORDER_CAPACITY {
            self.pool.push(buf);
        }
    }

    /// Insert one video RTP packet (an owned copy is taken). Newly-detected missing
    /// seqs (holes ahead of the release cursor) are appended to `nacks` (16-bit) so
    /// the caller can send ONE coalesced retransmit request per batch (A23).
    pub fn push(&mut self, seq: u16, pkt: &[u8], nacks: &mut Vec<u16>) {
        self.win_recv = self.win_recv.saturating_add(1);

        let next = match self.next {
            Some(n) => n,
            None => {
                // First packet primes the release cursor.
                let ext = seq as u64;
                self.next = Some(ext);
                self.seen_hi = Some(ext);
                let b = self.take_buf(pkt);
                self.buf.insert(ext, b);
                return;
            }
        };

        // Reconstruct the extended seq relative to the release cursor (wrap-aware:
        // the signed 16-bit delta picks the nearest extended value).
        let delta = seq.wrapping_sub(next as u16) as i16 as i32;
        let ext = (next as i64 + delta as i64) as u64;

        // A jump larger than any plausible reorder/loss burst = the encoder restarted
        // with a fresh RTP base. Resync to it (re-anchoring the decoder) instead of
        // treating the whole distance as loss and NACK-flooding / stalling forever.
        if delta.abs() > REORDER_RESYNC_DISTANCE {
            let drained: Vec<Vec<u8>> = self.buf.values_mut().map(std::mem::take).collect();
            self.buf.clear();
            for v in drained {
                self.recycle(v);
            }
            self.missing.clear();
            self.next = Some(ext);
            self.seen_hi = Some(ext);
            self.blocked_since = None;
            self.gap_declared = true; // ask the decoder to re-anchor on the new stream
            let b = self.take_buf(pkt);
            self.buf.insert(ext, b);
            return;
        }

        // Behind the cursor (already released or given up on) or a duplicate → drop.
        if ext < next || self.buf.contains_key(&ext) {
            return;
        }

        // New hole(s) between the highest seq seen and this one → NACK once each.
        if let Some(hi) = self.seen_hi {
            let mut s = hi + 1;
            while s < ext {
                if self.missing.insert(s) {
                    nacks.push(s as u16);
                }
                s += 1;
            }
        }
        self.missing.remove(&ext); // this seq has now arrived
        if self.seen_hi.is_none_or(|hi| ext > hi) {
            self.seen_hi = Some(ext);
        }

        let b = self.take_buf(pkt);
        self.buf.insert(ext, b);
    }

    /// Pop the next packet to feed the depacketizer, in sequence order. Returns
    /// `None` while the head-of-line packet is missing and still within its wait
    /// window. On overflow or timeout it skips the hole (declaring a gap — see
    /// `take_gap_declared`) and resumes releasing in order.
    pub fn pop_ready(&mut self, now: Instant) -> Option<Vec<u8>> {
        loop {
            let next = self.next?;
            let &first = self.buf.keys().next()?;

            if first < next {
                // Stale leftover (push normally drops these) — discard defensively.
                if let Some(v) = self.buf.remove(&first) {
                    self.recycle(v);
                }
                continue;
            }
            if first == next {
                let v = self.buf.remove(&first).expect("key just observed");
                self.next = Some(next.wrapping_add(1));
                self.missing.remove(&first);
                self.blocked_since = None;
                return Some(v);
            }

            // first > next → a hole at `next`. Hold until max_delay / capacity.
            let overflow = self.buf.len() > REORDER_CAPACITY;
            let timed_out = match self.blocked_since {
                Some(t) => now.saturating_duration_since(t) >= self.max_delay,
                None => {
                    self.blocked_since = Some(now);
                    false
                }
            };
            if !(overflow || timed_out) {
                return None;
            }

            // Declare a gap: count the skipped seqs as true loss and jump the cursor
            // to the next available packet (released in order on the next iteration).
            self.win_lost = self.win_lost.saturating_add((first - next) as u32);
            let mut s = next;
            while s < first {
                self.missing.remove(&s);
                s += 1;
            }
            self.next = Some(first);
            self.blocked_since = None;
            self.gap_declared = true;
        }
    }

    /// True (and cleared) if `pop_ready` skipped past a missing packet since the last
    /// call — the caller engages the awaiting-IDR gate + sends a keyframe NACK.
    pub fn take_gap_declared(&mut self) -> bool {
        std::mem::replace(&mut self.gap_declared, false)
    }

    /// Window `(received, lost)` packet counts for the ABR controller's *true* loss,
    /// resetting the window. `lost` counts only declared gaps — NOT reordering — so
    /// the controller no longer double-reacts to packets that merely arrived early.
    pub fn take_loss_window(&mut self) -> (u32, u32) {
        let out = (self.win_recv, self.win_lost);
        self.win_recv = 0;
        self.win_lost = 0;
        out
    }
}

/// Whether a depacketized access unit is a keyframe / random-access point. Used by
/// the client's awaiting-IDR gate (A7) to know when decoding may safely resume after
/// a declared packet-loss gap. Conservative: anything it cannot classify is treated
/// as a keyframe so the gate can never wedge the stream permanently.
pub fn au_is_keyframe(codec: Codec, au: &[u8]) -> bool {
    match codec {
        Codec::H264 => annexb_any_nal(au, |b| {
            let t = b & 0x1F;
            t == 5 || t == 7 // IDR slice or SPS
        }),
        Codec::H265 => annexb_any_nal(au, |b| {
            let t = (b >> 1) & 0x3F;
            (16..=23).contains(&t) || t == 32 || t == 33 // IRAP (BLA/IDR/CRA) or VPS/SPS
        }),
        Codec::Av1 => av1_tu_has_seq_header(au),
    }
}

/// Scan an Annex-B byte stream; return true if any NAL's first byte satisfies
/// `pred`. Matches the 3-byte start code `00 00 01` (the 4-byte `00 00 00 01`
/// contains it), which is how `Depacketizer::emit_nal` frames every NAL.
fn annexb_any_nal(au: &[u8], pred: impl Fn(u8) -> bool) -> bool {
    let mut i = 0usize;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let nal0 = i + 3;
            if pred(au[nal0]) {
                return true;
            }
            i = nal0;
        } else {
            i += 1;
        }
    }
    false
}

/// AV1: true if the temporal unit contains a sequence-header OBU (type 1), which the
/// encoder emits at every random-access point (keyframe). Walks the OBU chain using
/// its `obu_has_size_field` LEB128 sizes; on anything unparseable it fails safe
/// (returns true) so the awaiting-IDR gate never wedges.
fn av1_tu_has_seq_header(au: &[u8]) -> bool {
    let mut i = 0usize;
    while i < au.len() {
        let hdr = au[i];
        let obu_type = (hdr >> 3) & 0x0F;
        if obu_type == 1 {
            return true; // OBU_SEQUENCE_HEADER
        }
        let ext = hdr & 0x04 != 0;
        let has_size = hdr & 0x02 != 0;
        let mut p = i + 1;
        if ext {
            p += 1;
        }
        if !has_size {
            return true; // cannot advance without a size field — fail safe
        }
        match read_leb128(au.get(p..).unwrap_or(&[])) {
            Some((len, consumed)) => {
                p = p.saturating_add(consumed).saturating_add(len);
            }
            None => return true, // truncated / unparseable — fail safe
        }
        if p <= i {
            return true; // no forward progress — fail safe
        }
        i = p;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RTP datagram (no CSRC, no extension) around a payload.
    fn rtp(marker: bool, ts: u32, seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, if marker { 0x80 | 96 } else { 96 }];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 0]); // SSRC
        p.extend_from_slice(payload);
        p
    }

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&START_CODE);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn rtp_payload_strips_padding() {
        let mut p = rtp(true, 1, 1, &[0xAA, 0xBB, 0xCC]);
        // Set the P bit and append 2 pad bytes (last byte = pad count).
        p[0] |= 0x20;
        p.extend_from_slice(&[0x00, 0x02]);
        assert_eq!(rtp_payload(&p), Some(&[0xAA, 0xBB, 0xCC][..]));
    }

    #[test]
    fn rtp_payload_rejects_bad_padding_and_version() {
        // Pad count larger than the payload → malformed.
        let mut p = rtp(true, 1, 1, &[0xAA]);
        p[0] |= 0x20;
        p.push(200);
        assert_eq!(rtp_payload(&p), None);
        // Pad count of zero with the P bit set → malformed.
        let mut z = rtp(true, 1, 1, &[0xAA]);
        z[0] |= 0x20;
        z.push(0);
        assert_eq!(rtp_payload(&z), None);
        // Non-RTP-v2 datagram (e.g. a stray non-RTP packet) → rejected.
        let mut v = rtp(true, 1, 1, &[0xAA]);
        v[0] &= 0x3F;
        assert_eq!(rtp_payload(&v), None);
    }

    #[test]
    fn stale_retransmit_dropped_but_restart_accepted() {
        let mut d = Depacketizer::new(Codec::H264);
        let nal = [0x65u8, 0xAA];
        assert!(d.push(&rtp(true, 10_000, 1, &nal)).is_some());
        // A late NACK retransmit from an older frame must be ignored (its frame is
        // long flushed) — and must NOT disturb the next frame's assembly.
        assert!(d.push(&rtp(true, 7_000, 0, &nal)).is_none());
        let au = d.push(&rtp(true, 11_500, 2, &nal)).expect("next frame unaffected");
        assert_eq!(au, annexb(&[&nal]));
        // An encoder restart lands on a fresh random ts base (huge backward jump) —
        // accepted, stream resyncs.
        assert!(d.push(&rtp(true, 11_500u32.wrapping_sub(5_000_000), 3, &nal)).is_some());
    }

    #[test]
    fn h264_single_nal() {
        let mut d = Depacketizer::new(Codec::H264);
        let nal = [0x65u8, 0xAA, 0xBB]; // type 5 (IDR)
        let au = d.push(&rtp(true, 100, 1, &nal)).expect("au");
        assert_eq!(au, annexb(&[&nal]));
    }

    #[test]
    fn h264_stap_a() {
        let mut d = Depacketizer::new(Codec::H264);
        let sps = [0x67u8, 0x42, 0x00];
        let pps = [0x68u8, 0xCE, 0x3C];
        let mut payload = vec![0x18u8]; // STAP-A (24)
        payload.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        payload.extend_from_slice(&sps);
        payload.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        payload.extend_from_slice(&pps);
        let au = d.push(&rtp(true, 1, 1, &payload)).expect("au");
        assert_eq!(au, annexb(&[&sps, &pps]));
    }

    #[test]
    fn h264_fu_a_reassembly() {
        let mut d = Depacketizer::new(Codec::H264);
        // Fragment a type-5 NAL "HELLO" across two packets, NRI=3.
        let p1 = [0x7Cu8, 0x85, b'H', b'E']; // ind=F0|NRI3|28=0x7C, hdr S=1 type=5
        let p2 = [0x7Cu8, 0x45, b'L', b'L', b'O']; // hdr E=1 type=5
        assert!(d.push(&rtp(false, 7, 1, &p1)).is_none());
        let au = d.push(&rtp(true, 7, 2, &p2)).expect("au");
        assert_eq!(au, annexb(&[&[0x65, b'H', b'E', b'L', b'L', b'O']]));
    }

    #[test]
    fn h265_single_nal() {
        let mut d = Depacketizer::new(Codec::H265);
        let nal = [0x26u8, 0x01, 0xAA]; // type (0x26>>1)&0x3f = 19 (IDR_W_RADL)
        let au = d.push(&rtp(true, 5, 1, &nal)).expect("au");
        assert_eq!(au, annexb(&[&nal]));
    }

    #[test]
    fn h265_fu_reassembly() {
        let mut d = Depacketizer::new(Codec::H265);
        // FU type 49 (0x62,0x01), fragmented NAL of type 19, payload "AB"/"CD".
        let p1 = [0x62u8, 0x01, 0x93, b'A', b'B']; // hdr S=1 (0x80) | type19 (0x13) = 0x93
        let p2 = [0x62u8, 0x01, 0x53, b'C', b'D']; // hdr E=1 (0x40) | type19 = 0x53
        assert!(d.push(&rtp(false, 9, 1, &p1)).is_none());
        let au = d.push(&rtp(true, 9, 2, &p2)).expect("au");
        // reconstructed header byte0 = (0x62 & 0x81) | (19<<1) = 0x00|0x26 = 0x26
        assert_eq!(au, annexb(&[&[0x26, 0x01, b'A', b'B', b'C', b'D']]));
    }

    #[test]
    fn marker_groups_multiple_nals_into_one_au() {
        let mut d = Depacketizer::new(Codec::H264);
        let a = [0x41u8, 1, 2]; // non-IDR slice
        let b = [0x41u8, 3, 4];
        assert!(d.push(&rtp(false, 50, 1, &a)).is_none());
        let au = d.push(&rtp(true, 50, 2, &b)).expect("au");
        assert_eq!(au, annexb(&[&a, &b]));
    }

    #[test]
    fn timestamp_change_flushes_unmarked_frame() {
        let mut d = Depacketizer::new(Codec::H264);
        let a = [0x41u8, 1]; // frame 1, marker lost
        let b = [0x41u8, 2]; // frame 2
        assert!(d.push(&rtp(false, 1, 1, &a)).is_none());
        // new ts → previous AU flushed
        let au = d.push(&rtp(false, 2, 2, &b)).expect("flushed prev au");
        assert_eq!(au, annexb(&[&a]));
    }

    // -------------------------------------------------------------------------
    // LEB128 tests
    // -------------------------------------------------------------------------

    #[test]
    fn leb128_single_byte() {
        assert_eq!(read_leb128(&[0x00]), Some((0, 1)));
        assert_eq!(read_leb128(&[0x7F]), Some((127, 1)));
    }

    #[test]
    fn leb128_multibyte() {
        // 300 = 0x12C → LEB128: 0xAC 0x02
        assert_eq!(read_leb128(&[0xAC, 0x02]), Some((300, 2)));
        // 16384 = 0x4000 → LEB128: 0x80 0x80 0x01
        assert_eq!(read_leb128(&[0x80, 0x80, 0x01]), Some((16384, 3)));
    }

    #[test]
    fn leb128_empty_is_none() {
        assert_eq!(read_leb128(&[]), None);
    }

    #[test]
    fn leb128_truncated_is_none() {
        // High bit set on last byte means more bytes expected.
        assert_eq!(read_leb128(&[0x80]), None);
    }

    // -------------------------------------------------------------------------
    // AV1 RFC 9043 tests
    // -------------------------------------------------------------------------

    /// Build a minimal AV1 OBU aggregation header byte.
    /// z=continuation, y=fragmented-last, w=element-count (0..=7), n=new-sequence.
    fn av1_agg_hdr(z: bool, y: bool, w: u8, n: bool) -> u8 {
        let mut b = 0u8;
        if z { b |= 0x80; }
        if y { b |= 0x40; }
        b |= (w & 0x07) << 3;
        if n { b |= 0x04; }
        b
    }

    /// Build an AV1 RTP payload: [agg_hdr][elem...]
    /// Each elem is `(obu_bytes, include_length_prefix)`.
    fn av1_payload(hdr: u8, elems: &[(&[u8], bool)]) -> Vec<u8> {
        let mut v = vec![hdr];
        for (obu, with_len) in elems {
            if *with_len {
                // LEB128-encode the length.
                let mut n = obu.len();
                loop {
                    let mut byte = (n & 0x7F) as u8;
                    n >>= 7;
                    if n != 0 { byte |= 0x80; }
                    v.push(byte);
                    if n == 0 { break; }
                }
            }
            v.extend_from_slice(obu);
        }
        v
    }

    #[test]
    fn av1_single_obu_whole_packet() {
        // W=1: one element, last element → no length prefix.
        // Aggregation header: Z=0, Y=0, W=1, N=1 (new sequence).
        let obu = [0x0Au8, 0x00, 0xAA, 0xBB]; // sequence header OBU (type 1, hdr=0x0A)
        let hdr = av1_agg_hdr(false, false, 1, true);
        let payload = av1_payload(hdr, &[(&obu, false)]); // W=1 last elem: no len prefix
        let mut d = Depacketizer::new(Codec::Av1);
        let au = d.push(&rtp(true, 1, 1, &payload)).expect("av1 au");
        assert_eq!(au, obu);
    }

    #[test]
    fn av1_two_obus_in_one_packet() {
        // W=2: two OBU elements; first has LEB128 length, second (last) does not.
        let obu_a = [0x0Au8, 0x01]; // sequence header OBU body (2 bytes)
        let obu_b = [0x32u8, 0xAA, 0xBB, 0xCC]; // frame OBU (type 6)
        let hdr = av1_agg_hdr(false, false, 2, false);
        // First elem with length prefix (2 bytes), second without.
        let payload = av1_payload(hdr, &[(&obu_a, true), (&obu_b, false)]);
        let mut d = Depacketizer::new(Codec::Av1);
        let au = d.push(&rtp(true, 2, 1, &payload)).expect("av1 two-obu au");
        let mut expected = obu_a.to_vec();
        expected.extend_from_slice(&obu_b);
        assert_eq!(au, expected);
    }

    #[test]
    fn av1_fragmented_obu_across_two_packets() {
        // Packet 1: Z=0, Y=1, W=1 (last elem has no length prefix).
        //   Contains the first half of a frame OBU (header byte + data).
        let obu_hdr_byte = 0x32u8; // frame OBU header (type 6, no extension)
        let part1 = [obu_hdr_byte, b'H', b'E', b'L'];
        let hdr1 = av1_agg_hdr(false, true, 1, false);
        let payload1 = av1_payload(hdr1, &[(&part1, false)]);

        // Packet 2: Z=1, Y=0, W=1 (continuation + last, no length prefix).
        //   Contains the second half (tail of the OBU, WITHOUT re-emitting the header).
        let part2 = [b'L', b'O'];
        let hdr2 = av1_agg_hdr(true, false, 1, false);
        let payload2 = av1_payload(hdr2, &[(&part2, false)]);

        let mut d = Depacketizer::new(Codec::Av1);
        // First packet: Y=1 so last element is fragmented → starts accumulation.
        assert!(d.push(&rtp(false, 10, 1, &payload1)).is_none());
        // Second packet: Z=1, marker=true → continuation fragment + emit TU.
        let au = d.push(&rtp(true, 10, 2, &payload2)).expect("av1 frag au");

        // The reassembled OBU: header byte + full body (part1[1..] + part2).
        let mut expected = vec![obu_hdr_byte];
        expected.extend_from_slice(&part1[1..]);
        expected.extend_from_slice(&part2);
        assert_eq!(au, expected);
    }

    #[test]
    fn av1_w0_multiple_obus_leb128_all() {
        // W=0: element count not signalled; all elements are length-prefixed.
        // Two OBUs: [len_a][obu_a][len_b][obu_b]  (last must still have length when W=0)
        let obu_a = [0x0Cu8, 0x00]; // metadata OBU (type 2, hdr byte)
        let obu_b = [0x20u8, 0xAA, 0xBB]; // frame header OBU (type 4)
        let hdr = av1_agg_hdr(false, false, 0, false);
        let payload = av1_payload(hdr, &[(&obu_a, true), (&obu_b, true)]);
        let mut d = Depacketizer::new(Codec::Av1);
        let au = d.push(&rtp(true, 5, 1, &payload)).expect("av1 w0 au");
        let mut expected = obu_a.to_vec();
        expected.extend_from_slice(&obu_b);
        assert_eq!(au, expected);
    }

    #[test]
    fn av1_timestamp_change_flushes_av1_frame() {
        // Send first frame without marker (lost), then a second frame.
        // The timestamp change should flush the first frame's OBU.
        let obu1 = [0x32u8, 0xAA];
        let obu2 = [0x32u8, 0xBB];
        let hdr = av1_agg_hdr(false, false, 1, false);
        let p1 = av1_payload(hdr, &[(&obu1, false)]);
        let p2 = av1_payload(hdr, &[(&obu2, false)]);
        let mut d = Depacketizer::new(Codec::Av1);
        assert!(d.push(&rtp(false, 1, 1, &p1)).is_none());
        let au = d.push(&rtp(false, 2, 2, &p2)).expect("flushed av1 au");
        assert_eq!(au, obu1); // first frame was flushed
    }

    // -------------------------------------------------------------------------
    // A7 ReorderBuffer + keyframe-detection tests
    // -------------------------------------------------------------------------

    #[test]
    fn reorder_in_order_passes_through() {
        let mut rb = ReorderBuffer::new();
        let mut nacks = Vec::new();
        let now = Instant::now();
        rb.push(10, &[0xAA], &mut nacks);
        rb.push(11, &[0xBB], &mut nacks);
        rb.push(12, &[0xCC], &mut nacks);
        assert!(nacks.is_empty(), "no holes in an in-order run");
        assert_eq!(rb.pop_ready(now), Some(vec![0xAA]));
        assert_eq!(rb.pop_ready(now), Some(vec![0xBB]));
        assert_eq!(rb.pop_ready(now), Some(vec![0xCC]));
        assert_eq!(rb.pop_ready(now), None);
        assert!(!rb.take_gap_declared());
    }

    #[test]
    fn reorder_holds_out_of_order_then_releases() {
        let mut rb = ReorderBuffer::new();
        let mut nacks = Vec::new();
        let now = Instant::now();
        rb.push(1, &[1], &mut nacks);
        rb.push(3, &[3], &mut nacks); // gap at 2 → NACK 2
        assert_eq!(nacks, vec![2]);
        assert_eq!(rb.pop_ready(now), Some(vec![1]));
        assert_eq!(rb.pop_ready(now), None, "2 still missing, within window");
        rb.push(2, &[2], &mut nacks); // the missing packet arrives (reordered)
        assert_eq!(rb.pop_ready(now), Some(vec![2]));
        assert_eq!(rb.pop_ready(now), Some(vec![3]));
        assert!(!rb.take_gap_declared(), "filled before timeout → no gap");
        assert_eq!(rb.take_loss_window(), (3, 0), "reorder is not loss");
    }

    #[test]
    fn reorder_timeout_declares_gap() {
        let mut rb = ReorderBuffer::new();
        let mut nacks = Vec::new();
        let t0 = Instant::now();
        rb.push(1, &[1], &mut nacks);
        rb.push(3, &[3], &mut nacks);
        assert_eq!(rb.pop_ready(t0), Some(vec![1]));
        assert_eq!(rb.pop_ready(t0), None); // arms the wait timer
        let later = t0 + Duration::from_millis(200);
        assert_eq!(rb.pop_ready(later), Some(vec![3]), "flush past the hole");
        assert!(rb.take_gap_declared(), "skipping seq 2 declared a gap");
        assert_eq!(rb.take_loss_window().1, 1, "one truly-lost packet");
    }

    #[test]
    fn reorder_dedup_drops_duplicates() {
        let mut rb = ReorderBuffer::new();
        let mut nacks = Vec::new();
        let now = Instant::now();
        rb.push(5, &[5], &mut nacks);
        rb.push(5, &[0xFF], &mut nacks); // duplicate seq → dropped
        assert_eq!(rb.pop_ready(now), Some(vec![5]));
        assert_eq!(rb.pop_ready(now), None);
    }

    #[test]
    fn reorder_reset_clears_state() {
        let mut rb = ReorderBuffer::new();
        let mut nacks = Vec::new();
        let now = Instant::now();
        rb.push(100, &[1], &mut nacks);
        rb.reset();
        // After reset the next packet re-primes the cursor at its own seq.
        rb.push(7, &[7], &mut nacks);
        assert_eq!(rb.pop_ready(now), Some(vec![7]));
    }

    #[test]
    fn au_is_keyframe_h264() {
        let idr = annexb(&[&[0x65u8, 0x11]]); // NAL type 5 = IDR
        assert!(au_is_keyframe(Codec::H264, &idr));
        let p = annexb(&[&[0x41u8, 0x22]]); // NAL type 1 = non-IDR slice
        assert!(!au_is_keyframe(Codec::H264, &p));
    }

    #[test]
    fn au_is_keyframe_h265() {
        let idr = annexb(&[&[0x26u8, 0x01]]); // (0x26>>1)&0x3f = 19 = IDR_W_RADL
        assert!(au_is_keyframe(Codec::H265, &idr));
        let trail = annexb(&[&[0x02u8, 0x01]]); // type 1 = TRAIL_R (non-IRAP)
        assert!(!au_is_keyframe(Codec::H265, &trail));
    }
}
