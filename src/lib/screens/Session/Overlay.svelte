<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import Controllers from '$lib/Controllers.svelte';
	import VideoControls from './VideoControls.svelte';
	import { t } from '$lib/i18n.svelte';
	import { type Encoder, type VideoCodec } from '$lib/settings.svelte';

	// Game-only overlay (Ctrl+Shift+M). Opaque dialog so it stays visible while mpv is
	// paused on Linux. Perf HUD + the slim game controls (codec/encoder/decoder/res/fps/
	// bitrate/quality/pacing) + controllers + end — NO file/clipboard/mic/chat (remote-only).
	type Target = { name: string; id: string };
	type Props = {
		target: Target;
		connLabel: string;
		netClass: string;
		fps: number;
		latencyMs: number;
		decodeMs: number;
		mbps: number;
		codec: VideoCodec;
		encoder: Encoder;
		hostCodecs?: string[];
		hostEncoders?: string[];
		decoderInfo?: string;
		activeInfo?: string;
		activeFps?: string;
		activeRes?: string;
		streamRes: 'auto' | '1080p' | '1440p' | '4K';
		streamFps: 'auto' | '30' | '60' | '120';
		streamBitrate: number;
		streamQuality: 'latency' | 'quality';
		framePacing: boolean;
		onCodec: (v: VideoCodec) => void;
		onEncoder: (v: Encoder) => void;
		onRes: (v: 'auto' | '1080p' | '1440p' | '4K') => void;
		onFps: (v: 'auto' | '30' | '60' | '120') => void;
		onBitrate: (v: number) => void;
		onQuality: (v: 'latency' | 'quality') => void;
		onFramePacing: (on: boolean) => void;
		onClose: () => void;
		onEnd: () => void;
	};
	let {
		target,
		connLabel,
		netClass,
		fps,
		latencyMs,
		decodeMs,
		mbps,
		codec,
		encoder,
		hostCodecs = [],
		hostEncoders = [],
		decoderInfo = '',
		activeInfo = '',
		activeFps = '',
		activeRes = '',
		streamRes,
		streamFps,
		streamBitrate,
		streamQuality,
		framePacing,
		onCodec,
		onEncoder,
		onRes,
		onFps,
		onBitrate,
		onQuality,
		onFramePacing,
		onClose,
		onEnd
	}: Props = $props();
</script>

