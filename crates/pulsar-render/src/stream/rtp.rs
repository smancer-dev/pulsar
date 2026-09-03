//! RTP receive + depacketize → Annex-B (H.264/HEVC) / OBU-stream (AV1) access units.
//!
//! Pure logic (no D3D11/windows deps) so it unit-tests on any host. Mirrors the depacketizers
//! in `src/lib/{h264,h265,av1}.ts`, but the consumer here is Media Foundation, not WebCodecs:
//!
//!   - **H.264 / HEVC**: each NAL is emitted as **Annex-B** — prefixed with the 4-byte start
//!     code `00 00 00 01`. The access unit is the concatenation of its start-code-prefixed NALs.
//!   - **AV1**: the raw **OBU stream** (the concatenated OBUs of the Temporal Unit, low-overhead
//!     bitstream with each OBU's own header + LEB128 size). No Annex-B for AV1.
//!
//! `AccessUnit.key` marks an IDR/keyframe AU; `pts_90k` is the 90 kHz RTP timestamp of the AU.
//!
//! Loss handling mirrors the TS players: track the 16-bit RTP sequence number, and on a forward
//! gap set `awaiting_idr` + drop partial state until the next clean keyframe so a corrupt NAL/TU
//! never reaches the decoder. Duplicate (`fwd==0`) and backward/late (`fwd>=0x8000`, e.g. a NACK
//! retransmit that lands after its frame was assembled) packets are DROPPED without touching
//! payload state — splicing them into the in-flight AU would corrupt it.

#![allow(dead_code)]

use super::{AccessUnit, Codec};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

// ── Reorder / jitter buffer (adaptive streaming Phase 0.5) ─────────────────────────────
//
// The Linux backend gets this from libav's RTP demuxer (`reorder_queue_size` + `max_delay`);
// this depacketizer used to read straight off the socket, so a NACK retransmit — which by
// construction arrives ~1 RTT AFTER the packets that followed the hole — was always "late"
// and dropped, and every single loss was a wait-for-IDR. `Reorder` holds the packets that
// arrive after a gap for up to `max_delay` (the app's RTT-derived value, stdin `maxdelay`)
// and releases them in sequence once the hole is filled, or skips the hole when the wait
// expires / the buffer fills. In-order streams pass straight through (no added latency).

/// App-derived reorder wait (µs). 0 = not received yet → `DEFAULT_MAX_DELAY_US`.
static MAX_DELAY_US: AtomicU64 = AtomicU64::new(0);
const DEFAULT_MAX_DELAY_US: u64 = 100_000;
/// Packets held behind a gap before flushing regardless (≈ 0.5 s at 8 Mbit / 1200 B —
/// the same bound as the Linux demuxer's `reorder_queue_size`).
pub const REORDER_CAP: usize = 512;

/// App → renderer: the RTT-derived reorder wait (`maxdelay <us>` over stdin).
pub fn set_max_delay_us(us: u64) {
	MAX_DELAY_US.store(us, AtomicOrdering::Relaxed);
}

/// The reorder wait in effect: the `PULSAR_MAXDELAY` env pin, else the app's value, else
/// the 100 ms default.
pub fn max_delay() -> Duration {
	let us = std::env::var("PULSAR_MAXDELAY")
		.ok()
		.and_then(|v| v.parse::<u64>().ok())
		.or_else(|| Some(MAX_DELAY_US.load(AtomicOrdering::Relaxed)).filter(|&v| v > 0))
		.unwrap_or(DEFAULT_MAX_DELAY_US);
	Duration::from_micros(us)
}

/// Sequence-ordered jitter buffer in front of [`Depacketizer`]. Pure (time is passed in).
pub struct Reorder {
	/// The next sequence number expected in order. `None` until the first packet.
	next: Option<u16>,
	/// Packets ahead of `next` (a hole precedes them), sorted by forward distance from `next`,
	/// each with its arrival time.
	buf: Vec<(u16, Instant, Vec<u8>)>,
	cap: usize,
}

impl Reorder {
	pub fn new(cap: usize) -> Self {
		Self { next: None, buf: Vec::new(), cap: cap.max(2) }
	}

	fn seq_of(p: &[u8]) -> Option<u16> {
		(p.len() >= 12).then(|| ((p[2] as u16) << 8) | p[3] as u16)
	}

	/// Packets currently held behind a hole.
	pub fn pending(&self) -> usize {
		self.buf.len()
	}

	/// Feed one datagram; returns the packets that are deliverable now, in sequence order.
	/// Duplicates and stale (backward) packets pass straight through — the depacketizer
	/// already drops those without touching its state.
	pub fn push(&mut self, pkt: Vec<u8>, now: Instant, max_delay: Duration) -> Vec<Vec<u8>> {
		let mut out = Vec::new();
		let Some(seq) = Self::seq_of(&pkt) else {
			out.push(pkt);
			return out;
		};
		match self.next {
			None => {
				self.next = Some(seq.wrapping_add(1));
				out.push(pkt);
			}
			Some(next) => {
				let d = seq.wrapping_sub(next);
				if d == 0 {
					// The expected packet (possibly the retransmit that fills the hole).
					self.next = Some(seq.wrapping_add(1));
					out.push(pkt);
					self.drain(&mut out);
				} else if d >= 0x8000 {
					// Older than what we already released: stale / duplicate — pass through.
					out.push(pkt);
				} else {
					// Ahead of a hole: hold it, sorted by distance from `next`.
					if !self.buf.iter().any(|(s, _, _)| *s == seq) {
						let pos = self
							.buf
							.iter()
							.position(|(s, _, _)| s.wrapping_sub(next) > d)
							.unwrap_or(self.buf.len());
						self.buf.insert(pos, (seq, now, pkt));
					}
					if self.buf.len() >= self.cap {
						self.skip(&mut out);
					}
				}
			}
		}
		self.expire(now, max_delay, &mut out);
		out
	}

