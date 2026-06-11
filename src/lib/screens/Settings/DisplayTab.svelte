<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import type { Config } from '$lib/types';
	import {
		ui,
		saveUi,
		CODECS,
		encodersForPlatform,
		type VideoCodec,
		type Encoder
	} from '$lib/settings.svelte';
	import { caps, decodeAvailable, encoderAvailable } from '$lib/caps.svelte';
	import { gameStore, saveGames } from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

	let {
		config = $bindable(),
		detected,
		saveConfig,
		toggleNative,
		toggleTransmit,
		toggleMute
	}: {
		config: Config | null;
		detected: string[];
		saveConfig: () => void;
		toggleNative: () => void;
		toggleTransmit: () => void;
		toggleMute: () => void;
	} = $props();

	// CODECS/ENCODERS labels are mostly brand names (kept verbatim); only the
	// "auto"/"software" entries are translated.
	const codecLabel = (value: string, fallback: string) =>
		value === 'auto' ? t('codec.auto') : fallback;
	const encLabel = (value: string, fallback: string) =>
		value === 'auto' ? t('enc.auto') : value === 'software' ? t('enc.software') : fallback;

	// The maintainer's gating rule: show ONLY this platform's encoder families
	// (foreign ones never render), and DISABLE the platform-native ones the startup
	// probe found unusable. `auto`/`software` are always enabled (software = the
	// universal terminal fallback). Codec buttons gate on the CLIENT decode probe.
	const encoderOptions = $derived(encodersForPlatform(caps.v?.platform ?? 'linux'));
</script>

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
				disabled={!decodeAvailable(c.value)}
				title={decodeAvailable(c.value) ? '' : t('settings.notAvailable')}
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
		{#each encoderOptions as e (e.value)}
			<option value={e.value} disabled={!encoderAvailable(e.value)}>
				{encLabel(e.value, e.label)}{encoderAvailable(e.value) ? '' : ` — ${t('settings.notAvailable')}`}
			</option>
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
	<div class="st"><b>{t('settings.hdr')}</b><span>{t('settings.hdrDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.hdr')} class:on={ui.hdr} aria-pressed={ui.hdr} onclick={() => { ui.hdr = !ui.hdr; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.framePacing')}</b><span>{t('settings.framePacingDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.framePacing')} class:on={ui.framePacing} aria-pressed={ui.framePacing} onclick={() => { ui.framePacing = !ui.framePacing; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.statsHud')}</b><span>{t('settings.statsHudDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.statsHud')} class:on={ui.statsHud} aria-pressed={ui.statsHud} onclick={() => { ui.statsHud = !ui.statsHud; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.overlayButton')}</b><span>{t('settings.overlayButtonDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.overlayButton')} class:on={ui.overlayButton} aria-pressed={ui.overlayButton} onclick={() => { ui.overlayButton = !ui.overlayButton; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.nativePlayer')}</b><span>{t('settings.nativePlayerDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.nativePlayer')} class:on={config?.native_player} aria-pressed={config?.native_player ?? false} onclick={toggleNative}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.audioTransmit')}</b><span>{t('settings.audioTransmitDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.audioTransmit')} class:on={config?.transmit_audio} aria-pressed={config?.transmit_audio ?? false} onclick={toggleTransmit}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.audioMute')}</b><span>{t('settings.audioMuteDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.audioMute')} class:on={config?.mute_host_audio} aria-pressed={config?.mute_host_audio ?? false} onclick={toggleMute}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.audioInput')}</b><span>{t('settings.audioInputDesc')}</span></div>
	<div class="field relayfield">
		<Icon name="mic" size={15} />
		{#if config}
			<input
				bind:value={config.audio_input}
				onchange={saveConfig}
				aria-label={t('settings.audioInput')}
				placeholder={t('settings.audioInputPh')}
				style="font-family:var(--font-mono);font-size:12.5px"
			/>
		{/if}
	</div>
</div>

<!-- Host stream defaults (moved here from the Games tab). These edit gameStore.host,
     which the +page effect pushes to the core via api.setStreamSettings — same wiring
     the Games card used, so changes apply to the next stream. -->
<div class="group mono">{t('settings.streamDefaults')}</div>
<div class="groupdesc">{t('settings.streamDefaultsDesc')}</div>
<div class="srow">
	<div class="st"><b>{t('settings.resolution')}</b></div>
	<div class="seg">
		{#each ['1080p', '1440p', '4K'] as v (v)}
			<button class:active={gameStore.host.resolution === v} onclick={() => { gameStore.host.resolution = v; saveGames(); }}>{v}</button>
		{/each}
	</div>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.streamFps', { fps: gameStore.host.fps })}</b></div>
	<input class="prange" type="range" min="30" max="240" step="10" bind:value={gameStore.host.fps} onchange={saveGames} aria-label={t('settings.streamFpsAria')} />
</div>
<div class="srow">
	<div class="st"><b>{t('settings.streamBitrate', { n: gameStore.host.bitrate })}</b></div>
	<input class="prange" type="range" min="5" max="100" step="5" bind:value={gameStore.host.bitrate} onchange={saveGames} aria-label={t('settings.streamBitrateAria')} />
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
	/* "Yayın varsayılanları" group header — visually separates the host-stream
	   knobs from the per-session rows above. */
	.group {
		font-size: 10.5px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
		margin-top: 22px;
	}
	.groupdesc {
		font-size: 12.5px;
		color: var(--text-faint);
		margin-top: 5px;
		line-height: 1.45;
	}
	/* the global .prange is width:100%; cap it so it sits as a right-hand control */
	input.prange {
		width: 240px;
		flex: none;
		margin-top: 0;
	}
</style>
