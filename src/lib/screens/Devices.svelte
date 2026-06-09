<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import Modal from '$lib/Modal.svelte';
	import { savedPeers, addPeer, removePeer, toggleFav, type PeerCategory } from '$lib/peers.svelte';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = { onConnect: (t: Target, m?: 'remote' | 'game') => void };
	let { onConnect }: Props = $props();

	let q = $state('');
	let filter = $state<'all' | PeerCategory>('all');

	// add-device form
	let adding = $state(false);
	let newName = $state('');
	let newId = $state('');
	let newCat = $state<PeerCategory>('pc');

	// Full status label ("Oyun PC’si") vs. short segment label ("Oyun").
	const catFull = (c: PeerCategory) => t('cat.' + c);
	const catShort = (c: PeerCategory) => (c === 'console' ? t('cat.consoleShort') : t('cat.' + c));
	const catIcon: Record<PeerCategory, string> = {
		pc: 'monitor',
		server: 'devices',
		console: 'gaming'
	};
	const CATS: PeerCategory[] = ['pc', 'server', 'console'];
	const FILTERS = ['all', 'pc', 'server', 'console'] as const;

	const peers = $derived(savedPeers());
	const filtered = $derived(
		peers.filter(
			(d) =>
				(filter === 'all' || d.cat === filter) &&
				(d.name.toLowerCase().includes(q.toLowerCase()) || d.id.includes(q))
		)
	);

	const fmtId = (v: string) =>
		v
			.replace(/\D/g, '')
			.slice(0, 9)
			.replace(/(\d{3})(?=\d)/g, '$1 ')
			.trim();

	function submitAdd() {
		const id = fmtId(newId);
		if (id.replace(/\D/g, '').length < 6) return;
		addPeer(newName.trim() || t('devices.defaultName'), id, newCat);
		newName = '';
		newId = '';
		newCat = 'pc';
		adding = false;
	}

	function relTime(ms: number | null): string {
		if (ms == null) return t('devices.never');
		const m = Math.floor((Date.now() - ms) / 60000);
		if (m < 1) return t('devices.justNow');
		if (m < 60) return t('devices.minAgo', { n: m });
		const h = Math.floor(m / 60);
		if (h < 24) return t('devices.hourAgo', { n: h });
		return t('devices.dayAgo', { n: Math.floor(h / 24) });
	}
</script>

<div class="head">
	<div><h1>{t('devices.title')}</h1><p class="sub">{t('devices.sub')}</p></div>
	<button class="btn btn-primary" onclick={() => (adding = true)}>
		<Icon name="plus" size={17} />{t('devices.add')}
	</button>
</div>

