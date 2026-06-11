<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { ui } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';
	import Home from '$lib/screens/Home.svelte';
	import Devices from '$lib/screens/Devices.svelte';
	import Settings from '$lib/screens/Settings.svelte';
	import Games from '$lib/screens/Games.svelte';
	import Sidebar from './Sidebar.svelte';
	import type { Game } from '$lib/games.svelte';
	import type { Target } from './sessions.svelte';

	type View = 'home' | 'devices' | 'gaming' | 'settings';
	type Props = {
		nav: { id: View; icon: string }[];
		view: View;
		mode: 'remote' | 'game';
		selfId: string;
		selfPw: string;
		online: boolean;
		connecting: boolean;
		connError: string;
		connectErr: string;
		hostSessions: { peer: string; since: number }[];
		activity: string[];
		onView: (v: View) => void;
		onGoOnline: () => void;
		onMode: (m: 'remote' | 'game') => void;
		onRefreshPw: () => void;
		onDisconnect: (peer: string) => void;
		onConnect: (target: Target, m?: 'remote' | 'game', gameId?: string) => void;
		onStream: (game: Game) => void;
		onClearConnectErr: () => void;
		/** Settle the password prompt queued for `target` (game-list fetch auth). */
		onAuthDone?: (target: string) => void;
	};
	let {
		nav,
		view,
		mode,
		selfId,
		selfPw,
		online,
		connecting,
		connError,
		connectErr,
		hostSessions,
		activity,
		onView,
		onGoOnline,
		onMode,
		onRefreshPw,
		onDisconnect,
		onConnect,
		onStream,
		onClearConnectErr,
		onAuthDone = () => {}
	}: Props = $props();
</script>

<div class="body">
	<Sidebar {nav} {view} {online} {connecting} {connError} hostCount={hostSessions.length} {onView} {onGoOnline} />

	<!-- content -->
	<main class="content">
		{#if connectErr}
			<div class="flash" role="alert">
				<Icon name="shield" size={16} />
				<span>{connectErr}</span>
				<button class="flashx" aria-label={t('flash.close')} onclick={onClearConnectErr}>
					<Icon name="x" size={14} />
				</button>
			</div>
		{/if}
		{#if view === 'home'}
			<Home
				{selfId}
				{selfPw}
				{online}
				{connecting}
				{mode}
				{hostSessions}
				{activity}
				debug={ui.debug}
				{onMode}
				{onRefreshPw}
				{onDisconnect}
				{onConnect}
				{onAuthDone}
			/>
		{:else if view === 'devices'}
			<Devices {onConnect} />
		{:else if view === 'gaming'}
			<Games {onStream} />
		{:else if view === 'settings'}
			<Settings onReconnect={onGoOnline} />
		{/if}
	</main>
</div>

<style>
	.body {
		display: flex;
		flex: 1;
		min-height: 0;
		width: 100%;
	}
	.flash {
		display: flex;
		align-items: center;
		gap: 9px;
		margin-bottom: 16px;
		padding: 11px 14px;
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--danger) 12%, var(--surface));
		border: 1px solid color-mix(in oklch, var(--danger) 40%, var(--border));
		color: var(--danger);
		font-size: 13px;
		line-height: 1.4;
	}
	.flash span {
		flex: 1;
		word-break: break-word;
	}
	.flashx {
		flex: none;
		border: none;
		background: transparent;
		color: var(--danger);
		cursor: pointer;
		padding: 2px;
		display: grid;
		place-items: center;
		border-radius: 4px;
	}
	.flashx:hover {
		background: color-mix(in oklch, var(--danger) 18%, transparent);
	}
</style>
