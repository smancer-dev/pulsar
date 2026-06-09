<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api, type ScannedApp } from '$lib/api';
	import { gameStore, addScanned, addFolder, removeFolder, saveGames } from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

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
</script>

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

<style>
	.hlab {
		font-size: 10.5px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
		margin-bottom: 14px;
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
</style>