	/// Time-based release (call on read timeouts): a head that has waited `max_delay` is
	/// released past its hole.
	pub fn poll(&mut self, now: Instant, max_delay: Duration) -> Vec<Vec<u8>> {
		let mut out = Vec::new();
		self.expire(now, max_delay, &mut out);
		out
	}

	fn expire(&mut self, now: Instant, max_delay: Duration, out: &mut Vec<Vec<u8>>) {
		while let Some((_, t, _)) = self.buf.first() {
			if now.duration_since(*t) >= max_delay {
				self.skip(out);
			} else {
				break;
			}
		}
	}

	/// Give up on the hole in front of the head: jump `next` to it and drain the run.
	fn skip(&mut self, out: &mut Vec<Vec<u8>>) {
		if let Some((s, _, _)) = self.buf.first() {
			self.next = Some(*s);
			self.drain(out);
		}
	}

	/// Release every buffered packet that is now contiguous with `next`.
	fn drain(&mut self, out: &mut Vec<Vec<u8>>) {
		while let (Some(next), Some((s, _, _))) = (self.next, self.buf.first()) {
			if *s != next {
				break;
			}
			let (_, _, p) = self.buf.remove(0);
			self.next = Some(next.wrapping_add(1));
			out.push(p);
		}
	}
}

/// Depacketizer state machine: feed RTP packets, get complete access units.
pub struct Depacketizer {
	codec: Codec,
	// --- RTP sequence / loss tracking (shared across codecs) ---
	last_seq: Option<u16>,
	awaiting_idr: bool,
	// PTS (90 kHz RTP timestamp) of the AU currently being assembled.
	cur_ts: u32,
	// --- H.264 / HEVC: completed NALs of the current AU (raw, no start code yet) ---
	nals: Vec<Vec<u8>>,
	// In-flight fragmentation unit (FU-A / FU) — the reconstructed NAL bytes so far.
	fu: Option<Vec<u8>>,
	// --- AV1: bytes of the Temporal Unit being assembled across packets ---
	av1_tu: Vec<u8>,
	av1_new_seq: bool,
}

impl Depacketizer {
	pub fn new(codec: Codec) -> Self {
		Self {
			codec,
			last_seq: None,
			awaiting_idr: true,
			cur_ts: 0,
			nals: Vec::new(),
			fu: None,
			av1_tu: Vec::new(),
			av1_new_seq: false,
		}
	}

	/// Drop any partial AU/TU state (called on a detected sequence gap).
	fn drop_partial(&mut self) {
		self.fu = None;
		self.nals.clear();
		self.av1_tu.clear();
		self.av1_new_seq = false;
	}

	/// Feed one RTP packet (full UDP payload). Returns a complete access unit when the RTP
	/// marker bit closes one.
	pub fn push(&mut self, rtp_packet: &[u8]) -> Option<AccessUnit> {
		if rtp_packet.len() < 12 {
			return None;
		}
		let b0 = rtp_packet[0];
		let b1 = rtp_packet[1];
		let has_ext = (b0 & 0x10) != 0;
		let cc = (b0 & 0x0f) as usize;
		let marker = (b1 & 0x80) != 0;
		let seq = ((rtp_packet[2] as u16) << 8) | rtp_packet[3] as u16;
		let ts = ((rtp_packet[4] as u32) << 24)
			| ((rtp_packet[5] as u32) << 16)
			| ((rtp_packet[6] as u32) << 8)
			| rtp_packet[7] as u32;

		// Sequence-gap detection: forward distance only. On a gap drop partial state and wait
		// for the next keyframe.
		if let Some(last) = self.last_seq {
			let fwd = seq.wrapping_sub(last);
			// fwd==1: consecutive (normal). fwd==0: duplicate. fwd>=0x8000: backward/reorder/late.
			if fwd == 0 || fwd >= 0x8000 {
				// Duplicate, reordered, or a NACK retransmit that arrives ≥1 RTT after its frame
				// was already assembled: DROP it WITHOUT touching payload state. Splicing its
				// NAL/FU/OBU bytes into the AU currently being assembled would corrupt that
				// (unrelated) AU — during keyframe assembly it poisons the whole next GOP — and a
				// late retransmit can never heal a gap we are already past. Leave last_seq and all
				// partial state untouched. (Mirrors the mobile depacketizer's stale-retransmit
				// guard in mobile/src/rtp.rs.)
				return None;
			}
			// Forward packet (newer than last seen).
			if fwd != 1 {
				// Gap: one or more sequence numbers were skipped.
				self.awaiting_idr = true;
				self.drop_partial();
			}
			self.last_seq = Some(seq);
		} else {
			self.last_seq = Some(seq);
		}

		// Skip the CSRC list and (if present) the RTP header extension.
		let mut off = 12 + cc * 4;
		if has_ext {
			if off + 4 > rtp_packet.len() {
				return None;
			}
			let el = ((rtp_packet[off + 2] as usize) << 8) | rtp_packet[off + 3] as usize;
			off += 4 + el * 4;
		}
		if off > rtp_packet.len() {
			return None;
		}
		let pl = &rtp_packet[off..];
		if pl.is_empty() {
			return None;
		}

		// The timestamp of this packet is the PTS of the AU it belongs to.
		self.cur_ts = ts;

		match self.codec {
			Codec::H264 => self.push_h264(pl, marker),
			Codec::H265 => self.push_h265(pl, marker),
			Codec::Av1 => self.push_av1(pl, marker),
		}
	}

