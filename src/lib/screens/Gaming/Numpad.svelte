<script lang="ts">
	// On-screen numeric / IP keypad for entering a host target with a controller in
	// gaming mode. Edits the bound target string through the shared `fmtTarget` so the
	// value stays canonical (relay IDs grouped in threes; IP/IP:port kept literal).
	import { fmtTarget } from '$lib/connectTarget';
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import type { Action } from 'svelte/action';

	type Props = {
		value: string;
		setValue: (v: string) => void;
		/** GamepadNav.item — makes each key controller/keyboard focusable. */
		navItem: Action<HTMLElement>;
	};
	let { value, setValue, navItem }: Props = $props();

	// Phone-style layout; the last cell of row 4 carries ':' for IP:port targets, and a
	// wide backspace spans the bottom.
	const KEYS = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '0', ':'];

	function press(k: string) {
		setValue(fmtTarget(value + k));
	}
	function backspace() {
		setValue(fmtTarget(value.replace(/\s/g, '').slice(0, -1)));
	}
</script>

<div class="numpad" role="group" aria-label="Tuş takımı">
	{#each KEYS as k (k)}
		<button class="key" use:navItem onclick={() => press(k)} aria-label={k}>{k}</button>
	{/each}
	<button class="key wide del" use:navItem data-navdelete onclick={backspace} aria-label={t('gaming.delete')}>
		<Icon name="x" size={16} /> {t('gaming.delete')}
	</button>
</div>

<style>
	.numpad {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 10px;
		width: 100%;
		max-width: 340px;
		margin: 0 auto;
	}
	.key {
		aspect-ratio: 1.6 / 1;
		display: grid;
		place-items: center;
		gap: 6px;
		grid-auto-flow: column;
		font-family: var(--font-mono);
		font-size: 22px;
		font-weight: 600;
		color: var(--text);
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r);
		cursor: pointer;
		transition:
			transform var(--dur) var(--ease),
			background var(--dur) var(--ease),
			box-shadow var(--dur) var(--ease);
	}
	.key:hover {
		background: var(--surface-3);
	}
	.key.wide {
		grid-column: 1 / -1;
		aspect-ratio: auto;
		padding: 14px 0;
		font-size: 15px;
		font-family: var(--font-sans);
	}
	.key.del {
		color: var(--text-muted);
	}
	.key:active {
		transform: translateY(1px);
	}
</style>
