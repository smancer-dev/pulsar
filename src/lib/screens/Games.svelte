<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import Modal from '$lib/Modal.svelte';
	import { api, type ScannedApp } from '$lib/api';
	import {
		gameStore,
		addGame,
		updateGame,
		removeGame,
		addScanned,
		addFolder,
		removeFolder,
		saveGames,
		type Game,
		type GameType
	} from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = { onStream: (game: Game) => void };
	let { onStream }: Props = $props();

	const typeLabel = (ty: GameType) => t('type.' + ty);
	const TYPES: GameType[] = ['program', 'command', 'image'];

	// ---- add / edit modal ----
	let showForm = $state(false);
	let editingId = $state<string | null>(null);
	let form = $state<Omit<Game, 'id'>>(blank());

	function blank(): Omit<Game, 'id'> {
		return { title: '', type: 'program', path: '', args: '', command: '', image: '', cmdStart: '', cmdStop: '' };
	}
	function openAdd() {
		editingId = null;
		form = blank();
		showForm = true;
	}
	function openEdit(g: Game) {
		editingId = g.id;
		form = { title: g.title, type: g.type, path: g.path, args: g.args, command: g.command, image: g.image, cmdStart: g.cmdStart, cmdStop: g.cmdStop };
		showForm = true;
	}
	function submitForm() {
		if (!form.title.trim()) return;
		if (editingId) updateGame(editingId, form);
		else addGame(form);
		showForm = false;
	}

	// ---- launch ----
	async function launch(g: Game) {
		if (g.cmdStart) await api.runCommand(g.cmdStart);
		if (g.type === 'program' && g.path) await api.runCommand(`"${g.path}" ${g.args}`.trim());
		else if (g.type === 'command' && g.command) await api.runCommand(g.command);
		onStream(g);
	}

	// ---- folder scan ----
	let newFolder = $state('');
	let scanning = $state(false);
	let scanResults = $state<ScannedApp[]>([]);
	let scanMsg = $state('');
	let cancelScan = false;
	let autoTimer: ReturnType<typeof setInterval> | null = null;

	async function scanAll(autoAdd = false) {
		if (scanning) return;
		if (gameStore.scan.folders.length === 0) {
			if (!autoAdd) scanMsg = t('games.scanNeedFolder');
			return;
		}
		scanning = true;
		cancelScan = false;
		scanResults = [];
		scanMsg = autoAdd ? t('games.scanningAuto') : t('games.scanning');
		const seen = new Set(gameStore.games.map((g) => g.path));
		const found: ScannedApp[] = [];
		for (const folder of gameStore.scan.folders) {
			if (cancelScan) break;
			try {
				for (const a of await api.scanFolder(folder)) {
					if (!seen.has(a.path) && !found.some((f) => f.path === a.path)) found.push(a);
				}
			} catch {
				/* skip unreadable folder */
			}
		}
		if (autoAdd) {
			// Automatic scan: add everything new straight to the library.
			for (const a of found) addScanned(a.name, a.path);
			scanResults = [];
			scanMsg = cancelScan ? t('games.scanStopped') : t('games.scanAutoAdded', { n: found.length });
		} else {
			scanResults = found;
			scanMsg = cancelScan ? t('games.scanStopped') : t('games.scanFound', { n: found.length });
		}
		scanning = false;
	}
	function stopScan() {
		cancelScan = true;
		scanning = false;
	}
	function addFound(a: ScannedApp) {
		addScanned(a.name, a.path);
		scanResults = scanResults.filter((x) => x.path !== a.path);
	}
	function addAllFound() {
		for (const a of scanResults) addScanned(a.name, a.path);
		scanResults = [];
	}
	function submitFolder() {
		if (newFolder.trim()) {
			addFolder(newFolder);
			newFolder = '';
		}
	}

	function startAuto() {
		stopAuto();
		if (gameStore.scan.autoScan) {
			scanAll(true);
			autoTimer = setInterval(() => scanAll(true), Math.max(1, gameStore.scan.intervalMin) * 60_000);
		}
	}
	function stopAuto() {
		if (autoTimer) {
			clearInterval(autoTimer);
			autoTimer = null;
		}
	}
	function toggleAuto() {
		gameStore.scan.autoScan = !gameStore.scan.autoScan;
		saveGames();
		if (gameStore.scan.autoScan) startAuto();
		else stopAuto();
	}

	onMount(() => {
		// "auto scan: arrive at this tab → start in background"
		if (gameStore.scan.autoScan) startAuto();
	});
	onDestroy(() => stopAuto());

	function initials(t: string) {
		return t.split(' ').map((w) => w[0]).slice(0, 2).join('').toUpperCase();
	}
