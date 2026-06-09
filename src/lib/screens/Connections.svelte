<script lang="ts">
	// Dedicated connections-management window (opened by Rust as a separate OS window
	// with `window.__CONNECTIONS__` set). Lists every active inbound connection and
	// lets the host disconnect each — the single place to manage all connections.
	// Opens forward for a Remote connection, hidden for a Game one (decided host-side).
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api, onSessionEvent } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Row = { peer: string; since: number };
	let rows = $state<Row[]>([]);

	onMount(() => {
		// Initial snapshot (connections that existed before this window opened).
		api
			.listConnections()
			.then((list) => {
				rows = list.map((c) => ({ peer: c.peer, since: c.since_ms }));
			})
			.catch(() => {});
		// Live updates: the same `session` events the host emits broadcast to every window.
		let unlisten: (() => void) | undefined;
		onSessionEvent((e) => {
			if (e.kind === 'connected') {
				if (!rows.some((r) => r.peer === e.peer))
					rows = [...rows, { peer: e.peer, since: Date.now() }];
			} else if (e.kind === 'disconnected') {
				rows = rows.filter((r) => r.peer !== e.peer);
			}
		}).then((u) => {
			unlisten = u;
		});
		return () => unlisten?.();
	});

	// Live "for N" elapsed label, refreshed each second.
	let now = $state(Date.now());
	$effect(() => {
		const tmr = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(tmr);
	});
	function elapsed(since: number): string {
		const s = Math.max(0, Math.floor((now - since) / 1000));
		if (s < 60) return `${s}s`;
		const m = Math.floor(s / 60);
		return m < 60 ? `${m}d` : `${Math.floor(m / 60)}sa ${m % 60}d`;
	}
</script>

<div class="conns">
	<header class="chdr">
		<span class="live"></span>
		<b>{t('host.activeTitle')}</b>
		<span class="count">{rows.length}</span>
	</header>

	{#if rows.length === 0}
		<div class="empty">{t('host.noConnections')}</div>
	{:else}
		<div class="list">
			{#each rows as r (r.peer)}
				<div class="prow">
					<span class="pavatar">{r.peer.slice(0, 2)}</span>
					<div class="pmeta">
						<span class="pname mono">{r.peer}</span>
						<span class="ptime">{elapsed(r.since)}</span>
					</div>
					<button
						class="kick"
						onclick={() => api.disconnectPeer(r.peer).catch(() => {})}
						title={t('host.disconnect')}
					>
						<Icon name="power" size={13} />{t('host.disconnect')}
					</button>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.conns {
		height: 100vh;
		display: flex;
		flex-direction: column;
		background: var(--bg, var(--surface));
		color: var(--text);
		padding: 14px;
		gap: 12px;
		box-sizing: border-box;
	}
	.chdr {
		display: flex;
		align-items: center;
		gap: 9px;
	}
	.chdr b {
		font-size: 14px;
		font-weight: 600;
	}
	.live {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--ok);
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--ok) 22%, transparent);
		flex: none;
	}
	.count {
		margin-left: auto;
		background: var(--accent-soft);
		color: var(--accent-press);
		border-radius: 999px;
		font-size: 12px;
		font-weight: 700;
		min-width: 20px;
		height: 20px;
		display: grid;
		place-items: center;
		padding: 0 6px;
	}
	.empty {
		flex: 1;
		display: grid;
		place-items: center;
		color: var(--text-faint);
		font-size: 13px;
	}
	.list {
		display: flex;
		flex-direction: column;
		gap: 8px;
		overflow-y: auto;
	}
	.prow {
		display: flex;
		align-items: center;
		gap: 10px;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r-md);
		padding: 9px 11px;
	}
	.pavatar {
		width: 32px;
		height: 32px;
		border-radius: 50%;
		background: var(--accent-soft);
		color: var(--accent-press);
		display: grid;
		place-items: center;
		font-size: 12px;
		font-weight: 700;
		text-transform: uppercase;
		flex: none;
	}
	.pmeta {
		display: flex;
		flex-direction: column;
		min-width: 0;
		flex: 1;
	}
	.pname {
		font-size: 13px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ptime {
		font-size: 11px;
		color: var(--text-faint);
	}
	.kick {
		display: flex;
		align-items: center;
		gap: 4px;
		background: none;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		color: var(--danger, oklch(0.55 0.2 25));
		font: inherit;
		font-size: 11px;
		padding: 5px 8px;
		cursor: pointer;
		flex: none;
	}
	.kick:hover {
		background: color-mix(in oklch, var(--danger, red) 10%, transparent);
	}
</style>
