//! Media-over-session framing: RTP datagrams (video + audio) carried INSIDE the
//! encrypted session instead of as separate plain UDP flows to extra ports.
//!
//! Why: one external UDP socket per device — the session's hole-punched/relayed
//! path is the ONLY hole anyone needs (symmetric NAT then works via the relay,
//! and self-hosters open exactly one port). The media also becomes end-to-end
//! encrypted for free (it rides the session's ChaCha20-Poly1305 seal).
//!
//! Framing: one session payload per RTP datagram, `[tag][rtp…]`. The service's
//! control messages are JSON (first byte `{` = 0x7B), so a 0x01/0x02 tag byte can
//! never collide with them. Loss/reorder semantics are unchanged — the session
//! transport is still plain UDP underneath, RTP's own seq/jitter handling stays
//! in charge (plus the optional NACK retransmit, see `DataMsg::MediaNack`).

/// Tag byte: a video RTP datagram follows.
pub const TAG_VIDEO: u8 = 0x01;
/// Tag byte: an audio (Opus) RTP datagram follows.
pub const TAG_AUDIO: u8 = 0x02;

/// Feature ids a host advertises in `StreamCaps::features`:
/// `mos` = media-over-session supported; `nack` = it honors `MediaNack` retransmits.
pub const FEAT_MOS: &str = "mos";
pub const FEAT_NACK: &str = "nack";

/// Build one media session-payload: `[tag][rtp…]`.
pub fn frame(tag: u8, rtp: &[u8]) -> Vec<u8> {
	let mut v = Vec::with_capacity(1 + rtp.len());
	v.push(tag);
	v.extend_from_slice(rtp);
	v
}

/// Parse a session payload as a media frame; `None` for anything else (JSON
/// control messages, junk). Empty RTP is rejected.
pub fn parse(payload: &[u8]) -> Option<(u8, &[u8])> {
	match payload.split_first() {
		Some((&tag @ (TAG_VIDEO | TAG_AUDIO), rest)) if !rest.is_empty() => Some((tag, rest)),
		_ => None,
	}
}

/// The RTP sequence number of a datagram (bytes 2..4, big-endian), used for the
/// client's gap detection (NACK + loss accounting). `None` if too short.
pub fn rtp_seq(rtp: &[u8]) -> Option<u16> {
	if rtp.len() < 4 {
		return None;
	}
	Some(u16::from_be_bytes([rtp[2], rtp[3]]))
}

/// Forward-distance from `a` to `b` in u16 sequence space (wrap-aware): how many
/// steps forward `b` is from `a`. Values ≥ 0x8000 mean `b` is actually BEHIND `a`
/// (an old/reordered packet).
pub fn seq_forward(a: u16, b: u16) -> u16 {
	b.wrapping_sub(a)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn frame_parse_roundtrip() {
		let rtp = [0x80u8, 96, 0x12, 0x34, 0, 0, 0, 0];
		let f = frame(TAG_VIDEO, &rtp);
		let (tag, body) = parse(&f).expect("parses");
		assert_eq!(tag, TAG_VIDEO);
		assert_eq!(body, &rtp);
	}

	#[test]
	fn parse_rejects_control_and_junk() {
		assert!(parse(b"{\"Ping\":null}").is_none(), "JSON is not media");
		assert!(parse(&[]).is_none());
		assert!(parse(&[TAG_VIDEO]).is_none(), "empty RTP rejected");
		assert!(parse(&[0x07, 1, 2, 3]).is_none(), "unknown tag rejected");
	}

	#[test]
	fn rtp_seq_and_wraparound() {
		let rtp = [0x80u8, 96, 0xFF, 0xFE, 0, 0, 0, 0];
		assert_eq!(rtp_seq(&rtp), Some(0xFFFE));
		assert_eq!(rtp_seq(&[1, 2, 3]), None);
		assert_eq!(seq_forward(0xFFFE, 0x0001), 3, "wraps forward");
		assert!(seq_forward(5, 2) >= 0x8000, "behind reads as huge forward");
	}
}
