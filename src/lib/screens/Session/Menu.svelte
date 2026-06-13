<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import StatsPanel from './StatsPanel.svelte';
	import Chat from './Chat.svelte';
	import VideoMenu from './VideoMenu.svelte';
	import SessionActions from './SessionActions.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api } from '$lib/api';
	import { type Encoder, type VideoCodec } from '$lib/settings.svelte';

	// Parsec-style floating control handle + expandable menu (remote-desktop full menu). The
	// parent owns the session state; this shell renders the dock and routes events back through
	// the callbacks. Two-way state (statsHover/chatInput/panel/fitMode/chatBox) is $bindable.
	type Target = { name: string; id: string };
	type ChatMsg = { me: boolean; text: string };
	type Panel = 'menu' | 'chat' | 'files';
	type Props = {
		/** Play/tab id of this session — the file panel addresses its api calls with it. */
		playId: number;
		menuOpen: boolean;
		controlling: boolean;
		floating: boolean;
		pos: { x: number; y: number };
		keepVisible: boolean;
		statsHover: boolean;
		netClass: string;
		fps: number;
		latencyMs: number;
		spark: string;
		hostCodec: string;
		hostRes: string;
		hostEncoder: string;
		hostFps: string;
		decoderCodec: string;
		decodeMs: number;
		connLabel: string;
		rttMs: number;
		jitterMs: number;
		lossPct: number;
		mbps: number;
		target: Target;
		/** The HOST's identity image (its account photo / wallpaper, pushed over the
		 * session as `peer-avatar`) — shown next to the target name when known. */
		peerAvatar?: string;
		mode: 'remote' | 'game';
		fullscreen: boolean;
		panel: Panel;
		messages: ChatMsg[];
		chatInput: string;
		chatBox: HTMLDivElement | null;
		unread: number;
		note: string;
		micOn: boolean;
		transmitAudio: boolean;
		muteHost: boolean;
		framePacing: boolean;
		fitMode: 'fit' | 'stretch' | 'original';
		codec: VideoCodec;
		encoder: Encoder;
		hostCodecs?: string[];
		hostEncoders?: string[];
		hostDisplays?: import('$lib/api.types').HostDisplay[];
		activeInfo?: string;
		streamRes: 'auto' | '1080p' | '1440p' | '4K';
		streamFps: 'auto' | '30' | '60' | '120';
		streamBitrate: number;
		streamQuality: 'latency' | 'quality';
		streamDisplay: number;
		onMonitor: (idx: number) => void;
		onCloseMenu: () => void;
		onHandleClick: () => void;
		onHandleDown: (e: PointerEvent) => void;
		onHandleMove: (e: PointerEvent) => void;
		onHandleUp: (e: PointerEvent) => void;
		onHandleCancel: (e: PointerEvent) => void;
		onCodec: (v: VideoCodec) => void;
		onEncoder: (v: Encoder) => void;
		onRes: (v: 'auto' | '1080p' | '1440p' | '4K') => void;
		onFps: (v: 'auto' | '30' | '60' | '120') => void;
		onBitrate: (v: number) => void;
		onQuality: (v: 'latency' | 'quality') => void;
		onFullscreen: () => void;
		onSendClipboard: () => void;
		onPickFile: () => void;
		onToggleMic: () => void;
		onOpenChat: () => void;
		onToggleFloating: () => void;
		onReverse: () => void;
		onToggleTransmit: () => void;
		onToggleMute: () => void;
		onToggleKeepVisible: () => void;
		onToggleFramePacing: () => void;
		onSendChat: () => void;
		onEnd: () => void;
	};
	let {
		playId,
		menuOpen,
		controlling,
		floating,
		pos,
		keepVisible,
		statsHover = $bindable(),
		netClass,
		fps,
		latencyMs,
		spark,
		hostCodec,
		hostRes,
		hostEncoder,
		hostFps,
		decoderCodec,
		decodeMs,
		connLabel,
		rttMs,
		jitterMs,
		lossPct,
		mbps,
		target,
		peerAvatar = '',
		mode,
		fullscreen,
		panel = $bindable(),
		messages,
		chatInput = $bindable(),
		chatBox = $bindable(),
		unread,
		note,
		micOn,
		transmitAudio,
		muteHost,
		framePacing,
		fitMode = $bindable(),
		codec,
		encoder,
		hostCodecs = [],
		hostEncoders = [],
		hostDisplays = [],
		activeInfo = '',
		streamRes,
		streamFps,
		streamBitrate,
		streamQuality,
		streamDisplay,
		onMonitor,
		onCloseMenu,
		onHandleClick,
		onHandleDown,
		onHandleMove,
		onHandleUp,
		onHandleCancel,
		onCodec,
		onEncoder,
		onRes,
		onFps,
		onBitrate,
		onQuality,
		onFullscreen,
		onSendClipboard,
		onPickFile,
		onToggleMic,
		onOpenChat,
		onToggleFloating,
		onReverse,
		onToggleTransmit,
		onToggleMute,
		onToggleKeepVisible,
		onToggleFramePacing,
		onSendChat,
		onEnd
	}: Props = $props();
