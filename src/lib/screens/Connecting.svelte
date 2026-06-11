<script lang="ts">
	import PulsarMark from '$lib/PulsarMark.svelte';
	import { onConnPhase } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		target: Target;
		mode: 'remote' | 'game';
		/** True once the host has been reached and we're waiting on its approval /
		 * the user's one-time password (a real milestone, driven by the parent). */
		awaitingApproval?: boolean;
		/** True once the host ACCEPTED (auth done, stream starting) — the screen stays
		 * up until first frames (`play-ready`), but the status must say "preparing",
		 * not "waiting for approval" (wrong for unattended hosts). */
		preparing?: boolean;
		onCancel: () => void;
	};
	let { target, mode, awaitingApproval = false, preparing = false, onCancel }: Props = $props();

	// Real milestones only (no faked "peer found"): we show "reaching out", then the
	// ACTUAL transport once the core establishes it (direct P2P vs relay), then
	// "awaiting host approval" when the parent says a prompt is pending.
	let transport = $state('');
	$effect(() => {
		let un: (() => void) | undefined;
		let dead = false;
		onConnPhase((e) => {
			if (e.target === target.id) transport = e.transport;
		}).then((u) => {
			// If the effect already tore down before listen() resolved, unlisten immediately
			// (otherwise the cleanup ran the initial no-op and this real unlisten would leak).
			if (dead) u();
			else un = u;
		});
		return () => {
			dead = true;
			un?.();
		};
	});
	// Step list (replaces the single status line): each REAL milestone is a row
	// that turns green when passed — reach (→ the actual transport), authorization,
	// then stream start. Driven only by signals we truly have (no faked progress).
	type StepState = 'pending' | 'active' | 'done';
	const steps = $derived.by((): { label: string; state: StepState }[] => {
		const reached = transport !== '';
		const authDone = preparing;
		return [
			{
				label: reached
					? transport === 'relay'
						? t('connecting.stepReachedRelay')
						: t('connecting.stepReachedP2p')
					: t('connecting.reaching'),
				state: reached ? 'done' : 'active'
			},
			{
				label: authDone
					? t('connecting.stepAuthDone')
					: awaitingApproval
						? t('connecting.awaiting')
						: t('connecting.stepAuth'),
				state: authDone ? 'done' : reached ? 'active' : 'pending'
			},
			{
				label: t('connecting.preparing'),
				state: preparing ? 'active' : 'pending'
			}
		];
	});

	// A connect stuck this long usually means an offline host or a blocking
	// firewall — surface a hint instead of spinning silently forever.
	let slow = $state(false);
	$effect(() => {
		const tmr = setTimeout(() => (slow = true), 12_000);
		return () => clearTimeout(tmr);
	});
</script>

<div class="overlay">
	<div class="stage">
		<span class="cring"></span>
		<span class="cring" style="animation-delay:0.9s"></span>
		<span class="cring" style="animation-delay:1.8s"></span>
		<PulsarMark size={48} />
	</div>
	<h2>{target.name}</h2>
	<div class="tid mono">{target.id} · {mode === 'game' ? t('connecting.modeGame') : t('connecting.modeRemote')}</div>
	<div class="steps">
		{#each steps as s, i (i)}
			<div class="step" class:done={s.state === 'done'} class:dim={s.state === 'pending'}>
				{#if s.state === 'done'}
					<span class="sicon ok"><svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M4 12l5 5L20 6"/></svg></span>
				{:else if s.state === 'active'}
					<span class="sicon spin"></span>
				{:else}
					<span class="sicon hollow"></span>
				{/if}
				<span class="slabel">{s.label}</span>
			</div>
		{/each}
	</div>
	{#if slow && !preparing}
		<div class="slowhint">{t('connecting.slowHint')}</div>
	{/if}
	<button class="btn btn-ghost" onclick={onCancel}>{t('connecting.cancel')}</button>
</div>

<style>
	.overlay {
		position: absolute;
		inset: 0;
		background: var(--bg);
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 14px;
		text-align: center;
		z-index: 5;
	}
	.stage {
		position: relative;
		display: grid;
		place-items: center;
		width: 120px;
		height: 120px;
		margin-bottom: 8px;
	}
	h2 {
		font-size: 22px;
	}
	.tid {
		font-size: 12.5px;
		color: var(--text-faint);
	}
	.steps {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 9px;
		margin: 16px 0 8px;
	}
	.step {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 13.5px;
		color: var(--text-muted);
		transition: color 0.25s;
	}
	.step.done {
		color: var(--ok);
	}
	.step.dim {
		color: var(--text-faint);
		opacity: 0.65;
	}
	.sicon {
		width: 15px;
		height: 15px;
		display: grid;
		place-items: center;
		flex: none;
	}
	.sicon.ok {
		border-radius: 50%;
		background: color-mix(in oklab, var(--ok) 18%, transparent);
		color: var(--ok);
		animation: pop 0.25s ease-out;
	}
	.sicon.hollow {
		border-radius: 50%;
		border: 2px solid var(--border-strong);
		width: 11px;
		height: 11px;
	}
	@keyframes pop {
		from {
			transform: scale(0.5);
			opacity: 0;
		}
	}
	.slowhint {
		font-size: 12.5px;
		color: var(--text-faint);
		max-width: 44ch;
		line-height: 1.5;
		margin-bottom: 4px;
	}
	.spin {
		width: 13px;
		height: 13px;
		border-radius: 50%;
		border: 2px solid var(--border-strong);
		border-top-color: var(--accent);
		animation: spin 0.8s linear infinite;
		flex: none;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