<button class="ovscrim" aria-label={t('overlay.title')} onclick={onClose}></button>
<div class="overlay" role="dialog" aria-label={t('overlay.title')}>
	<div class="ov-head">
		<div>
			<div class="ov-title">{t('overlay.title')} <span class="ov-paused">⏸ {t('overlay.paused')}</span></div>
			<div class="ov-sub mono">
				<span class="net-dot {netClass}"></span>{target.id} · {connLabel} · {fps} fps
			</div>
		</div>
	</div>

	<div class="ov-hud mono">
		<div class="ov-stat">
			<b class:warn={latencyMs >= 30} class:bad={latencyMs >= 60}>{latencyMs}</b>
			<span>{t('session.statLatency')}</span>
		</div>
		<div class="ov-stat"><b>{fps}</b><span>{t('session.statFps')}</span></div>
		<div class="ov-stat"><b>{decodeMs}</b><span>{t('overlay.decode')} ms</span></div>
		<div class="ov-stat"><b>{mbps}</b><span>Mbps</span></div>
	</div>

	<div class="ov-fields">
		<VideoControls
			idPrefix="ov"
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
		<div class="m-field ov-wide">
			<span class="m-flab">{t('session.quality')}</span>
			<div class="m-seg" role="group" aria-label={t('session.quality')}>
				<button class:on={streamQuality === 'latency'} onclick={() => onQuality('latency')}>{t('session.qLatency')}</button>
				<button class:on={streamQuality === 'quality'} onclick={() => onQuality('quality')}>{t('session.qQuality')}</button>
			</div>
		</div>
		<div class="m-field ov-wide">
			<span class="m-flab">{t('session.framePacing')}</span>
			<div class="m-seg" role="group" aria-label={t('session.framePacing')}>
				<button class:on={framePacing} onclick={() => onFramePacing(true)}>{t('session.pacingOn')}</button>
				<button class:on={!framePacing} onclick={() => onFramePacing(false)}>{t('session.pacingOff')}</button>
			</div>
		</div>
	</div>

	<div class="ov-ctrls">
		<div class="m-seg-lab">{t('controllers.title')}</div>
		<Controllers compact />
	</div>

	<button class="m-end" role="menuitem" onclick={onEnd}>
		<Icon name="power" size={16} />{t('session.end')}
	</button>
	<div class="ov-foot mono">{t('overlay.resume')}<br />{t('overlay.exit')}</div>
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
	.net-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex: none;
		display: inline-block;
	}
	.net-dot.ok {
		background: var(--ok);
	}
	.net-dot.mid {
		background: #f4bf4f;
	}
	.net-dot.bad {
		background: var(--danger);
	}
	.m-seg {
		display: flex;
		flex: 1;
		border: 1px solid oklch(0.32 0.016 265 / 0.7);
		border-radius: var(--r-sm);
		overflow: hidden;
	}
	.m-seg button {
		flex: 1;
		padding: 7px 4px;
		border: none;
		border-left: 1px solid oklch(0.32 0.016 265 / 0.7);
		background: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.86 0.01 265);
		font-size: 11.5px;
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
	.m-seg-lab {
		font-size: 11.5px;
		color: oklch(0.74 0.02 265);
		flex: none;
	}
	.m-end {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		width: 100%;
		margin-top: 10px;
		padding: 11px 0;
		border: 1px solid color-mix(in oklch, var(--danger) 50%, transparent);
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--danger) 22%, transparent);
		color: #ffd9d4;
		font-size: 13.5px;
		font-weight: 600;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.m-end:hover {
		background: color-mix(in oklch, var(--danger) 40%, transparent);
		color: #fff;
	}
	/* Game overlay (Ctrl+Shift+M). Full-bleed click-away above the dock (z 7), then an
	   opaque centered card so it stays readable while the Linux mpv video is paused. */
	.ovscrim {
		position: absolute;
		inset: 0;
		border: none;
		/* Near-opaque so the home screen behind the transparent embedded session never bleeds
		   through; a dark, blurred "paused" backdrop while the video is stopped. Click to resume. */
		background:
			radial-gradient(900px 520px at 50% 36%, oklch(0.24 0.06 272 / 0.5), transparent 72%),
			oklch(0.055 0.012 265 / 0.985);
		backdrop-filter: blur(10px);
		padding: 0;
		margin: 0;
		cursor: pointer;
		z-index: 8;
	}
	.ov-paused {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.02em;
		color: oklch(0.83 0.14 75); /* amber — "stream paused" */
		white-space: nowrap;
	}
	.ov-foot {
		margin-top: 12px;
		padding-top: 10px;
		border-top: 1px solid oklch(0.32 0.016 265 / 0.5);
		font-size: 11px;
		line-height: 1.7;
		text-align: center;
		color: oklch(0.74 0.02 265);
	}
	.overlay {
		position: absolute;
		left: 50%;
		top: 50%;
		transform: translate(-50%, -50%);
		width: 420px;
		max-width: calc(100% - 28px);
		max-height: calc(100% - 28px);
		overflow-y: auto;
		padding: 16px;
		border-radius: var(--r-lg);
		background: oklch(0.17 0.012 265 / 0.98);
		border: 1px solid color-mix(in oklch, var(--accent) 40%, oklch(0.36 0.016 265 / 0.7));
		box-shadow: var(--shadow-lg);
		color: oklch(0.96 0.008 265);
		/* Dark-theme all native form controls (selects, scrollbars, the option popup) inside
		   the overlay — WebKitGTK defaults them to a light scheme that's unreadable here. */
		color-scheme: dark;
		backdrop-filter: blur(12px);
		z-index: 9;
	}
	.ov-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
		padding-bottom: 12px;
		border-bottom: 1px solid oklch(0.32 0.016 265 / 0.6);
		margin-bottom: 12px;
	}
	.ov-title {
		font-family: var(--font-display);
		font-size: 16px;
		font-weight: 600;
	}
	.ov-sub {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 11px;
		color: oklch(0.72 0.02 265);
		margin-top: 4px;
	}
	/* perf HUD: a mono row of headline numbers (latency / fps / decode / Mbps) */
	.ov-hud {
		display: flex;
		gap: 8px;
		margin-bottom: 14px;
	}
	.ov-stat {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		padding: 9px 4px;
		border-radius: var(--r-sm);
		background: oklch(0.22 0.013 265 / 0.6);
	}
	.ov-stat b {
		font-size: 20px;
		font-weight: 700;
		color: var(--accent);
	}
	.ov-stat b.warn {
		color: #f4bf4f;
	}
	.ov-stat b.bad {
		color: var(--danger);
	}
	.ov-stat span {
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: oklch(0.65 0.02 265);
	}
	.ov-fields {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 10px;
	}
	.ov-fields .ov-wide {
		grid-column: 1 / -1;
	}
	.ov-ctrls {
		margin-top: 14px;
		padding-top: 12px;
		border-top: 1px solid oklch(0.32 0.016 265 / 0.5);
	}
	.ov-ctrls .m-seg-lab {
		display: block;
		margin-bottom: 6px;
	}
</style>
