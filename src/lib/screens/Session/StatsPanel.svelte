<script lang="ts">
	import { t } from '$lib/i18n.svelte';

	// Read-only perf panel shown on hover over the dock handle: a network-latency headline,
	// an fps sparkline, then aligned rows. All values are derived in the parent and passed in.
	type Props = {
		netClass: string;
		fps: number;
		latencyMs: number;
		spark: string;
		hostCodec: string;
		hostRes: string;
		hostEncoder: string;
		hostFps: string;
		decoderCodec: string;
		decodeMs: number;
		connLabel: string;
		rttMs: number;
		jitterMs: number;
		lossPct: number;
		mbps: number;
	};
	let {
		netClass,
		fps,
		latencyMs,
		spark,
		hostCodec,
		hostRes,
		hostEncoder,
		hostFps,
		decoderCodec,
		decodeMs,
		connLabel,
		rttMs,
		jitterMs,
		lossPct,
		mbps
	}: Props = $props();
</script>

<div class="stats mono">
	<div class="stat-big">
		<b class:warn={latencyMs >= 30} class:bad={latencyMs >= 60}>{latencyMs}</b> ms
		<span class="dim">{t('session.statLatency')}</span>
		<span class="net-dot {netClass}"></span>
	</div>
	{#if spark}
		<svg class="spark" viewBox="0 0 120 32" preserveAspectRatio="none" aria-hidden="true">
			<polyline points={spark} />
		</svg>
	{/if}
	<div class="stat-rows">
		<div><span>{t('session.statVideo')}</span><b>{[hostCodec, hostRes].filter(Boolean).join(' · ') || '—'}</b></div>
		<div><span>{t('session.statEncoder')}</span><b>{[hostEncoder, hostFps].filter(Boolean).join(' · ') || '—'}</b></div>
		<div><span>{t('session.statDecoder')}</span><b>{decoderCodec || '—'}</b></div>
		<div><span>{t('session.statFps')}</span><b>{fps}{hostFps ? ` / ${hostFps.replace('fps', '')}` : ''}</b></div>
		<div><span>{t('session.statDecodeMs')}</span><b>{decodeMs} ms</b></div>
		<div><span>{t('session.statNet')}</span><b>{connLabel}{rttMs > 0 ? ` · ${rttMs} ms` : ''}</b></div>
		<div><span>{t('session.statJitter')}</span><b class:warn={jitterMs >= 5} class:bad={jitterMs >= 15}>{jitterMs} ms</b></div>
		<div><span>{t('session.statLoss')}</span><b class:warn={lossPct >= 1} class:bad={lossPct >= 5}>{lossPct.toFixed(1)}%</b></div>
		<div><span>{t('session.statBitrate')}</span><b>{mbps} Mbps</b></div>
	</div>
</div>

<style>
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
	/* perf panel: a network-latency headline, an fps sparkline, then aligned rows
	   (video / encoder / decoder / fps / decode / net / jitter / loss / bitrate). */
	.stats {
		margin-top: 6px;
		width: 232px;
		padding: 11px 13px;
		border-radius: var(--r-md, 10px);
		background: oklch(0.16 0.012 265 / 0.97);
		border: 1px solid oklch(0.36 0.016 265 / 0.7);
		box-shadow: var(--shadow-lg);
		color: oklch(0.96 0.008 265);
		backdrop-filter: blur(10px);
		pointer-events: none;
	}
	.stat-big {
		display: flex;
		align-items: baseline;
		gap: 5px;
		font-size: 13px;
		letter-spacing: 0.01em;
	}
	.stat-big b {
		font-size: 19px;
		font-weight: 700;
		color: #fff;
	}
	.stat-big .dim {
		margin-left: auto;
		font-size: 10.5px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: oklch(0.62 0.02 265);
	}
	.stat-big .net-dot {
		align-self: center;
	}
	.spark {
		width: 100%;
		height: 28px;
		margin: 8px 0 6px;
		display: block;
	}
	.spark polyline {
		fill: none;
		stroke: var(--accent);
		stroke-width: 1.5;
		vector-effect: non-scaling-stroke;
		stroke-linejoin: round;
		stroke-linecap: round;
	}
	.stat-rows {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-top: 2px;
		font-size: 11px;
	}
	.stat-rows div {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 12px;
	}
	.stat-rows span {
		color: oklch(0.65 0.02 265);
		flex: none;
	}
	.stat-rows b {
		color: #fff;
		font-weight: 600;
		text-align: right;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* shared severity tints for latency / jitter / loss values */
	.stats b.warn {
		color: #f4bf4f;
	}
	.stats b.bad {
		color: var(--danger);
	}
</style>
