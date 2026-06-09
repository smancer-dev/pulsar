// The video + audio + live-stats engine for a remote-play session, lifted verbatim out of
// Session.svelte so the component stays under the size budget. It owns all the streaming
// metric state, the host-summary derivations, and the WebSocket/timer effects. Inputs that
// change over the session lifetime are passed as GETTERS so the effects re-run exactly when
// they did inline (reading the getter inside the effect tracks the source $state/prop).
//
// Behaviour is identical to the original inline script: same effect bodies, same teardown,
// same WebCodecs sink wiring. Instantiated once at component init (so its $effect()s register
// in the component's reactive scope and tear down with it).

// Video is rendered NATIVELY in-app on every platform now (pulsar-render: rkmpp on Linux,
// Media Foundation+D3D11 on Windows; mpv/VideoToolbox on macOS) — the webview WebCodecs
// video path was removed (too slow). Only audio still decodes in the webview (WebAudio).
import { startOpusAudio } from '$lib/opus-audio';
import { onPlayRtt, onPlayStats, onPlayVStats } from '$lib/api';
import { type Encoder } from '$lib/settings.svelte';

type Inputs = {
	playId: () => number;
	wsPort: () => number;
	audioWsPort: () => number;
	native: () => boolean;
	embedded: () => boolean;
};

export class SessionMedia {
	#in: Inputs;

	hasVideo = $state(false);
	videoErr = $state('');
	fps = $state(0);
	// Once video has started, "stalled" means frames stopped arriving — e.g. the
	// host revoked/closed the screen share — so we surface an error to the user.
	stalled = $state(false);
	#staleSecs = 0;
	// Rolling fps samples (one per second) for the perf hover graph.
	fpsHistory = $state<number[]>([]);
	// Live performance metrics shown in the in-session stats panel. Client-side video numbers
	// now come from the native renderer's `play-vstats` events (no WebCodecs sink).
	lossPct = $state(0);
	rttMs = $state(0); // network round-trip (keepalive ping/pong)
	decodeMs = $state(0); // client decoder latency (submit→frame-out)
	jitterMs = $state(0); // network inter-arrival jitter
	mbps = $state(0); // incoming video bitrate
	// WebCodecs decoder codec string (e.g. avc1.640028), shown in the stats panel.
	decoderCodec = $state('');
	// Host encode summary string "codec · encoder · 1080p · 60fps", pushed by the host.
	hostStats = $state('');
	audioErr = $state('');

	// Split the host summary into its parts so the panel can lay them out in clean rows
	// (the single cramped string used to overflow). Order matches lib.rs's format!.
	#hostParts = $derived(this.hostStats ? this.hostStats.split(' · ') : []);
	hostCodec = $derived(this.#hostParts[0] ?? '');
	hostEncoder = $derived(this.#hostParts[1] ?? '');
	hostRes = $derived(this.#hostParts[2] ?? '');
	hostFps = $derived(this.#hostParts[3] ?? '');
	// One-way network latency estimate (half the round-trip), the headline "connection
	// latency" number — like Moonlight's network latency readout.
	latencyMs = $derived(this.rttMs > 0 ? Math.round(this.rttMs / 2) : 0);

	netClass = $derived(!this.hasVideo ? 'bad' : this.fps >= 24 ? 'ok' : this.fps >= 12 ? 'mid' : 'bad');
	// Polyline points for the fps sparkline in the perf tooltip.
	spark = $derived.by(() => {
		const h = this.fpsHistory;
		if (h.length < 2) return '';
		const max = Math.max(60, ...h);
		const W = 120;
		const H = 32;
		return h
			.map((v, i) => `${((i / (h.length - 1)) * W).toFixed(1)},${(H - (v / max) * H).toFixed(1)}`)
			.join(' ');
	});

	constructor(inputs: Inputs, _canvas: () => HTMLCanvasElement) {
		this.#in = inputs;

		// Real client-side stats from mpv (no WebCodecs sink in the native/single-surface
		// paths). CRITICAL: the default --wid path has embedded===false but native===true, so
		// gate on (native || embedded) — otherwise the host's --wid IPC vstats are ignored and
		// every number stays zero. Win/macOS use native=false here and keep the WebCodecs
		// videoSink.stats() path below, so the two sources never fight.
		$effect(() => {
			if (!(inputs.native() || inputs.embedded()) || inputs.playId() < 0) return;
			const playId = inputs.playId();
			let un: () => void = () => {};
			onPlayVStats((e) => {
				if (e.id !== playId) return;
				this.fps = e.fps > 0 ? Math.round(e.fps) : parseInt(this.hostFps, 10) || 0;
				this.mbps = Math.round(e.mbps * 10) / 10;
				// decodeMs is mpv's real pipeline-buffer latency on --wid (demuxer-cache-duration) — never
				// faked; 0 when mpv can't report it. Feeds the Decode-time row + the overlay HUD.
				this.decodeMs = Math.round(e.decodeMs * 10) / 10;
				this.decoderCodec = 'rkmpp (HW)';
				if (e.mbps > 0 || e.fps > 0) this.hasVideo = true; // video is flowing
			}).then((u) => (un = u));
			return () => un();
		});

		// Audio: a second loopback WebSocket carries the host's Opus stream; decode +
		// play it (best-effort — silent if the webview lacks WebCodecs audio).
		$effect(() => {
			const audioWsPort = inputs.audioWsPort();
			if (!audioWsPort) return;
			this.audioErr = '';
			const sink = startOpusAudio((m) => (this.audioErr = m));
			let ws: WebSocket | null = null;
			try {
				ws = new WebSocket(`ws://127.0.0.1:${audioWsPort}`);
				ws.binaryType = 'arraybuffer';
				ws.onmessage = (ev) => sink.push(new Uint8Array(ev.data as ArrayBuffer));
			} catch {
				/* audio is optional; ignore connect failures */
			}
			return () => {
				ws?.close();
				sink.close();
			};
		});

		// RTT (keepalive ping/pong) + host encode summary arrive as events from the play
		// session; keep only this tab's (by playId).
		$effect(() => {
			const playId = inputs.playId();
			let unRtt: () => void = () => {};
			let unStats: () => void = () => {};
			onPlayRtt((e) => {
				if (e.id === playId) this.rttMs = Math.round(e.rtt);
			}).then((u) => (unRtt = u));
			onPlayStats((e) => {
				if (e.id === playId) this.hostStats = e.label;
			}).then((u) => (unStats = u));
			return () => {
				unRtt();
				unStats();
			};
		});

		// Sample the (native-reported) fps into the rolling history once a second for the perf
		// sparkline, and flag a stall if frames stop after video had started.
		$effect(() => {
			const timer = setInterval(() => {
				this.fpsHistory = [...this.fpsHistory, this.fps].slice(-48);
				if (this.hasVideo) {
					if (this.fps === 0) {
						this.#staleSecs++;
						if (this.#staleSecs >= 3) this.stalled = true;
					} else {
						this.#staleSecs = 0;
						this.stalled = false;
					}
				}
			}, 1000);
			return () => clearInterval(timer);
		});
	}

	// Decoder switching is handled host-side via the in-session overlay/menu now (the native
	// renderer picks the HW decoder from the stream codec); no client-side WebCodecs hint.
	reconfigure(_v: Encoder) {}
}
