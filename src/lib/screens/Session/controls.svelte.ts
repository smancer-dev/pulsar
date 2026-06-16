// Host-stream control state + live setters for a session, lifted out of Session.svelte.
// Owns the resolution/fps/bitrate/quality/frame-pacing/audio selections shown in the menu +
// overlay and the codec/encoder switches; each setter mutates the local state and pokes the
// host over `api`. Persisted prefs still go through the shared `ui` store. (There is no
// decoder setter: the client decoder is auto-selected and shown read-only.)
//
// Instantiated at component init so the persisted frame-pacing one-shot $effect scopes to the
// component. `playId`/`native` are read through getters.

import { api } from '$lib/api';
import { ui, saveUi, type Encoder, type VideoCodec } from '$lib/settings.svelte';

type Inputs = {
	playId: () => number;
	native: () => boolean;
	mode: () => 'remote' | 'game';
};

export class SessionControls {
	#in: Inputs;
	/// True for a short window after a codec/encoder switch — the session screen
	/// shows a "restarting the stream" veil (the native container is hidden by the
	/// Rust side for the same window, so the veil is actually visible).
	switching = $state(false);
	#switchTimer: ReturnType<typeof setTimeout> | undefined;
	#beginSwitch(ms: number) {
		this.switching = true;
		clearTimeout(this.#switchTimer);
		this.#switchTimer = setTimeout(() => (this.switching = false), ms);
	}

	// Stream resolution requested from the host, changed live from the menu.
	// 'auto' = let the host use its own configured size.
	streamRes = $state<'auto' | '1080p' | '1440p' | '4K'>('auto');
	// Live frame-rate switch ('auto' = host default). The host restarts ffmpeg with it.
	streamFps = $state<'auto' | '30' | '60' | '120'>('auto');
	// Live bitrate switch in Mbit (0 = auto / host default). Persisted via ui.bitrate.
	streamBitrate = $state<number>(ui.bitrate);
	// Live quality/perf profile. Two-tier (latency|quality); ephemeral (default latency in
	// game mode for lowest latency, quality in remote). The host maps it to its encode flag.
	streamQuality: 'latency' | 'quality';
	// Which HOST monitor is streamed (index into host_displays; 0 = primary, the default).
	// Changed live from the menu's screen picker.
	streamDisplay = $state<number>(0);
	// Moonlight-style frame pacing on the Linux native renderer (client-local, persisted).
	framePacing = $state<boolean>(ui.framePacing);
	// Audio session-menu toggles. Defaults mirror the host: transmit on; mute the host only
	// in game mode (so the sound moves to the player). Both change live.
	transmitAudio = $state(true);
	muteHost: boolean;

	constructor(inputs: Inputs) {
		this.#in = inputs;
		const game = inputs.mode() === 'game';
		this.streamQuality = $state(game ? 'latency' : 'quality');
		this.muteHost = $state(game);

		// Push the persisted frame-pacing default to the native renderer once the play is live
		// (spawn_render started from the env default; the frontend is authoritative). One-shot.
		let pacingApplied = false;
		$effect(() => {
			if (inputs.native() && inputs.playId() >= 0 && !pacingApplied) {
				pacingApplied = true;
				api.setFramePacing(inputs.playId(), ui.framePacing).catch(() => {});
				// Same one-shot for the persisted always-on stats HUD + overlay button.
				if (ui.statsHud) api.setStatsHud(inputs.playId(), true).catch(() => {});
				if (!ui.overlayButton) api.setOverlayButton(inputs.playId(), false).catch(() => {});
				// Persisted dragged button position (skip when still at the default —
				// the renderer already starts there).
				const p = ui.overlayBtnPos;
				if (p && (p.x !== 90 || p.y !== 70))
					api.setOverlayButtonPos(inputs.playId(), p.x, p.y).catch(() => {});
			}
		});

		// The persisted bitrate renders as the ACTIVE menu/overlay selection, so push it to
		// the host once the play is live — start_remote_play carries no bitrate, so without
		// this the host streams its default while the UI claims e.g. "20 Mbit". One-shot,
		// NOT native-gated (the webview menu shows it too); 0 = auto → nothing to send.
		let bitrateApplied = false;
		$effect(() => {
			if (inputs.playId() >= 0 && !bitrateApplied) {
				bitrateApplied = true;
				if (ui.bitrate > 0)
					api.setPlayBitrate(inputs.playId(), ui.bitrate * 1000).catch(() => {});
			}
		});
	}