	// ---- H.264 (RFC 6184) ------------------------------------------------------------------

	fn push_h264(&mut self, pl: &[u8], marker: bool) -> Option<AccessUnit> {
		let t = pl[0] & 0x1f;
		if (1..=23).contains(&t) {
			// Single NAL unit.
			self.nals.push(pl.to_vec());
		} else if t == 24 {
			// STAP-A: aggregated NALs, each prefixed by a 2-byte size.
			let mut q = 1usize;
			while q + 2 <= pl.len() {
				let s = ((pl[q] as usize) << 8) | pl[q + 1] as usize;
				q += 2;
				if q + s > pl.len() {
					break;
				}
				self.nals.push(pl[q..q + s].to_vec());
				q += s;
			}
		} else if t == 28 {
			// FU-A: a NAL fragmented across packets.
			if pl.len() < 2 {
				return None;
			}
			let fh = pl[1];
			let start = (fh & 0x80) != 0;
			let end = (fh & 0x40) != 0;
			let orig_type = fh & 0x1f;
			let nri = pl[0] & 0x60;
			if start {
				// Reconstruct the original 1-byte NAL header.
				self.fu = Some(vec![nri | orig_type]);
			}
			if let Some(fu) = self.fu.as_mut() {
				fu.extend_from_slice(&pl[2..]);
				if end {
					let nal = self.fu.take().unwrap();
					self.nals.push(nal);
				}
			}
		}
		if marker {
			self.emit_annexb_h264()
		} else {
			None
		}
	}

	fn emit_annexb_h264(&mut self) -> Option<AccessUnit> {
		if self.nals.is_empty() {
			return None;
		}
		let nals = std::mem::take(&mut self.nals);
		let mut key = false;
		for n in &nals {
			if n.is_empty() {
				continue;
			}
			let t = n[0] & 0x1f;
			if t == 5 || t == 7 {
				// IDR (5) or SPS (7) → keyframe AU.
				key = true;
			}
		}
		if self.awaiting_idr {
			if !key {
				return None; // wait for a clean keyframe before resuming.
			}
			self.awaiting_idr = false;
		}
		let mut data = Vec::new();
		for n in &nals {
			data.extend_from_slice(&START_CODE);
			data.extend_from_slice(n);
		}
		Some(AccessUnit {
			data,
			pts_90k: self.cur_ts,
			key,
		})
	}

	// ---- HEVC (RFC 7798) -------------------------------------------------------------------

	fn push_h265(&mut self, pl: &[u8], marker: bool) -> Option<AccessUnit> {
		if pl.len() < 2 {
			return None;
		}
		let nal_type = (pl[0] >> 1) & 0x3f;
		if nal_type <= 47 {
			// Single NAL unit — already carries its 2-byte header.
			self.nals.push(pl.to_vec());
		} else if nal_type == 48 {
			// Aggregation Packet: 2-byte PayloadHdr + [16-bit size + NAL]xN. Each aggregated NAL
			// includes its own 2-byte nal_unit_header (no DONL/DOND; matches the TS path).
			let mut q = 2usize;
			while q + 2 <= pl.len() {
				let sz = ((pl[q] as usize) << 8) | pl[q + 1] as usize;
				q += 2;
				if sz == 0 || q + sz > pl.len() {
					break;
				}
				self.nals.push(pl[q..q + sz].to_vec());
				q += sz;
			}
		} else if nal_type == 49 {
			// Fragmentation Unit: 2-byte PayloadHdr + 1-byte FU header + payload.
			if pl.len() < 3 {
				return None;
			}
			let fu_hdr0 = pl[0];
			let fu_hdr1 = pl[1];
			let fuhdr = pl[2];
			let start = (fuhdr & 0x80) != 0;
			let end = (fuhdr & 0x40) != 0;
			let fu_type = fuhdr & 0x3f;
			// nuh_layer_id (6 bits) + nuh_temporal_id_plus1 (3 bits) from the FU PayloadHdr.
			let layer_id = ((fu_hdr0 & 0x01) << 5) | ((fu_hdr1 >> 3) & 0x1f);
			let tid = fu_hdr1 & 0x07;
			if start {
				// Reconstruct the original 2-byte HEVC NAL header.
				let h0 = (fu_type << 1) | ((layer_id >> 5) & 0x01);
				let h1 = ((layer_id & 0x1f) << 3) | (tid & 0x07);
				self.fu = Some(vec![h0, h1]);
			}
			if let Some(fu) = self.fu.as_mut() {
				fu.extend_from_slice(&pl[3..]);
				if end {
					let nal = self.fu.take().unwrap();
					self.nals.push(nal);
				}
			}
		}
		if marker {
			self.emit_annexb_h265()
		} else {
			None
		}
	}

