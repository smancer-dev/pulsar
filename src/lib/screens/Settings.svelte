<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import type { Config, NetworkMode } from '$lib/types';
	import {
		ui,
		saveUi,
		CODECS,
		ENCODERS,
		DECODERS,
		type VideoCodec,
		type Encoder
	} from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';

	// Called after the relay/network settings change so the app re-registers.
	let { onReconnect }: { onReconnect?: () => void } = $props();

	let tab = $state<'display' | 'network' | 'security' | 'general'>('display');
	let config = $state<Config | null>(null);
	let saved = $state(false);
	let detected = $state<string[]>([]);

	const tabs = [
		{ id: 'display', icon: 'monitor' },
		{ id: 'network', icon: 'wifi' },
		{ id: 'security', icon: 'shield' },
		{ id: 'general', icon: 'settings' }
	] as const;

	// CODECS/ENCODERS labels are mostly brand names (kept verbatim); only the
	// "auto"/"software" entries are translated.
	const codecLabel = (value: string, fallback: string) =>
		value === 'auto' ? t('codec.auto') : fallback;
	const encLabel = (value: string, fallback: string) =>
		value === 'auto' ? t('enc.auto') : value === 'software' ? t('enc.software') : fallback;

	onMount(async () => {
		config = await api.getConfig();
		detected = await api.availableEncoders().catch(() => []);
	});

	// Core config (relay / network mode) — persisted via the Rust core, then
	// re-register so the change takes effect.
	async function saveConfig() {
		if (!config) return;
		await api.setConfig($state.snapshot(config));
		saved = true;
		setTimeout(() => (saved = false), 1200);
		onReconnect?.();
	}
	function setMode(m: NetworkMode) {
		if (config) {
			config.network_mode = m;
			saveConfig();
		}
	}
</script>

<div class="head"><h1>{t('settings.title')}</h1><p class="sub">{t('settings.sub')}</p></div>

