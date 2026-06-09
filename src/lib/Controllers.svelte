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
</script>

<div class="ctrls" class:compact>
	{#if !compact}
		<div class="chead">
			<Icon name="gaming" size={16} />
			<b>{t('controllers.title')}</b>
			<span class="count mono">{connected}/{pads.length}</span>
		</div>
	{/if}
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
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 7px;
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
</style>
