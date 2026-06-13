<script lang="ts">
	import VideoControls from './VideoControls.svelte';
	import { t } from '$lib/i18n.svelte';
	import { type Encoder, type VideoCodec } from '$lib/settings.svelte';
	import type { HostDisplay } from '$lib/api.types';

	// Left column of the full (remote-desktop) session menu: the shared video selects plus the
	// menu-only quality + display-fit segmented controls. fitMode is local UI state in the
	// parent (drives the canvas object-fit), so it's $bindable here.
	type Props = {
		codec: VideoCodec;
		encoder: Encoder;
		hostCodecs?: string[];
		hostEncoders?: string[];
		hostDisplays?: HostDisplay[];
		decoderInfo?: string;
		activeInfo?: string;
		activeFps?: string;
		activeRes?: string;
		streamRes: 'auto' | '1080p' | '1440p' | '4K';
		streamFps: 'auto' | '30' | '60' | '120';
		streamBitrate: number;
		streamQuality: 'latency' | 'quality';
		streamDisplay: number;
		fitMode: 'fit' | 'stretch' | 'original';
		onCodec: (v: VideoCodec) => void;
		onEncoder: (v: Encoder) => void;
		onRes: (v: 'auto' | '1080p' | '1440p' | '4K') => void;
		onFps: (v: 'auto' | '30' | '60' | '120') => void;
		onBitrate: (v: number) => void;
		onQuality: (v: 'latency' | 'quality') => void;
		onMonitor: (idx: number) => void;
	};
	let {
		codec,
		encoder,
		hostCodecs = [],
		hostEncoders = [],
		hostDisplays = [],
		decoderInfo = '',
		activeInfo = '',
		activeFps = '',
		activeRes = '',
		streamRes,
		streamFps,
		streamBitrate,
		streamQuality,
		streamDisplay,
		fitMode = $bindable(),
		onCodec,
		onEncoder,
		onRes,
		onFps,
		onBitrate,
		onQuality,
		onMonitor
	}: Props = $props();
</script>

<div class="m-col">
	<div class="m-sec-head">{t('session.secVideo')}</div>
	<VideoControls
		idPrefix="m"
		{codec}
		{encoder}
		{hostCodecs}
		{hostEncoders}
		{decoderInfo}
		{activeInfo}
		{activeFps}
		{activeRes}
		{streamRes}
		{streamFps}
		{streamBitrate}
		{onCodec}
		{onEncoder}
		{onRes}
		{onFps}
		{onBitrate}
	/>
	{#if hostDisplays.length > 1}
		<div class="m-field">
			<label class="m-flab" for="m-monitor">{t('session.monitor')}</label>
			<select
				id="m-monitor"
				class="m-sel mono"
				value={streamDisplay}
				onchange={(e) => onMonitor(Number(e.currentTarget.value))}
			>
				{#each hostDisplays as d (d.idx)}
					<option value={d.idx}>
						{d.name}{d.primary ? ` (${t('session.monitorPrimary')})` : ''} · {d.width}×{d.height}
					</option>
				{/each}
			</select>
		</div>
	{/if}
	<div class="m-field">
		<span class="m-flab">{t('session.quality')}</span>
		<div class="m-seg" role="group" aria-label={t('session.quality')}>
			<button class:on={streamQuality === 'latency'} onclick={() => onQuality('latency')}>{t('session.qLatency')}</button>
			<button class:on={streamQuality === 'quality'} onclick={() => onQuality('quality')}>{t('session.qQuality')}</button>
		</div>
	</div>
	<div class="m-field">
		<span class="m-flab">{t('session.display')}</span>
		<div class="m-seg" role="group" aria-label={t('session.display')}>
			<button class:on={fitMode === 'fit'} onclick={() => (fitMode = 'fit')}>{t('session.fitFit')}</button>
			<button class:on={fitMode === 'stretch'} onclick={() => (fitMode = 'stretch')}>{t('session.fitStretch')}</button>
			<button class:on={fitMode === 'original'} onclick={() => (fitMode = 'original')}>{t('session.fitOriginal')}</button>
		</div>
	</div>
</div>

<style>
	.m-col {
		flex: 1 1 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.m-sec-head {
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: oklch(0.6 0.02 265);
		padding: 2px 2px 4px;
	}
	.m-field {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.m-flab {
		font-size: 11px;
		color: oklch(0.7 0.02 265);
	}
	.m-sel {
		width: 100%;
		min-width: 0;
		padding: 6px 26px 6px 8px;
		border: 1px solid oklch(0.32 0.016 265 / 0.7);
		border-radius: var(--r-sm);
		background-color: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.92 0.01 265);
		font-size: 11.5px;
		font-weight: 500;
		cursor: pointer;
		/* Match VideoControls: force a dark form-control scheme on WebKitGTK + draw our own arrow. */
		appearance: none;
		-webkit-appearance: none;
		color-scheme: dark;
		background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6'%3E%3Cpath d='M1 1l4 4 4-4' fill='none' stroke='%23aab2c5' stroke-width='1.5'/%3E%3C/svg%3E");
		background-repeat: no-repeat;
		background-position: right 9px center;
	}
	.m-sel:hover {
		background-color: oklch(0.3 0.016 272 / 0.7);
	}
	.m-sel option {
		background: oklch(0.18 0.012 265);
		color: oklch(0.92 0.01 265);
	}
	.m-seg {
		display: flex;
		flex: none;
		width: 100%;
		border: 1px solid oklch(0.32 0.016 265 / 0.7);
		border-radius: var(--r-sm);
		overflow: hidden;
	}
	.m-seg button {
		flex: 1;
		padding: 6px 2px;
		border: none;
		border-left: 1px solid oklch(0.32 0.016 265 / 0.7);
		background: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.86 0.01 265);
		font-size: 11px;
		font-weight: 500;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.m-seg button:first-child {
		border-left: none;
	}
	.m-seg button:hover {
		background: oklch(0.3 0.016 272 / 0.7);
	}
	.m-seg button.on {
		background: color-mix(in oklch, var(--accent) 36%, transparent);
		color: #fff;
	}
</style>
