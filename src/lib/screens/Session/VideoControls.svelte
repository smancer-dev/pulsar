<script lang="ts">
	import { t } from '$lib/i18n.svelte';
	import { ENCODERS, CODECS, type Encoder, type VideoCodec } from '$lib/settings.svelte';

	// Shared video/codec select fields used by BOTH the floating menu and the game
	// overlay (codec · encoder · resolution · fps · bitrate + the read-only active
	// decoder). The two parents wrap these in their own layout and add their own extra
	// controls. `idPrefix` keeps the <label for=…> ids unique per parent.
	// Options are CAPABILITY-GATED: hostCodecs/hostEncoders are the HOST's validated
	// caps (QueryStreamCaps) — codecs the host can't emit are disabled, encoder
	// backends it doesn't have are hidden. Empty lists = unknown host (no gating).
	type Props = {
		idPrefix: string;
		codec: VideoCodec;
		encoder: Encoder;
		hostCodecs?: string[];
		hostEncoders?: string[];
		/** The client renderer's ACTUAL decoder (auto-selected; display only). */
		decoderInfo?: string;
		/** The host's ACTIVE codec+encoder (faint truth line under the selectors). */
		activeInfo?: string;
		streamRes: 'auto' | '1080p' | '1440p' | '4K';
		streamFps: 'auto' | '30' | '60' | '120';
		streamBitrate: number;
		onCodec: (v: VideoCodec) => void;
		onEncoder: (v: Encoder) => void;
		onRes: (v: 'auto' | '1080p' | '1440p' | '4K') => void;
		onFps: (v: 'auto' | '30' | '60' | '120') => void;
		onBitrate: (v: number) => void;
	};
	let {
		idPrefix,
		codec,
		encoder,
		hostCodecs = [],
		hostEncoders = [],
		decoderInfo = '',
		activeInfo = '',
		streamRes,
		streamFps,
		streamBitrate,
		onCodec,
		onEncoder,
		onRes,
		onFps,
		onBitrate
	}: Props = $props();

	const codecAllowed = (v: string) =>
		v === 'auto' || hostCodecs.length === 0 || hostCodecs.includes(v);
	const encoderOptions = $derived(
		ENCODERS.filter(
			(e) => e.value === 'auto' || hostEncoders.length === 0 || hostEncoders.includes(e.value)
		)
	);
</script>

<div class="m-field">
	<label class="m-flab" for="{idPrefix}-codec">Codec</label>
	<select id="{idPrefix}-codec" class="m-sel mono" value={codec} onchange={(e) => onCodec(e.currentTarget.value as VideoCodec)}>
		{#each CODECS as c (c.value)}
			<option value={c.value} disabled={!codecAllowed(c.value)}>
				{c.label}{codecAllowed(c.value) ? '' : ` · ${t('session.hostUnsupported')}`}
			</option>
		{/each}
	</select>
</div>
<div class="m-field">
	<label class="m-flab" for="{idPrefix}-enc">{t('session.statEncoder')}</label>
	<select id="{idPrefix}-enc" class="m-sel mono" value={encoder} onchange={(e) => onEncoder(e.currentTarget.value as Encoder)}>
		{#each encoderOptions as e (e.value)}<option value={e.value}>{e.label}</option>{/each}
	</select>
	{#if activeInfo}
		<span class="m-active mono">{t('session.activeNow')}: {activeInfo}</span>
	{/if}
</div>
<!-- The decoder is auto-selected by the renderer (rkmpp/MediaFoundation/software) and
     only DISPLAYED here — a picker would be a lie (nothing consumes it). -->
<div class="m-field">
	<span class="m-flab">{t('session.statDecoder')}</span>
	<span class="m-ro mono">{decoderInfo || t('session.resAuto')}</span>
</div>
<div class="m-field">
	<label class="m-flab" for="{idPrefix}-res">{t('settings.resolution')}</label>
	<select id="{idPrefix}-res" class="m-sel mono" value={streamRes} onchange={(e) => onRes(e.currentTarget.value as 'auto' | '1080p' | '1440p' | '4K')}>
		<option value="auto">{t('session.resAuto')}</option>
		<option value="1080p">1080p</option>
		<option value="1440p">1440p</option>
		<option value="4K">4K</option>
	</select>
</div>
<div class="m-field">
	<label class="m-flab" for="{idPrefix}-fps">FPS</label>
	<select id="{idPrefix}-fps" class="m-sel mono" value={streamFps} onchange={(e) => onFps(e.currentTarget.value as 'auto' | '30' | '60' | '120')}>
		<option value="auto">{t('session.resAuto')}</option>
		<option value="30">30</option>
		<option value="60">60</option>
		<option value="120">120</option>
	</select>
</div>
<div class="m-field">
	<label class="m-flab" for="{idPrefix}-bitrate">{t('session.bitrate')}</label>
	<select id="{idPrefix}-bitrate" class="m-sel mono" value={String(streamBitrate)} onchange={(e) => onBitrate(Number(e.currentTarget.value) || 0)}>
		<option value="0">{t('session.bitrateAuto')}</option>
		<option value="10">10 Mbit</option>
		<option value="20">20 Mbit</option>
		<option value="30">30 Mbit</option>
		<option value="50">50 Mbit</option>
		<option value="100">100 Mbit</option>
	</select>
</div>

<style>
	.m-field {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.m-flab {
		font-size: 11px;
		color: oklch(0.7 0.02 265);
	}
	/* compact dropdown for the encoder / decoder pickers (labels are too long for a
	   segmented control) */
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
		/* WebKitGTK (Linux) otherwise renders <select> with the native GTK widget — its own
		   light colors regardless of the bg/color above — so the field + option popup are
		   unreadable on the dark overlay. Force custom rendering + a dark form-control scheme,
		   and draw our own dropdown arrow (appearance:none removes the native one). */
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
	/* faint "what's REALLY in use" line under the encoder selector */
	.m-active {
		margin-top: 2px;
		font-size: 10px;
		color: oklch(0.55 0.015 265);
	}
	/* read-only value row (active decoder) */
	.m-ro {
		padding: 6px 8px;
		border: 1px dashed oklch(0.32 0.016 265 / 0.7);
		border-radius: var(--r-sm);
		color: oklch(0.8 0.01 265);
		font-size: 11.5px;
	}
</style>