	fn emit_annexb_h265(&mut self) -> Option<AccessUnit> {
		if self.nals.is_empty() {
			return None;
		}
		let nals = std::mem::take(&mut self.nals);
		let mut key = false;
		for n in &nals {
			if n.is_empty() {
				continue;
			}
			let nal_type = (n[0] >> 1) & 0x3f;
			// Key NALs: VPS=32 / SPS=33 / PPS=34, and IRAP VCL types 16..=21 (incl.
			// IDR_W_RADL=19, IDR_N_LP=20, CRA=21).
			if (16..=21).contains(&nal_type) || (32..=34).contains(&nal_type) {
				key = true;
			}
		}
		if self.awaiting_idr {
			if !key {
				return None;
			}
			self.awaiting_idr = false;
		}
		let mut data = Vec::new();
		for n in &nals {
			data.extend_from_slice(&START_CODE);
			data.extend_from_slice(n);
		}
		Some(AccessUnit {
			data,
			pts_90k: self.cur_ts,
			key,
		})
	}

	// ---- AV1 (aomedia "RTP Payload Format For AV1") ----------------------------------------

	fn push_av1(&mut self, pl: &[u8], marker: bool) -> Option<AccessUnit> {
		// Aggregation header: Z(7) Y(6) W(5..4) N(3).
		let agg = pl[0];
		let w = (agg >> 4) & 0x03; // element count (0 = all length-prefixed)
		let n = (agg & 0x08) != 0; // start of a new coded video sequence
		if n {
			self.av1_new_seq = true;
		}

		let mut p = 1usize;
		let mut idx = 0u8;
		while p < pl.len() {
			idx += 1;
			let is_last = w != 0 && idx == w;
			let elem_len: usize = if is_last {
				// W>0: the last element has no length prefix; runs to the end of the payload.
				pl.len() - p
			} else {
				let (val, len) = read_leb128(pl, p);
				p += len;
				val
			};
			if p + elem_len > pl.len() {
				// Malformed/truncated — bail rather than splice garbage.
				break;
			}
			// Z/Y continuation is handled transparently by raw byte concatenation: the OBU's own
			// size field (set by the host) spans the reassembled whole.
			self.av1_tu.extend_from_slice(&pl[p..p + elem_len]);
			p += elem_len;
		}

		if marker {
			self.emit_av1()
		} else {
			None
		}
	}

	fn emit_av1(&mut self) -> Option<AccessUnit> {
		if self.av1_tu.is_empty() {
			self.av1_new_seq = false;
			return None;
		}
		let data = std::mem::take(&mut self.av1_tu);
		let new_seq = self.av1_new_seq;
		self.av1_new_seq = false;

		let (has_seq_header, has_frame) = inspect_av1_tu(&data);
		// A key TU carries a sequence header + a frame (Sunshine/Moonlight emit a seq-header OBU
		// before every keyframe). The RTP N bit is a corroborating hint.
		let key = (has_seq_header && has_frame) || new_seq;

		if self.awaiting_idr {
			if !key {
				return None;
			}
			self.awaiting_idr = false;
		}
		Some(AccessUnit {
			data,
			pts_90k: self.cur_ts,
			key,
		})
	}
}

/// Read a LEB128 unsigned integer (AV1 spec 4.10.5). Returns (value, bytes consumed).
fn read_leb128(data: &[u8], pos: usize) -> (usize, usize) {
	let mut value: usize = 0;
	let mut len = 0usize;
	for i in 0..8 {
		if pos + i >= data.len() {
			break;
		}
		let b = data[pos + i];
		value |= ((b & 0x7f) as usize) << (7 * i);
		len += 1;
		if (b & 0x80) == 0 {
			break;
		}
	}
	(value, len)
}

/// Scan a reassembled Temporal Unit's OBUs for a sequence-header OBU (type 1) and a frame
/// (FRAME=6 / FRAME_HEADER=3). Returns (has_seq_header, has_frame). Walks OBU headers by their
/// LEB128 size fields (low-overhead bitstream).
fn inspect_av1_tu(tu: &[u8]) -> (bool, bool) {
	const OBU_SEQUENCE_HEADER: u8 = 1;
	const OBU_FRAME_HEADER: u8 = 3;
	const OBU_FRAME: u8 = 6;
	let mut has_seq_header = false;
	let mut has_frame = false;
	let mut p = 0usize;
	while p < tu.len() {
		let b = tu[p];
		// obu_header: forbidden(1) type(4) ext(1) has_size(1) reserved(1)
		let obu_type = (b >> 3) & 0x0f;
		let has_ext = (b & 0x04) != 0;
		let has_size = (b & 0x02) != 0;
		let mut q = p + 1;
		if has_ext {
			q += 1; // obu_extension_header
		}
		if q > tu.len() {
			break; // truncated OBU header — bail safely
		}
		let payload_len: usize = if has_size {
			let (val, len) = read_leb128(tu, q);
			q += len;
			val
		} else {
			tu.len() - q // no size field → rest of the TU
		};
		if obu_type == OBU_SEQUENCE_HEADER {
			has_seq_header = true;
		} else if obu_type == OBU_FRAME || obu_type == OBU_FRAME_HEADER {
			has_frame = true;
		}
		if q + payload_len > tu.len() {
			break;
		}
		p = q + payload_len;
		if !has_size && payload_len == 0 {
			break; // guard against a no-size trailing OBU loop
		}
	}
	(has_seq_header, has_frame)
}

