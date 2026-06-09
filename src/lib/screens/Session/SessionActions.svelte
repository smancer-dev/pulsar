<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import Controllers from '$lib/Controllers.svelte';
	import { t } from '$lib/i18n.svelte';

	// Right column of the full (remote-desktop) session menu: the session-action grid
	// (fullscreen / clipboard / files / mic / chat — the side channels are remote-only),
	// the toggle switches, and the controller status. All actions are parent callbacks.
	type Props = {
		mode: 'remote' | 'game';
		fullscreen: boolean;
		micOn: boolean;
		unread: number;
		note: string;
		floating: boolean;
		transmitAudio: boolean;
		muteHost: boolean;
		keepVisible: boolean;
		framePacing: boolean;
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
	};
	let {
		mode,
		fullscreen,
		micOn,
		unread,
		note,
		floating,
		transmitAudio,
		muteHost,
		keepVisible,
		framePacing,
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
		onToggleFramePacing
	}: Props = $props();
</script>

<div class="m-col">
	<div class="m-sec-head">{t('session.secSession')}</div>
	<div class="m-grid">
		<button class="m-item" role="menuitem" onclick={onFullscreen}>
			<Icon name="expand" size={18} />
			<span>{fullscreen ? t('session.exitFullscreen') : t('session.fullscreen')}</span>
		</button>
		{#if mode !== 'game'}
			<button class="m-item" role="menuitem" onclick={onSendClipboard}>
				<Icon name="clipboard" size={18} />
				<span>{t('session.clipboard')}</span>
			</button>
			<button class="m-item" role="menuitem" onclick={onPickFile}>
				<Icon name="file" size={18} />
				<span>{t('session.files')}</span>
			</button>
			<button class="m-item" class:active={micOn} role="menuitem" onclick={onToggleMic}>
				<Icon name="mic" size={18} />
				<span>{micOn ? t('session.micOn') : t('session.mic')}</span>
			</button>
			<button class="m-item wide" role="menuitem" onclick={onOpenChat}>
				<Icon name="chat" size={18} />
				<span>{t('session.chat')}</span>
				{#if unread > 0}<span class="badge">{unread}</span>{/if}
			</button>
		{/if}
	</div>
	{#if note}<div class="m-note">{note}</div>{/if}
	<button class="m-toggle" class:on={floating} role="menuitemcheckbox" aria-checked={floating} onclick={onToggleFloating}>
		<Icon name="grip" size={16} />
		<span>{t('session.floatMenu')}</span>
		<span class="sw" class:on={floating}></span>
	</button>
	{#if mode !== 'game'}
		<button class="m-toggle" role="menuitem" onclick={onReverse}>
			<Icon name="refresh" size={16} />
			<span>{t('session.reverse')}</span>
		</button>
	{/if}
	<button class="m-toggle" class:on={transmitAudio} role="menuitemcheckbox" aria-checked={transmitAudio} onclick={onToggleTransmit}>
		<Icon name="speaker" size={16} />
		<span>{t('session.audioTransmit')}</span>
		<span class="sw" class:on={transmitAudio}></span>
	</button>
	<button class="m-toggle" class:on={muteHost} role="menuitemcheckbox" aria-checked={muteHost} onclick={onToggleMute}>
		<Icon name="speaker" size={16} />
		<span>{t('session.audioMute')}</span>
		<span class="sw" class:on={muteHost}></span>
	</button>
	<button class="m-toggle" class:on={keepVisible} role="menuitemcheckbox" aria-checked={keepVisible} onclick={onToggleKeepVisible}>
		<Icon name="menu" size={16} />
		<span>{t('session.keepVisible')}</span>
		<span class="sw" class:on={keepVisible}></span>
	</button>
	<button class="m-toggle" class:on={framePacing} role="menuitemcheckbox" aria-checked={framePacing} onclick={onToggleFramePacing}>
		<Icon name="refresh" size={16} />
		<span>{t('session.framePacing')}</span>
		<span class="sw" class:on={framePacing}></span>
	</button>
	<div class="m-ctrls">
		<div class="m-seg-lab">{t('controllers.title')}</div>
		<Controllers compact />
	</div>
</div>

<style>
	/* right column carries the divider between the two menu columns (the original used a
	   `.m-col + .m-col` adjacency selector, which can't cross component boundaries). */
	.m-col {
		flex: 1 1 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
		border-left: 1px solid oklch(0.32 0.016 265 / 0.5);
		padding-left: 14px;
	}
	.m-sec-head {
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: oklch(0.6 0.02 265);
		padding: 2px 2px 4px;
	}
	.m-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 6px;
	}
	.m-item {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		padding: 12px 6px;
		border: 1px solid oklch(0.3 0.014 265 / 0.6);
		border-radius: var(--r-sm);
		background: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.94 0.008 265);
		font-size: 12px;
		font-weight: 500;
		cursor: pointer;
		position: relative;
		transition: background var(--dur) var(--ease);
	}
	.m-item:hover:not(:disabled) {
		background: oklch(0.3 0.016 272 / 0.8);
	}
	.m-item:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.m-item.wide {
		grid-column: 1 / -1;
		flex-direction: row;
		justify-content: center;
		padding: 11px 6px;
	}
	.m-item.active {
		background: color-mix(in oklch, var(--accent) 30%, transparent);
		border-color: var(--accent);
		color: #fff;
	}
	.badge {
		min-width: 17px;
		height: 17px;
		padding: 0 4px;
		border-radius: var(--r-pill);
		background: var(--accent);
		color: #fff;
		font-size: 10.5px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.m-note {
		margin-top: 9px;
		font-size: 11.5px;
		color: oklch(0.82 0.02 265);
		text-align: center;
		line-height: 1.4;
	}
	.m-ctrls {
		margin-top: auto;
		padding-top: 8px;
		border-top: 1px solid oklch(0.32 0.016 265 / 0.5);
	}
	.m-ctrls .m-seg-lab {
		display: block;
		margin-bottom: 6px;
	}
	.m-seg-lab {
		font-size: 11.5px;
		color: oklch(0.74 0.02 265);
		flex: none;
	}
	/* floating-menu toggle row with a switch */
	.m-toggle {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		margin-top: 8px;
		padding: 10px;
		border: 1px solid oklch(0.3 0.014 265 / 0.6);
		border-radius: var(--r-sm);
		background: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.94 0.008 265);
		font-size: 12.5px;
		font-weight: 500;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.m-toggle:hover {
		background: oklch(0.3 0.016 272 / 0.7);
	}
	.m-toggle span:not(.sw) {
		flex: 1;
		text-align: left;
	}
	.sw {
		flex: none;
		width: 34px;
		height: 18px;
		border-radius: var(--r-pill);
		background: oklch(0.36 0.016 265);
		position: relative;
		transition: background var(--dur) var(--ease);
	}
	.sw::after {
		content: '';
		position: absolute;
		top: 2px;
		left: 2px;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: #fff;
		transition: transform var(--dur) var(--ease);
	}
	.sw.on {
		background: var(--accent);
	}
	.sw.on::after {
		transform: translateX(16px);
	}
</style>
