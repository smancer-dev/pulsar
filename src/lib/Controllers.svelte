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
	import type { Action } from 'svelte/action';

	// Seg-button options (dark-mode styled, pad-operable) — replaces the native <select>s,
	// which weren't dark-mode-consistent and couldn't be changed with a controller. Labels
	// are i18n keys (the values are the stable tokens persisted/sent to the core).
	const EMU_OPTS = [
		{ v: 'auto', k: 'controllers.emuAuto' },
		{ v: 'xbox', k: 'controllers.emuXbox' },
		{ v: 'ds4', k: 'controllers.emuDs4' }
	] as const;
	const RUMBLE_OPTS = [
		{ v: 'off', k: 'controllers.rumbleOff' },
		{ v: 'weak', k: 'controllers.rumbleWeak' },
		{ v: 'medium', k: 'controllers.rumbleMedium' },
		{ v: 'strong', k: 'controllers.rumbleStrong' }
	] as const;

	// `compact` is the small in-session variant (no card chrome). `navItem` is the gaming
	// GamepadNav action so the controls are pad-navigable when shown inside the gaming-mode
	// controllers popup (no-op everywhere else, so the remote-mode Games tab is unaffected).
	const noopAction: Action<HTMLElement> = () => {};
	let { compact = false, navItem = noopAction }: { compact?: boolean; navItem?: Action<HTMLElement> } =
		$props();

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
	// Also push the current emulation map so the play reader has it before a session starts.
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
		// Push emulation + per-pad rumble maps once on every refresh so the Arcs are
		// populated before a session starts (no extra IPC if unchanged — same values).
		api.setControllerEmulation($state.snapshot(ui.controllerEmulation) as Record<string, string>);
		api.setControllerRumble($state.snapshot(ui.controllerRumble) as Record<string, string>);
		// Same for the disabled set, so the play reader knows which pads to ignore.
		api.setDisabledControllers(Object.keys($state.snapshot(ui.controllerDisabled)));
	});

	function setRumble(uuid: string, value: string) {
		ui.controllerRumble[uuid] = value as (typeof RUMBLE_OPTS)[number]['v'];
		saveUi();
		api.setControllerRumble($state.snapshot(ui.controllerRumble) as Record<string, string>);
	}

	function setDisabled(uuid: string, off: boolean) {
		if (off) ui.controllerDisabled[uuid] = true;
		else delete ui.controllerDisabled[uuid];
		saveUi();
		api.setDisabledControllers(Object.keys($state.snapshot(ui.controllerDisabled)));
	}

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

	function setEmulation(uuid: string, value: 'auto' | 'xbox' | 'ds4') {
		ui.controllerEmulation[uuid] = value;
		saveUi();
		api.setControllerEmulation($state.snapshot(ui.controllerEmulation) as Record<string, string>);
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
					<li class="pad-card" class:off={!pad.connected} class:disabled={!!ui.controllerDisabled[pad.uuid]}>
						<div class="pad-top">
							<span class="slot-label mono">{t('controllers.slot')} {slot + 1}</span>
							<span class="dot" class:on={pad.connected}></span>
							<span class="name">{pad.name}</span>
							<span class="kind mono">{pad.label}</span>
							{#if pad.battery != null}
								<span class="batt mono" title={t('controllers.battery')}>{pad.battery}%</span>
							{/if}
							<button
								class="enable-btn"
								class:off={!!ui.controllerDisabled[pad.uuid]}
								use:navItem
								aria-pressed={!ui.controllerDisabled[pad.uuid]}
								onclick={() => setDisabled(pad.uuid, !ui.controllerDisabled[pad.uuid])}
							>{ui.controllerDisabled[pad.uuid] ? t('controllers.disabled') : t('controllers.enabled')}</button>
							<span class="reorder-btns">
								<button class="mv-btn" use:navItem aria-label={t('controllers.aMoveUp')} onclick={() => moveUp(pad.uuid)}>▲</button>
								<button class="mv-btn" use:navItem aria-label={t('controllers.aMoveDown')} onclick={() => moveDown(pad.uuid)}>▼</button>
							</span>
						</div>
						<!-- Emulation target: dark-mode seg buttons (mouse + controller). -->
						<div class="seg-row">
							<span class="seg-lab">{t('controllers.emulation')}</span>
							<div class="seg" role="group" aria-label={t('controllers.emulation')}>
								{#each EMU_OPTS as o}
									{@const on = (ui.controllerEmulation[pad.uuid] ?? 'auto') === o.v}
									<button class="seg-btn" class:on use:navItem aria-pressed={on} onclick={() => setEmulation(pad.uuid, o.v)}>{t(o.k)}</button>
								{/each}
							</div>
						</div>
						<!-- Per-pad vibration strength: dark-mode seg buttons + a TEST pulse. -->
						<div class="seg-row">
							<span class="seg-lab">{t('controllers.rumble')}</span>
							<div class="seg vib" role="group" aria-label={t('controllers.rumble')}>
								{#each RUMBLE_OPTS as o}
									{@const on = (ui.controllerRumble[pad.uuid] ?? 'medium') === o.v}
									<button class="seg-btn" class:on use:navItem aria-pressed={on} onclick={() => setRumble(pad.uuid, o.v)}>{t(o.k)}</button>
								{/each}
							</div>
							<button
								class="test-btn"
								use:navItem
								disabled={!pad.connected || (ui.controllerRumble[pad.uuid] ?? 'medium') === 'off'}
								title={t('controllers.test')}
								onclick={() => api.testControllerRumble(pad.uuid).catch(() => {})}
							>{t('controllers.testBtn')}</button>
						</div>
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
	.pad-card {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 10px 12px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm, 6px);
		background: var(--surface-2, var(--surface));
	}
	.pad-top {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 13px;
	}
	.seg-row {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.seg-lab {
		font-size: 11.5px;
		color: var(--text-muted);
		font-weight: 600;
		min-width: 8ch;
		flex: none;
	}
	.seg {
		display: flex;
		gap: 4px;
		flex-wrap: wrap;
		margin-left: auto;
	}
	.seg-btn {
		font-size: 11.5px;
		font-family: inherit;
		color: var(--text-muted);
		background: var(--surface-3, var(--surface));
		border: 1px solid var(--border-strong, var(--border));
		border-radius: var(--r-sm, 4px);
		padding: 3px 9px;
		cursor: pointer;
		transition: all var(--dur) var(--ease);
	}
	.seg-btn:hover {
		color: var(--text);
		background: var(--accent-soft);
	}
	.seg-btn.on {
		color: var(--text-on-accent, #fff);
		background: var(--accent);
		border-color: var(--accent);
	}
	.test-btn {
		font-size: 11.5px;
		font-family: inherit;
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		border: 1px solid var(--accent);
		border-radius: var(--r-sm, 4px);
		padding: 3px 10px;
		cursor: pointer;
		flex: none;
		transition: all var(--dur) var(--ease);
	}
	.test-btn:hover:not(:disabled) {
		color: var(--text-on-accent, #fff);
		background: var(--accent);
	}
	.test-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.batt {
		font-size: 11px;
		color: var(--text-muted);
		border: 1px solid var(--border);
		border-radius: 999px;
		padding: 1px 6px;
		flex: none;
	}
	.enable-btn {
		font-size: 11px;
		font-family: inherit;
		font-weight: 600;
		color: var(--text-on-accent, #fff);
		background: var(--ok, oklch(0.62 0.16 150));
		border: none;
		border-radius: var(--r-sm, 4px);
		padding: 2px 9px;
		cursor: pointer;
		flex: none;
	}
	.enable-btn.off {
		background: var(--danger, oklch(0.55 0.2 25));
	}
	.pad-card.disabled {
		opacity: 0.55;
	}
	.pad-card.disabled .seg-btn,
	.pad-card.disabled .name {
		filter: grayscale(0.6);
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
