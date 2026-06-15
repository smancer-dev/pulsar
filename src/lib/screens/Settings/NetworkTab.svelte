<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import type { Config, NetworkMode } from '$lib/types';
	import { api, onNodePort } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	let {
		config = $bindable(),
		saveConfig,
		setMode
	}: {
		config: Config | null;
		saveConfig: () => void;
		setMode: (m: NetworkMode) => void;
	} = $props();

	// When no port is pinned (node_port == 0) the box shows the ACTUAL random port
	// in use as its placeholder. Snapshot at mount; the node-port event keeps it
	// live across go_online rebinds. Falls back to "Random" while unknown/0.
	let livePort = $state(0);
	onMount(() => {
		api.nodePort().then((p) => (livePort = p)).catch(() => {});
		let off: (() => void) | undefined;
		let dead = false;
		onNodePort((p) => (livePort = p)).then((o) => {
			if (dead) o();
			else off = o;
		});
		return () => {
			dead = true;
			off?.();
		};
	});
</script>

<div class="srow">
	<div class="st">
		<b>{t('settings.connMethod')}</b>
		<span>{t('settings.connMethodDesc')}</span>
	</div>
	<div class="seg">
		{#each [['auto', t('settings.modeAuto')], ['p2p-only', t('settings.modeP2p')], ['relay-only', t('settings.modeRelay')]] as [v, l] (v)}
			<button
				class:active={config?.network_mode === v}
				onclick={() => setMode(v as NetworkMode)}>{l}</button
			>
		{/each}
	</div>
</div>
<div class="srow">
	<div class="st">
		<b>{t('settings.relay')}</b>
		<span>{t('settings.relayDesc')}</span>
	</div>
	<div class="field relayfield">
		<Icon name="plug" size={15} />
		{#if config}
			<input
				bind:value={config.relay}
				onchange={saveConfig}
				aria-label={t('settings.relayAria')}
				style="font-family:var(--font-mono);font-size:12.5px"
			/>
		{/if}
	</div>
</div>
<div class="srow">
	<div class="st">
		<b>{t('settings.nodePort')}</b>
		<span>{t('settings.nodePortDesc')}</span>
	</div>
	<div class="field" style="width:150px">
		<Icon name="plug" size={15} />
		{#if config}
			<!-- Unset (0) renders as EMPTY with a "random" placeholder — a literal 0 in
			     the box read like a (nonsense) port. Clearing the field returns to the
			     random-port default; the live port shows on Home's ip:port line. -->
			<input
				type="number"
				min="1"
				max="65535"
				value={config.node_port > 0 ? config.node_port : ''}
				onchange={(e) => {
					if (!config) return;
					const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
					config.node_port = Number.isFinite(v) && v > 0 && v <= 65535 ? v : 0;
					saveConfig();
				}}
				aria-label={t('settings.nodePort')}
				placeholder={livePort > 0 ? String(livePort) : t('settings.portRandom')}
				style="font-family:var(--font-mono);font-size:12.5px;width:90px"
			/>
		{/if}
	</div>
</div>

<style>
	.srow {
		display: flex;
		align-items: center;
		gap: 20px;
		padding: 16px 0;
		border-bottom: 1px solid var(--border);
	}
	.st {
		flex: 1;
	}
	.st b {
		font-size: 14px;
		font-weight: 600;
		display: block;
	}
	.st span {
		font-size: 12.5px;
		color: var(--text-faint);
		margin-top: 3px;
		line-height: 1.45;
		display: block;
		max-width: 46ch;
	}
	.relayfield {
		width: 250px;
	}
</style>
