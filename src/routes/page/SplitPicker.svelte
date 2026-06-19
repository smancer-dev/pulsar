<script lang="ts">
	// "Nasıl bölünsün?" — the split-layout chooser shown when the user presses the split
	// button. Three layouts (2 side-by-side, 2 stacked, 2×2) → enterSplit(layout). While
	// already split it also offers a "böl modundan çık" affordance to leave split mode.
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	type Layout = 'h2' | 'v2' | 'grid4';
	type Props = {
		/** Whether split mode is currently on (shows the exit affordance + active marker). */
		splitMode: 'off' | Layout;
		/** The personality every pane will run (the app's mode when split was opened). Shown
		 * in the subtitle so the user knows the split is all-gaming or all-remote. */
		entryMode?: 'game' | 'remote';
		onPick: (layout: Layout) => void;
		onExit: () => void;
		onClose: () => void;
	};
	let { splitMode, entryMode = 'remote', onPick, onExit, onClose }: Props = $props();

	const LAYOUTS: { id: Layout; labelKey: string }[] = [
		{ id: 'h2', labelKey: 'split.h2' },
		{ id: 'v2', labelKey: 'split.v2' },
		{ id: 'grid4', labelKey: 'split.grid4' }
	];

	function pick(layout: Layout) {
		onPick(layout);
		onClose();
	}
	function exit() {
		onExit();
		onClose();
	}
</script>

<div
	class="backdrop"
	role="presentation"
	onclick={(e) => {
		if (e.target === e.currentTarget) onClose();
	}}
>
	<div class="modal" role="dialog" aria-modal="true" aria-label={t('split.title')}>
		<button class="close" onclick={onClose} aria-label={t('split.close')}>
			<Icon name="x" size={16} />
		</button>

		<div class="mhead">{t('split.title')}</div>
		<div class="msub">{entryMode === 'game' ? t('split.subGame') : t('split.subRemote')}</div>

		<div class="layouts">
			{#each LAYOUTS as l (l.id)}
				<button class="layout" class:on={splitMode === l.id} onclick={() => pick(l.id)}>
					<!-- A tiny diagram of the layout. -->
					<span class="diagram {l.id}" aria-hidden="true">
						{#if l.id === 'grid4'}
							<span></span><span></span><span></span><span></span>
						{:else}
							<span></span><span></span>
						{/if}
					</span>
					<span class="ltext">{t(l.labelKey)}</span>
				</button>
			{/each}
		</div>

		{#if splitMode !== 'off'}
			<button class="exit" onclick={exit}>
				<Icon name="x" size={15} />
				{t('split.exit')}
			</button>
		{/if}
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: grid;
		place-items: center;
		background: color-mix(in oklch, var(--bg) 40%, transparent);
		backdrop-filter: blur(8px);
		padding: 24px;
	}
	.modal {
		position: relative;
		width: 100%;
		max-width: 440px;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 26px 24px 22px;
		background: var(--surface);
		border: 1px solid var(--border-strong);
		border-radius: var(--r-xl);
		box-shadow: var(--shadow-lg), var(--shadow-accent);
	}
	.close {
		position: absolute;
		top: 12px;
		right: 12px;
		width: 30px;
		height: 30px;
		display: grid;
		place-items: center;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
	}
	.close:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.mhead {
		font-family: var(--font-display);
		font-size: 19px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}
	.msub {
		font-size: 13px;
		color: var(--text-muted);
		margin-bottom: 12px;
	}
	.layouts {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 12px;
	}
	.layout {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
		padding: 16px 10px;
		border: 1px solid var(--border);
		border-radius: var(--r-lg);
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		transition: all var(--dur) var(--ease);
	}
	.layout:hover {
		background: var(--surface-3);
		color: var(--text);
		border-color: var(--border-strong);
	}
	.layout.on {
		background: var(--accent-soft);
		border-color: var(--accent);
		color: var(--accent-press);
	}
	.ltext {
		font-size: 12.5px;
		font-weight: 600;
	}
	/* Layout diagrams: two/four little cells in the right arrangement. */
	.diagram {
		display: grid;
		width: 48px;
		height: 36px;
		gap: 3px;
	}
	.diagram span {
		background: currentColor;
		opacity: 0.55;
		border-radius: 3px;
	}
	.diagram.h2 {
		grid-template-columns: 1fr 1fr;
	}
	.diagram.v2 {
		grid-template-rows: 1fr 1fr;
	}
	.diagram.grid4 {
		grid-template-columns: 1fr 1fr;
		grid-template-rows: 1fr 1fr;
	}
	.exit {
		margin-top: 16px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 7px;
		padding: 10px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text-muted);
		font: inherit;
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
		transition: all var(--dur) var(--ease);
	}
	.exit:hover {
		background: var(--surface-3);
		color: var(--text);
	}
</style>
