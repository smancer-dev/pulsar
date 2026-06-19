<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import { gamingNav } from '$lib/gamepadNav.svelte';
	import { portal } from '$lib/portal';
	import { openModal, closeModal } from '$lib/overlayModals.svelte';

	let {
		title,
		onClose,
		children,
		/** Mark the dialog `[data-navmodal]` so the gaming-mode GamepadNav confines roving
		 * focus to it (controller-navigable popups). Off for normal remote-mode modals. */
		navModal = false
	}: { title: string; onClose: () => void; children: Snippet; navModal?: boolean } = $props();

	function onKey(e: KeyboardEvent) {
		if (e.key === 'Escape') onClose();
	}

	// While this modal is up, occlude every pane's native render (split-mode #8): on Linux the
	// native video composites OVER the webview, so without this a live pane would hide the modal.
	$effect(() => {
		openModal();
		return closeModal;
	});

	// Register the close (×) button with the gaming nav when this is a controller-navigable
	// popup, so the pad can reach it (otherwise the × at the top was unreachable). No-op for
	// normal modals (the action just doesn't register the node).
	function closeNav(node: HTMLElement) {
		if (!navModal) return;
		return gamingNav.item(node);
	}
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" use:portal onclick={onClose} role="presentation"></div>
<div class="modal" use:portal role="dialog" aria-modal="true" aria-label={title} data-navmodal={navModal ? '' : undefined}>
	<div class="mhead">
		<h3>{title}</h3>
		<button class="x" use:closeNav aria-label={t('modal.close')} onclick={onClose}><Icon name="x" size={16} /></button>
	</div>
	<div class="mbody">
		{@render children()}
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: oklch(0.2 0.02 268 / 0.45);
		z-index: 70;
	}
	.modal {
		position: fixed;
		z-index: 71;
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
