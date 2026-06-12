<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import type { Config, NetworkMode } from '$lib/types';
	import { ui, saveUi } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';

	let {
		config = $bindable(),
		saved,
		saveConfig,
		setMode
	}: {
		config: Config | null;
		saved: boolean;
		saveConfig: () => void;
		setMode: (m: NetworkMode) => void;
	} = $props();
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
				placeholder={t('settings.portRandom')}
				style="font-family:var(--font-mono);font-size:12.5px;width:90px"
			/>
		{/if}
	</div>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.bwlimit')}</b><span>{t('settings.bwlimitDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.bwlimit')} class:on={ui.bwlimit} aria-pressed={ui.bwlimit} onclick={() => { ui.bwlimit = !ui.bwlimit; saveUi(); }}><span class="knob"></span></button>
</div>
{#if saved}<div class="saved mono">{t('settings.saved')}</div>{/if}

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
	.saved {
		font-size: 12px;
		color: var(--ok);
		padding-top: 12px;
	}
</style>
