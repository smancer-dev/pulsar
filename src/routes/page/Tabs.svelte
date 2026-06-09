<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	type Tab = { tabId: number; phase: 'connecting' | 'active'; target: { name: string; id: string } };
	type Props = {
		sessions: Tab[];
		activeTab: 'home' | number;
		onSelect: (tab: 'home' | number) => void;
		onEnd: (tabId: number) => void;
	};
	let { sessions, activeTab, onSelect, onEnd }: Props = $props();
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
			<span class="tname">{s.target.name}</span>
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
		max-width: 200px;
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
</style>
