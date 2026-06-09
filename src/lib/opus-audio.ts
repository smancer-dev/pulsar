// RTP/Opus → WebCodecs AudioDecoder → WebAudio player for the embedded session.
//
// The host sends Opus over RTP/UDP; the Rust viewer relays each datagram over a
// loopback WebSocket. One RTP packet carries one Opus frame (RFC 7587), so we
// strip the RTP header, decode with WebCodecs `AudioDecoder`, and schedule the PCM
// into a WebAudio `AudioContext` with a small jitter buffer. Best-effort: if the
// webview lacks `AudioDecoder` we degrade to silence (the video still plays).

export type AudioSink = {
	/** Feed one RTP packet. */
	push: (pkt: Uint8Array) => void;
	/** Tear down the decoder + audio context. */
	close: () => void;
};

const SAMPLE_RATE = 48000;
const CHANNELS = 2;

/**
 * Start decoding RTP/Opus packets to the speakers. Returns a sink you push raw RTP
 * packets into. `onError` surfaces setup/decode problems (e.g. no AudioDecoder).
 */
export function startOpusAudio(onError?: (msg: string) => void): AudioSink {
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	const W = window as any;
	if (!W.AudioDecoder || !W.EncodedAudioChunk || !(W.AudioContext || W.webkitAudioContext)) {
		onError?.('Bu webview WebCodecs ses çözmeyi desteklemiyor (ses kapalı).');
		return { push: () => {}, close: () => {} };
	}

	const Ctor = W.AudioContext || W.webkitAudioContext;
	const ac = new Ctor({ sampleRate: SAMPLE_RATE });
	let playHead = 0; // next scheduled buffer start time (s)
	let closed = false;
	let ts = 0;

	const decoder = new W.AudioDecoder({
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		output: (data: any) => {
			if (closed) {
				data.close();
				return;
			}
			const frames = data.numberOfFrames;
			const rate = data.sampleRate || SAMPLE_RATE;
			const ch = Math.min(CHANNELS, data.numberOfChannels || CHANNELS);
			const buf = ac.createBuffer(ch, frames, rate);
			for (let c = 0; c < ch; c++) {
				const tmp = new Float32Array(frames);
				try {
					data.copyTo(tmp, { planeIndex: c, format: 'f32-planar' });
				} catch {
					/* some impls only expose interleaved — best effort */
				}
				buf.copyToChannel(tmp, c);
			}
			data.close();
			const src = ac.createBufferSource();
			src.buffer = buf;
			src.connect(ac.destination);
			const now = ac.currentTime;
			// Keep ~60 ms of lead to absorb network jitter; if we've fallen behind
			// (underrun), reset the play head rather than scheduling in the past.
			if (playHead < now + 0.02) playHead = now + 0.06;
			src.start(playHead);
			playHead += buf.duration;
		},
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		error: (e: any) => onError?.('ses çöz: ' + e)
	});

	try {
		decoder.configure({ codec: 'opus', sampleRate: SAMPLE_RATE, numberOfChannels: CHANNELS });
	} catch (e) {
		onError?.('ses ayarı: ' + e);
		return { push: () => {}, close: () => {} };
	}

	function push(pkt: Uint8Array) {
		if (closed || pkt.length < 12) return;
		// Strip the RTP header (12 bytes + CSRC list + optional extension).
		const x = (pkt[0] & 0x10) !== 0;
		const cc = pkt[0] & 0x0f;
		let off = 12 + cc * 4;
		if (x) {
			const el = (pkt[off + 2] << 8) | pkt[off + 3];
			off += 4 + el * 4;
		}
		const payload = pkt.subarray(off);
		if (!payload.length) return;
		// A browser tab can't start audio until a user gesture; the click that opened
		// the session counts, but resume() defensively in case it's suspended.
		if (ac.state === 'suspended') ac.resume().catch(() => {});
		try {
			decoder.decode(new W.EncodedAudioChunk({ type: 'key', timestamp: ts, data: payload }));
			ts += 20000; // ~20 ms; informational (Opus packets are self-describing)
		} catch (e) {
			onError?.('ses parça: ' + e);
		}
	}

	return {
		push,
		close: () => {
			closed = true;
			try {
				decoder.close();
			} catch {
				/* already closed */
			}
			try {
				ac.close();
			} catch {
				/* already closed */
			}
		}
	};
}
