<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import Controllers from '$lib/Controllers.svelte';
	import GameForm from '$lib/screens/Games/GameForm.svelte';
	import FolderScan from '$lib/screens/Games/FolderScan.svelte';
	import { api } from '$lib/api';
	import {
		gameStore,
		removeGame,
		saveGames,
		isBuiltin,
		ensureSteamDefault,
		type Game,
		type GameType
	} from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = { onStream: (game: Game) => void };
	let { onStream }: Props = $props();

	const typeLabel = (ty: GameType) => t('type.' + ty);

	// Opened by the add / edit buttons; the modal + its state live in <GameForm>.
	let openForm = $state<(g?: Game) => void>(() => {});

	// ---- launch ----
	async function launch(g: Game) {
		if (g.cmdStart) await api.runCommand(g.cmdStart);
		if (g.type === 'program' && g.path) await api.runCommand(`"${g.path}" ${g.args}`.trim());
		else if (g.type === 'command' && g.command) await api.runCommand(g.command);
		onStream(g);
	}

	onMount(() => {
		// Seed a deletable Steam default once, if Steam is installed.
		api.steamPath().then((p) => ensureSteamDefault(p)).catch(() => {});
	});

	function initials(t: string) {
		return t.split(' ').map((w) => w[0]).slice(0, 2).join('').toUpperCase();
	}
</script>

<div class="head">
	<div><h1>{t('games.title')}</h1><p class="sub">{t('games.sub')}</p></div>
	<button class="btn btn-primary" onclick={() => openForm()}><Icon name="plus" size={17} />{t('games.add')}</button>
</div>

<!-- General host streaming settings (not per-game) -->
<div class="card host">
	<div class="hlab mono">{t('games.hostSettings')}</div>
	<div class="hrow">
		<span class="cfg-lab">{t('games.resolution')}</span>
		<div class="seg">
			{#each ['1080p', '1440p', '4K'] as v (v)}
				<button class:active={gameStore.host.resolution === v} onclick={() => { gameStore.host.resolution = v; saveGames(); }}>{v}</button>
			{/each}
		</div>
	</div>
	<div class="hrow">
		<span class="cfg-lab">{t('games.fps', { fps: gameStore.host.fps })}</span>
		<input class="prange" type="range" min="30" max="240" step="10" bind:value={gameStore.host.fps} onchange={saveGames} aria-label={t('games.fpsAria')} />
	</div>
	<div class="hrow">
		<span class="cfg-lab">{t('games.bitrate', { n: gameStore.host.bitrate })}</span>
		<input class="prange" type="range" min="5" max="100" step="5" bind:value={gameStore.host.bitrate} onchange={saveGames} aria-label={t('games.bitrateAria')} />
	</div>
</div>

<!-- Detected controllers (forwarded to the host while streaming) -->
<div style="margin-bottom:22px"><Controllers /></div>

<!-- Games -->
{#if gameStore.games.length === 0}
	<div class="empty card">
		<Icon name="gaming" size={28} />
		<div class="et">{t('games.empty')}</div>
		<!-- eslint-disable-next-line svelte/no-at-html-tags -->
		<p>{@html t('games.emptyBody')}</p>
	</div>
{:else}
	<div class="grid">
		{#each gameStore.games as g (g.id)}
			<div class="game">
				<span class="cover" style={g.image ? `background-image:url(${g.image});background-size:cover` : ''}>
					{#if !g.image}{initials(g.title)}{/if}
				</span>
				<div class="gmeta">
					<div class="gtitle">{g.title}</div>
					<div class="gtype mono">{typeLabel(g.type)}</div>
				</div>
				<div class="gactions">
					<button class="btn btn-primary play" onclick={() => launch(g)}><Icon name="gaming" size={15} />{t('games.launch')}</button>
					<button class="ic" aria-label={t('games.edit')} onclick={() => openForm(g)}><Icon name="settings" size={15} /></button>
					{#if !isBuiltin(g.id)}
						<button class="ic" aria-label={t('games.remove')} onclick={() => removeGame(g.id)}><Icon name="x" size={15} /></button>
					{/if}
				</div>
			</div>
		{/each}
	</div>
{/if}

<!-- Folder scan -->
<FolderScan />

<GameForm bind:open={openForm} />

<style>
	.head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		margin-bottom: 24px;
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
	.host {
		margin-bottom: 18px;
	}
	.hlab {
		font-size: 10.5px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
		margin-bottom: 14px;
	}
	.hrow {
		padding: 8px 0;
	}
	.cfg-lab {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--text-muted);
		margin-bottom: 8px;
		display: block;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		gap: 12px;
		margin-bottom: 18px;
	}
	.game {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 12px;
		border: 1px solid var(--border);
		border-radius: var(--r-lg);
		background: var(--surface);
	}
	.cover {
		width: 48px;
		height: 48px;
		border-radius: 11px;
		display: grid;
		place-items: center;
		font-family: var(--font-display);
		font-weight: 700;
		flex: none;
		background: var(--accent-soft);
		color: var(--accent);
	}
	.gmeta {
		flex: 1;
		min-width: 0;
	}
	.gtitle {
		font-size: 14.5px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.gtype {
		font-size: 11px;
		color: var(--text-faint);
		margin-top: 2px;
	}
	.gactions {
		display: flex;
		align-items: center;
		gap: 6px;
		flex: none;
	}
	.play {
		padding: 7px 12px;
		font-size: 13px;
	}
	.ic {
		width: 30px;
		height: 30px;
		display: grid;
		place-items: center;
		border: 1px solid var(--border);
		background: var(--surface);
		border-radius: var(--r-sm);
		color: var(--text-faint);
		cursor: pointer;
	}
	.ic:hover {
		color: var(--text);
		border-color: var(--border-strong);
	}
	.empty {
		text-align: center;
		color: var(--text-faint);
		padding: 32px 24px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
		margin-bottom: 18px;
	}
	.empty .et {
		font-family: var(--font-display);
		font-size: 17px;
		color: var(--text);
	}
	.empty p {
		font-size: 13.5px;
		margin: 0;
	}
</style>