{#if adding}
	<Modal title={t('devices.add')} onClose={() => (adding = false)}>
		<div class="addform">
			<span class="fl">{t('devices.name')}</span>
			<div class="field"><input bind:value={newName} placeholder={t('devices.name')} aria-label={t('devices.name')} /></div>
			<span class="fl">{t('devices.id')}</span>
			<div class="field">
				<Icon name="connect" size={15} />
				<input
					value={newId}
					oninput={(e) => (newId = fmtId(e.currentTarget.value))}
					placeholder="000 000 000"
					inputmode="numeric"
					aria-label={t('devices.id')}
					style="font-family:var(--font-mono)"
				/>
			</div>
			<span class="fl">{t('devices.type')}</span>
			<div class="seg">
				{#each CATS as v (v)}
					<button class:active={newCat === v} onclick={() => (newCat = v)}>{catShort(v)}</button>
				{/each}
			</div>
			<div class="factions">
				<button class="btn btn-ghost" onclick={() => (adding = false)}>{t('devices.cancel')}</button>
				<button class="btn btn-primary" onclick={submitAdd}>{t('devices.addBtn')}</button>
			</div>
		</div>
	</Modal>
{/if}

<div class="toolbar">
	<div class="field search">
		<Icon name="search" size={16} />
		<input bind:value={q} placeholder={t('devices.search')} aria-label={t('devices.searchAria')} />
	</div>
	<div class="seg">
		{#each FILTERS as v (v)}
			<button class:active={filter === v} onclick={() => (filter = v)}>{v === 'all' ? t('filter.all') : catShort(v)}</button>
		{/each}
	</div>
</div>

{#if peers.length === 0}
	<div class="empty card">
		<Icon name="devices" size={28} />
		<div class="et">{t('devices.empty')}</div>
		<!-- eslint-disable-next-line svelte/no-at-html-tags -->
		<p>{@html t('devices.emptyBody')}</p>
	</div>
{:else}
	<div class="grid">
		{#each filtered as d (d.id)}
			<div class="device-tile">
				<div class="tico"><Icon name={catIcon[d.cat]} size={22} /></div>
				<div class="meta">
					<div class="nrow">
						<button
							class="favbtn"
							class:on={d.fav}
							aria-label={t('devices.fav')}
							onclick={() => toggleFav(d.id)}><Icon name="star" size={13} /></button
						>
						<span class="dname">{d.name}</span>
					</div>
					<div class="did mono">{d.id}</div>
					<div class="dstatus">{relTime(d.lastConnected) + ' · ' + catFull(d.cat)}</div>
				</div>
				<div class="actions">
					<button
						class="btn btn-ghost connect"
						onclick={() => onConnect({ name: d.name, id: d.id }, d.cat === 'console' ? 'game' : 'remote')}
					>
						{d.cat === 'console' ? t('devices.play') : t('devices.connect')}
					</button>
					<button class="rm" aria-label={t('devices.remove')} onclick={() => removePeer(d.id)}>
						<Icon name="x" size={15} />
					</button>
				</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	.head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		margin-bottom: 28px;
	}
	h1 {
		font-size: 27px;
		letter-spacing: -0.03em;
	}
	.sub {
		color: var(--text-muted);
		font-size: 14.5px;
		margin: 7px 0 0;
	}
	.addform {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.addform .fl {
		display: block;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		margin-top: 8px;
	}
	.addform .factions {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
	.toolbar {
		display: flex;
		gap: 12px;
		margin-bottom: 18px;
		align-items: center;
	}
	.search {
		flex: 1;
		max-width: 340px;
	}
	.empty {
		text-align: center;
		color: var(--text-faint);
		padding: 40px 24px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
	}
	.empty .et {
		font-family: var(--font-display);
		font-size: 17px;
		color: var(--text);
	}
	.empty p {
		font-size: 13.5px;
		max-width: 40ch;
		line-height: 1.5;
		margin: 0;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		gap: 14px;
	}
	.tico {
		width: 44px;
		height: 44px;
		border-radius: 11px;
		flex: none;
		display: grid;
		place-items: center;
		background: var(--accent-soft);
		color: var(--accent);
	}
	.meta {
		flex: 1;
		min-width: 0;
	}
	.nrow {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.favbtn {
		border: none;
		background: transparent;
		color: var(--border-strong);
		cursor: pointer;
		padding: 0;
		display: grid;
		place-items: center;
	}
	.favbtn.on {
		color: var(--warn);
	}
	.dname {
		font-size: 15px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.did {
		font-size: 11.5px;
		color: var(--text-faint);
		margin-top: 3px;
	}
	.dstatus {
		font-size: 11.5px;
		color: var(--text-faint);
		margin-top: 5px;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: 6px;
		flex: none;
	}
	.connect {
		padding: 8px 14px;
		font-size: 13.5px;
	}
	.rm {
		width: 30px;
		height: 30px;
		border: 1px solid var(--border);
		background: var(--surface);
		border-radius: var(--r-sm);
		color: var(--text-faint);
		cursor: pointer;
		display: grid;
		place-items: center;
	}
	.rm:hover {
		color: var(--danger);
		border-color: var(--danger);
	}
</style>