	setRes = (v: 'auto' | '1080p' | '1440p' | '4K') => {
		this.streamRes = v;
		const playId = this.#in.playId();
		if (playId < 0) return;
		// The host kills its current encoder and rebuilds capture at the new geometry
		// (set_play_resolution in session_cmds.rs). A 4K NVENC/DXGI rebuild can easily
		// exceed 3 s, so suppress the stall detector for 4 s.
		this.#beginSwitch(4000);
		const [w, h] =
			v === '4K' ? [3840, 2160] : v === '1440p' ? [2560, 1440] : v === '1080p' ? [1920, 1080] : [0, 0];
		api.setPlayResolution(playId, w, h).catch(() => {});
	};
	setFps = (v: 'auto' | '30' | '60' | '120') => {
		this.streamFps = v;
		const playId = this.#in.playId();
		if (playId < 0) return;
		// The host restarts ffmpeg with the new frame rate (same pipeline rebuild as a
		// codec switch); suppress the stall detector for the same window.
		this.#beginSwitch(2800);
		api.setPlayFps(playId, v === 'auto' ? 0 : Number(v)).catch(() => {});
	};
	setBitrate = (v: number) => {
		this.streamBitrate = v;
		ui.bitrate = v;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setPlayBitrate(playId, v * 1000).catch(() => {}); // Mbit → kbps
	};
	setQuality = (v: 'latency' | 'quality') => {
		this.streamQuality = v;
		const playId = this.#in.playId();
		if (playId < 0) return;
		// set_play_quality sends Restream::Quality → request_stream on the host,
		// which kills and rebuilds the encoder pipeline (same depth as a codec/fps
		// switch). Suppress the stall detector for the same window so the rebuild
		// gap does not flash a false "stream stopped" error.
		this.#beginSwitch(2800);
		api.setPlayQuality(playId, v).catch(() => {});
	};
	setMonitor = (idx: number) => {
		this.streamDisplay = idx;
		const playId = this.#in.playId();
		if (playId < 0) return;
		// A monitor switch forces the host to stop capture, rebuild the DXGI/VAAPI
		// pipeline on the new output (possibly at a different resolution), and respawn
		// the native renderer on the client — the same depth of rebuild as setRes.
		// Without the switch window the stall detector trips after ~3 s of zero fps,
		// flashing a false "stream stopped" error on a healthy session.
		this.#beginSwitch(4000);
		api.setPlayMonitor(playId, idx).catch(() => {});
	};
	setFramePacing = (on: boolean) => {
		this.framePacing = on;
		ui.framePacing = on;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setFramePacing(playId, on).catch(() => {});
	};
	toggleFramePacing = () => {
		this.setFramePacing(!this.framePacing);
	};
	#applyAudio() {
		const playId = this.#in.playId();
		if (playId >= 0) api.setPlayAudio(playId, this.transmitAudio, this.muteHost).catch(() => {});
	}
	toggleTransmit = () => {
		this.transmitAudio = !this.transmitAudio;
		this.#applyAudio();
	};
	toggleMute = () => {
		this.muteHost = !this.muteHost;
		this.#applyAudio();
	};
	// Always-on mini stats HUD (persisted; the renderer draws it while the overlay
	// is closed).
	setStatsHud = (on: boolean) => {
		ui.statsHud = on;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setStatsHud(playId, on).catch(() => {});
	};
	// Parsec-style always-visible overlay-open button (persisted).
	setOverlayButton = (on: boolean) => {
		ui.overlayButton = on;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setOverlayButton(playId, on).catch(() => {});
	};
	// Live encoder switch (host re-encodes) — sent to the host, which restarts ffmpeg.
	setEncoder = (v: Encoder) => {
		ui.encoder = v;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) {
			this.#beginSwitch(1600);
			api.setPlayEncoder(playId, v).catch(() => {});
		}
	};
	// Live codec switch (H.264/H.265/AV1) — the host restarts ffmpeg with it; the client
	// decoder re-derives its codec string from the new stream automatically.
	setCodec = (v: VideoCodec) => {
		ui.codec = v;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) {
			this.#beginSwitch(2800);
			api.setPlayCodec(playId, v).catch(() => {});
		}
	};
}
