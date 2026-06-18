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
//! never reaches the decoder.

#![allow(dead_code)]

use super::{AccessUnit, Codec};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

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

		// Sequence-gap detection: forward distance only (ignore reorder/dupes). On a gap drop
		// partial state and wait for the next keyframe.
		if let Some(last) = self.last_seq {
			let fwd = seq.wrapping_sub(last);
			// fwd==1: consecutive (normal). fwd==0: duplicate. fwd>=0x8000: backward/reorder.
			// Only trigger a gap and advance last_seq for a genuine forward packet.
			if fwd < 0x8000 {
				// Forward packet (newer than last seen).
				if fwd != 1 && fwd != 0 {
					// Gap: one or more sequence numbers were skipped.
					self.awaiting_idr = true;
					self.drop_partial();
				}
				self.last_seq = Some(seq);
			}
			// else: reordered / duplicate — leave last_seq unchanged, no gap triggered.
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
	// Short read timeout so `stop` is honored promptly between datagrams.
	let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));

	let mut depacketizer = Depacketizer::new(codec);
	let mut buf = [0u8; 65536];
	while !stop.load(Ordering::SeqCst) {
		match sock.recv(&mut buf) {
			Ok(n) => {
				if let Some(au) = depacketizer.push(&buf[..n]) {
					sink(au);
				}
			}
			Err(e) => match e.kind() {
				std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => continue,
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
}