/// Bind `0.0.0.0:<port>` (large SO_RCVBUF if easy), receive RTP, depacketize, and push completed
/// `AccessUnit`s into `sink`. Blocks; run on a thread. Stops when `stop` is set. Loss handling
/// (seq gaps → await keyframe) lives in `Depacketizer`.
pub fn recv_loop(
	port: u16,
	codec: Codec,
	mut sink: impl FnMut(AccessUnit),
	stop: &std::sync::atomic::AtomicBool,
) {
	use std::net::UdpSocket;
	use std::sync::atomic::Ordering;
	use std::time::Duration;

	// BIG receive buffer: the OS default (64 KiB on Windows) overflows on IDR bursts
	// at high fps — the depacketizer then waits for a keyframe that never arrives
	// whole. 4 MiB matches the app's node socket (pulsar-core node.rs).
	//
	// Retry on EADDRINUSE for up to ~800 ms: on a codec/monitor switch the orchestrator
	// (respawn_render_for_codec) kills the old renderer BEFORE spawning this one
	// (kill+wait via stop_render_child), so the port should already be free. But there
	// can be a brief OS-level delay between TerminateProcess returning and the kernel
	// fully releasing the bound UDP socket, and on some Windows versions a killed-but-
	// not-yet-reaped child still holds the port for a tick. Retrying here (instead of
	// returning immediately) turns a transient race into a non-event rather than a
	// permanent black screen for the rest of the session.
	let sock = 'bind: {
		let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
		let mut last_err = None;
		for attempt in 0..8u32 {
			if attempt > 0 {
				std::thread::sleep(Duration::from_millis(100));
			}
			if stop.load(Ordering::SeqCst) {
				return; // session torn down while we were waiting — don't spin longer
			}
			match (|| -> std::io::Result<UdpSocket> {
				let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
				let _ = s.set_recv_buffer_size(4 * 1024 * 1024);
				s.bind(&addr.into())?;
				Ok(s.into())
			})() {
				Ok(s) => break 'bind s,
				Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
					eprintln!(
						"pulsar-render(win): rtp bind 0.0.0.0:{port} EADDRINUSE (attempt {attempt}), retrying…"
					);
					last_err = Some(e);
				}
				Err(e) => {
					eprintln!("pulsar-render(win): rtp bind 0.0.0.0:{port} failed: {e}");
					return;
				}
			}
		}
		eprintln!(
			"pulsar-render(win): rtp bind 0.0.0.0:{port} still busy after retries: {}",
			last_err.unwrap()
		);
		return;
	};
	// Short read timeout so `stop` is honored promptly between datagrams AND the reorder
	// buffer's time-based release runs while the socket is quiet (a hole with nothing
	// arriving behind it must still be skipped after `max_delay`).
	let _ = sock.set_read_timeout(Some(Duration::from_millis(20)));

	let mut depacketizer = Depacketizer::new(codec);
	let mut reorder = Reorder::new(REORDER_CAP);
	let mut buf = [0u8; 65536];
	while !stop.load(Ordering::SeqCst) {
		match sock.recv(&mut buf) {
			Ok(n) => {
				for p in reorder.push(buf[..n].to_vec(), Instant::now(), max_delay()) {
					if let Some(au) = depacketizer.push(&p) {
						sink(au);
					}
				}
			}
			Err(e) => match e.kind() {
				std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
					for p in reorder.poll(Instant::now(), max_delay()) {
						if let Some(au) = depacketizer.push(&p) {
							sink(au);
						}
					}
					continue;
				}
				_ => {
					eprintln!("pulsar-render(win): rtp recv error: {e}");
					break;
				}
			},
		}
	}
}

// ===========================================================================================
#[cfg(test)]
mod tests {
	use super::*;

