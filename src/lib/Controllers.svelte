<script lang="ts">
	// Live list of detected game controllers — shown both in-app (Games screen) and
	// in-session (Session menu). Polls the Rust `controllers` command, which reads
	// every pad via gilrs (XInput/DInput/IOKit/evdev) with its name + connected
	// state, so the user can see which controllers are available to forward.
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import type { ControllerInfo } from '$lib/types';
	import { t } from '$lib/i18n.svelte';
	import { ui, saveUi, slotOf } from '$lib/settings.svelte';

	// `compact` is the small in-session variant (no card chrome).
	let { compact = false }: { compact?: boolean } = $props();

	let pads = $state<ControllerInfo[]>([]);
	let timer: ReturnType<typeof setInterval> | undefined;

	async function refresh() {
		try {
			pads = await api.controllers();
		} catch {
			pads = [];
		}
	}

	onMount(() => {
		refresh();
		timer = setInterval(refresh, 2000);
	});
	onDestroy(() => clearInterval(timer));

	const connected = $derived(pads.filter((p) => p.connected).length);

	// Seed any connected pads whose uuids are not yet in controllerOrder.
	// Runs as a side-effect so $derived.by stays pure.
	$effect(() => {
		if (compact) return;
		const seen = new Set(ui.controllerOrder);
		let changed = false;
		for (const p of pads) {
			if (p.connected && !seen.has(p.uuid)) {
				ui.controllerOrder.push(p.uuid);
				seen.add(p.uuid);
				changed = true;
			}
		}
		if (changed) {
			saveUi();
			api.setControllerOrder($state.snapshot(ui.controllerOrder) as string[]);
		}
	});

	// Build display rows ordered by current player slot (non-compact only).
	// Uses slotOf(p.uuid) to compute each pad's slot index.
	const slotRows = $derived.by(() => {
		return pads
			.map((p) => ({ slot: slotOf(p.uuid), pad: p }))
			.sort((a, b) => a.slot - b.slot);
	});

	function ensureSeeded() {
		const seen = new Set(ui.controllerOrder);
		for (const p of pads) {
			if (!seen.has(p.uuid)) {
				ui.controllerOrder.push(p.uuid);
				seen.add(p.uuid);
			}
		}
	}

	function moveUp(uuid: string) {
		ensureSeeded();
		const idx = ui.controllerOrder.indexOf(uuid);
		if (idx <= 0) return;
		const order = $state.snapshot(ui.controllerOrder) as string[];
		[order[idx - 1], order[idx]] = [order[idx], order[idx - 1]];
		ui.controllerOrder.length = 0;
		ui.controllerOrder.push(...order);
		saveUi();
		api.setControllerOrder($state.snapshot(ui.controllerOrder) as string[]);
	}

	function moveDown(uuid: string) {
		ensureSeeded();
		const idx = ui.controllerOrder.indexOf(uuid);
		if (idx < 0 || idx >= ui.controllerOrder.length - 1) return;
		const order = $state.snapshot(ui.controllerOrder) as string[];
		[order[idx], order[idx + 1]] = [order[idx + 1], order[idx]];
		ui.controllerOrder.length = 0;
		ui.controllerOrder.push(...order);
		saveUi();
		api.setControllerOrder($state.snapshot(ui.controllerOrder) as string[]);
	}
</script>

<div class="ctrls" class:compact>
	{#if !compact}
		<div class="chead">
			<Icon name="gaming" size={16} />
			<b>{t('controllers.title')}</b>
			<span class="count mono">{connected}/{pads.length}</span>
		</div>
		{#if pads.length === 0}
			<p class="none">{t('controllers.none')}</p>
		{:else}
			<p class="hint">{t('controllers.reorderHint')}</p>
			<ul class="slot-list">
				{#each slotRows as { slot, pad } (pad.uuid)}
					<li class:off={!pad.connected}>
						<span class="slot-label mono">{t('controllers.slot')} {slot + 1}</span>
						<span class="dot" class:on={pad.connected}></span>
						<span class="name">{pad.name}</span>
						<span class="kind mono">{pad.label}</span>
						<span class="reorder-btns">
							<button
								class="mv-btn"
								aria-label="Yukarı taşı"
								onclick={() => moveUp(pad.uuid)}
							>▲</button>
							<button
								class="mv-btn"
								aria-label="Aşağı taşı"
								onclick={() => moveDown(pad.uuid)}
							>▼</button>
						</span>
					</li>
				{/each}
			</ul>
		{/if}
		<p class="both-modes">{t('controllers.bothModes')}</p>
	{:else}
		{#if pads.length === 0}
			<p class="none">{t('controllers.none')}</p>
		{:else}
			<ul>
				{#each pads as p (p.index)}
					<li class:off={!p.connected}>
						<span class="dot" class:on={p.connected}></span>
						<span class="name">{p.name}</span>
						<span class="kind mono">{p.label}</span>
						<span class="state">{p.connected ? t('controllers.live') : t('controllers.idle')}</span>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>

<style>
	.ctrls {
		border: 1px solid var(--border);
		border-radius: var(--r-md);
		padding: 14px 16px;
		background: var(--surface);
	}
	.ctrls.compact {
		border: none;
		padding: 0;
		background: transparent;
	}
	.chead {
		display: flex;
		align-items: center;
		gap: 9px;
		margin-bottom: 10px;
	}
	.chead b {
		font-size: 14px;
		font-weight: 600;
	}
	.count {
		margin-left: auto;
		font-size: 12px;
		color: var(--text-muted);
	}
	.none {
		font-size: 12.5px;
		color: var(--text-faint);
		margin: 0;
	}
	.hint {
		font-size: 11.5px;
		color: var(--text-faint);
		margin: 0 0 8px 0;
	}
	.both-modes {
		font-size: 11.5px;
		color: var(--text-faint);
		margin: 10px 0 0 0;
		font-style: italic;
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 7px;
	}
	.slot-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	li {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 13px;
	}
	li.off {
		opacity: 0.5;
	}
	.slot-label {
		font-size: 11.5px;
		color: var(--accent);
		font-weight: 600;
		min-width: 7ch;
		flex: none;
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--text-faint);
		flex: none;
	}
	.dot.on {
		background: var(--ok);
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--ok) 22%, transparent);
	}
	.name {
		font-weight: 500;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 18ch;
	}
	.kind {
		font-size: 11.5px;
		color: var(--text-muted);
	}
	.state {
		margin-left: auto;
		font-size: 11.5px;
		color: var(--text-faint);
	}
	.reorder-btns {
		margin-left: auto;
		display: flex;
		gap: 2px;
	}
	.mv-btn {
		background: none;
		border: 1px solid var(--border);
		border-radius: var(--r-sm, 4px);
		padding: 1px 5px;
		font-size: 10px;
		cursor: pointer;
		color: var(--text-muted);
		line-height: 1.4;
	}
	.mv-btn:hover {
		background: var(--surface-raised, var(--surface));
		color: var(--text);
	}
</style>
