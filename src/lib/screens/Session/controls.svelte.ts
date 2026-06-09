// Host-stream control state + live setters for a session, lifted out of Session.svelte.
// Owns the resolution/fps/bitrate/quality/frame-pacing/audio selections shown in the menu +
// overlay and the codec/encoder/decoder switches; each setter mutates the local state and
// pokes the host over `api` exactly as the inline code did. Persisted prefs still go through
// the shared `ui` store. Behaviour is identical to the original inline script.
//
// Instantiated at component init so the persisted frame-pacing one-shot $effect scopes to the
// component. `playId`/`native` are read through getters; `reconfigure` re-hints the WebCodecs
// decoder (the media engine owns the sink).

import { api } from '$lib/api';
import { ui, saveUi, type Encoder, type VideoCodec } from '$lib/settings.svelte';

type Inputs = {
	playId: () => number;
	native: () => boolean;
	mode: () => 'remote' | 'game';
	reconfigure: (v: Encoder) => void;
};

export class SessionControls {
	#in: Inputs;

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
			}
		});
	}

	setRes = (v: 'auto' | '1080p' | '1440p' | '4K') => {
		this.streamRes = v;
		const playId = this.#in.playId();
		if (playId < 0) return;
		const [w, h] =
			v === '4K' ? [3840, 2160] : v === '1440p' ? [2560, 1440] : v === '1080p' ? [1920, 1080] : [0, 0];
		api.setPlayResolution(playId, w, h).catch(() => {});
	};
	setFps = (v: 'auto' | '30' | '60' | '120') => {
		this.streamFps = v;
		const playId = this.#in.playId();
		if (playId >= 0) api.setPlayFps(playId, v === 'auto' ? 0 : Number(v)).catch(() => {});
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
		if (playId >= 0) api.setPlayQuality(playId, v).catch(() => {});
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
	// Keep the control handle/menu always visible (even fullscreen / while controlling).
	toggleKeepVisible = () => {
		ui.keepVisible = !ui.keepVisible;
		saveUi();
	};
	// Live encoder switch (host re-encodes) — sent to the host, which restarts ffmpeg.
	setEncoder = (v: Encoder) => {
		ui.encoder = v;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setPlayEncoder(playId, v).catch(() => {});
	};
	// Live decoder switch (client-side WebCodecs hint: hardware vs software). Applied at
	// the next keyframe; no host round-trip. The actual codec is dictated by the stream.
	setDecoder = (v: Encoder) => {
		ui.decoder = v;
		saveUi();
		this.#in.reconfigure(v);
	};
	// Live codec switch (H.264/H.265/AV1) — the host restarts ffmpeg with it; the client
	// decoder re-derives its codec string from the new stream automatically.
	setCodec = (v: VideoCodec) => {
		ui.codec = v;
		saveUi();
		const playId = this.#in.playId();
		if (playId >= 0) api.setPlayCodec(playId, v).catch(() => {});
	};
}