	// --- RTP packet builder (12-byte header, no CSRC/extension) ---
	fn rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
		let mut p = Vec::with_capacity(12 + payload.len());
		p.push(0x80); // V=2, P=0, X=0, CC=0
		p.push(if marker { 0x80 } else { 0x00 }); // M + PT=0
		p.push((seq >> 8) as u8);
		p.push(seq as u8);
		p.extend_from_slice(&ts.to_be_bytes());
		p.extend_from_slice(&[0, 0, 0, 0]); // SSRC
		p.extend_from_slice(payload);
		p
	}

	fn leb128(mut v: usize) -> Vec<u8> {
		let mut out = Vec::new();
		loop {
			let mut b = (v & 0x7f) as u8;
			v >>= 7;
			if v != 0 {
				b |= 0x80;
			}
			out.push(b);
			if v == 0 {
				break;
			}
		}
		out
	}

	#[test]
	fn h264_fu_a_three_fragments() {
		// Original NAL: header (type 5 = IDR, nri=3 → 0x65) + payload bytes.
		let nal_header = 0x65u8;
		let body: Vec<u8> = (0..30u8).collect();
		// Split body across 3 FU-A fragments.
		let f0 = &body[0..10];
		let f1 = &body[10..20];
		let f2 = &body[20..30];
		let nri = nal_header & 0x60; // 0x60
		let orig = nal_header & 0x1f; // 5

		let mut d = Depacketizer::new(Codec::H264);
		// Start fragment.
		let mut p0 = vec![0x60 | 28u8, 0x80 | orig]; // FU indicator (nri|28), FU header S=1
		p0.extend_from_slice(f0);
		assert!(d.push(&rtp(1, 9000, false, &p0)).is_none());
		// Middle.
		let mut p1 = vec![nri | 28u8, orig];
		p1.extend_from_slice(f1);
		assert!(d.push(&rtp(2, 9000, false, &p1)).is_none());
		// End fragment with marker.
		let mut p2 = vec![nri | 28u8, 0x40 | orig]; // E=1
		p2.extend_from_slice(f2);
		let au = d.push(&rtp(3, 9000, true, &p2)).expect("AU on marker");

		// Annex-B: start code + reconstructed NAL header + full body.
		let mut expect = vec![0, 0, 0, 1, nal_header];
		expect.extend_from_slice(&body);
		assert_eq!(au.data, expect);
		assert_eq!(au.pts_90k, 9000);
		assert!(au.key, "IDR (type 5) → key");
	}

	#[test]
	fn h264_single_nal_and_stap_a() {
		let mut d = Depacketizer::new(Codec::H264);
		// STAP-A bundling an SPS (type 7) and PPS (type 8).
		let sps = [0x67u8, 0x42, 0x00, 0x1e];
		let pps = [0x68u8, 0xce, 0x3c, 0x80];
		let mut stap = vec![24u8];
		stap.push((sps.len() >> 8) as u8);
		stap.push(sps.len() as u8);
		stap.extend_from_slice(&sps);
		stap.push((pps.len() >> 8) as u8);
		stap.push(pps.len() as u8);
		stap.extend_from_slice(&pps);
		let au = d.push(&rtp(1, 100, true, &stap)).expect("AU");
		let mut expect = vec![0, 0, 0, 1];
		expect.extend_from_slice(&sps);
		expect.extend_from_slice(&[0, 0, 0, 1]);
		expect.extend_from_slice(&pps);
		assert_eq!(au.data, expect);
		assert!(au.key, "SPS (type 7) → key");
	}

	#[test]
	fn hevc_fu_reassembly() {
		// IDR_W_RADL = 19 (key). 2-byte HEVC header reconstructed from FU header + PayloadHdr.
		let fu_type = 19u8;
		let layer_id = 0u8;
		let tid = 1u8; // nuh_temporal_id_plus1
				 // FU PayloadHdr bytes: byte0 carries nal_type=49 in bits 6..1.
		let ph0 = (49u8 << 1) | ((layer_id >> 5) & 0x01);
		let ph1 = ((layer_id & 0x1f) << 3) | (tid & 0x07);
		let body: Vec<u8> = (50..80u8).collect();

		let mut d = Depacketizer::new(Codec::H265);
		// Start.
		let mut p0 = vec![ph0, ph1, 0x80 | fu_type];
		p0.extend_from_slice(&body[0..15]);
		assert!(d.push(&rtp(10, 7, false, &p0)).is_none());
		// End + marker.
		let mut p1 = vec![ph0, ph1, 0x40 | fu_type];
		p1.extend_from_slice(&body[15..30]);
		let au = d.push(&rtp(11, 7, true, &p1)).expect("AU");

		// Reconstructed 2-byte header.
		let h0 = (fu_type << 1) | ((layer_id >> 5) & 0x01);
		let h1 = ((layer_id & 0x1f) << 3) | (tid & 0x07);
		let mut expect = vec![0, 0, 0, 1, h0, h1];
		expect.extend_from_slice(&body);
		assert_eq!(au.data, expect);
		assert!(au.key, "IDR_W_RADL (19) → key");
	}

	#[test]
	fn av1_w0_and_w2_with_continuation() {
		// Build a TU: a sequence-header OBU (type 1) + a frame OBU (type 6).
		// OBU header: forbidden0 type(4) ext0 has_size1 reserved0.
		let seq_obu_payload = [0xaau8, 0xbb];
		let mut seq_obu = vec![(1u8 << 3) | 0x02]; // type=1, has_size=1
		seq_obu.extend_from_slice(&leb128(seq_obu_payload.len()));
		seq_obu.extend_from_slice(&seq_obu_payload);

		let frame_payload: Vec<u8> = (0..20u8).collect();
		let mut frame_obu = vec![(6u8 << 3) | 0x02]; // type=6, has_size=1
		frame_obu.extend_from_slice(&leb128(frame_payload.len()));
		frame_obu.extend_from_slice(&frame_payload);

		let full_tu: Vec<u8> = seq_obu.iter().chain(frame_obu.iter()).copied().collect();

		// --- W=2 single packet: first element LEB-prefixed, last runs to end. N=1 (new seq). ---
		{
			let mut d = Depacketizer::new(Codec::Av1);
			let agg = (2u8 << 4) | 0x08; // W=2, N=1
			let mut pl = vec![agg];
			pl.extend_from_slice(&leb128(seq_obu.len()));
			pl.extend_from_slice(&seq_obu);
			pl.extend_from_slice(&frame_obu); // last element, no prefix
			let au = d.push(&rtp(1, 42, true, &pl)).expect("AU");
			assert_eq!(au.data, full_tu);
			assert_eq!(au.pts_90k, 42);
			assert!(au.key, "seq header + frame → key");
		}

		// --- W=0 across two packets with Z/Y continuation. ---
		{
			let mut d = Depacketizer::new(Codec::Av1);
			// Packet 1: W=0, all elements LEB-prefixed. Carry the seq OBU whole, then half the
			// frame OBU as a length-prefixed element with Y=1 (continues into next packet).
			let frame_first = &frame_obu[0..5];
			let frame_rest = &frame_obu[5..];
			let agg1 = 0x40 | 0x08; // Y=1, W=0, N=1
			let mut p1 = vec![agg1];
			p1.extend_from_slice(&leb128(seq_obu.len()));
			p1.extend_from_slice(&seq_obu);
			p1.extend_from_slice(&leb128(frame_first.len()));
			p1.extend_from_slice(frame_first);
			assert!(d.push(&rtp(5, 99, false, &p1)).is_none());

			// Packet 2: W=0, Z=1 (first element continues prev OBU). Marker ends the TU.
			let agg2 = 0x80; // Z=1, W=0
			let mut p2 = vec![agg2];
			p2.extend_from_slice(&leb128(frame_rest.len()));
			p2.extend_from_slice(frame_rest);
			let au = d.push(&rtp(6, 99, true, &p2)).expect("AU");
			assert_eq!(au.data, full_tu, "continuation stitched back to whole TU");
			assert!(au.key);
		}
	}

	#[test]
	fn seq_gap_awaits_keyframe() {
		let mut d = Depacketizer::new(Codec::H264);
		// Prime the depacketizer with a keyframe so awaiting_idr is cleared and last_seq is
		// established (rtp-3: new depacketizer starts with awaiting_idr=true).
		let idr0 = [0x65u8, 0xff]; // type=5 (IDR) → key
		assert!(d.push(&rtp(99, 0, true, &idr0)).is_some(), "initial IDR clears awaiting_idr");
		// Now a clean delta AU (type 1) so last_seq advances.
		let delta = [0x41u8, 1, 2, 3]; // nri=2, type=1
		let au = d.push(&rtp(100, 1, true, &delta)).expect("first AU");
		assert!(!au.key);

		// Now SKIP seq 101 → gap. seq 102 is a delta NAL: must be dropped (awaiting IDR).
		let delta2 = [0x41u8, 4, 5, 6];
		assert!(
			d.push(&rtp(102, 2, true, &delta2)).is_none(),
			"delta after a gap is dropped until a keyframe"
		);

		// seq 103: still a delta → still dropped.
		let delta3 = [0x41u8, 7, 8];
		assert!(d.push(&rtp(103, 3, true, &delta3)).is_none());

		// seq 104: an IDR (type 5) → resumes.
		let idr = [0x65u8, 9, 10];
		let au = d.push(&rtp(104, 4, true, &idr)).expect("resumes on key");
		assert!(au.key);
		assert_eq!(au.data, vec![0, 0, 0, 1, 0x65, 9, 10]);
	}

	#[test]
	fn dup_and_stale_retransmit_dropped() {
		// Mirrors mobile/src/rtp.rs::stale_retransmit_dropped_but_restart_accepted: a duplicate
		// or a late (backward) NACK retransmit must be dropped without emitting an AU and without
		// disturbing the assembly of the next frame.
		let mut d = Depacketizer::new(Codec::H264);
		// Prime with an IDR (clears awaiting_idr, establishes last_seq=100).
		let idr = [0x65u8, 0xaa];
		assert!(d.push(&rtp(100, 0, true, &idr)).is_some(), "initial IDR");
		// A clean delta AU advances last_seq to 101.
		let delta = [0x41u8, 1, 2, 3];
		let au = d.push(&rtp(101, 1, true, &delta)).expect("delta AU");
		assert!(!au.key);

		// DUPLICATE of seq 101 (fwd==0): dropped, no AU, state untouched.
		assert!(
			d.push(&rtp(101, 1, true, &delta)).is_none(),
			"duplicate packet dropped, not re-emitted"
		);
		// STALE/backward NACK retransmit (seq 99 < 101 → fwd>=0x8000): dropped, not spliced, and
		// must NOT trigger a gap (awaiting_idr stays clear).
		let stale = [0x41u8, 9, 9];
		assert!(
			d.push(&rtp(99, 0, true, &stale)).is_none(),
			"stale retransmit dropped"
		);

		// The next genuine forward delta (seq 102) is unaffected — last_seq is still 101, so 102
		// is consecutive (no gap) and emits a clean AU.
		let delta2 = [0x41u8, 4, 5, 6];
		let au = d.push(&rtp(102, 2, true, &delta2)).expect("next AU unaffected");
		assert!(!au.key);
		assert_eq!(au.data, vec![0, 0, 0, 1, 0x41, 4, 5, 6]);
	}

	#[test]
	fn stale_packet_not_spliced_into_open_au() {
		let mut d = Depacketizer::new(Codec::H264);
		// Prime with an IDR so we are not awaiting a keyframe (last_seq=50).
		assert!(d.push(&rtp(50, 0, true, &[0x65u8, 0x00])).is_some());
		// Begin a 2-NAL delta AU (first packet has no marker), seq 51.
		let n1 = [0x41u8, 1, 1];
		assert!(
			d.push(&rtp(51, 5, false, &n1)).is_none(),
			"first NAL — AU still open"
		);
		// A STALE retransmit (seq 40, backward) arrives mid-assembly: it must be DROPPED, not
		// appended to the open AU.
		let stale = [0x41u8, 7, 7, 7];
		assert!(
			d.push(&rtp(40, 0, false, &stale)).is_none(),
			"stale packet dropped mid-AU"
		);
		// Second NAL + marker closes the AU (seq 52 — consecutive after 51).
		let n2 = [0x41u8, 2, 2];
		let au = d.push(&rtp(52, 5, true, &n2)).expect("AU closes on marker");
		// The AU contains ONLY n1 and n2 — the stale packet's bytes are absent.
		let expect = vec![0, 0, 0, 1, 0x41, 1, 1, 0, 0, 0, 1, 0x41, 2, 2];
		assert_eq!(au.data, expect, "stale bytes not spliced into the AU");
	}
}

