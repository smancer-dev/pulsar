<script lang="ts">
	// Bottom navigation dock for gaming mode — replaces the left sidebar. The gaming
	// personality is a pure client (no host), so the dock carries only Bağlan + Ayarlar
	// (no Devices / Connections), an identity+status chip, and a live controller count.
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import type { Action } from 'svelte/action';

	type GView = 'home' | 'settings' | 'games';
	type Props = {
		gview: GView;
		onView: (v: GView) => void;
		/** GamepadNav.item — dock buttons are controller/keyboard focusable. */
		navItem: Action<HTMLElement>;
		online: boolean;
		connecting: boolean;
		connError: string;
		fullscreen: boolean;
		onToggleFullscreen: () => void;
		onGoOnline: () => void;
	};
	let {
		gview,
		onView,
		navItem,
		online,
		connecting,
		connError,
		fullscreen,
		onToggleFullscreen,
		onGoOnline
	}: Props = $props();

	const NAV: { id: GView; icon: string }[] = [
		{ id: 'home', icon: 'gaming' },
		{ id: 'settings', icon: 'settings' }
	];

	let avatarUrl = $state('');
	let userName = $state('');
	let pads = $state(0);
	let timer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		api.selfAvatar().then((u) => (avatarUrl = u ?? '')).catch(() => {});
		api.deviceUserName().then((n) => (userName = n ?? '')).catch(() => {});
		const refresh = () =>
			api.controllers()
				.then((c) => (pads = c.filter((p) => p.connected).length))
				.catch(() => {});
		refresh();
		timer = setInterval(refresh, 2000);
	});
	onDestroy(() => clearInterval(timer));
</script>

<nav class="dock">
	<div class="me">
		<div class="meavatar">
			{#if avatarUrl}<img src={avatarUrl} alt={t('sidebar.me')} />{:else}{t('sidebar.me')}{/if}
		</div>
		<div class="meinfo">
			<div class="mename">{userName || t('sidebar.thisDevice')}</div>
			<div class="mestatus" class:off={!online}>
				<span class="dot"></span>
				{#if connecting}{t('status.connecting')}{:else if online}{t('status.online')}{:else}{t('status.offline')}{/if}
			</div>
		</div>
		{#if !online && !connecting}
			<button class="reconnect" onclick={onGoOnline} title={connError}>{t('status.goOnline')}</button>
		{/if}
	</div>

	<div class="navmid">
		{#each NAV as n (n.id)}
			<button class="navlink" class:on={gview === n.id} use:navItem onclick={() => onView(n.id)}>
				<Icon name={n.icon} size={20} />
				<span>{t('nav.' + (n.id === 'home' ? 'home' : 'settings'))}</span>
			</button>
		{/each}
	</div>

	<div class="right">
		<button
			class="fsbtn"
			class:on={fullscreen}
			use:navItem
			onclick={onToggleFullscreen}
			title={t('gaming.fullscreen')}
			aria-label={t('gaming.fullscreen')}
			aria-pressed={fullscreen}
		>
			<Icon name="expand" size={17} />
		</button>
		<div class="pads" title={t('controllers.title')}>
			<Icon name="gaming" size={16} />
			<span class="mono">{pads}</span>
		</div>
	</div>
</nav>

<style>
	.dock {
		flex: none;
		display: grid;
		grid-template-columns: 1fr auto 1fr;
		align-items: center;
		gap: 12px;
		height: 72px;
		padding: 0 18px;
		background: var(--surface-2);
		border-top: 1px solid var(--border);
	}
	.me {
		display: flex;
		align-items: center;
		gap: 9px;
		min-width: 0;
	}
	.meavatar {
		width: 34px;
		height: 34px;
		border-radius: 9px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 11px;
		font-family: var(--font-display);
		overflow: hidden;
		flex: none;
	}
	.meavatar img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.meinfo {
		min-width: 0;
	}
	.mename {
		font-size: 13px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.mestatus {
		font-size: 11.5px;
		color: var(--ok);
		display: flex;
		align-items: center;
		gap: 5px;
	}
	.mestatus .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--ok);
	}
	.mestatus.off {
		color: var(--text-faint);
	}
	.mestatus.off .dot {
		background: var(--border-strong);
	}
	.reconnect {
		margin-left: 4px;
		font-size: 11.5px;
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
		border: 1px solid var(--accent-soft-2);
		border-radius: var(--r-sm);
		padding: 5px 10px;
		cursor: pointer;
	}
	.navmid {
		display: flex;
		gap: 8px;
	}
	.navlink {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 3px;
		min-width: 84px;
		padding: 9px 16px;
		border: 1px solid transparent;
		border-radius: var(--r);
		cursor: pointer;
		font-family: var(--font-sans);
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		background: transparent;
		transition: all var(--dur) var(--ease);
	}
	.navlink:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.navlink.on {
		color: var(--accent-press);
		background: var(--accent-soft);
	}
	.right {
		justify-self: end;
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.fsbtn {
		display: grid;
		place-items: center;
		width: 38px;
		height: 38px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface);
		color: var(--text-muted);
		cursor: pointer;
		transition: all var(--dur) var(--ease);
	}
	.fsbtn:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.fsbtn.on {
		background: var(--accent-soft);
		border-color: var(--accent);
		color: var(--accent-press);
	}
	.pads {
		display: flex;
		align-items: center;
		gap: 6px;
		color: var(--text-muted);
		font-size: 13px;
	}
</style>
