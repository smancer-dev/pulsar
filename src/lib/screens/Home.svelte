<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { historyPeers } from '$lib/peers.svelte';
	import { api, type GameInfo } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import SelfCard from './Home/SelfCard.svelte';
	import LanDevices from './Home/LanDevices.svelte';

	type Target = { name: string; id: string };
	type Props = {
		selfId: string;
		selfPw?: string;
		online?: boolean;
		connecting?: boolean;
		mode: 'remote' | 'game';
		hostSessions: { peer: string; since: number }[];
		activity: string[];
		debug?: boolean;
		onMode: (m: 'remote' | 'game') => void;
		onRefreshPw?: () => void;
		onDisconnect?: (peer: string) => void;
		onConnect: (t: Target, m?: 'remote' | 'game', gameId?: string) => void;
	};
	let {
		selfId,
		selfPw = '',
		online = false,
		connecting = false,
		mode,
		hostSessions,
		activity,
		debug = false,
		onMode,
		onRefreshPw = () => {},
		onDisconnect = () => {},
		onConnect
	}: Props = $props();

	let showAllHistory = $state(false);
	const allHistory = $derived(historyPeers());
	const recents = $derived(showAllHistory ? allHistory : allHistory.slice(0, 3));

	let localIp = $state('');
	onMount(() => {
		api.localIp().then((ip) => (localIp = ip)).catch(() => {});
	});

	let target = $state('');

	// client → game mode: fetch the host's published games
	let hostGames = $state<GameInfo[] | null>(null);
	let loadingGames = $state(false);
	let gamesErr = $state('');

	// Auth (password / host approval) is handled by the connect flow via events.
	async function fetchGames() {
		if (!canConnect) return;
		loadingGames = true;
		gamesErr = '';
		hostGames = null;
		try {
			hostGames = await api.listRemoteGames(fmt(target));
		} catch (e) {
			gamesErr = e instanceof Error ? e.message : String(e);
		} finally {
			loadingGames = false;
		}
	}
	function playGame(g: GameInfo) {
		onConnect({ name: g.title, id: fmt(target) }, 'game', g.id);
	}

	// A target is either a 9-digit relay ID (grouped) or an IP / IP:port (has '.'/':').
	const isAddr = (v: string) => /[.:]/.test(v);
	const fmt = (v: string) =>
		isAddr(v)
			? v.replace(/[^0-9.:]/g, '').slice(0, 21)
			: v
					.replace(/\D/g, '')
					.slice(0, 9)
					.replace(/(\d{3})(?=\d)/g, '$1 ')
					.trim();
	const digits = $derived(target.replace(/\D/g, ''));
	const ipRe = /^\d{1,3}(\.\d{1,3}){3}(:\d{1,5})?$/;
	const canConnect = $derived(isAddr(target) ? ipRe.test(target.trim()) : digits.length >= 6);

	function go() {
		// No password up front — startConnect prompts via a popup if the host asks.
		if (canConnect) onConnect({ name: t('home.remoteDevice'), id: fmt(target) }, mode);
	}
	function initials(name: string) {
		return name
			.split(' ')
			.map((w) => w[0])
			.slice(0, 2)
			.join('')
			.toUpperCase();
	}
</script>

<div class="head">
	<div>
		<h1>{t('home.title')}</h1>
		<p class="sub">{t('home.sub')}</p>
	</div>
	<div class="seg">
		<button class:active={mode === 'remote'} onclick={() => onMode('remote')}>{t('home.modeRemote')}</button>
		<button class:active={mode === 'game'} onclick={() => onMode('game')}>{t('home.modeGame')}</button>
	</div>
</div>

