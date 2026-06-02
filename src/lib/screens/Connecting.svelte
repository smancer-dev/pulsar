<script lang="ts">
	import { onMount } from 'svelte';
	import PulsarMark from '$lib/PulsarMark.svelte';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		target: Target;
		mode: 'remote' | 'game';
		onCancel: () => void;
	};
	let { target, mode, onCancel }: Props = $props();

	// The session only becomes "active" when the host actually approves and the
	// stream starts (driven by the parent), so we stop on the last step and wait.
	const steps = $derived([
		t('connecting.step1'),
		t('connecting.step2'),
		t('connecting.step3'),
		t('connecting.step4')
	]);
	let step = $state(0);

	onMount(() => {
		const a = setInterval(() => {
			step = Math.min(step + 1, steps.length - 1);
		}, 480);
		return () => clearInterval(a);
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
		{#each steps as s, i (s)}
			<div class="step" class:done={i < step} class:active={i === step}>
				<span class="bullet"></span>{s}
			</div>
		{/each}
	</div>
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
		gap: 8px;
		margin: 14px 0 6px;
		text-align: left;
		width: 320px;
	}
	.step {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 13px;
		color: var(--text-faint);
	}
	.step.active {
		color: var(--text);
	}
	.step.done {
		color: var(--ok);
	}
	.bullet {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--border-strong);
		flex: none;
	}
	.step.active .bullet {
		background: var(--accent);
	}
	.step.done .bullet {
		background: var(--ok);
	}
</style>
