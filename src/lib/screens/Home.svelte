<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { recentPeers } from '$lib/peers.svelte';
	import { api, copyText, type GameInfo } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		selfId: string;
		selfPw?: string;
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
			<span class="badge online"><span class="dot"></span>{t('home.ready')}</span>
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
			<span class="pw mono">{selfPw || '—'}</span>
			<button class="icon-btn push" title={t('home.refresh')} aria-label={t('home.refreshPw')} onclick={onRefreshPw}>
				<Icon name="refresh" size={16} />
			</button>
		</div>
		<p class="help">{t('home.help')}</p>
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
	.help {
		font-size: 12.5px;
		color: var(--text-faint);
		margin-top: 16px;
		line-height: 1.5;
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
</style>
