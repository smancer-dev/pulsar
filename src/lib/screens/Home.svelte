<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { historyPeers, removeFromHistory, fmtPeerId, addPeer, isSaved } from '$lib/peers.svelte';
	import { api, onNodePort } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import { canConnectTarget, fmtTarget } from '$lib/connectTarget';
	import SelfCard from './Home/SelfCard.svelte';
	import LanDevices from './Home/LanDevices.svelte';

	type Target = { name: string; id: string };
	type Props = {
		selfId: string;
		selfPw?: string;
		online?: boolean;
		connecting?: boolean;
		/** Host's unattended-access toggle — no one-time password is issued when ON. */
		unattended?: boolean;
		hostSessions: { peer: string; since: number }[];
		activity: string[];
		debug?: boolean;
		onRefreshPw?: () => void;
		onDisconnect?: (peer: string) => void;
		onConnect: (t: Target, m?: 'remote' | 'game', gameId?: string) => void;
	};
	let {
		selfId,
		selfPw = '',
		online = false,
		connecting = false,
		unattended = false,
		hostSessions,
		activity,
		debug = false,
		onRefreshPw = () => {},
		onDisconnect = () => {},
		onConnect
	}: Props = $props();

	let showAllHistory = $state(false);
	const allHistory = $derived(historyPeers());
	const recents = $derived(showAllHistory ? allHistory : allHistory.slice(0, 3));

	// Local IP + the node's ACTUAL bound port (direct-connect target "ip:port").
	// Snapshot at mount for late opens; the node-port event keeps it live across
	// go_online rebinds (e.g. after a Settings port change → reconnect).
	let localIp = $state('');
	let nodePort = $state(0);
	onMount(() => {
		api.localIp().then((ip) => (localIp = ip)).catch(() => {});
		api.nodePort().then((p) => (nodePort = p)).catch(() => {});
		let off: (() => void) | undefined;
		let dead = false;
		onNodePort((p) => (nodePort = p)).then((o) => {
			// If Home unmounted before listen() resolved, unlisten right away —
			// otherwise this late registration would leak past the cleanup below.
			if (dead) o();
			else off = o;
		});
		return () => {
			dead = true;
			off?.();
		};
	});

	let target = $state('');

	function setTarget(v: string) {
		target = fmtTarget(v);
	}
	const canConnect = $derived(canConnectTarget(target));

	function go() {
		// No password up front — startConnect prompts via a popup if the host asks. No
		// mode argument: the shell's default connect mode is remote (gaming streaming has
		// its own screen now), so a plain connect here is always remote desktop.
		if (canConnect) onConnect({ name: t('home.remoteDevice'), id: fmtTarget(target) });
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
</div>

<div class="grid">
	<SelfCard
		{selfId}
		{selfPw}
		{online}
		{unattended}
		{connecting}
		{hostSessions}
		{activity}
		{debug}
		{localIp}
		{nodePort}
		{onRefreshPw}
		{onDisconnect}
	/>

	<div class="card col">
		<span class="eyebrow mono">{t('home.connectRemote')}</span>
		<div class="lab mt">{t('home.deviceId')}</div>
		<div class="field">
			<Icon name="connect" size={17} />
			<input
				value={target}
				oninput={(e) => setTarget(e.currentTarget.value)}
				onkeydown={(e) => e.key === 'Enter' && go()}
				placeholder="000 000 000"
				aria-label={t('home.targetAria')}
				style="font-family:var(--font-mono);font-size:19px;letter-spacing:0.06em"
			/>
		</div>
		<div style="font-size:12px;color:var(--text-faint);margin-top:7px">{t('home.idOrIp')}</div>
		<button class="btn btn-primary go" disabled={!canConnect} onclick={go}>
			<Icon name="connect" size={17} />{t('home.connect')}
		</button>

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
				<div class="rrow">
					<button class="recent-row" onclick={() => onConnect({ name: r.name, id: r.id })}>
						<span class="ravatar">
							{#if r.avatar}<img class="rimg" src={r.avatar} alt="" />{:else}{initials(r.name)}{/if}
						</span>
						<span class="rmeta">
							<span class="rname">{r.name}</span>
							<span class="rid mono">{fmtPeerId(r.id)}</span>
						</span>
						<Icon name="arrowRight" size={15} class="push" />
					</button>
					<!-- Save to the address book (Devices) — hidden once it's saved. -->
					{#if !isSaved(r.id)}
						<button
							class="rsave"
							title={t('home.saveRecent')}
							aria-label={t('home.saveRecent')}
							onclick={() => addPeer(r.name, r.id)}><Icon name="star" size={13} /></button
						>
					{/if}
					<button
						class="rdel"
						title={t('home.removeRecent')}
						aria-label={t('home.removeRecent')}
						onclick={() => removeFromHistory(r.id)}>×</button
					>
				</div>
				{/each}
			{/if}
		</div>
	</div>
</div>

<LanDevices {onConnect} />

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
	.recents .recent-row {
		margin-bottom: 6px;
	}
	/* row wrapper: the connect button + the history-remove × side by side */
	.rrow {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.rrow .recent-row {
		flex: 1;
		min-width: 0;
	}
	.rsave {
		flex: none;
		width: 26px;
		height: 26px;
		margin-bottom: 6px;
		border: none;
		border-radius: 7px;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		display: grid;
		place-items: center;
	}
	.rsave:hover {
		color: var(--warn);
		background: var(--surface-3);
	}
	.rdel {
		flex: none;
		width: 26px;
		height: 26px;
		margin-bottom: 6px;
		border: none;
		border-radius: 7px;
		background: transparent;
		color: var(--text-faint);
		font-size: 16px;
		line-height: 1;
		cursor: pointer;
		display: grid;
		place-items: center;
	}
	.rdel:hover {
		background: var(--accent-soft);
		color: var(--accent);
	}
	.ravatar {
		overflow: hidden;
	}
	.rimg {
		width: 100%;
		height: 100%;
		object-fit: cover;
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
