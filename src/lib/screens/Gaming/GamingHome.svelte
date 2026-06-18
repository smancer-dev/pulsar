<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import ConnectModal from './ConnectModal.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api, type LanDevice } from '$lib/api';
	import { gameHistoryPeers, removeFromGameHistory, fmtPeerId, avatarFor } from '$lib/peers.svelte';
	import type { GamepadNav } from '$lib/gamepadNav.svelte';

	type Props = {
		/** The shared GamepadNav (our items + the connect modal register against it). */
		nav: GamepadNav;
		/** Pick a host by id → the shell opens the connecting/games screen (no popup for games). */
		onPickHost: (id: string) => void;
	};
	let { nav, onPickHost }: Props = $props();
	const navItem = nav.item;

	// Game-only recents (separate timeline from remote recents / Devices).
	const recents = $derived(gameHistoryPeers(12));

	// LAN auto-discovery: Pulsar hosts announcing on this network (multicast beacon).
	let lan = $state<LanDevice[]>([]);
	const lanHosts = $derived(lan.filter((d) => d.has_id && d.id));
	onMount(() => {
		const refresh = () => api.lanDevices().then((d) => (lan = d)).catch(() => {});
		refresh();
		const timer = setInterval(refresh, 2000);
		return () => clearInterval(timer);
	});

	// The connect pop-up collects ONLY the host id (ID box + numpad); on submit the shell
	// shows the games screen. A recent/LAN card already has the id, so it skips the popup.
	let modalOpen = $state(false);

	function initials(name: string) {
		return name.split(' ').map((w) => w[0]).slice(0, 2).join('').toUpperCase();
	}
</script>

<div class="ghome">
	<div class="hero">
		<span class="eyebrow mono">{t('gaming.eyebrow')}</span>
		<h1>{t('gaming.title')}</h1>
		<button class="btn btn-primary connect" use:navItem onclick={() => (modalOpen = true)}>
			<Icon name="connect" size={20} />
			{t('gaming.connectCta')}
		</button>
	</div>

	{#if lanHosts.length}
		<div class="recents">
			<div class="rlab"><span class="lpulse"></span>{t('gaming.lan')}</div>
			<div class="rgrid">
				{#each lanHosts as d (d.addr + '|' + d.id)}
					<button class="rcard" use:navItem onclick={() => onPickHost(d.id)}>
						<span class="ravatar">
							{#if avatarFor(d.id)}<img class="rimg" src={avatarFor(d.id)} alt="" />{:else}{initials(d.name)}{/if}
						</span>
						<span class="rmeta">
							<span class="rname">{d.name}</span>
							<span class="rid mono">{fmtPeerId(d.id)}</span>
						</span>
					</button>
				{/each}
			</div>
		</div>
	{/if}

	<div class="recents">
		<div class="rlab">{t('gaming.recents')}</div>
		{#if recents.length === 0}
			<div class="empty">{t('gaming.noRecents')}</div>
		{:else}
			<div class="rgrid">
				{#each recents as r (r.id)}
					<div class="rrow">
						<button class="rcard" use:navItem onclick={() => onPickHost(r.id)}>
							<span class="ravatar">
								{#if r.avatar}<img class="rimg" src={r.avatar} alt="" />{:else}{initials(r.name)}{/if}
							</span>
							<span class="rmeta">
								<span class="rname">{r.name}</span>
								<span class="rid mono">{fmtPeerId(r.id)}</span>
							</span>
						</button>
						<button
							class="rdel"
							title={t('home.removeRecent')}
							aria-label={t('home.removeRecent')}
							onclick={() => removeFromGameHistory(r.id)}>×</button
						>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>

{#if modalOpen}
	<ConnectModal
		{nav}
		onPick={(id) => onPickHost(id)}
		onClose={() => (modalOpen = false)}
	/>
{/if}

<style>
	.ghome {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		align-items: center;
		padding: 48px 24px 24px;
		gap: 40px;
	}
	.hero {
		width: 100%;
		max-width: 440px;
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 18px;
	}
	.eyebrow {
		font-size: 11px;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--accent);
	}
	h1 {
		font-size: 34px;
		letter-spacing: -0.03em;
	}
	.connect {
		justify-content: center;
		min-width: 240px;
		padding: 16px 28px;
		font-size: 17px;
		margin-top: 4px;
	}
	.recents {
		width: 100%;
		max-width: 760px;
	}
	.rlab {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		font-size: 11.5px;
		color: var(--text-faint);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		margin-bottom: 14px;
		text-align: center;
	}
	.lpulse {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ok);
		flex: none;
		animation: lpulse 1.8s ease-out infinite;
	}
	@keyframes lpulse {
		0% {
			box-shadow: 0 0 0 0 color-mix(in oklch, var(--ok) 55%, transparent);
		}
		70% {
			box-shadow: 0 0 0 7px transparent;
		}
		100% {
			box-shadow: 0 0 0 0 transparent;
		}
	}
	.empty {
		font-size: 12.5px;
		color: var(--text-faint);
		text-align: center;
		padding: 16px;
		border: 1px dashed var(--border);
		border-radius: var(--r-sm);
	}
	.rgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
		gap: 10px;
	}
	.rrow {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.rrow .rcard {
		flex: 1;
		min-width: 0;
	}
	.rcard {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 11px 13px;
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r);
		cursor: pointer;
		text-align: left;
		color: var(--text);
		transition: background var(--dur) var(--ease);
	}
	.rcard:hover {
		background: var(--surface-3);
	}
	.ravatar {
		width: 38px;
		height: 38px;
		border-radius: 10px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 13px;
		font-family: var(--font-display);
		flex: none;
		overflow: hidden;
	}
	.rimg {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}
	.rmeta {
		display: flex;
		flex-direction: column;
		line-height: 1.3;
		min-width: 0;
	}
	.rname {
		font-size: 14.5px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.rid {
		font-size: 11px;
		color: var(--text-faint);
	}
	.rdel {
		flex: none;
		width: 28px;
		height: 28px;
		border: none;
		border-radius: 8px;
		background: transparent;
		color: var(--text-faint);
		font-size: 17px;
		line-height: 1;
		cursor: pointer;
	}
	.rdel:hover {
		background: var(--accent-soft);
		color: var(--accent);
	}
</style>
