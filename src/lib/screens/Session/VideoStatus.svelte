<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	// The non-canvas video status overlays (native-active note, error/loading/stall, and the
	// click-to-control hint) drawn over the session canvas. The canvas itself stays in the
	// parent (it owns the bind:this + input handlers); this only renders the status chain.
	type Target = { name: string; id: string };
	type Props = {
		native: boolean;
		embedded: boolean;
		mode: 'remote' | 'game';
		target: Target;
		/** Native capture engaged (clicked in — input is being forwarded). */
		nativeEngaged?: boolean;
		hasVideo: boolean;
		videoErr: string;
		stalled: boolean;
		controlling: boolean;
		hintFade: boolean;
		onStartControl: (e: PointerEvent) => void;
	};
	let {
		native,
		embedded,
		mode,
		target,
		nativeEngaged = false,
		hasVideo,
		videoErr,
		stalled,
		controlling,
		hintFade,
		onStartControl
	}: Props = $props();
</script>

{#if stalled && !embedded}
	<!-- FIRST in the chain: the native ghost branch below would otherwise shadow it
	     on every shipping path, leaving the "stream stopped" UI unreachable. -->
	<div class="stall">
		<Icon name="shield" size={34} />
		<div class="stallmsg">{t('session.streamStopped')}</div>
	</div>
{:else if !hasVideo && videoErr && !embedded}
	<div class="ghost">
		<Icon name={mode === 'game' ? 'gaming' : 'monitor'} size={46} />
		<div class="gname">{target.name}</div>
		<div class="gid mono">{target.id}</div>
		<div class="note err">{videoErr}</div>
	</div>
{:else if !hasVideo && !embedded}
	<!-- Connected, but the host hasn't started sending frames yet (it may be choosing a
	     screen or granting the OS screen-share permission — Wayland/macOS/Android). Show
	     the animated "waiting for the host" loader so this never reads as an error / a
	     dead screen. MUST come before the `native` branch below: on Linux the video is a
	     native child window, and the old order hid this loader entirely (blank/black while
	     the host was still deciding). The renderer keeps its window unmapped until the
	     first frame, so this webview loader is visible underneath until video starts. -->
	<div class="loading" class:game={mode === 'game'}>
		<div class="pulse" aria-hidden="true"><span></span><span></span><span></span></div>
		<div class="gname">{target.name}</div>
		<div class="lstat">{t('session.connecting')}</div>
		<div class="lhint">{t('session.waiting')}</div>
	</div>
{:else if native && !embedded}
	<!-- Native renderer plays in its own child window (video is up) — show NOTHING behind
	     it. The old "ghost" (icon + name + click-to-control note) must never appear
	     (maintainer decision). -->
{:else if !controlling}
	<button class="focushint" onpointerdown={onStartControl}>{t('session.clickToControl')}</button>
{:else}
	<div class="focushint locked" class:faded={hintFade}>
		{t('session.controllingPre')}<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F12</kbd>{t(
			'session.controllingSuf'
		)}
	</div>
{/if}

<style>
	.ghost {
		position: absolute;
		text-align: center;
		color: oklch(0.6 0.02 265);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
	}
	.gname {
		font-family: var(--font-display);
		font-size: 20px;
		color: oklch(0.82 0.02 265);
		margin-top: 6px;
	}
	.gid {
		font-size: 12px;
	}
	.note {
		max-width: 360px;
		margin-top: 14px;
		font-size: 12px;
		line-height: 1.5;
		color: oklch(0.62 0.02 265);
	}
	.note.err {
		color: var(--danger);
	}
	/* "waiting for the host's first frame" — animated pulse rings (the Pulsar mark
	   motif) so it clearly reads as connecting/loading, not an error or a dead screen. */
	.loading {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 6px;
	}
	.pulse {
		position: relative;
		width: 96px;
		height: 96px;
		margin-bottom: 10px;
	}
	.pulse span {
		position: absolute;
		inset: 0;
		margin: auto;
		width: 96px;
		height: 96px;
		border-radius: 50%;
		border: 2px solid var(--accent);
		opacity: 0;
		animation: pulsering 1.8s cubic-bezier(0.2, 0.6, 0.3, 1) infinite;
	}
	.pulse span:nth-child(2) {
		animation-delay: 0.6s;
	}
	.pulse span:nth-child(3) {
		animation-delay: 1.2s;
	}
	/* the steady "core" of the pulsar */
	.pulse::after {
		content: '';
		position: absolute;
		inset: 0;
		margin: auto;
		width: 13px;
		height: 13px;
		border-radius: 50%;
		background: var(--accent);
		box-shadow: 0 0 18px 3px color-mix(in oklch, var(--accent) 70%, transparent);
	}
	.loading.game .pulse span {
		border-color: var(--cyan);
	}
	.loading.game .pulse::after {
		background: var(--cyan);
		box-shadow: 0 0 18px 3px color-mix(in oklch, var(--cyan) 70%, transparent);
	}
	@keyframes pulsering {
		0% {
			transform: scale(0.22);
			opacity: 0.9;
		}
		70% {
			opacity: 0.12;
		}
		100% {
			transform: scale(1);
			opacity: 0;
		}
	}
	.lstat {
		font-family: var(--font-display);
		font-size: 17px;
		color: oklch(0.9 0.015 265);
	}
	.lhint {
		max-width: 300px;
		text-align: center;
		font-size: 12px;
		line-height: 1.5;
		color: oklch(0.62 0.02 265);
	}
	.stall {
		position: absolute;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 12px;
		max-width: 340px;
		padding: 22px 26px;
		text-align: center;
		color: #ffd9d4;
		background: oklch(0.18 0.03 25 / 0.86);
		border: 1px solid color-mix(in oklch, var(--danger) 55%, transparent);
		border-radius: var(--r-lg);
		box-shadow: var(--shadow-lg);
		backdrop-filter: blur(6px);
		z-index: 3;
	}
	.stallmsg {
		font-size: 13.5px;
		line-height: 1.5;
		font-weight: 500;
	}
	.focushint {
		position: absolute;
		bottom: 18px;
		left: 50%;
		transform: translateX(-50%);
		font-size: 12.5px;
		color: oklch(0.95 0.008 265);
		background: oklch(0.2 0.012 265 / 0.92);
		border: 1px solid oklch(0.4 0.016 265);
		padding: 8px 14px;
		border-radius: var(--r-pill);
		cursor: pointer;
		z-index: 2;
		transition: opacity 0.5s ease;
	}
	.focushint.locked {
		cursor: default;
	}
	.focushint.faded {
		opacity: 0;
		pointer-events: none;
	}
	.focushint kbd {
		font-family: var(--font-mono);
		background: oklch(0.3 0.015 265);
		padding: 1px 6px;
		border-radius: 4px;
	}
</style>