<div class="grid">
	<SelfCard
		{selfId}
		{selfPw}
		{online}
		{connecting}
		{hostSessions}
		{activity}
		{debug}
		{localIp}
		{onRefreshPw}
		{onDisconnect}
	/>

	<div class="card col">
		<span class="eyebrow mono">{mode === 'game' ? t('home.startGameSession') : t('home.connectRemote')}</span>
		<div class="lab mt">{t('home.deviceId')}</div>
		<div class="field">
			<Icon name="connect" size={17} />
			<input
				value={target}
				oninput={(e) => (target = fmt(e.currentTarget.value))}
				onkeydown={(e) => e.key === 'Enter' && go()}
				placeholder="000 000 000"
				aria-label={t('home.targetAria')}
				style="font-family:var(--font-mono);font-size:19px;letter-spacing:0.06em"
			/>
		</div>
		<div style="font-size:12px;color:var(--text-faint);margin-top:7px">{t('home.idOrIp')}</div>
		{#if mode === 'game'}
			<button class="btn btn-primary go" disabled={!canConnect || loadingGames} onclick={() => fetchGames()}>
				<Icon name="gaming" size={17} />
				{loadingGames ? t('home.fetching') : t('home.fetchGames')}
			</button>
			{#if gamesErr}<div class="ginfo err">{gamesErr}</div>{/if}
			{#if hostGames}
				{#if hostGames.length === 0}
					<div class="ginfo">{t('home.noHostGames')}</div>
				{:else}
					<div class="hostgames">
						{#each hostGames as g (g.id)}
							<button class="recent-row" onclick={() => playGame(g)}>
								<span class="ravatar">{initials(g.title)}</span>
								<span class="rmeta"><span class="rname">{g.title}</span><span class="rid mono">{g.kind}</span></span>
								<Icon name="gaming" size={15} class="push" />
							</button>
						{/each}
					</div>
				{/if}
			{/if}
		{:else}
			<button class="btn btn-primary go" disabled={!canConnect} onclick={go}>
				<Icon name="connect" size={17} />{t('home.connect')}
			</button>
		{/if}

		<div class="recents">
			<div class="rlab" style="display:flex;align-items:center;gap:8px">
				<span>{t('home.recents')}</span>
				{#if allHistory.length > 3}
					<button
						type="button"
						onclick={() => (showAllHistory = !showAllHistory)}
						style="margin-left:auto;background:none;border:none;color:var(--accent-press);font:inherit;font-size:12px;cursor:pointer"
					>
						{showAllHistory ? t('home.showLess') : t('home.seeAll')}
					</button>
				{/if}
			</div>
			{#if recents.length === 0}
				<div class="empty">{t('home.noRecents')}</div>
			{:else}
				{#each recents as r (r.id)}
				<button class="recent-row" onclick={() => onConnect({ name: r.name, id: r.id }, mode)}>
					<span class="ravatar">{initials(r.name)}</span>
					<span class="rmeta">
						<span class="rname">{r.name}</span>
						<span class="rid mono">{r.id}</span>
					</span>
					<Icon name="arrowRight" size={15} class="push" />
				</button>
				{/each}
			{/if}
		</div>
	</div>
</div>

<LanDevices {mode} {onConnect} />

<style>
	.head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		margin-bottom: 28px;
	}
	h1 {
		font-size: 27px;
		letter-spacing: -0.03em;
	}
	.sub {
		color: var(--text-muted);
		font-size: 14.5px;
		margin: 7px 0 0;
	}
	.grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 18px;
	}
	.card.col {
		display: flex;
		flex-direction: column;
	}
	.eyebrow {
		font-size: 11px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.lab {
		font-size: 12.5px;
		color: var(--text-muted);
		font-weight: 600;
		margin-bottom: 7px;
	}
	.lab.mt {
		margin-top: 18px;
	}
	.go {
		justify-content: center;
		margin-top: 12px;
	}
	.recents {
		margin-top: auto;
		padding-top: 20px;
	}
	.rlab {
		font-size: 11.5px;
		color: var(--text-faint);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		margin-bottom: 10px;
	}
	.empty {
		font-size: 12.5px;
		color: var(--text-faint);
		line-height: 1.5;
		padding: 10px 12px;
		border: 1px dashed var(--border);
		border-radius: var(--r-sm);
	}
	.ginfo {
		font-size: 12.5px;
		color: var(--text-faint);
		margin-top: 10px;
		line-height: 1.5;
	}
	.ginfo.err {
		color: var(--danger);
		word-break: break-word;
	}
	.hostgames {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin-top: 12px;
	}
	.recents .recent-row {
		margin-bottom: 6px;
	}
	.ravatar {
		width: 30px;
		height: 30px;
		border-radius: 8px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 11px;
		font-family: var(--font-display);
		flex: none;
	}
	.rmeta {
		display: flex;
		flex-direction: column;
		line-height: 1.25;
	}
	.rname {
		font-size: 13.5px;
		font-weight: 600;
	}
	.rid {
		font-size: 11px;
		color: var(--text-faint);
	}
</style>
