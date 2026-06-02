//! Streaming pipeline: capture → encode → transport → decode → present.
//!
//! The pipeline is expressed as traits so the heavy, platform-specific pieces
//! (GPU screen capture + hardware H.264/H.265/AV1 encoders, and the matching
//! decoders) can plug in without touching the rest of the app. A lossless
//! software baseline ([`RleEncoder`]/[`RleDecoder`] + [`SolidColorSource`]) ships
//! so the whole path is exercised by tests today; production builds swap in the
//! GPU backends (NVENC / AMF / QuickSync / VideoToolbox / VAAPI).
//!
//! Frames travel as [`EncodedPacket`]s, which serialize straight onto a
//! [`crate::connection::Session`] (already encrypted end-to-end).

use serde::{Deserialize, Serialize};

/// The video codec a packet was produced with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
	/// Uncompressed RGBA (debugging only).
	Raw,
	/// The lossless software baseline shipped here.
	Rle,
	/// Hardware codecs — wired up by the platform backends.
	H264,
	H265,
	Av1,
}

/// A decoded RGBA frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFrame {
	pub width: u16,
	pub height: u16,
	pub pts_micros: u64,
	/// `width * height * 4` bytes, RGBA8.
	pub rgba: Vec<u8>,
}

impl RawFrame {
	pub fn solid(width: u16, height: u16, pts_micros: u64, rgba: [u8; 4]) -> Self {
		let mut buf = Vec::with_capacity(width as usize * height as usize * 4);
		for _ in 0..(width as usize * height as usize) {
			buf.extend_from_slice(&rgba);
		}
		Self {
			width,
			height,
			pts_micros,
			rgba: buf,
		}
	}
}

/// An encoded video packet, ready to seal + send over a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedPacket {
	pub codec: Codec,
	pub key_frame: bool,
	pub width: u16,
	pub height: u16,
	pub pts_micros: u64,
	pub data: Vec<u8>,
}

/// Captures frames from a display/window.
pub trait FrameSource: Send {
	fn next_frame(&mut self) -> Option<RawFrame>;
}

/// Compresses raw frames.
pub trait VideoEncoder: Send {
	fn codec(&self) -> Codec;
	fn encode(&mut self, frame: &RawFrame) -> EncodedPacket;
}

/// Decompresses packets.
pub trait VideoDecoder: Send {
	fn decode(&mut self, packet: &EncodedPacket) -> Option<RawFrame>;
}

/// Presents decoded frames (e.g. to a Tauri window / GPU surface).
pub trait FrameSink: Send {
	fn present(&mut self, frame: &RawFrame);
}

/// Rolling stream metrics surfaced to the UI (fps / latency / bitrate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStats {
	pub fps: u32,
	pub latency_ms: u32,
	pub bitrate_kbps: u32,
}

// ----------------------------------------------------------------------------
// Software baseline
// ----------------------------------------------------------------------------

/// A synthetic source that emits solid-color frames (for tests / smoke runs).
pub struct SolidColorSource {
	width: u16,
	height: u16,
	frames: u32,
	emitted: u32,
	pts: u64,
	frame_interval_micros: u64,
}

impl SolidColorSource {
	pub fn new(width: u16, height: u16, frames: u32, fps: u32) -> Self {
		Self {
			width,
			height,
			frames,
			emitted: 0,
			pts: 0,
			frame_interval_micros: if fps == 0 { 16_666 } else { 1_000_000 / fps as u64 },
		}
	}
}

impl FrameSource for SolidColorSource {
	fn next_frame(&mut self) -> Option<RawFrame> {
		if self.emitted >= self.frames {
			return None;
		}
		let v = (self.emitted % 256) as u8;
		let frame = RawFrame::solid(self.width, self.height, self.pts, [v, 64, 128, 255]);
		self.emitted += 1;
		self.pts += self.frame_interval_micros;
		Some(frame)
	}
}

/// Lossless byte run-length encoder — stands in for a real video encoder so the
/// transport path can be tested without GPU codecs.
#[derive(Default)]
pub struct RleEncoder;

