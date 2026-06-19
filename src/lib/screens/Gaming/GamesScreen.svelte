<script lang="ts">
	// Gaming-mode host games — a full IN-STAGE screen (NOT a popup). Shown after picking a
	// host on the home: a connecting/loading state while the host's library is fetched, then
	// the games as big cards (Desktop pinned first). Back (button / B / Esc) returns home.
	import { tick } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { type GameInfo } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import { fmtPeerId } from '$lib/peers.svelte';
	import type { GamepadNav } from '$lib/gamepadNav.svelte';

	type Props = {
		nav: GamepadNav;
		/** Host id being connected to (for the header). */
		target: string;
		/** Fetched games, or null while loading / on error. */
		games: GameInfo[] | null;
		/** Live host-desktop thumbnail (data URL) for the pinned Desktop card, if any. */
		desktopImg?: string;
		loading: boolean;
		err: string;
		/** Launch: '' = whole Desktop, else the game id. */
		onPlay: (gameId: string) => void;
		onBack: () => void;
	};
	let { nav, target, games, desktopImg = '', loading, err, onPlay, onBack }: Props = $props();
	const navItem = nav.item;

	// Cover image for a fetched game: the host-sent cover, else the bundled Steam logo for
	// the Steam default, else none (the card shows initials).
	function coverFor(g: GameInfo): string {
		if (g.image) return g.image;
		if (g.id === 'steam' || g.title.toLowerCase() === 'steam') return '/steam.svg';
		return '';
	}

	// The first game card (the always-present "Desktop" card). Once the list has loaded
	// we select it by default so controller nav starts ON a game — not parked on the Back
	// button (which registers first) or the bottom dock. Without this the user had to
	// d-pad up from the bottom to reach the first game.
	let firstCard = $state<HTMLButtonElement>();
	let didAutofocus = false;
	$effect(() => {
		// Track loading/err/games so this re-runs as the fetch resolves.
		const ready = !loading && !err;
		void games;
		if (loading) {
			didAutofocus = false; // re-arm for the next fetch
			return;
		}
		if (!ready || didAutofocus) return;
		didAutofocus = true;
		// Wait for the cards to mount + lay out, then focus the first one. nav.focus()
		// validates registration + visibility, so a not-yet-painted node is a no-op.
		tick().then(() => {
			if (firstCard) nav.focus(firstCard);
		});
	});

	function initials(name: string) {
		return name.split(' ').map((w) => w[0]).slice(0, 2).join('').toUpperCase();
	}
</script>

<div class="gscreen">
	<div class="ghead">
		<button class="back" use:navItem onclick={onBack} aria-label={t('gaming.back')}>
			<Icon name="arrowRight" size={16} class="flip" />
		</button>
		<span class="htitle mono">{fmtPeerId(target)}</span>
	</div>

	{#if loading}
		<div class="state">
			<span class="spinner"></span>
			<span>{t('home.fetching')}</span>
		</div>
	{:else if err}
		<div class="state err">{err}</div>
		<button class="btn" use:navItem onclick={onBack}>{t('gaming.back')}</button>
	{:else}
		<div class="games">
			<button class="gcard" use:navItem bind:this={firstCard} onclick={() => onPlay('')}>
				<span class="gicon" class:img={!!desktopImg}>
					{#if desktopImg}<img src={desktopImg} alt="" />{:else}<Icon name="monitor" size={20} />{/if}
				</span>
				<span class="gmeta"><span class="gname">{t('gaming.desktop')}</span><span class="gkind mono">{t('gaming.wholeScreen')}</span></span>
				<Icon name="gaming" size={16} class="push" />
			</button>
			{#if games && games.length}
				{#each games as g (g.id)}
					{@const cover = coverFor(g)}
					<button class="gcard" use:navItem onclick={() => onPlay(g.id)}>
						<span class="gicon" class:img={!!cover}>
							{#if cover}<img src={cover} alt="" />{:else}{initials(g.title)}{/if}
						</span>
						<span class="gmeta"><span class="gname">{g.title}</span><span class="gkind mono">{g.kind}</span></span>
						<Icon name="gaming" size={16} class="push" />
					</button>
				{/each}
			{:else}
				<div class="hint">{t('home.noHostGames')}</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.gscreen {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		align-items: center;
		padding: 32px 24px 24px;
		gap: 20px;
	}
	.ghead {
		width: 100%;
		max-width: 480px;
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.back {
		width: 36px;
		height: 36px;
		display: grid;
		place-items: center;
		border: 1px solid var(--border);
		border-radius: 9px;
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		flex: none;
	}
	.back:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.back :global(.flip) {
		transform: rotate(180deg);
	}
	.htitle {
		font-size: 17px;
		font-weight: 600;
		letter-spacing: 0.04em;
	}
	.state {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 14px;
		color: var(--text-muted);
		padding: 40px 0;
	}
	.state.err {
		color: var(--danger);
		word-break: break-word;
		text-align: center;
		max-width: 480px;
	}
	.spinner {
		width: 18px;
		height: 18px;
		border: 2px solid var(--border-strong);
		border-top-color: var(--accent);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	.games {
		width: 100%;
		max-width: 480px;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.hint {
		font-size: 12.5px;
		color: var(--text-faint);
		text-align: center;
		padding: 12px;
	}
	.gcard {
		display: flex;
		align-items: center;
		gap: 12px;
		width: 100%;
		padding: 13px 15px;
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r);
		cursor: pointer;
		text-align: left;
		color: var(--text);
	}
	.gcard:hover {
		background: var(--surface-3);
	}
	.gicon {
		width: 40px;
		height: 40px;
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
	/* Cover-image variant: a 16:9 thumbnail (host desktop screenshot / Steam logo / cover). */
	.gicon.img {
		width: 64px;
		background: var(--surface-3);
	}
	.gicon img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.gmeta {
		display: flex;
		flex-direction: column;
		line-height: 1.3;
		min-width: 0;
	}
	.gname {
		font-size: 15px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.gkind {
		font-size: 11px;
		color: var(--text-faint);
	}
	.gcard :global(.push) {
		margin-left: auto;
		color: var(--text-faint);
	}
</style>
