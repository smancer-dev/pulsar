<script lang="ts">
	import { onMount } from 'svelte';
	import { addPeer } from '$lib/peers.svelte';
	import { api, type LanDevice } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		mode: 'remote' | 'game';
		onConnect: (t: Target, m?: 'remote' | 'game', gameId?: string) => void;
	};
	let { mode, onConnect }: Props = $props();

	// LAN auto-discovery: poll the core for Pulsar devices announcing on this
	// network (the multicast beacon). Works even offline (relay-less).
	let lan = $state<LanDevice[]>([]);
	async function refreshLan() {
		try {
			lan = await api.lanDevices();
		} catch {
			/* core not bound yet — keep the last list */
		}
	}
	onMount(() => {
		refreshLan();
		const timer = setInterval(refreshLan, 2000);
		return () => clearInterval(timer);
	});

	function initials(name: string) {
		return name
			.split(' ')
			.map((w) => w[0])
			.slice(0, 2)
			.join('')
			.toUpperCase();
	}
</script>

<section class="lan">
	<div class="lanhdr"><span class="lpulse"></span>{t('devices.lanTitle')}</div>
	{#if lan.length === 0}
		<div class="lanempty">{t('devices.lanScanning')}</div>
	{:else}
		<div class="langrid">
			{#each lan as d (d.addr + '|' + d.id)}
				<div class="device-tile">
					<span class="ravatar lavatar">{initials(d.name)}</span>
					<div class="lmeta">
						<div class="lname">{d.name}</div>
						<div class="lsub mono">{d.id || d.addr}</div>
					</div>
					{#if d.has_id}
						<div class="lactions">
							<button class="btn btn-primary lbtn" onclick={() => onConnect({ name: d.name, id: d.id }, mode)}>
								{t('home.connect')}
							</button>
							<button class="btn btn-ghost lbtn" onclick={() => addPeer(d.name, d.id, 'pc')}>
								{t('devices.lanSave')}
							</button>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</section>

<style>
	/* LAN auto-discovery section */
	.lan {
		margin-top: 22px;
	}
	.lanhdr {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 11px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
		margin-bottom: 12px;
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
	.lanempty {
		font-size: 12.5px;
		color: var(--text-faint);
		padding: 14px 16px;
		border: 1px dashed var(--border);
		border-radius: var(--r-sm);
	}
	.langrid {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		gap: 12px;
	}
	.ravatar {
		width: 30px;
		height: 30px;
		border-radius: 8px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 11px;
		font-family: var(--font-display);
		flex: none;
	}
	.lavatar {
		flex: none;
	}
	.lmeta {
		flex: 1;
		min-width: 0;
	}
	.lname {
		font-size: 14px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.lsub {
		font-size: 11.5px;
		color: var(--text-faint);
		margin-top: 3px;
	}
	.lactions {
		display: flex;
		gap: 6px;
		flex: none;
	}
	.lbtn {
		padding: 7px 12px;
		font-size: 13px;
	}
</style>