</script>

{#if menuOpen}
	<button class="scrim" aria-label={t('session.menu')} onclick={onCloseMenu}></button>
{/if}
<div
	class="dock"
	class:open={menuOpen}
	class:floating
	class:hidden={controlling && !keepVisible}
	style={floating ? `left:${pos.x}px; top:${pos.y}px; transform:none;` : ''}
>
	<button
		class="handle"
		class:active={menuOpen}
		class:grab={floating}
		onclick={onHandleClick}
		onpointerdown={onHandleDown}
		onpointermove={onHandleMove}
		onpointerup={onHandleUp}
		onpointercancel={onHandleCancel}
		onpointerenter={() => (statsHover = true)}
		onpointerleave={() => (statsHover = false)}
		title={t('session.menu')}
		aria-label={t('session.menu')}
		aria-expanded={menuOpen}
	>
		<Icon name="grip" size={15} />
		<span class="net-dot {netClass}"></span>
		<span class="hfps mono">{fps} fps</span>
	</button>

	{#if statsHover && !menuOpen}
		<StatsPanel
			{netClass}
			{fps}
			{latencyMs}
			{spark}
			{hostCodec}
			{hostRes}
			{hostEncoder}
			{hostFps}
			{decoderCodec}
			{decodeMs}
			{connLabel}
			{rttMs}
			{jitterMs}
			{lossPct}
			{mbps}
		/>
	{/if}

	{#if menuOpen}
		<div class="menu" class:wide={panel === 'files'} role="menu">
			<div class="m-head">
				{#if peerAvatar}
					<img class="m-avatar" src={peerAvatar} alt="" />
				{/if}
				<div>
					<div class="m-name">{target.name}</div>
					<div class="m-sub mono">
						<span class="net-dot {netClass}"></span>{target.id} · {connLabel} · {fps} fps
					</div>
				</div>
			</div>

			{#if panel === 'chat'}
				<Chat {messages} bind:chatInput bind:chatBox onSend={onSendChat} onBack={() => (panel = 'menu')} />
			{:else}
				<div class="m-cols">
					<VideoMenu
						{codec}
						{encoder}
						{hostCodecs}
						{hostEncoders}
						{hostDisplays}
						decoderInfo={decoderCodec}
						{activeInfo}
						activeFps={hostFps}
						activeRes={hostRes}
						{streamRes}
						{streamFps}
						{streamBitrate}
						{streamQuality}
						{streamDisplay}
						bind:fitMode
						{onCodec}
						{onEncoder}
						{onRes}
						{onFps}
						{onBitrate}
						{onQuality}
						{onMonitor}
					/>
					<SessionActions
						{mode}
						{fullscreen}
						{micOn}
						{unread}
						{note}
						{floating}
						{transmitAudio}
						{muteHost}
						{keepVisible}
						{framePacing}
						{onFullscreen}
						{onSendClipboard}
						{onPickFile}
						onOpenFiles={() => api.openFilesWindow(playId, target.name).catch(() => {})}
						{onToggleMic}
						{onOpenChat}
						{onToggleFloating}
						{onReverse}
						{onToggleTransmit}
						{onToggleMute}
						{onToggleKeepVisible}
						{onToggleFramePacing}
					/>
				</div>
			{/if}

			<button class="m-end" role="menuitem" onclick={onEnd}>
				<Icon name="power" size={16} />{t('session.end')}
			</button>
		</div>
	{/if}
</div>

<style>
	.net-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex: none;
		display: inline-block;
	}
	.net-dot.ok {
		background: var(--ok);
	}
	.net-dot.mid {
		background: #f4bf4f;
	}
	.net-dot.bad {
		background: var(--danger);
	}
	.scrim {
		position: absolute;
		inset: 0;
		border: none;
		background: transparent;
		padding: 0;
		margin: 0;
		cursor: default;
		z-index: 6;
	}
	.dock {
		position: absolute;
		top: 0;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		align-items: center;
		z-index: 7;
	}
	.dock.hidden {
		display: none;
	}
	.dock.floating {
		align-items: flex-start;
	}
	.handle {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		padding: 5px 12px;
		border: 1px solid oklch(0.42 0.016 265 / 0.6);
		border-top: none;
		border-radius: 0 0 var(--r-pill) var(--r-pill);
		background: oklch(0.18 0.012 265 / 0.78);
		color: oklch(0.96 0.008 265);
		cursor: pointer;
		opacity: 0.55;
		transition: opacity var(--dur) var(--ease), background var(--dur) var(--ease);
		backdrop-filter: blur(6px);
		touch-action: none;
	}
	.dock.floating .handle {
		border-top: 1px solid oklch(0.42 0.016 265 / 0.6);
		border-radius: var(--r-pill);
	}
	.handle.grab {
		cursor: grab;
	}
	.handle:hover,
	.handle.active {
		opacity: 1;
		background: oklch(0.24 0.014 265 / 0.92);
	}
	.hfps {
		font-size: 11px;
	}
	/* the two-pane file panel needs more room than the single-column bodies */
	.menu.wide {
		width: 560px;
	}
	.menu {
		margin-top: 6px;
		width: 360px;
		padding: 12px;
		border-radius: var(--r-lg);
		background: oklch(0.17 0.012 265 / 0.96);
		border: 1px solid oklch(0.36 0.016 265 / 0.7);
		box-shadow: var(--shadow-lg);
		color: oklch(0.96 0.008 265);
		backdrop-filter: blur(10px);
	}
	.m-cols {
		display: flex;
		gap: 14px;
		align-items: stretch;
	}
	.m-head {
		padding: 2px 4px 12px;
		border-bottom: 1px solid oklch(0.32 0.016 265 / 0.6);
		margin-bottom: 10px;
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.m-avatar {
		width: 34px;
		height: 34px;
		border-radius: 9px;
		object-fit: cover;
		flex: none;
	}
	.m-name {
		font-family: var(--font-display);
		font-size: 15px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.m-sub {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 11px;
		color: oklch(0.72 0.02 265);
		margin-top: 4px;
	}
	.m-end {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		width: 100%;
		margin-top: 10px;
		padding: 11px 0;
		border: 1px solid color-mix(in oklch, var(--danger) 50%, transparent);
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--danger) 22%, transparent);
		color: #ffd9d4;
		font-size: 13.5px;
		font-weight: 600;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.m-end:hover {
		background: color-mix(in oklch, var(--danger) 40%, transparent);
		color: #fff;
	}
</style>
