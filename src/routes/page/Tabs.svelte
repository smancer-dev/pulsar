<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	type Tab = {
		tabId: number;
		phase: 'connecting' | 'active';
		target: { name: string; id: string };
		label?: string;
	};
	type Props = {
		sessions: Tab[];
		activeTab: 'home' | number;
		onSelect: (tab: 'home' | number) => void;
		onEnd: (tabId: number) => void;
		onRename: (tabId: number, name: string) => void;
	};
	let { sessions, activeTab, onSelect, onEnd, onRename }: Props = $props();

	// Inline rename: which tab is being edited + its draft text. Commit on
	// Enter/blur, cancel on Escape; an emptied name restores the default.
	let editing = $state<number | null>(null);
	let draft = $state('');
	function startEdit(s: Tab) {
		editing = s.tabId;
		draft = s.label ?? s.target.name;
	}
	function commitEdit() {
		if (editing !== null) onRename(editing, draft);
		editing = null;
	}
	function editKey(e: KeyboardEvent) {
		if (e.key === 'Enter') commitEdit();
		else if (e.key === 'Escape') editing = null;
	}
	// Focus the input as soon as it mounts (it appears on the pencil click).
	function autofocus(node: HTMLInputElement) {
		node.focus();
		node.select();
	}
</script>

<div class="tabs">
	<button class="tab" class:on={activeTab === 'home'} onclick={() => onSelect('home')}>
		<Icon name="home" size={15} />{t('tab.home')}
	</button>
	{#each sessions as s (s.tabId)}
		<div
			class="tab"
			class:on={activeTab === s.tabId}
			role="button"
			tabindex="0"
			onclick={() => onSelect(s.tabId)}
			onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && onSelect(s.tabId)}
		>
			<span class="tdot" class:live={s.phase === 'active'}></span>
			{#if editing === s.tabId}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<input
					class="tedit"
					bind:value={draft}
					use:autofocus
					onblur={commitEdit}
					onkeydown={(e) => {
						e.stopPropagation();
						editKey(e);
					}}
					onclick={(e) => e.stopPropagation()}
					aria-label={t('tab.rename')}
				/>
			{:else}
				<span class="tname">{s.label ?? s.target.name}</span>
				<button
					class="tx tpen"
					aria-label={t('tab.rename')}
					title={t('tab.rename')}
					onclick={(e) => {
						e.stopPropagation();
						startEdit(s);
					}}
				>
					<Icon name="edit" size={11} />
				</button>
			{/if}
			<button
				class="tx"
				aria-label={t('tab.close')}
				onclick={(e) => {
					e.stopPropagation();
					onEnd(s.tabId);
				}}
			>
				<Icon name="x" size={11} />
			</button>
		</div>
	{/each}
</div>

<style>
	/* tab strip (home + connected hosts) */
	.tabs {
		flex: none;
		display: flex;
		align-items: stretch;
		gap: 2px;
		height: 36px;
		padding: 0 8px;
		background: var(--surface-2);
		border-bottom: 1px solid var(--border);
		overflow-x: auto;
	}
	.tab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		max-width: 220px;
		padding: 0 10px;
		border: none;
		border-bottom: 2px solid transparent;
		background: transparent;
		color: var(--text-muted);
		font-size: 13px;
		font-weight: 500;
		cursor: pointer;
		white-space: nowrap;
	}
	.tab:hover {
		background: var(--surface-3);
	}
	.tab.on {
		color: var(--accent-press);
		border-bottom-color: var(--accent);
		background: var(--surface);
	}
	.tname {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.tedit {
		width: 110px;
		border: 1px solid var(--accent);
		border-radius: 5px;
		background: var(--surface);
		color: var(--text);
		font-size: 12.5px;
		padding: 2px 6px;
		outline: none;
	}
	.tdot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--border-strong);
		flex: none;
	}
	.tdot.live {
		background: var(--ok);
	}
	.tx {
		display: grid;
		place-items: center;
		width: 18px;
		height: 18px;
		border: none;
		border-radius: 5px;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
	}
	.tx:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	/* The pencil stays invisible until the tab is hovered (keeps the strip clean). */
	.tpen {
		opacity: 0;
		transition: opacity 0.12s;
	}
	.tab:hover .tpen,
	.tpen:focus-visible {
		opacity: 1;
	}
</style>