impl VideoEncoder for RleEncoder {
	fn codec(&self) -> Codec {
		Codec::Rle
	}
	fn encode(&mut self, frame: &RawFrame) -> EncodedPacket {
		EncodedPacket {
			codec: Codec::Rle,
			key_frame: true,
			width: frame.width,
			height: frame.height,
			pts_micros: frame.pts_micros,
			data: rle_encode(&frame.rgba),
		}
	}
}

#[derive(Default)]
pub struct RleDecoder;

impl VideoDecoder for RleDecoder {
	fn decode(&mut self, packet: &EncodedPacket) -> Option<RawFrame> {
		if packet.codec != Codec::Rle {
			return None;
		}
		Some(RawFrame {
			width: packet.width,
			height: packet.height,
			pts_micros: packet.pts_micros,
			rgba: rle_decode(&packet.data),
		})
	}
}

/// Collects presented frames (test sink).
#[derive(Default)]
pub struct CollectingSink {
	pub frames: Vec<RawFrame>,
}

impl FrameSink for CollectingSink {
	fn present(&mut self, frame: &RawFrame) {
		self.frames.push(frame.clone());
	}
}

/// Pixel-level (4-byte RGBA) run-length encoding: `[run, r, g, b, a]` per run.
/// Operating on whole pixels (rather than bytes) means solid/flat regions of a
/// frame actually compress.
fn rle_encode(data: &[u8]) -> Vec<u8> {
	let pixels: Vec<&[u8]> = data.chunks_exact(4).collect();
	let mut out = Vec::new();
	let mut i = 0;
	while i < pixels.len() {
		let px = pixels[i];
		let mut run = 1usize;
		while i + run < pixels.len() && pixels[i + run] == px && run < 255 {
			run += 1;
		}
		out.push(run as u8);
		out.extend_from_slice(px);
		i += run;
	}
	out
}

fn rle_decode(data: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	for chunk in data.chunks_exact(5) {
		let run = chunk[0];
		let px = &chunk[1..5];
		for _ in 0..run {
			out.extend_from_slice(px);
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn solid_source_emits_the_requested_count() {
		let mut src = SolidColorSource::new(16, 16, 5, 60);
		let mut n = 0;
		while let Some(f) = src.next_frame() {
			assert_eq!(f.rgba.len(), 16 * 16 * 4);
			n += 1;
		}
		assert_eq!(n, 5);
	}

	#[test]
	fn rle_round_trips_a_frame_losslessly() {
		let frame = RawFrame::solid(64, 64, 1234, [10, 20, 30, 255]);
		let mut enc = RleEncoder;
		let mut dec = RleDecoder;
		let packet = enc.encode(&frame);
		assert_eq!(packet.codec, Codec::Rle);
		// A solid frame compresses hugely.
		assert!(packet.data.len() < frame.rgba.len());
		let back = dec.decode(&packet).unwrap();
		assert_eq!(back, frame);
	}

	#[test]
	fn packet_serializes_for_the_wire() {
		let frame = RawFrame::solid(8, 8, 0, [1, 2, 3, 4]);
		let packet = RleEncoder.encode(&frame);
		let bytes = serde_json::to_vec(&packet).unwrap();
		let back: EncodedPacket = serde_json::from_slice(&bytes).unwrap();
		assert_eq!(back, packet);
	}

	#[test]
	fn full_pipeline_capture_encode_decode_present() {
		let mut src = SolidColorSource::new(32, 18, 4, 60);
		let mut enc = RleEncoder;
		let mut dec = RleDecoder;
		let mut sink = CollectingSink::default();
		while let Some(frame) = src.next_frame() {
			let packet = enc.encode(&frame);
			let decoded = dec.decode(&packet).unwrap();
			sink.present(&decoded);
		}
		assert_eq!(sink.frames.len(), 4);
		assert_eq!(sink.frames[0].width, 32);
	}

	#[test]
	fn decoder_rejects_foreign_codecs() {
		let packet = EncodedPacket {
			codec: Codec::H265,
			key_frame: true,
			width: 1,
			height: 1,
			pts_micros: 0,
			data: vec![],
		};
		assert!(RleDecoder.decode(&packet).is_none());
	}
}
