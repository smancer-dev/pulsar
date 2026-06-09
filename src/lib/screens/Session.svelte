<script lang="ts">
	import Menu from './Session/Menu.svelte';
	import Overlay from './Session/Overlay.svelte';
	import VideoStatus from './Session/VideoStatus.svelte';
	import { SessionMedia } from './Session/media.svelte';
	import {
		api,
		onKbdLeave,
		onOverlayToggle,
		onOverlayCmd,
		onOverlayEnd,
		onOverlayClose,
		onWindowBlur,
		onPlayEnded
	} from '$lib/api';
	import { SessionInput } from './Session/input.svelte';
	import { SessionControls } from './Session/controls.svelte';
	import { SessionSideChannels } from './Session/sidechannels.svelte';
	import { SessionUi } from './Session/ui.svelte';
	import { ui, type Encoder, type VideoCodec } from '$lib/settings.svelte';

	type Target = { name: string; id: string };
	type Props = {
		playId: number;
		target: Target;
		mode: 'remote' | 'game';
		conn: 'direct' | 'relay';
		wsPort?: number;
		audioWsPort?: number;
		selfId?: string;
		native?: boolean;
		embedded?: boolean;
		fullscreen?: boolean;
		onToggleFullscreen: () => void;
		onEnd?: () => void;
	};
	let {
		playId,
		target,
		mode,
		conn,
		wsPort = 0,
		audioWsPort = 0,
		selfId = '',
		native = false,
		embedded = false,
		fullscreen = false,
		onToggleFullscreen,
		onEnd = () => {}
	}: Props = $props();

	// Reverse direction: ask the host to connect back to us so roles swap.
	function reverse() {
		if (playId >= 0 && selfId) api.reversePlay(playId, selfId.replace(/\s/g, '')).catch(() => {});
	}

	const connLabel = $derived(conn === 'relay' ? 'Relay' : 'P2P');

	let canvas: HTMLCanvasElement;
	// The video + audio + live-stats engine (canvas WebSocket sink, mpv vstats, RTT/host
	// summary, the 1s fps/stall timer). Owns all the streaming metric state + derivations;
	// see Session/media.svelte.ts. Created at component init so its effects scope to + tear
	// down with this component. Inputs are getters so the effects re-run exactly as before.
	const media = new SessionMedia(
		{
			playId: () => playId,
			wsPort: () => wsPort,
			audioWsPort: () => audioWsPort,
			native: () => native,
			embedded: () => embedded
		},
		() => canvas
	);

	// How the video fills the viewport (AnyDesk-style): fit (keep aspect),
	// stretch (fill, may distort), or original (1:1 native pixels).
	let fitMode = $state<'fit' | 'stretch' | 'original'>('fit');
	// Host-stream control state + live setters (resolution/fps/bitrate/quality/pacing/audio +
	// codec/encoder/decoder). See Session/controls.svelte.ts. Created at component init so its
	// persisted frame-pacing one-shot effect scopes to the component; setDecoder re-hints the
	// media engine's WebCodecs sink.
	const controls = new SessionControls({
		playId: () => playId,
		native: () => native,
		mode: () => mode,
		reconfigure: (v) => media.reconfigure(v)
	});

	// Native renderer: video is in the ffplay fullscreen window, so skip the webview
	// canvas/WS path entirely. Arm the Interception keyboard immediately (no canvas to
	// click) so the remote receives input right away.
	$effect(() => {
		if (!native || playId < 0) return;
		api.kbdCaptureStart(playId, true).catch(() => {}); // true = also capture mouse
		return () => api.kbdCaptureStop().catch(() => {});
	});

	// Single-surface (Linux): the rkmpp video is a GtkGLArea BEHIND this webview, so the
	// whole page must be transparent over the video — flag <html> so the app frame
	// (`.window`) goes transparent and the GLArea shows through. Chrome/dock stay opaque.
	$effect(() => {
		if (typeof document === 'undefined') return;
		document.documentElement.toggleAttribute('data-embedded', embedded);
		return () => document.documentElement.removeAttribute('data-embedded');
	});

	// Remote-control input engine (the `controlling` flag, absolute-positioning pointer/key
	// forwarding, the rAF pump). See Session/input.svelte.ts. Created at component init so its
	// effect scopes to + tears down with this component; inputs are getters so it tracks them.
	const input = new SessionInput({
		playId: () => playId,
		wsPort: () => wsPort,
		canvas: () => canvas
	});
	function stopControl() {
		input.stopControl();
	}

	// Side channels — clipboard, file transfer, two-way chat, microphone (remote-desktop only).
	// Owns the chat/clipboard/file/mic state + the inbound-events effect; the active menu body
	// (`panel`) lives here too (chat is a side channel). See Session/sidechannels.svelte.ts.
	const sidechan = new SessionSideChannels({
		playId: () => playId,
		menuOpen: () => dock.menuOpen
	});

	// Floating-menu + game-overlay UI controller (dock open/floating-drag, the Ctrl+Shift+M
	// overlay's debounced open/close, Escape-to-close). See Session/ui.svelte.ts. Cross-cutting
	// actions are passed as callbacks (release control, end the tab, fullscreen, reset panel).
	const dock = new SessionUi({
		playId: () => playId,
		stopControl,
		onEnd: () => onEnd(),
		onToggleFullscreen: () => onToggleFullscreen(),
		resetPanel: () => (sidechan.panel = 'menu')
	});

	// Auto-fade the in-control hint after a couple of seconds (Parsec-style); it
	// reappears each time control is (re-)taken.
	let hintFade = $state(false);
	$effect(() => {
		if (!input.controlling) {
			hintFade = false;
			return;
		}
		hintFade = false;
		const tmr = setTimeout(() => (hintFade = true), 2500);
		return () => clearTimeout(tmr);
	});

	// Windows: the OS-level keyboard hook saw the Ctrl+Shift+F12 leave combo (the
	// webview never gets those keys while the hook suppresses them). In native mode
	// the ffplay window covers the app, so leaving ends the session; otherwise it
	// just releases control.
	$effect(() => {
		let off: (() => void) | undefined;
		onKbdLeave(() => (native ? onEnd() : stopControl())).then((o) => (off = o));
		return () => off?.();
	});

	// Host closed the session (or a network error) — the hold-loop emits `play-ended`. Release
	// the input grab and end the tab so the native path doesn't freeze on mpv's last frame with
	// the keyboard/mouse still captured (you'd be stuck needing an SSH kill). kbdCaptureStop is
	// called directly (not via stopControl) so it ungrabs regardless of the `controlling` flag.
	$effect(() => {
		let off: (() => void) | undefined;
		onPlayEnded((eid) => {
			if (eid !== playId) return;
			api.kbdCaptureStop().catch(() => {});
			onEnd();
		}).then((o) => (off = o));
		return () => off?.();
	});

	// Ctrl+Shift+M (from the OS-level keyboard hook / evdev capture) toggles the game
	// overlay without ending the session. Payload-less, like kbd-leave; applies to this
	// active play tab.
	$effect(() => {
		let off: (() => void) | undefined;
		// The OS keyboard hook / evdev emits overlay-toggle whenever capture is active, with no
		// knowledge of mode. The webview overlay only renders in game mode (`{#if mode === 'game'}`),
		// so on Windows/macOS remote sessions (native=false) a toggle would just drop control with
		// nothing shown. Only honor it in game mode, or when a native renderer (Linux) is up — there
		// the overlay is drawn on the video and works in any mode.
		onOverlayToggle(() => {
			if (mode === 'game' || native) dock.toggleOverlay();
		}).then((o) => (off = o));
		return () => off?.();
	});

	// Linux native overlay (`pulsar-render`): the egui overlay is the real UI on Linux (the
	// webview overlay is occluded by the video), so its interactions arrive as events and apply
	// through the SAME setters — codec/encoder/fps/etc go to the host, End/Close mirror locally.
	$effect(() => {
		let offs: Array<(() => void) | undefined> = [];
		onOverlayCmd((field, val) => {
			switch (field) {
				case 'codec': controls.setCodec(val as VideoCodec); break;
				case 'encoder': controls.setEncoder(val as Encoder); break;
				case 'decoder': controls.setDecoder(val as Encoder); break;
				case 'res': controls.setRes(val as 'auto' | '1080p' | '1440p' | '4K'); break;
				case 'fps': controls.setFps(val as 'auto' | '30' | '60' | '120'); break;
				case 'bitrate': controls.setBitrate(Number(val) || 0); break;
				case 'quality': controls.setQuality(val as 'latency' | 'quality'); break;
				case 'pace': controls.setFramePacing(val === 'on' || val === '1' || val === 'true'); break;
			}
		}).then((o) => offs.push(o));
		onOverlayEnd(() => dock.endSession()).then((o) => offs.push(o));
		onOverlayClose(() => dock.closeOverlay()).then((o) => offs.push(o));
		// Window lost focus → close the overlay so the focus-gated combo can't strand it open.
		onWindowBlur(() => dock.closeOverlay()).then((o) => offs.push(o));
		return () => offs.forEach((o) => o?.());
	});