</script>

<div class="head">
	<div><h1>{t('games.title')}</h1><p class="sub">{t('games.sub')}</p></div>
	<button class="btn btn-primary" onclick={openAdd}><Icon name="plus" size={17} />{t('games.add')}</button>
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
					<button class="ic" aria-label={t('games.edit')} onclick={() => openEdit(g)}><Icon name="settings" size={15} /></button>
					<button class="ic" aria-label={t('games.remove')} onclick={() => removeGame(g.id)}><Icon name="x" size={15} /></button>
				</div>
			</div>
		{/each}
	</div>
{/if}

<!-- Folder scan -->
<div class="card scan">
	<div class="hlab mono">{t('games.folderScan')}</div>
	<div class="folderadd">
		<div class="field">
			<Icon name="file" size={15} />
			<input bind:value={newFolder} placeholder={t('games.folderPlaceholder')} aria-label={t('games.folderAria')} onkeydown={(e) => e.key === 'Enter' && submitFolder()} />
		</div>
		<button class="btn btn-ghost" onclick={submitFolder}>{t('games.addFolder')}</button>
	</div>
	{#if gameStore.scan.folders.length > 0}
		<div class="folders">
			{#each gameStore.scan.folders as f (f)}
				<span class="chip mono">{f}<button aria-label={t('games.remove')} onclick={() => removeFolder(f)}><Icon name="x" size={12} /></button></span>
			{/each}
		</div>
	{/if}
	<div class="scanrow">
		{#if scanning}
			<button class="btn btn-ghost" onclick={stopScan}>{t('games.stop')}</button>
		{:else}
			<button class="btn btn-primary" onclick={() => scanAll()}><Icon name="refresh" size={15} />{t('games.scanAll')}</button>
		{/if}
		<div class="auto">
			<button class="toggle" aria-label={t('games.autoScan')} class:on={gameStore.scan.autoScan} aria-pressed={gameStore.scan.autoScan} onclick={toggleAuto}><span class="knob"></span></button>
			{t('games.autoScan')}
		</div>
		<span class="every">{t('games.every')}
			<input class="num" type="number" min="1" max="1440" bind:value={gameStore.scan.intervalMin} onchange={() => { saveGames(); if (gameStore.scan.autoScan) startAuto(); }} aria-label={t('games.intervalAria')} />
			{t('games.minutes')}</span>
		{#if scanMsg}<span class="scanmsg">{scanMsg}</span>{/if}
	</div>
	{#if scanResults.length > 0}
		<div class="results">
			<div class="reshead">
				<span>{t('games.found')}</span>
				<button class="btn btn-ghost sm" onclick={addAllFound}>{t('games.addAll')}</button>
			</div>
			{#each scanResults as a (a.path)}
				<div class="resrow">
					<span class="rname">{a.name}</span>
					<span class="rpath mono">{a.path}</span>
					<button class="btn btn-ghost sm" onclick={() => addFound(a)}>{t('games.addOne')}</button>
				</div>
			{/each}
		</div>
	{/if}
</div>

{#if showForm}
	<Modal title={editingId ? t('games.editTitle') : t('games.add')} onClose={() => (showForm = false)}>
		<div class="f">
			<span class="fl">{t('games.fTitle')}</span>
			<div class="field"><input bind:value={form.title} placeholder={t('games.fTitlePlaceholder')} aria-label={t('games.fTitle')} /></div>

			<span class="fl">{t('games.fType')}</span>
			<div class="seg">
				{#each TYPES as v (v)}
					<button class:active={form.type === v} onclick={() => (form.type = v)}>{typeLabel(v)}</button>
				{/each}
			</div>

			{#if form.type === 'program'}
				<span class="fl">{t('games.fExePath')}</span>
				<div class="field"><input bind:value={form.path} placeholder={t('games.fExePlaceholder')} aria-label={t('games.fPathAria')} style="font-family:var(--font-mono)" /></div>
				<span class="fl">{t('games.fArgs')}</span>
				<div class="field"><input bind:value={form.args} placeholder="--fullscreen" aria-label={t('games.fArgsAria')} /></div>
			{:else if form.type === 'command'}
				<span class="fl">{t('games.fCommand')}</span>
				<div class="field"><input bind:value={form.command} placeholder="steam steam://rungameid/…" aria-label={t('games.fCommand')} style="font-family:var(--font-mono)" /></div>
			{/if}

			<span class="fl">{t('games.fCover')}</span>
			<div class="field"><input bind:value={form.image} placeholder={t('games.fCoverPlaceholder')} aria-label={t('games.fCover')} /></div>

			<span class="fl">{t('games.fCmdStart')}</span>
			<div class="field"><input bind:value={form.cmdStart} placeholder={t('games.fCmdStartPlaceholder')} aria-label={t('games.fCmdStartAria')} style="font-family:var(--font-mono)" /></div>
			<span class="fl">{t('games.fCmdStop')}</span>
			<div class="field"><input bind:value={form.cmdStop} placeholder={t('games.fCmdStopPlaceholder')} aria-label={t('games.fCmdStopAria')} style="font-family:var(--font-mono)" /></div>

			<div class="factions">
				<button class="btn btn-ghost" onclick={() => (showForm = false)}>{t('games.cancel')}</button>
				<button class="btn btn-primary" disabled={!form.title.trim()} onclick={submitForm}>{t('games.fSave')}</button>
			</div>
		</div>
	</Modal>
{/if}

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
	.scan .folderadd {
		display: flex;
		gap: 10px;
		align-items: center;
	}
	.scan .folderadd .field {
		flex: 1;
	}
	.folders {
		display: flex;
		flex-wrap: wrap;
		gap: 7px;
		margin-top: 12px;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-size: 11.5px;
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r-pill);
		padding: 4px 6px 4px 11px;
		color: var(--text-muted);
	}
	.chip button {
		border: none;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		display: grid;
		place-items: center;
	}
	.scanrow {
		display: flex;
		align-items: center;
		gap: 14px;
		margin-top: 14px;
		flex-wrap: wrap;
	}
	.auto {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		color: var(--text-muted);
	}
	.every {
		font-size: 13px;
		color: var(--text-muted);
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.num {
		width: 58px;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		padding: 5px 8px;
		background: var(--surface);
		color: var(--text);
		font-family: var(--font-mono);
	}
	.scanmsg {
		font-size: 12.5px;
		color: var(--text-faint);
	}
	.results {
		margin-top: 16px;
		border-top: 1px solid var(--border);
		padding-top: 14px;
	}
	.reshead {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-size: 12px;
		color: var(--text-faint);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		margin-bottom: 10px;
	}
	.resrow {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 7px 0;
	}
	.rname {
		font-size: 13.5px;
		font-weight: 600;
		flex: none;
	}
	.rpath {
		flex: 1;
		font-size: 11.5px;
		color: var(--text-faint);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.btn.sm {
		padding: 6px 12px;
		font-size: 12.5px;
	}
	.f {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.fl {
		display: block;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		margin-top: 8px;
	}
	.factions {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
</style>
