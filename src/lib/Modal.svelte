<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	let {
		title,
		onClose,
		children
	}: { title: string; onClose: () => void; children: Snippet } = $props();

	function onKey(e: KeyboardEvent) {
		if (e.key === 'Escape') onClose();
	}
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" onclick={onClose} role="presentation"></div>
<div class="modal" role="dialog" aria-modal="true" aria-label={title}>
	<div class="mhead">
		<h3>{title}</h3>
		<button class="x" aria-label={t('modal.close')} onclick={onClose}><Icon name="x" size={16} /></button>
	</div>
	<div class="mbody">
		{@render children()}
	</div>
</div>

<style>
	.backdrop {
		position: absolute;
		inset: 0;
		background: oklch(0.2 0.02 268 / 0.45);
		z-index: 20;
	}
	.modal {
		position: absolute;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		width: min(520px, 90%);
		max-height: 84%;
		overflow: auto;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r-lg);
		box-shadow: var(--shadow-lg);
		z-index: 21;
	}
	.mhead {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 18px 20px;
		border-bottom: 1px solid var(--border);
	}
	.mhead h3 {
		font-size: 18px;
	}
	.x {
		width: 30px;
		height: 30px;
		display: grid;
		place-items: center;
		border: 1px solid var(--border);
		background: var(--surface);
		border-radius: var(--r-sm);
		color: var(--text-muted);
		cursor: pointer;
	}
	.x:hover {
		color: var(--text);
		border-color: var(--border-strong);
	}
	.mbody {
		padding: 20px;
	}
</style>
