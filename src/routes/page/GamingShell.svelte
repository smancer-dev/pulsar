<script lang="ts">
	// The gaming-mode app shell: a controller-first, immersive client. Owns the single
	// GamepadNav (so focus moves across the centered home, the connect pop-up, and the
	// bottom dock), swaps between the connect home and Settings, and renders the bottom
	// dock in place of the left sidebar. No host UI (devices / connections) — gaming mode
	// is a pure client and this device refuses inbound while it is on.
	import { onMount, onDestroy } from 'svelte';
	import GamingHome from '$lib/screens/Gaming/GamingHome.svelte';
	import GamesScreen from '$lib/screens/Gaming/GamesScreen.svelte';
	import Settings from '$lib/screens/Settings.svelte';
	import BottomDock from './BottomDock.svelte';
	import { gamingNav } from '$lib/gamepadNav.svelte';
	import { api, onGamepadNav, type GameInfo } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import type { Target } from './sessions.svelte';

	type Props = {
		/** Whether the gaming home is the active view (false while a session tab is up) —
		 * gates the controller-nav bridge so it doesn't poll/move focus during a session. */
		active: boolean;
		online: boolean;
		connecting: boolean;
		connError: string;
		fullscreen: boolean;
		onToggleFullscreen: () => void;
		onGoOnline: () => void;
		onConnect: (t: Target, m?: 'remote' | 'game', gameId?: string) => void;
		onAuthDone?: (target: string) => void;
		/** Current split layout ('off' = single-session). */
		splitMode?: 'off' | 'h2' | 'v2' | 'grid4';
		/** Open the split-layout chooser. */
		onSplit?: () => void;
	};
	let {
		active,
		online,
		connecting,
		connError,
		fullscreen,
		onToggleFullscreen,
		onGoOnline,
		onConnect,
		onAuthDone = () => {},
		splitMode = 'off',
		onSplit
	}: Props = $props();

	let gview = $state<'home' | 'settings' | 'games'>('home');

	// Connect flow (NO popups): pick a host on the home → 'games' view shows a connecting/
	// loading state while the host library is fetched, then the games as a full screen.
	let gamesTarget = $state('');
	let hostGames = $state<GameInfo[] | null>(null);
	let gamesLoading = $state(false);
	let gamesErr = $state('');
	// Live host-desktop thumbnail (from the host's built-in `desktop` entry) for the
	// pinned Desktop card.
	let desktopImg = $state('');

	async function openGames(id: string) {
		gamesTarget = id;
		hostGames = null;
		desktopImg = '';
		gamesErr = '';
		gamesLoading = true;
		gview = 'games';
		nav.focusFirst();
		try {
			const fetched = await api.listRemoteGames(id);
			// The host always publishes a built-in "desktop" entry (carrying a fresh live
			// screenshot). Use its image for the pinned Desktop card and drop it from the
			// list so Desktop isn't shown twice.
			desktopImg = fetched.find((g) => g.id === 'desktop')?.image ?? '';
			hostGames = fetched.filter((g) => g.id !== 'desktop');
		} catch (e) {
			gamesErr = e instanceof Error ? e.message : String(e);
		} finally {
			gamesLoading = false;
			onAuthDone(id); // dismiss any password prompt the games-fetch opened
			// GamesScreen self-selects its first card once the list mounts (focusFirst()
			// here would land on the Back button, which registers before the cards).
		}
	}
	function backHome() {
		gview = 'home';
		hostGames = null;
		gamesErr = '';
		gamesTarget = '';
	}
	function playPick(gameId: string) {
		const name = gameId
			? (hostGames?.find((g) => g.id === gameId)?.title ?? gameId)
			: t('gaming.desktop');
		onConnect({ name, id: gamesTarget }, 'game', gameId);
		backHome();
	}

	// The shared gaming nav singleton (so the top-bar chrome buttons can join the same
	// roving focus). Wire this shell's view-specific back/bumper handlers into it.
	const nav = gamingNav;
	nav.setOpts({
		// B / Escape: from games/settings return to the connect home; on the home it's a
		// no-op (leaving gaming mode is the top-bar toggle, not a stray B).
		onBack: () => {
			if (gview !== 'home') backHome();
		},
		// Bumpers cycle the dock destinations.
		onBumper: () => {
			gview = gview === 'home' ? 'settings' : 'home';
		}
	});

	// F11 toggles fullscreen (the top bar — and its gaming toggle — is hidden while
	// fullscreen, so the dock's button is the other way out).
	function onKey(e: KeyboardEvent) {
		if (e.key === 'F11') {
			e.preventDefault();
			onToggleFullscreen();
		}
	}

	// The gilrs→webview controller-nav bridge: the ONLY pad-nav input on Linux (WebKitGTK
	// has no Gamepad API) and the preferred one everywhere. Feed each snapshot into the nav.
	let unlistenNav: (() => void) | null = null;
	let dead = false;

	onMount(() => {
		window.addEventListener('keydown', onKey);
		onGamepadNav((s) => nav.ingestBridge(s)).then((off) => { if (dead) off(); else unlistenNav = off; });
	});
	onDestroy(() => {
		dead = true;
		nav.stop();
		api.gamepadNavStop().catch(() => {});
		window.removeEventListener('keydown', onKey);
		unlistenNav?.();
	});

	// Start/stop the pad nav + bridge with visibility: don't poll gilrs or move focus on
	// the (hidden) home while a session tab is active.
	$effect(() => {
		if (active) {
			nav.start();
			api.gamepadNavStart().catch(() => {});
		} else {
			nav.stop();
			api.gamepadNavStop().catch(() => {});
		}
	});
</script>

<div class="gshell">
	<div class="gstage">
		{#if gview === 'games'}
			<GamesScreen
				{nav}
				target={gamesTarget}
				games={hostGames}
				{desktopImg}
				loading={gamesLoading}
				err={gamesErr}
				onPlay={playPick}
				onBack={backHome}
			/>
		{:else if gview === 'settings'}
			<div class="settings-wrap">
				<Settings onReconnect={onGoOnline} padNav />
			</div>
		{:else}
			<GamingHome {nav} onPickHost={openGames} />
		{/if}
	</div>

	<BottomDock
		{gview}
		{nav}
		navItem={nav.item}
		{online}
		{connecting}
		{connError}
		{fullscreen}
		{onToggleFullscreen}
		{onGoOnline}
		{splitMode}
		{onSplit}
		onView={(v) => (gview = v)}
	/>
</div>

<style>
	.gshell {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		width: 100%;
		/* Positioned ancestor so the controllers popup (Modal, position:absolute inset:0)
		   covers the whole gaming shell instead of escaping to the document. */
		position: relative;
		/* A cooler, more immersive backdrop than the neutral remote shell. */
		background:
			radial-gradient(120% 80% at 50% -10%, var(--accent-soft) 0%, transparent 60%),
			var(--bg-tint);
	}
	.gstage {
		flex: 1;
		min-height: 0;
		display: flex;
	}
	.settings-wrap {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 24px;
		background: var(--bg);
	}
</style>
