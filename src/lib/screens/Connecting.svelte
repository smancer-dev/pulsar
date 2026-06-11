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
	const status = $derived(
		preparing
			? t('connecting.preparing')
			: awaitingApproval
				? t('connecting.awaiting')
				: transport === 'direct'
					? t('connecting.p2p')
					: transport === 'relay'
						? t('connecting.relay')
						: t('connecting.reaching')
	);

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
	<div class="status"><span class="spin"></span>{status}</div>
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
	.status {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 13.5px;
		color: var(--text-muted);
		margin: 16px 0 8px;
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
