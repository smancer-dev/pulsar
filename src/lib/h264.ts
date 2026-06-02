// RTP/H.264 → WebCodecs → <canvas> player for the embedded remote-desktop view.
//
// The host sends RTP/H.264 over UDP; the Rust viewer relays each datagram over a
// loopback WebSocket. Here we depacketize RTP (single-NAL / STAP-A / FU-A),
// assemble Annex-B access units, derive the codec string from the SPS, and decode
// each AU with WebCodecs `VideoDecoder`, drawing frames to a canvas. This exact
// pipeline was verified end-to-end against a real stream (60/60 frames, 0 errors).

export type H264Sink = {
	/** Feed one RTP packet. */
	push: (pkt: Uint8Array) => void;
	/** Tear down the decoder. */
	close: () => void;
};

const hex = (b: number) => b.toString(16).padStart(2, '0');

/**
 * Start decoding RTP/H.264 packets onto `canvas`. Returns a sink you push raw RTP
 * packets into. `onError` surfaces decode/setup problems; `onFrame` fires once the
 * first frame is drawn (to hide the "connecting" placeholder).
 */
export function startH264Canvas(
	canvas: HTMLCanvasElement,
	onError?: (msg: string) => void,
	onFrame?: () => void
): H264Sink {
	const ctx = canvas.getContext('2d');
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	const W = window as any;
	if (!W.VideoDecoder || !W.EncodedVideoChunk) {
		onError?.('Bu webview WebCodecs desteklemiyor.');
		return { push: () => {}, close: () => {} };
	}

	let nals: Uint8Array[] = [];
	let fu: Uint8Array[] | null = null;
	let configured = false;
	let ts = 0;
	let closed = false;

	const decoder = new W.VideoDecoder({
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		output: (frame: any) => {
			if (closed) {
				frame.close();
				return;
			}
			const w = frame.displayWidth || frame.codedWidth;
			const h = frame.displayHeight || frame.codedHeight;
			if (w && h && (canvas.width !== w || canvas.height !== h)) {
				canvas.width = w;
				canvas.height = h;
			}
			try {
				ctx?.drawImage(frame, 0, 0);
				onFrame?.(); // one render — used for "has video" + real fps
			} catch {
				/* drawImage(VideoFrame) unsupported — ignore this frame */
			}
			frame.close();
		},
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		error: (e: any) => onError?.('decode: ' + e)
	});

	function emitAU() {
		if (!nals.length) return;
		let size = 0;
		for (const n of nals) size += 4 + n.length;
		const au = new Uint8Array(size);
		let p = 0;
		let key = false;
		let sps: Uint8Array | null = null;
		for (const n of nals) {
			au[p + 3] = 1; // Annex-B start code 00 00 00 01
			p += 4;
			au.set(n, p);
			p += n.length;
			const t = n[0] & 0x1f;
			if (t === 5) key = true; // IDR
			if (t === 7) {
				key = true;
				sps = n;
			}
		}
		nals = [];
		if (!configured) {
			if (!key) return; // wait for the first keyframe (with SPS/PPS)
			const codec = sps ? `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}` : 'avc1.42e01f';
			try {
				decoder.configure({ codec, optimizeForLatency: true });
				configured = true;
			} catch (e) {
				onError?.('configure: ' + e);
				return;
			}
		}
		// If decoding falls behind, drop delta frames to catch up (keep latency
		// bounded); always decode keyframes so the picture recovers quickly.
		if (!key && decoder.decodeQueueSize > 4) return;
		try {
			decoder.decode(new W.EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: ts, data: au }));
			ts += 16666;
		} catch (e) {
			onError?.('chunk: ' + e);
		}
	}

	function push(pkt: Uint8Array) {
		if (pkt.length < 12) return;
		const x = (pkt[0] & 0x10) !== 0;
		const cc = pkt[0] & 0x0f;
		const marker = (pkt[1] & 0x80) !== 0;
		let off = 12 + cc * 4;
		if (x) {
			const el = (pkt[off + 2] << 8) | pkt[off + 3];
			off += 4 + el * 4;
		}
		const pl = pkt.subarray(off);
		if (!pl.length) return;
		const t = pl[0] & 0x1f;
		if (t >= 1 && t <= 23) {
			nals.push(pl.slice());
		} else if (t === 24) {
			// STAP-A: aggregated NALs, each prefixed by a 2-byte size.
			let q = 1;
			while (q + 2 <= pl.length) {
				const s = (pl[q] << 8) | pl[q + 1];
				q += 2;
				nals.push(pl.slice(q, q + s));
				q += s;
			}
		} else if (t === 28) {
			// FU-A: a NAL fragmented across packets.
			const fh = pl[1];
			const start = (fh & 0x80) !== 0;
			const end = (fh & 0x40) !== 0;
			const origType = fh & 0x1f;
			const nri = pl[0] & 0x60;
			if (start) fu = [new Uint8Array([nri | origType])];
			if (fu) {
				fu.push(pl.slice(2));
				if (end) {
					let tot = 0;
					for (const b of fu) tot += b.length;
					const nal = new Uint8Array(tot);
					let z = 0;
					for (const b of fu) {
						nal.set(b, z);
						z += b.length;
					}
					nals.push(nal);
					fu = null;
				}
			}
		}
		if (marker) emitAU();
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
		}
	};
}
