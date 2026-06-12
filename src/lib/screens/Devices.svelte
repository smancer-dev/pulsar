<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import Modal from '$lib/Modal.svelte';
	import { savedPeers, addPeer, updatePeer, removePeer, toggleFav, fmtPeerId, normalizeId } from '$lib/peers.svelte';
	import { api, type LanDevice } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = { onConnect: (t: Target, m?: 'remote' | 'game') => void };
	let { onConnect }: Props = $props();

	let q = $state('');

	// add/edit-device form: name + id-or-IP + an image (a built-in icon or an
	// upload). `editing` holds the ORIGINAL id of the device being edited
	// (null = adding a new one); the same modal serves both.
	let adding = $state(false);
	let editing = $state<string | null>(null);
	let newName = $state('');
	let newId = $state('');
	let newImage = $state('icon:monitor');
	let fileInput = $state<HTMLInputElement | null>(null);

	function openEdit(d: { id: string; name: string; image?: string }) {
		editing = d.id;
		newName = d.name;
		newId = fmtPeerId(d.id);
		newImage = d.image ?? 'icon:monitor';
		adding = true;
	}
	function closeForm() {
		adding = false;
		editing = null;
		newName = '';
		newId = '';
		newImage = 'icon:monitor';
	}

	const ICONS = ['monitor', 'devices', 'gaming', 'keyboard'] as const;

	const peers = $derived(savedPeers());
	// Ids are stored canonical (despaced) — despace the query too so typing the
	// grouped form ("641 724…") still matches.
	const filtered = $derived.by(() => {
		const ql = q.trim().toLowerCase();
		const qd = ql.replace(/\s/g, '');
		return peers.filter(
			(d) => !ql || d.name.toLowerCase().includes(ql) || (!!qd && d.id.includes(qd))
		);
	});

	// Online presence: a saved device is "online" when its id (or address) is seen
	// in the LAN discovery beacon list. Poll like the Home LAN section does.
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
		const timer = setInterval(refreshLan, 3000);
		return () => clearInterval(timer);
	});
	const isOnline = (id: string) =>
		lan.some((d) => (d.id && normalizeId(d.id) === id) || d.addr.startsWith(id));

	// Display id input as-is (IPs welcome); a pure 9-digit relay id gets grouped.
	const tidyId = (v: string) => {
		const despaced = v.replace(/\s/g, '');
		return /^\d{1,9}$/.test(despaced)
			? despaced.replace(/(\d{3})(?=\d)/g, '$1 ').trim()
			: v.trim();
	};

	// Uploaded picture → small cover-cropped data URL (96px) so the persisted
	// store never carries multi-MB originals.
	function onPickImage(e: Event) {
		const file = (e.currentTarget as HTMLInputElement).files?.[0];
		(e.currentTarget as HTMLInputElement).value = '';
		if (!file) return;
		const url = URL.createObjectURL(file);
		const img = new Image();
		img.onload = () => {
			const S = 96;
			const c = document.createElement('canvas');
			c.width = S;
			c.height = S;
			const g = c.getContext('2d');
			if (g) {
				const scale = Math.max(S / img.width, S / img.height);
				const w = img.width * scale;
				const h = img.height * scale;
				g.drawImage(img, (S - w) / 2, (S - h) / 2, w, h);
				newImage = c.toDataURL('image/jpeg', 0.82);
			}
			URL.revokeObjectURL(url);
		};
		img.onerror = () => URL.revokeObjectURL(url);
		img.src = url;
	}

	function submitAdd() {
		const id = normalizeId(newId);
		if (!id) return;
		if (editing) {
			updatePeer(editing, { name: newName, newId: id, image: newImage });
		} else {
			addPeer(newName.trim() || t('devices.defaultName'), id, 'pc', newImage);
		}
		closeForm();
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

	function initials(name: string) {
		return name
			.split(' ')
			.map((w) => w[0])
			.slice(0, 2)
			.join('')
			.toUpperCase();
	}
</script>

<div class="head">
	<div><h1>{t('devices.title')}</h1><p class="sub">{t('devices.sub')}</p></div>
	<button class="btn btn-primary" onclick={() => (adding = true)}>
		<Icon name="plus" size={17} />{t('devices.add')}
	</button>
</div>

{#if adding}
	<Modal title={editing ? t('devices.editTitle') : t('devices.add')} onClose={closeForm}>
		<div class="addform">
			<span class="fl">{t('devices.name')}</span>
			<div class="field"><input bind:value={newName} placeholder={t('devices.name')} aria-label={t('devices.name')} /></div>
			<span class="fl">{t('devices.idOrIp')}</span>
			<div class="field">
				<Icon name="connect" size={15} />
				<input
					value={newId}
					oninput={(e) => (newId = tidyId(e.currentTarget.value))}
					placeholder="000 000 000 · 192.168.1.42"
					aria-label={t('devices.idOrIp')}
					style="font-family:var(--font-mono)"
				/>
			</div>
			<span class="fl">{t('devices.image')}</span>
			<div class="imgrow">
				{#each ICONS as ic (ic)}
					<button
						class="imgopt"
						class:active={newImage === 'icon:' + ic}
						aria-label={ic}
						onclick={() => (newImage = 'icon:' + ic)}><Icon name={ic} size={20} /></button
					>
				{/each}
				<button class="imgopt upload" class:active={!newImage.startsWith('icon:')} onclick={() => fileInput?.click()}>
					{#if !newImage.startsWith('icon:')}
						<img class="uimg" src={newImage} alt="" />
					{:else}
						<Icon name="upload" size={18} />
					{/if}
				</button>
				<input type="file" accept="image/*" bind:this={fileInput} onchange={onPickImage} style="display:none" />
				<span class="imghint">{t('devices.imageHint')}</span>
			</div>
			<div class="factions">
				<button class="btn btn-ghost" onclick={closeForm}>{t('devices.cancel')}</button>
				<button class="btn btn-primary" onclick={submitAdd}>{editing ? t('devices.saveBtn') : t('devices.addBtn')}</button>
			</div>
		</div>
	</Modal>
{/if}

<div class="toolbar">
	<div class="field search">
		<Icon name="search" size={16} />
		<input bind:value={q} placeholder={t('devices.search')} aria-label={t('devices.searchAria')} />
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
				<div class="tico">
					{#if d.image && !d.image.startsWith('icon:')}
						<img class="timg" src={d.image} alt="" />
					{:else if d.image?.startsWith('icon:')}
						<Icon name={d.image.slice(5)} size={22} />
					{:else if d.avatar}
						<img class="timg" src={d.avatar} alt="" />
					{:else}
						<span class="tinit">{initials(d.name)}</span>
					{/if}
				</div>
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
					<div class="did mono">{fmtPeerId(d.id)}</div>
					<div class="dstatus">
						<span class="dot" class:on={isOnline(d.id)}></span>
						{isOnline(d.id) ? t('devices.online') : t('devices.offline')} · {relTime(d.lastConnected)}
					</div>
				</div>
				<div class="actions">
					<button class="btn btn-ghost connect" onclick={() => onConnect({ name: d.name, id: d.id })}>
						{t('devices.connect')}
					</button>
					<button class="rm" aria-label={t('devices.edit')} title={t('devices.edit')} onclick={() => openEdit(d)}>
						<Icon name="edit" size={14} />
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
	.imgrow {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.imgopt {
		width: 42px;
		height: 42px;
		border: 1px solid var(--border);
		border-radius: 10px;
		background: var(--surface);
		color: var(--text-muted);
		cursor: pointer;
		display: grid;
		place-items: center;
		overflow: hidden;
		padding: 0;
	}
	.imgopt.active {
		border-color: var(--accent);
		color: var(--accent);
		background: var(--accent-soft);
	}
	.uimg {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}
	.imghint {
		font-size: 11.5px;
		color: var(--text-faint);
		margin-left: 4px;
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
		overflow: hidden;
	}
	.timg {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}
	.tinit {
		font-weight: 700;
		font-size: 13px;
		font-family: var(--font-display);
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
		display: flex;
		align-items: center;
		gap: 5px;
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--border-strong);
		flex: none;
	}
	.dot.on {
		background: var(--ok);
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