<div class="layout">
	<div class="rail">
		{#each tabs as tb (tb.id)}
			<button class="tab" class:on={tab === tb.id} onclick={() => (tab = tb.id)}>
				<Icon name={tb.icon} size={17} />{t('settings.tab.' + tb.id)}
			</button>
		{/each}
	</div>

	<div class="card">
		{#if tab === 'display'}
			<div class="srow">
				<div class="st"><b>{t('settings.quality')}</b><span>{t('settings.qualityDesc')}</span></div>
				<div class="seg">
					{#each [['auto', t('settings.qAuto')], ['hq', t('settings.qHq')], ['fast', t('settings.qFast')]] as [v, l] (v)}
						<button class:active={ui.quality === v} onclick={() => { ui.quality = v; saveUi(); }}>{l}</button>
					{/each}
				</div>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.resolution')}</b></div>
				<div class="seg">
					{#each ['1080p', '1440p', '4K'] as v (v)}
						<button class:active={ui.res === v} onclick={() => { ui.res = v; saveUi(); }}>{v}</button>
					{/each}
				</div>
			</div>
			<div class="srow">
				<div class="st">
					<b>{t('settings.codec')}</b><span>{t('settings.codecDesc')}</span>
				</div>
				<div class="seg">
					{#each CODECS as c (c.value)}
						<button
							class:active={ui.codec === c.value}
							onclick={() => { ui.codec = c.value as VideoCodec; saveUi(); }}>{codecLabel(c.value, c.label)}</button
						>
					{/each}
				</div>
			</div>
			<div class="srow">
				<div class="st">
					<b>{t('settings.encoder')}</b>
					<span>{t('settings.encoderDesc')}</span>
				</div>
				<select
					class="select"
					aria-label={t('settings.encoder')}
					value={ui.encoder}
					onchange={(e) => { ui.encoder = e.currentTarget.value as Encoder; saveUi(); }}
				>
					{#each ENCODERS as e (e.value)}
						<option value={e.value}>{encLabel(e.value, e.label)}</option>
					{/each}
				</select>
			</div>
			{#if detected.length}
				<div class="detnote mono">
					{t('settings.detected', {
						list: detected.filter((d) => d !== 'software').join(', ') || t('settings.detectedNone')
					})}
				</div>
			{/if}
			<div class="srow">
				<div class="st">
					<b>{t('settings.decoder')}</b>
					<span>{t('settings.decoderDesc')}</span>
				</div>
				<select
					class="select"
					aria-label={t('settings.decoder')}
					value={ui.decoder}
					onchange={(e) => { ui.decoder = e.currentTarget.value as Encoder; saveUi(); }}
				>
					{#each DECODERS as d (d.value)}
						<option value={d.value}>{encLabel(d.value, d.label)}</option>
					{/each}
				</select>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.hdr')}</b><span>{t('settings.hdrDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.hdr')} class:on={ui.hdr} aria-pressed={ui.hdr} onclick={() => { ui.hdr = !ui.hdr; saveUi(); }}><span class="knob"></span></button>
			</div>
		{:else if tab === 'network'}
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
				<div class="st"><b>{t('settings.bwlimit')}</b><span>{t('settings.bwlimitDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.bwlimit')} class:on={ui.bwlimit} aria-pressed={ui.bwlimit} onclick={() => { ui.bwlimit = !ui.bwlimit; saveUi(); }}><span class="knob"></span></button>
			</div>
			{#if saved}<div class="saved mono">{t('settings.saved')}</div>{/if}
		{:else if tab === 'security'}
			<div class="srow">
				<div class="st"><b>{t('settings.unattended')}</b><span>{t('settings.unattendedDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.unattended')} class:on={ui.unattended} aria-pressed={ui.unattended} onclick={() => { ui.unattended = !ui.unattended; saveUi(); }}><span class="knob"></span></button>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.twofa')}</b><span>{t('settings.twofaDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.twofa')} class:on={ui.twofa} aria-pressed={ui.twofa} onclick={() => { ui.twofa = !ui.twofa; saveUi(); }}><span class="knob"></span></button>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.record')}</b><span>{t('settings.recordDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.record')} class:on={ui.record} aria-pressed={ui.record} onclick={() => { ui.record = !ui.record; saveUi(); }}><span class="knob"></span></button>
			</div>
		{:else}
			<div class="srow">
				<div class="st"><b>{t('settings.startup')}</b><span>{t('settings.startupDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.startup')} class:on={ui.startup} aria-pressed={ui.startup} onclick={() => { ui.startup = !ui.startup; saveUi(); }}><span class="knob"></span></button>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.tray')}</b><span>{t('settings.trayDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.tray')} class:on={ui.tray} aria-pressed={ui.tray} onclick={() => { ui.tray = !ui.tray; saveUi(); }}><span class="knob"></span></button>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.debug')}</b><span>{t('settings.debugDesc')}</span></div>
				<button class="toggle" aria-label={t('settings.debug')} class:on={ui.debug} aria-pressed={ui.debug} onclick={() => { ui.debug = !ui.debug; saveUi(); }}><span class="knob"></span></button>
			</div>
			<div class="srow">
				<div class="st"><b>{t('settings.version')}</b><span>{t('settings.versionDesc')}</span></div>
				<span class="mono ver">Pulsar v1.0.0</span>
			</div>
		{/if}
	</div>
</div>

<style>
	.head {
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
	.layout {
		display: grid;
		grid-template-columns: 186px 1fr;
		gap: 24px;
		align-items: start;
	}
	.rail {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.tab {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 12px;
		border: none;
		border-radius: var(--r-sm);
		cursor: pointer;
		text-align: left;
		font-family: var(--font-sans);
		font-size: 14px;
		font-weight: 500;
		color: var(--text-muted);
		background: transparent;
	}
	.tab.on {
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
	}
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
	.select {
		font-family: var(--font-sans);
		font-size: 13px;
		color: var(--text);
		background: var(--surface);
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		padding: 8px 11px;
		cursor: pointer;
		min-width: 180px;
	}
	.select:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	.detnote {
		font-size: 11.5px;
		color: var(--text-faint);
		padding: 10px 0 0;
	}
	.relayfield {
		width: 250px;
	}
	.saved {
		font-size: 12px;
		color: var(--ok);
		padding-top: 12px;
	}
	.ver {
		font-size: 13px;
		color: var(--text-muted);
	}
</style>
