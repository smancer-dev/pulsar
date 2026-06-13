// RTP/Opus → WebCodecs AudioDecoder → WebAudio player for the embedded session.
//
// The host sends Opus over RTP/UDP; the Rust viewer relays each datagram over a
// loopback WebSocket. One RTP packet carries one Opus frame (RFC 7587), so we
// strip the RTP header, decode with WebCodecs `AudioDecoder`, and schedule the PCM
// into a WebAudio `AudioContext` with a small jitter buffer. Best-effort: if the
// webview lacks `AudioDecoder` we degrade to silence (the video still plays).

import { t } from '$lib/i18n.svelte';

export type AudioSink = {
	/** Feed one RTP packet. */
	push: (pkt: Uint8Array) => void;
	/** Tear down the decoder + audio context. */
	close: () => void;
};

const SAMPLE_RATE = 48000;
// Stereo is the negotiated default (the host's StreamReq asks for `ChannelLayout::Stereo`
// and the SDP rtpmap is `opus/48000/2` per RFC 7587). A surround stream (5.1/7.1) carries
// its real channel count in-band via the Opus mapping_family-1 header, so the decoder reports
// it on each AudioData; we never clamp to this value, it only seeds the initial config.
const DEFAULT_CHANNELS = 2;

/**
 * Start decoding RTP/Opus packets to the speakers. Returns a sink you push raw RTP
 * packets into. `onError` surfaces setup/decode problems (e.g. no AudioDecoder).
 *
 * `channels` is the negotiated channel count (2 / 6 / 8). It only seeds the
 * `AudioDecoder.configure` call — the decoder still reports the stream's true channel
 * count on each decoded `AudioData`, and we honour THAT, so a surround stream is never
 * folded down to stereo even if the negotiated hint was wrong. Defaults to stereo, which
 * keeps the existing (stereo-only) call sites byte-for-byte identical.
 *
 * NOTE: this WebAudio path is effectively dormant — the native renderer (pulsar-render) is
 * now universal and `start_remote_play` returns `audio_ws_port: 0` whenever the native
 * ffmpeg→PulseAudio player runs, so the webview never opens the audio socket on Linux. It
 * can still fire on a WebKit/WebView2 build that decodes Opus in-webview; the surround-correct
 * change below is minimal and keeps stereo behaviour identical there.
 */
export function startOpusAudio(
	onError?: (msg: string) => void,
	channels = DEFAULT_CHANNELS
): AudioSink {
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	const W = window as any;
	if (!W.AudioDecoder || !W.EncodedAudioChunk || !(W.AudioContext || W.webkitAudioContext)) {
		onError?.(t('audio.noWebcodecs'));
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
			// Render the decoder's actual channel count — NOT a clamp to stereo. For a 5.1/7.1
			// stream the Opus decoder reports 6/8 here, and WebAudio handles >2ch buffers, so
			// surround is delivered un-folded. Falls back to the negotiated hint if unreported.
			const ch = data.numberOfChannels || channels;
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
		error: (e: any) => onError?.(t('audio.decodeErr') + e)
	});

	try {
		decoder.configure({ codec: 'opus', sampleRate: SAMPLE_RATE, numberOfChannels: channels });
	} catch (e) {
		onError?.(t('audio.configErr') + e);
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