// ===========================================================================================
#[cfg(test)]
mod reorder_tests {
	use super::*;

	fn pkt(seq: u16) -> Vec<u8> {
		let mut p = vec![0x80, 0x60, (seq >> 8) as u8, seq as u8, 0, 0, 0, 0, 0, 0, 0, 1];
		p.push(seq as u8); // payload marker so we can tell packets apart
		p
	}
	fn seqs(v: &[Vec<u8>]) -> Vec<u16> {
		v.iter().map(|p| ((p[2] as u16) << 8) | p[3] as u16).collect()
	}
	const MD: Duration = Duration::from_millis(100);

	#[test]
	fn in_order_is_passthrough_with_no_buffering() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		for s in 0u16..50 {
			let out = r.push(pkt(s), t, MD);
			assert_eq!(seqs(&out), vec![s]);
			assert_eq!(r.pending(), 0);
		}
	}

	#[test]
	fn gap_waits_for_the_retransmit_then_releases_in_order() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		assert_eq!(seqs(&r.push(pkt(10), t, MD)), vec![10]);
		// 11 is lost; 12..15 arrive and are held.
		for s in 12u16..=15 {
			assert!(r.push(pkt(s), t + Duration::from_millis(5), MD).is_empty());
		}
		assert_eq!(r.pending(), 4);
		// Nothing released while the wait is still running.
		assert!(r.poll(t + Duration::from_millis(60), MD).is_empty());
		// The NACK retransmit lands at +80 ms: the whole run comes out in sequence.
		let out = r.push(pkt(11), t + Duration::from_millis(80), MD);
		assert_eq!(seqs(&out), vec![11, 12, 13, 14, 15]);
		assert_eq!(r.pending(), 0);
		assert_eq!(seqs(&r.push(pkt(16), t + Duration::from_millis(81), MD)), vec![16]);
	}

	#[test]
	fn gap_times_out_then_skips_and_the_late_packet_passes_as_stale() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		r.push(pkt(0), t, MD);
		assert!(r.push(pkt(2), t, MD).is_empty());
		assert!(r.push(pkt(3), t + Duration::from_millis(10), MD).is_empty());
		// max_delay elapsed for the head (2): skip the hole, release 2,3.
		let out = r.poll(t + MD, MD);
		assert_eq!(seqs(&out), vec![2, 3]);
		// The retransmit of 1 arrives too late: passed through (the depacketizer drops it).
		assert_eq!(seqs(&r.push(pkt(1), t + Duration::from_millis(150), MD)), vec![1]);
		// And the stream continues in order behind the skip.
		assert_eq!(seqs(&r.push(pkt(4), t + Duration::from_millis(151), MD)), vec![4]);
	}

	#[test]
	fn a_second_hole_behind_the_first_waits_its_own_turn() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		r.push(pkt(0), t, MD);
		assert!(r.push(pkt(2), t, MD).is_empty()); // hole at 1
		assert!(r.push(pkt(4), t + Duration::from_millis(50), MD).is_empty()); // hole at 3
		// Filling 1 releases 2 only; 4 still waits for 3.
		assert_eq!(seqs(&r.push(pkt(1), t + Duration::from_millis(60), MD)), vec![1, 2]);
		assert_eq!(r.pending(), 1);
		// 4 arrived at +50 ms → released past its hole at +150 ms, not before.
		assert!(r.poll(t + Duration::from_millis(140), MD).is_empty());
		assert_eq!(seqs(&r.poll(t + Duration::from_millis(150), MD)), vec![4]);
	}

	#[test]
	fn buffer_cap_flushes_past_the_hole() {
		let mut r = Reorder::new(8);
		let t = Instant::now();
		r.push(pkt(0), t, MD);
		let mut out = Vec::new();
		for s in 2u16..=9 {
			out.extend(r.push(pkt(s), t, MD));
		}
		assert_eq!(seqs(&out), (2u16..=9).collect::<Vec<_>>(), "8 held packets → flushed");
		assert_eq!(r.pending(), 0);
	}

	#[test]
	fn sequence_wraparound_is_ordered_correctly() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		r.push(pkt(65533), t, MD);
		r.push(pkt(65534), t, MD);
		// 65535 lost; 0, 1 arrive (across the wrap) and wait; the retransmit releases all.
		assert!(r.push(pkt(0), t, MD).is_empty());
		assert!(r.push(pkt(1), t, MD).is_empty());
		assert_eq!(seqs(&r.push(pkt(65535), t, MD)), vec![65535, 0, 1]);
	}

	#[test]
	fn duplicates_of_held_packets_are_absorbed_and_stale_ones_pass_through() {
		let mut r = Reorder::new(REORDER_CAP);
		let t = Instant::now();
		r.push(pkt(5), t, MD);
		assert!(r.push(pkt(7), t, MD).is_empty());
		assert!(r.push(pkt(7), t, MD).is_empty(), "duplicate of a held packet");
		assert_eq!(r.pending(), 1);
		assert_eq!(seqs(&r.push(pkt(3), t, MD)), vec![3], "stale packet passes through");
		assert_eq!(seqs(&r.push(pkt(6), t, MD)), vec![6, 7]);
	}
}