</script>

<div class="screen" class:embedded>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<canvas
		bind:this={canvas}
		class="video {fitMode}"
		class:on={media.hasVideo}
		class:control={input.controlling}
		tabindex="0"
		onpointerdown={input.onDown}
		onpointermove={input.onMove}
		onpointerup={input.onUp}
		onwheel={input.onWheel}
		onpointerleave={input.clearMove}
		onblur={stopControl}
		oncontextmenu={(e) => e.preventDefault()}
	></canvas>

	<VideoStatus
		{native}
		{embedded}
		{mode}
		{target}
		hasVideo={media.hasVideo}
		videoErr={media.videoErr}
		stalled={media.stalled}
		controlling={input.controlling}
		{hintFade}
		onStartControl={input.startControl}
	/>

	<!-- Floating control handle + expandable menu (auto-hides while controlling) -->
	<Menu
		menuOpen={dock.menuOpen}
		controlling={input.controlling}
		floating={dock.floating}
		pos={dock.pos}
		keepVisible={ui.keepVisible}
		bind:statsHover={dock.statsHover}
		netClass={media.netClass}
		fps={media.fps}
		latencyMs={media.latencyMs}
		spark={media.spark}
		hostCodec={media.hostCodec}
		hostRes={media.hostRes}
		hostEncoder={media.hostEncoder}
		hostFps={media.hostFps}
		decoderCodec={media.decoderCodec}
		decodeMs={media.decodeMs}
		{connLabel}
		rttMs={media.rttMs}
		jitterMs={media.jitterMs}
		lossPct={media.lossPct}
		mbps={media.mbps}
		{target}
		{mode}
		{fullscreen}
		bind:panel={sidechan.panel}
		messages={sidechan.messages}
		bind:chatInput={sidechan.chatInput}
		bind:chatBox={sidechan.chatBox}
		unread={sidechan.unread}
		note={sidechan.note}
		micOn={sidechan.micOn}
		transmitAudio={controls.transmitAudio}
		muteHost={controls.muteHost}
		framePacing={controls.framePacing}
		bind:fitMode
		codec={ui.codec}
		encoder={ui.encoder}
		decoder={ui.decoder}
		streamRes={controls.streamRes}
		streamFps={controls.streamFps}
		streamBitrate={controls.streamBitrate}
		streamQuality={controls.streamQuality}
		onCloseMenu={dock.closeMenu}
		onHandleClick={dock.handleClick}
		onHandleDown={dock.onHandleDown}
		onHandleMove={dock.onHandleMove}
		onHandleUp={dock.onHandleUp}
		onCodec={controls.setCodec}
		onEncoder={controls.setEncoder}
		onDecoder={controls.setDecoder}
		onRes={controls.setRes}
		onFps={controls.setFps}
		onBitrate={controls.setBitrate}
		onQuality={controls.setQuality}
		onFullscreen={dock.doFullscreen}
		onSendClipboard={sidechan.sendClipboard}
		onPickFile={sidechan.pickFile}
		onToggleMic={sidechan.toggleMic}
		onOpenChat={sidechan.openChat}
		onToggleFloating={dock.toggleFloating}
		onReverse={reverse}
		onToggleTransmit={controls.toggleTransmit}
		onToggleMute={controls.toggleMute}
		onToggleKeepVisible={controls.toggleKeepVisible}
		onToggleFramePacing={controls.toggleFramePacing}
		onSendChat={sidechan.sendChatLine}
		onEnd={dock.endSession}
	/>

	<!-- Game-only overlay (Ctrl+Shift+M). Opaque dialog so it stays visible while mpv is
	     paused on Linux. Perf HUD + the slim game controls (codec/encoder/decoder/res/fps/
	     bitrate/quality) + controllers + end — NO file/clipboard/mic/chat (remote-only). -->
	{#if mode === 'game' && dock.overlayOpen}
		<Overlay
			{target}
			{connLabel}
			netClass={media.netClass}
			fps={media.fps}
			latencyMs={media.latencyMs}
			decodeMs={media.decodeMs}
			mbps={media.mbps}
			codec={ui.codec}
			encoder={ui.encoder}
			decoder={ui.decoder}
			streamRes={controls.streamRes}
			streamFps={controls.streamFps}
			streamBitrate={controls.streamBitrate}
			streamQuality={controls.streamQuality}
			framePacing={controls.framePacing}
			onCodec={controls.setCodec}
			onEncoder={controls.setEncoder}
			onDecoder={controls.setDecoder}
			onRes={controls.setRes}
			onFps={controls.setFps}
			onBitrate={controls.setBitrate}
			onQuality={controls.setQuality}
			onFramePacing={controls.setFramePacing}
			onClose={dock.closeOverlay}
			onEnd={dock.endSession}
		/>
	{/if}

	<!-- hidden picker for "send file" -->
	<input class="filepick" type="file" bind:this={sidechan.fileInput} onchange={sidechan.onFilePicked} />
</div>

<style>
	.screen {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		background:
			radial-gradient(700px 380px at 50% 30%, oklch(0.3 0.06 272 / 0.3), transparent 70%),
			#0c0d12;
		overflow: hidden;
	}
	/* Single-surface (Linux): the video is a GtkGLArea BEHIND this webview, so the screen
	   must be transparent to show it through; the dock/menu/hints stay opaque on top. */
	.screen.embedded {
		background: transparent;
	}
	.video {
		display: none;
		background: #000;
	}
	.video.on {
		display: block;
	}
	/* AnyDesk-style fit modes */
	.video.fit {
		max-width: 100%;
		max-height: 100%;
	}
	/* "Doldur" = fill the height (vertical), preserving aspect; width overflows and
	   is cropped by the screen's overflow:hidden rather than distorting horizontally. */
	.video.stretch {
		height: 100%;
		width: auto;
		max-width: none;
		max-height: none;
	}
	.video.original {
		max-width: none;
		max-height: none;
	}
	.video:focus {
		outline: none;
	}
	.video.control {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
	}
	.filepick {
		display: none;
	}
</style>
