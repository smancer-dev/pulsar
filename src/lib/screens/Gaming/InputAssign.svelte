<script lang="ts">
	// Couch co-op input assignment (split + gaming): lock each physical input device — controllers
	// AND keyboards/mice — to a specific pane's session, so two players use separate gear. Controllers
	// route via set_controller_lock (existing); kb/mice via set_kbm_lock (the per-device evdev route).
	// Write-only v1: it SETS the assignment (the backend map persists until pane teardown); it does
	// not read back the current owner, so the selects reflect what was chosen in THIS modal session.
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import type { ControllerInfo } from '$lib/types';
	import { t } from '$lib/i18n.svelte';

	type Pane = { index: number; playId: number };
	let { panes, onClose }: { panes: Pane[]; onClose: () => void } = $props();

	let pads = $state<ControllerInfo[]>([]);
	let kbms = $state<string[]>([]);
	// Local choice per device (key → playId; -1 = unassigned/follows focus). Not read from backend.
	let padPick = $state<Record<string, number>>({});
	let kbmPick = $state<Record<string, number>>({});

	onMount(async () => {
		pads = await api.controllers().catch(() => []);
		kbms = await api.listInputDevices().catch(() => []);
	});

	// A `set_*_lock(.., true)` OVERWRITES any prior owner, so re-assigning needs no explicit unlock;
	// only an unassign (-1) clears the previously-chosen owner.
	function pickPad(uuid: string, playId: number) {
		const prev = padPick[uuid] ?? -1;
		if (playId >= 0) api.setControllerLock(uuid, playId, true).catch(() => {});
		else if (prev >= 0) api.setControllerLock(uuid, prev, false).catch(() => {});
		padPick[uuid] = playId;
	}
	function pickKbm(key: string, playId: number) {
		const prev = kbmPick[key] ?? -1;
		if (playId >= 0) api.setKbmLock(key, playId, true).catch(() => {});
		else if (prev >= 0) api.setKbmLock(key, prev, false).catch(() => {});
		kbmPick[key] = playId;
	}
	const paneLabel = (p: Pane) => `${t('split.pane')} ${p.index + 1}`;
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="ia-back" onpointerdown={(e) => e.target === e.currentTarget && onClose()}>
	<div class="ia-card">
		<div class="ia-hdr">
			<b>{t('input.assignTitle')}</b><span>{t('input.assignDesc')}</span>
		</div>

		<div class="ia-sec">{t('input.controllers')}</div>
		{#if pads.length === 0}
			<div class="ia-empty">{t('input.noControllers')}</div>
		{/if}
		{#each pads as pad (pad.uuid)}
			<div class="ia-row">
				<span class="ia-name">🎮 {pad.name}</span>
				<select
					value={padPick[pad.uuid] ?? -1}
					onchange={(e) => pickPad(pad.uuid, +(e.currentTarget as HTMLSelectElement).value)}
				>
					<option value={-1}>{t('input.followFocus')}</option>
					{#each panes as p (p.playId)}
						<option value={p.playId}>{paneLabel(p)}</option>
					{/each}
				</select>
			</div>
		{/each}

		<div class="ia-sec">{t('input.keyboards')}</div>
		{#if kbms.length === 0}
			<div class="ia-empty">{t('input.noKbm')}</div>
		{/if}
		{#each kbms as key (key)}
			<div class="ia-row">
				<span class="ia-name">⌨ {key}</span>
				<select
					value={kbmPick[key] ?? -1}
					onchange={(e) => pickKbm(key, +(e.currentTarget as HTMLSelectElement).value)}
				>
					<option value={-1}>{t('input.followFocus')}</option>
					{#each panes as p (p.playId)}
						<option value={p.playId}>{paneLabel(p)}</option>
					{/each}
				</select>
			</div>
		{/each}

		<div class="ia-act"><button class="ia-btn" onclick={onClose}>{t('pw.cancel')}</button></div>
	</div>
</div>

<style>
	.ia-back {
		position: absolute;
		inset: 0;
		z-index: 30;
		display: grid;
		place-items: center;
		background: oklch(0.2 0.01 265 / 0.5);
		backdrop-filter: blur(3px);
	}
	.ia-card {
		width: 460px;
		max-width: calc(100% - 40px);
		max-height: calc(100% - 60px);
		overflow: auto;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r);
		box-shadow: var(--shadow-lg);
		padding: 20px;
	}
	.ia-hdr b {
		font-size: 16px;
		font-weight: 700;
		color: var(--accent-press);
		display: block;
	}
	.ia-hdr span {
		font-size: 12.5px;
		color: var(--text-muted);
		display: block;
		margin-top: 4px;
		line-height: 1.45;
	}
	.ia-sec {
		margin: 16px 0 6px;
		font-size: 12px;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.ia-empty {
		font-size: 12.5px;
		color: var(--text-faint);
		padding: 6px 0;
	}
	.ia-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 0;
		border-bottom: 1px solid var(--border);
	}
	.ia-name {
		flex: 1;
		min-width: 0;
		font-size: 13.5px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ia-row select {
		padding: 6px 8px;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text);
		font-size: 13px;
	}
	.ia-act {
		margin-top: 16px;
		display: flex;
		justify-content: flex-end;
	}
	.ia-btn {
		padding: 9px 16px;
		border-radius: var(--r-sm);
		font-weight: 600;
		font-size: 14px;
		cursor: pointer;
		border: 1px solid var(--border);
		background: var(--surface-2);
		color: var(--text);
	}
	.ia-btn:hover {
		background: var(--surface-3);
	}
</style>
