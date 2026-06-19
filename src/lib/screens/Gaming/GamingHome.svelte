<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import Modal from '$lib/Modal.svelte';
	import ConnectModal from './ConnectModal.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api, type LanDevice } from '$lib/api';
	import {
		gameHistoryPeers,
		removeFromGameHistory,
		renameGamePeer,
		setGamePeerImage,
		fmtPeerId,
		normalizeId,
		avatarFor
	} from '$lib/peers.svelte';
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
	// LAN-reachable keys (normalized ids + addresses) for the recents online/offline dot.
	const lanKeys = $derived(
		new Set(
			lan.flatMap((d) => [d.id ? normalizeId(d.id) : '', d.addr, d.addr?.split(':')[0]]).filter(Boolean)
		)
	);
	const isOnline = (id: string) => lanKeys.has(normalizeId(id)) || lanKeys.has(id);
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

	// Recents edit modal: change a recent host's NAME + IMAGE (icon set or an
	// uploaded picture), persisted via renameGamePeer / setGamePeerImage. `editId`
	// holds the recent being edited (null = closed). Mirrors the Devices add/edit
	// form's image handling.
	const ICONS = ['monitor', 'devices', 'gaming', 'keyboard'] as const;
	let editId = $state<string | null>(null);
	let editName = $state('');
	let editImage = $state('icon:monitor');
	let fileInput = $state<HTMLInputElement | null>(null);

	function openEdit(r: { id: string; name: string; image?: string; avatar?: string }) {
		editId = r.id;
		editName = r.name;
		editImage = r.image ?? (r.avatar ? r.avatar : 'icon:monitor');
	}
	function closeEdit() {
		editId = null;
		editName = '';
		editImage = 'icon:monitor';
	}
	function saveEdit() {
		if (!editId) return;
		renameGamePeer(editId, editName);
		setGamePeerImage(editId, editImage);
		closeEdit();
	}

	// Uploaded picture → small cover-cropped data URL (96px) so the persisted store
	// never carries multi-MB originals (same as the Devices add/edit modal).
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
				editImage = c.toDataURL('image/jpeg', 0.82);
			}
			URL.revokeObjectURL(url);
		};
		img.onerror = () => URL.revokeObjectURL(url);
		img.src = url;
	}

	// Display image for a recent: the user-chosen `image` (icon: or data URL) wins,
	// then the pushed identity avatar, else initials.
	const recentImg = (r: { image?: string; avatar?: string }) =>
		r.image && !r.image.startsWith('icon:') ? r.image : r.avatar;
	const recentIcon = (r: { image?: string }) =>
		r.image?.startsWith('icon:') ? r.image.slice(5) : '';
</script>

<div class="ghome">
	<div class="hero">
		<span class="eyebrow mono">{t('gaming.eyebrow')}</span>
		<h1>{t('gaming.title')}</h1>
		<button class="btn btn-primary connect" data-navdefault use:navItem onclick={() => (modalOpen = true)}>
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
								{#if recentImg(r)}
									<img class="rimg" src={recentImg(r)} alt="" />
								{:else if recentIcon(r)}
									<Icon name={recentIcon(r)} size={20} />
								{:else}{initials(r.name)}{/if}
							</span>
							<span class="rmeta">
								<span class="rname">
									<span class="rdot" class:on={isOnline(r.id)} title={isOnline(r.id) ? t('home.online') : t('home.offline')}></span>
									{r.name}
								</span>
								<span class="rid mono">{fmtPeerId(r.id)}</span>
							</span>
						</button>
						<button
							class="redit"
							use:navItem
							title={t('recent.edit')}
							aria-label={t('recent.edit')}
							onclick={() => openEdit(r)}><Icon name="edit" size={14} /></button
						>
						<button
							class="rdel"
							use:navItem
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

{#if editId}
	<Modal title={t('recent.editTitle')} onClose={closeEdit} navModal>
		<div class="editform">
			<span class="fl">{t('recent.name')}</span>
			<div class="field">
				<input bind:value={editName} placeholder={t('recent.namePlaceholder')} aria-label={t('recent.name')} />
			</div>
			<span class="fl">{t('recent.image')}</span>
			<div class="imgrow">
				{#each ICONS as ic (ic)}
					<button
						class="imgopt"
						class:active={editImage === 'icon:' + ic}
						aria-label={ic}
						onclick={() => (editImage = 'icon:' + ic)}><Icon name={ic} size={20} /></button
					>
				{/each}
				<button
					class="imgopt upload"
					class:active={!editImage.startsWith('icon:')}
					aria-label={t('recent.image')}
					onclick={() => fileInput?.click()}
				>
					{#if !editImage.startsWith('icon:')}
						<img class="uimg" src={editImage} alt="" />
					{:else}
						<Icon name="upload" size={18} />
					{/if}
				</button>
				<input type="file" accept="image/*" bind:this={fileInput} onchange={onPickImage} style="display:none" />
				<span class="imghint">{t('recent.imageHint')}</span>
			</div>
			<div class="factions">
				<button class="btn btn-ghost" onclick={closeEdit}>{t('recent.cancel')}</button>
				<button class="btn btn-primary" onclick={saveEdit}>{t('recent.save')}</button>
			</div>
		</div>
	</Modal>
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
		/* Clip the glass-shine sweep to the button's rounded box. */
		position: relative;
		overflow: hidden;
	}
	/* Slow left→right "glass/lightning" highlight gliding across the hero connect
	 * button. Cheap: a single skewed gradient layer animated with `transform`
	 * (translate only — no blur/filter), so it stays light even on software paint. */
	.connect::before {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(
			105deg,
			transparent 30%,
			rgba(255, 255, 255, 0.35) 50%,
			transparent 70%
		);
		transform: translateX(-150%);
		animation: connect-shine 3.5s ease-in-out infinite;
		pointer-events: none;
	}
	@keyframes connect-shine {
		0% {
			transform: translateX(-150%);
		}
		100% {
			transform: translateX(150%);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.connect::before {
			animation: none;
			opacity: 0;
		}
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
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}
	.rdot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-faint);
		flex: none;
	}
	.rdot.on {
		background: var(--ok);
		box-shadow: 0 0 0 2px color-mix(in oklch, var(--ok) 22%, transparent);
	}
	.rid {
		font-size: 11px;
		color: var(--text-faint);
	}
	.redit,
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
		display: grid;
		place-items: center;
	}
	.redit:hover {
		background: var(--accent-soft);
		color: var(--accent);
	}
	.rdel:hover {
		background: var(--accent-soft);
		color: var(--accent);
	}

	/* Recents edit modal form — mirrors the Devices add/edit form. */
	.editform {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.editform .fl {
		display: block;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		margin-top: 8px;
	}
	.editform .factions {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
	.imgrow {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
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
</style>
