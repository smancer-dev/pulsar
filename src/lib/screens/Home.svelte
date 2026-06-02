<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { recentPeers, addPeer } from '$lib/peers.svelte';
	import { api, copyText, type GameInfo, type LanDevice } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

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

	const recents = $derived(recentPeers(3));

	// LAN auto-discovery: poll the core for Pulsar devices announcing on this
	// network (the multicast beacon). Works even offline (relay-less).
	let lan = $state<LanDevice[]>([]);
	async function refreshLan() {
		try {
			lan = await api.lanDevices();
		} catch {
			/* core not bound yet — keep the last list */
		}
	}
	onMount(() => {
		refreshLan();
		const timer = setInterval(refreshLan, 2000);
		return () => clearInterval(timer);
	});

	let target = $state('');
	let copied = $state(false);

	// client → game mode: fetch the host's published games
	let hostGames = $state<GameInfo[] | null>(null);
	let loadingGames = $state(false);
	let gamesErr = $state('');

	// Auth (password / host approval) is handled by the connect flow via events.
	async function fetchGames() {
		if (digits.length < 6) return;
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

	const fmt = (v: string) =>
		v
			.replace(/\D/g, '')
			.slice(0, 9)
			.replace(/(\d{3})(?=\d)/g, '$1 ')
			.trim();
	const digits = $derived(target.replace(/\D/g, ''));

	async function copyId() {
		const ok = await copyText(selfId.replace(/\s/g, ''));
		if (ok) {
			copied = true;
			setTimeout(() => (copied = false), 1400);
		}
	}
	function go() {
		// No password up front — startConnect prompts via a popup if the host asks.
		if (digits.length >= 6) onConnect({ name: t('home.remoteDevice'), id: fmt(target) }, mode);
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
	<div class="card">
		<div class="row sb">
			<span class="eyebrow mono">{t('home.allowThis')}</span>
			<span class="badge" class:online class:pending={connecting && !online} class:off={!online && !connecting}>
				<span class="dot"></span>
				{#if connecting}{t('status.connecting')}{:else if online}{t('home.ready')}{:else}{t('status.offline')}{/if}
			</span>
		</div>
		<div class="lab">{t('home.deviceId')}</div>
		<div class="row">
			<span class="bigid mono">{selfId}</span>
			<button class="icon-btn push" onclick={copyId} title={t('home.copy')} aria-label={t('home.copyId')}>
				<Icon name={copied ? 'check' : 'copy'} size={17} />
			</button>
		</div>
		<div class="sep"></div>
		<div class="lab">{t('home.otp')}</div>
		<div class="row">
			<span class="pw mono">{online ? selfPw || '—' : '—'}</span>
			<button
				class="icon-btn push"
				title={t('home.refresh')}
				aria-label={t('home.refreshPw')}
				onclick={onRefreshPw}
				disabled={!online}
			>
				<Icon name="refresh" size={16} />
			</button>
		</div>
		<div class="sep"></div>
		<div class="connhdr">{t('home.connectedHdr')}</div>
		{#if hostSessions.length === 0}
			<div class="connempty">{t('home.noConnected')}</div>
		{:else}
			{#each hostSessions as s (s.peer)}
				<div class="connrow">
					<span class="cdot"></span><span class="mono">{s.peer}</span>
					<button class="kick" onclick={() => onDisconnect(s.peer)} title={t('home.kick')}>
						<Icon name="x" size={12} />{t('home.kickLabel')}
					</button>
				</div>
			{/each}
		{/if}
		{#if debug && activity.length > 0}
			<div class="actlog">
				{#each activity as line, i (i)}<div class="actline">{line}</div>{/each}
			</div>
		{/if}
	</div>

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
				inputmode="numeric"
				aria-label={t('home.targetAria')}
				style="font-family:var(--font-mono);font-size:19px;letter-spacing:0.06em"
			/>
		</div>
		{#if mode === 'game'}
			<button class="btn btn-primary go" disabled={digits.length < 6 || loadingGames} onclick={() => fetchGames()}>
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
			<button class="btn btn-primary go" disabled={digits.length < 6} onclick={go}>
				<Icon name="connect" size={17} />{t('home.connect')}
			</button>
		{/if}

		<div class="recents">
			<div class="rlab">{t('home.recents')}</div>
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

<section class="lan">
	<div class="lanhdr"><span class="lpulse"></span>{t('devices.lanTitle')}</div>
	{#if lan.length === 0}
		<div class="lanempty">{t('devices.lanScanning')}</div>
	{:else}
		<div class="langrid">
			{#each lan as d (d.addr + '|' + d.id)}
				<div class="device-tile">
					<span class="ravatar lavatar">{initials(d.name)}</span>
					<div class="lmeta">
						<div class="lname">{d.name}</div>
						<div class="lsub mono">{d.id || d.addr}</div>
					</div>
					{#if d.has_id}
						<div class="lactions">
							<button class="btn btn-primary lbtn" onclick={() => onConnect({ name: d.name, id: d.id }, mode)}>
								{t('home.connect')}
							</button>
							<button class="btn btn-ghost lbtn" onclick={() => addPeer(d.name, d.id, 'pc')}>
								{t('devices.lanSave')}
							</button>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</section>

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
	.row {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.row.sb {
		justify-content: space-between;
		margin-bottom: 18px;
	}
	.push {
		margin-left: auto;
	}
	.icon-btn:disabled {
		opacity: 0.4;
		cursor: not-allowed;
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
	.bigid {
		font-size: 27px;
		font-weight: 500;
		letter-spacing: 0.04em;
		white-space: nowrap;
	}
	.pw {
		font-size: 22px;
		font-weight: 500;
		letter-spacing: 0.12em;
	}
	.sep {
		height: 1px;
		background: var(--border);
		margin: 20px 0;
	}
	.connhdr {
		font-size: 11.5px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-faint);
		margin-bottom: 8px;
	}
	.connempty {
		font-size: 12.5px;
		color: var(--text-faint);
	}
	.connrow {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 0;
		font-size: 13px;
	}
	.cdot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ok);
		flex: none;
	}
	.kick {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-size: 11px;
		font-weight: 600;
		padding: 3px 8px;
		border-radius: var(--r-sm);
		border: 1px solid color-mix(in oklch, var(--danger) 35%, var(--border));
		background: color-mix(in oklch, var(--danger) 10%, transparent);
		color: var(--danger);
		cursor: pointer;
	}
	.kick:hover {
		background: color-mix(in oklch, var(--danger) 20%, transparent);
	}
	.actlog {
		margin-top: 10px;
		border-top: 1px solid var(--border);
		padding-top: 8px;
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.actline {
		font-size: 11.5px;
		color: var(--text-faint);
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
	/* LAN auto-discovery section */
	.lan {
		margin-top: 22px;
	}
	.lanhdr {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 11px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
		margin-bottom: 12px;
	}
	.lpulse {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ok);
		flex: none;
		animation: lpulse 1.8s ease-out infinite;
	}
	@keyframes lpulse {
		0% {
			box-shadow: 0 0 0 0 color-mix(in oklch, var(--ok) 55%, transparent);
		}
		70% {
			box-shadow: 0 0 0 7px transparent;
		}
		100% {
			box-shadow: 0 0 0 0 transparent;
		}
	}
	.lanempty {
		font-size: 12.5px;
		color: var(--text-faint);
		padding: 14px 16px;
		border: 1px dashed var(--border);
		border-radius: var(--r-sm);
	}
	.langrid {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		gap: 12px;
	}
	.lavatar {
		flex: none;
	}
	.lmeta {
		flex: 1;
		min-width: 0;
	}
	.lname {
		font-size: 14px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.lsub {
		font-size: 11.5px;
		color: var(--text-faint);
		margin-top: 3px;
	}
	.lactions {
		display: flex;
		gap: 6px;
		flex: none;
	}
	.lbtn {
		padding: 7px 12px;
		font-size: 13px;
	}
</style>
