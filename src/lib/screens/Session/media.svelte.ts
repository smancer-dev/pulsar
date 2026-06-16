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
import { onPlayDecoder, onPlayRtt, onPlayStats, onPlayVStats } from '$lib/api';
import { listenScope } from '$lib/api.events';

type Inputs = {
	playId: () => number;
	wsPort: () => number;
	audioWsPort: () => number;
	native: () => boolean;
	embedded: () => boolean;
	// True while a codec/encoder/resolution switch is in flight: the host restarts
	// ffmpeg, so frames deliberately stop — the stall detector must not fire then.
	switching: () => boolean;
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
	// Stall detection runs on the RAW renderer-reported fps + vstats arrival time:
	// the DISPLAYED `fps` substitutes the host's target rate when the renderer
	// reports 0, which would otherwise mask a frozen stream as healthy forever.
	#rawFps = 0;
	#lastVStatsAt = 0;
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

	netClass = $derived(
		!this.hasVideo || this.stalled ? 'bad' : this.fps >= 24 ? 'ok' : this.fps >= 12 ? 'mid' : 'bad'
	);
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
			const scope = listenScope();
			// The renderer reports which decoder it REALLY opened (read-only display —
			// there is no decoder picker; selection is automatic per platform/stream).
			scope.add(
				onPlayDecoder((e) => {
					if (e.id !== playId) return;
					const suffix = e.hw === 'hw' ? ' (HW)' : e.hw === 'sw' ? ' (SW)' : '';
					this.decoderCodec = `${e.name}${suffix}`;
				}),
				onPlayVStats((e) => {
					if (e.id !== playId) return;
					this.#rawFps = e.fps;
					this.#lastVStatsAt = Date.now();
					// Display only: substitute the host's target rate for a momentary 0 —
					// the stall detector below runs on #rawFps, never on this.
					this.fps = e.fps > 0 ? Math.round(e.fps) : parseInt(this.hostFps, 10) || 0;
					this.mbps = Math.round(e.mbps * 10) / 10;
					// decodeMs is mpv's real pipeline-buffer latency on --wid (demuxer-cache-duration) — never
					// faked; 0 when mpv can't report it. Feeds the Decode-time row + the overlay HUD.
					this.decodeMs = Math.round(e.decodeMs * 10) / 10;
					if (e.mbps > 0 || e.fps > 0) this.hasVideo = true; // video is flowing
				})
			);
			return scope.dispose;
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
			const scope = listenScope();
			scope.add(
				onPlayRtt((e) => {
					if (e.id === playId) this.rttMs = Math.round(e.rtt);
				}),
				onPlayStats((e) => {
					if (e.id === playId) this.hostStats = e.label;
				})
			);
			return scope.dispose;
		});

		// Sample the (native-reported) fps into the rolling history once a second for the perf
		// sparkline, and flag a stall if frames stop after video had started. The detector runs
		// on the RAW fps (the displayed one substitutes the host target for 0) and also trips
		// when vstats themselves go silent (renderer/IPC died → fps would freeze at its last value).
		$effect(() => {
			const timer = setInterval(() => {
				this.fpsHistory = [...this.fpsHistory, this.fps].slice(-48);
				// A codec/encoder/resolution switch deliberately interrupts the frame flow
				// while the host restarts ffmpeg; don't let that look like a stall (a restart
				// that runs past the 3 s threshold would otherwise flash the "stream stopped"
				// error). Reset the counters so the detector re-arms cleanly once frames resume.
				if (this.#in.switching()) {
					this.#staleSecs = 0;
					this.stalled = false;
					// Reset #rawFps so the stall detector re-arms via the `#rawFps <= 0`
					// path after the switch window ends — even if the renderer died and
					// no new vstats arrive. Do NOT zero #lastVStatsAt: if we did, then
					// after a dead switch both #rawFps and statsSilent would be false
					// (lastVStatsAt=0 makes statsSilent=false) and the detector would
					// never trip, permanently disarming it for the rest of the session.
					this.#rawFps = 0;
					return;
				}
				if (this.hasVideo) {
					const statsSilent =
						this.#lastVStatsAt > 0 && Date.now() - this.#lastVStatsAt > 3000;
					if (this.#rawFps <= 0 || statsSilent) {
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

}
