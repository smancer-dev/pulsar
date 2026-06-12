<script lang="ts">
	import { onMount } from 'svelte';
	import PulsarMark from '$lib/PulsarMark.svelte';
	import {
		api,
		isTauri,
		onSessionEvent,
		onAuthPrompt,
		onReverseRequest,
		onFullscreenToggle,
		onLocalCaps,
		onPeerAvatar,
		onPeerName
	} from '$lib/api';
	import { setPeerIdentity } from '$lib/peers.svelte';
	import { gameStore } from '$lib/games.svelte';
	import { ui, saveTick } from '$lib/settings.svelte';
	import { initCaps } from '$lib/caps.svelte';
	import { t, i18n } from '$lib/i18n.svelte';
	import { theme, toggleTheme } from '$lib/theme.svelte';
	import type { Config } from '$lib/types';
	import Connecting from '$lib/screens/Connecting.svelte';
	import SessionView from '$lib/screens/Session.svelte';
	import Approve from '$lib/screens/Approve.svelte';
	import Connections from '$lib/screens/Connections.svelte';
	import HostChat from '$lib/screens/HostChat.svelte';
	import Chrome from './page/Chrome.svelte';
	import Tabs from './page/Tabs.svelte';
	import HomeView from './page/HomeView.svelte';
	import PasswordModal from './page/PasswordModal.svelte';
	import { SessionManager } from './page/sessions.svelte';

	// When opened as the Allow/Deny approval popup (a separate window), render only
	// that prompt and skip all the main-app logic (don't re-register with the relay).
	const approveReq = (() => {
		if (typeof window === 'undefined') return null;
		// Primary: injected by the Rust window builder before page load.
		const inj = (window as unknown as { __APPROVE__?: { id: number; peer: string; pw: string } })
			.__APPROVE__;
		if (inj) return { id: Number(inj.id), peer: String(inj.peer ?? ''), pw: String(inj.pw ?? 'none') };
		// Fallback: query string.
		const p = new URLSearchParams(location.search);
		if (!p.has('approve')) return null;
		return { id: Number(p.get('approve')), peer: p.get('peer') ?? '', pw: p.get('pw') ?? 'none' };
	})();

	// When opened as the dedicated connections-management window (a separate window,
	// like the approval popup), render only the connections list and skip the main app.
	const connReq =
		typeof window !== 'undefined' &&
		!!(window as unknown as { __CONNECTIONS__?: boolean }).__CONNECTIONS__;

	// Either popup window short-circuits the main-app bootstrap (no relay re-register).
	const isPopup = !!approveReq || connReq;

	type View = 'home' | 'devices' | 'gaming' | 'settings';

	const NAV: { id: View; icon: string }[] = [
		{ id: 'home', icon: 'connect' },
		{ id: 'devices', icon: 'devices' },
		{ id: 'gaming', icon: 'gaming' },
		{ id: 'settings', icon: 'settings' }
	];

	let view = $state<View>('home');
	let mode = $state<'remote' | 'game'>('remote');
	let selfId = $state('—');
	let selfPw = $state('');
	let online = $state(false);
	let config = $state<Config | null>(null);
	let connecting = $state(false);
	let connError = $state('');
	// Multiple concurrent host connections (tabs), the active tab, fullscreen, and the
	// connect/disconnect lifecycle live in the session manager.
	const sm = new SessionManager({ getMode: () => mode, onAuthDone: (target) => closePwFor(target) });
	// Client-side password prompts — driven by the host's `auth-prompt` event, so they
	// appear at the same time as the host's Allow/Deny popup. Replying with the password
	// OR the host clicking Allow (whichever first) completes the connect. Multiple tabs
	// can be connecting at once, so these are a FIFO queue (one modal shown at a time):
	// a second host's prompt no longer overwrites and drops the first.
	let pwQueue = $state<{ req: number; peer: string }[]>([]);
	const pwPrompt = $derived(pwQueue[0] ?? null);
	let pwInput = $state('');
	let pwError = $state('');
	let pwChecking = $state(false);

	function submitPw() {
		if (!pwPrompt) return;
		api.submitPassword(pwPrompt.req, pwInput).catch(() => {});
		pwChecking = true;
		pwError = '';
	}
	function cancelPw() {
		if (pwPrompt) api.submitPassword(pwPrompt.req, null).catch(() => {});
		closePw();
	}
	// Dequeue the current (head) prompt and re-arm the inputs for the next one, if any.
	function closePw() {
		pwQueue = pwQueue.slice(1);
		pwInput = '';
		pwError = '';
		pwChecking = false;
	}
	// A finished connect dismisses only ITS OWN queued prompt: with concurrent tabs,
	// dequeuing the head would remove the prompt the user is typing into whenever an
	// UNRELATED connect settles first — that prompt's req would never be answered and
	// its tab would hang on Connecting. Matched on the despaced id; the prompt's peer
	// may carry a resolved `ip:port` while the target was typed as a bare ip.
	function closePwFor(targetId: string) {
		const want = targetId.replace(/\s/g, '');
		const i = pwQueue.findIndex((q) => {
			const p = q.peer.replace(/\s/g, '');
			return p === want || p.startsWith(want + ':') || want.startsWith(p + ':');
		});
		if (i < 0) return;
		pwQueue = pwQueue.filter((_, j) => j !== i);
		// The visible head was removed → re-arm the inputs for the next prompt (if any).
		if (i === 0) {
			pwInput = '';
			pwError = '';
			pwChecking = false;
		}
	}
	// Host-side activity: who's connected + a recent event log.
	let hostSessions = $state<{ peer: string; since: number }[]>([]);
	let activity = $state<string[]>([]);

	// Bind + register with the configured relay. Re-runnable: called on startup,
	// on manual retry, and whenever the relay/network settings change.
	async function goOnline() {
		connecting = true;
		connError = '';
		try {
			config = await api.getConfig();
			selfId = await api.goOnline();
			selfPw = await api.sessionPassword();
			online = true;
		} catch (e) {
			online = false;
			selfId = '—';
			selfPw = '';
			const msg = e instanceof Error ? e.message : String(e);
			// The sidebar shows this as the offline tooltip; the auto-retry effect below
			// keeps trying, so say so instead of presenting a dead end.
			connError = isTauri ? `${msg} — otomatik olarak yeniden denenecek` : msg;
		} finally {
			connecting = false;
		}
	}

	// Keep the shell's config copy in sync with Settings saves: Settings persists via
	// the core into ITS OWN config snapshot, and this copy (driving the Home screen's
	// unattended-access warning + blanked one-time password) otherwise refreshed only
	// inside goOnline() — a toggle in Settings didn't reach Home until a reconnect.
	// `saveTick` bumps on every save (core + ui-only); re-fetch on it.
	let lastSaveTick = saveTick.n;
	$effect(() => {
		if (saveTick.n === lastSaveTick || isPopup || !isTauri) return;
		lastSaveTick = saveTick.n;
		api.getConfig().then((c) => (config = c)).catch(() => {});
	});

	// Unattended hosts must come online without a human: a machine that boots before
	// its network is up (or loses the relay) would otherwise stay offline until someone
	// clicks "Çevrimiçi ol". While offline and not mid-attempt, retry go_online on a
	// capped backoff (3s → 12s); each attempt surfaces via the normal connecting state.
	let retryDelay = 0;
	$effect(() => {
		if (isPopup || !isTauri || online || connecting) {
			if (online) retryDelay = 0; // a real success re-arms the fresh backoff
			return;
		}
		retryDelay = retryDelay > 0 ? Math.min(12_000, retryDelay * 2) : 3000;
		const tmr = setTimeout(goOnline, retryDelay);
		return () => clearTimeout(tmr);
	});

	// Roll a fresh one-time password (host side).
	async function refreshPw() {
		if (!online) return; // no relay session → nothing to authenticate, don't mint a password
		try {
			selfPw = await api.newPassword();
		} catch {
			/* keep the current password */
		}
	}

	// Launch splash: the window starts hidden (tauri.conf `visible:false`); we paint the
	// branded splash, REVEAL the window onto it (never an empty frame), and only fade to
	// the UI after the splash has been shown for a visible moment. The fade timer starts
	// AFTER the window is actually shown (not at mount) so the splash is never skipped by
	// a slow `show()`.
	let booting = $state(true);
	let splashOn = $state(true);
	// Resolves once the launch splash has fully faded out. The CLI `--connect` auto-connect
	// awaits this before kicking off the actual network connect, so the Connecting screen
	// (with the real P2P/relay milestone) is shown AFTER the splash instead of a fast LAN
	// connect finishing behind the splash and jumping straight to video.
	let splashGone: () => void = () => {};
	const splashDone = new Promise<void>((resolve) => (splashGone = resolve));
	// Startup capability probe (encoders+decoders, re-run every launch): the splash
	// holds until it lands so the UI never shows un-gated options. Safety-capped in
	// boot() so a hung probe can't block startup.
	const capsReady = new Promise<void>((resolve) => {
		if (!isTauri) return resolve();
		onLocalCaps(() => resolve());
		api.localCaps()
			.then((c) => {
				if (c) resolve();
			})
			.catch(() => {});
	});
	// CLI --connect auto-connect: password to auto-submit if the host asks for one.
	let autoPw = '';
	const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
	const nextPaint = () =>
		new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));

	async function boot() {
		if (typeof window === 'undefined') {
			splashGone();
			return;
		}
		await nextPaint(); // ensure the splash is on screen before the window appears
		if (isTauri) {
			try {
				const { getCurrentWindow } = await import('@tauri-apps/api/window');
				const w = getCurrentWindow();
				await w.show();
				await w.setFocus();
			} catch {
				/* show failed — the Rust fallback reveals the window; carry on */
			}
		}
		if (isPopup) {
			// The popup windows (approve / connections) have no splash.
			booting = false;
			splashOn = false;
			splashGone();
			return;
		}
		// Window is up now → keep the splash visible a beat AND until the startup
		// capability probe lands (≤5 s cap), then cross-fade to the UI.
		await Promise.all([wait(1300), Promise.race([capsReady, wait(5000)])]);
		booting = false; // start the opacity fade
		await wait(500);
		splashOn = false; // unmount once faded
		splashGone(); // release the held --connect auto-connect (Connecting screen now visible)
	}

	onMount(async () => {
		initCaps();
		boot();
		if (isPopup) return; // approval popup: nothing else to bootstrap
		await goOnline();
		// Surface incoming connections on the host UI.
		await onSessionEvent((e) => {
			if (e.kind === 'request') {
				activity = [t('activity.wants', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'connected') {
				if (!hostSessions.some((s) => s.peer === e.peer))
					hostSessions = [...hostSessions, { peer: e.peer, since: Date.now() }];
				activity = [t('activity.connected', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'disconnected') {
				hostSessions = hostSessions.filter((s) => s.peer !== e.peer);
				activity = [t('activity.left', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'rejected') {
				activity = [t('activity.rejected', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'launch') {
				activity = [t('activity.launch', { peer: e.peer, detail: e.detail }), ...activity].slice(0, 8);
			} else if (e.kind === 'stream') {
				activity = [t('activity.stream', { peer: e.peer, detail: e.detail }), ...activity].slice(0, 8);
			}
		});
		// Host role: a connecting CLIENT pushed its identity (name/avatar, peer-keyed
		// by its device id) — cache it so recents / LAN / devices show who it is.
		// Client-side pushes use a play id ("3") as the peer key; only real device
		// ids (9 digits / an address) belong in the cache.
		const isDeviceId = (p: string) => /^\d{9}$/.test(p.replace(/\s/g, '')) || /[.:]/.test(p);
		await onPeerAvatar((e) => {
			if (isDeviceId(e.peer)) setPeerIdentity(e.peer, { avatar: e.dataUrl });
		});
		await onPeerName((e) => {
			if (isDeviceId(e.peer)) setPeerIdentity(e.peer, { name: e.name });
		});
		// A controlled client asked to reverse direction — connect back to it so the
		// roles swap (it must be online/serving for this to land).
		await onReverseRequest((e) => {
			if (e.id) sm.startConnect({ name: t('home.remoteDevice'), id: e.id }, 'remote');
		});
		// Client (Linux native): Ctrl+Shift+F12 (evdev-captured, so it never reaches the
		// webview as a keydown) — toggle the window's fullscreen state.
		await onFullscreenToggle(() => sm.toggleFullscreen());
		// A host is asking us for a password — show the prompt (a re-fire means the
		// previous password was wrong).
		await onAuthPrompt((e) => {
			// Auto-connect: answer the host's password prompt without UI.
			if (autoPw) {
				api.submitPassword(e.req, autoPw).catch(() => {});
				return;
			}
			// A re-fire for the connection currently being checked (same peer, fresh req) means
			// the previous password was wrong — replace the head's req in place and flag the error.
			if (pwPrompt && pwChecking && pwPrompt.peer === e.peer) {
				pwQueue = [{ req: e.req, peer: e.peer }, ...pwQueue.slice(1)];
				pwError = t('pw.error');
				pwChecking = false;
				return;
			}
			// Otherwise it's a (possibly concurrent) new connection's prompt — queue it. If it
			// becomes the visible head, arm the inputs for it.
			const wasEmpty = pwQueue.length === 0;
			pwQueue = [...pwQueue, { req: e.req, peer: e.peer }];
			if (wasEmpty) {
				pwInput = '';
				pwChecking = false;
				pwError = '';
			}
		});
		// CLI `--connect <id|ip>`: initiate a session on startup (kiosk / automated test).
		// `--mode game` + `--app <name|id>` start a game session; an empty/Desktop app in
		// game mode streams the whole desktop (host's tolerant on_launch match launches
		// nothing). Remote mode always carries an empty gameId.
		const ac = await api.autoConnectTarget().catch(() => null);
		if (ac && ac.id) {
			autoPw = ac.pw ?? '';
			const m = ac.mode === 'game' ? 'game' : 'remote';
			const gameId = m === 'game' ? (ac.app || '') : '';
			// Headless --connect: show the splash, THEN the Connecting screen. The session is
			// created now (so the splash fades onto the Connecting screen, not the home view),
			// but `holdConnecting` defers the real network connect until the splash is gone — so
			// the P2P/relay milestone is on-screen instead of a fast connect finishing unseen.
			sm.startConnect({ name: ac.app || t('home.remoteDevice'), id: ac.id }, m, gameId, {
				holdConnecting: splashDone
			});
			// Kiosk / headless start (CLI --connect): begin fullscreen so the host fills
			// the screen with no app chrome. Toggle off with Ctrl+Shift+F12.
			if (!sm.fullscreen) sm.toggleFullscreen();
		}
	});

	// Theme lives in the shared `theme` store (persisted + cross-window via a storage
	// event), so every window — main and the approval/connections popups — reflects
	// the current theme and follows live toggles. Reading `theme.dark` here makes this
	// re-apply whenever any window changes it.
	$effect(() => {
		document.documentElement.setAttribute('data-theme', theme.dark ? 'dark' : 'light');
	});

	// Keep <html lang> in sync so CSS text-transform uses the right casing rules
	// (Turkish lowercase i uppercases to a dotted İ — wrong for English copy).
	$effect(() => {
		document.documentElement.lang = i18n.lang;
	});

	// Native-video occlusion: when the active tab is a native session whose child window
	// is actually painting over the webview (`play-ready` landed), the entire webview is
	// hidden behind it — so every decorative CSS animation (Connect pulse rings, LAN radar,
	// session veils, spinners) is just wasted repaint work (≈0.5 core of software rendering
	// on the Pi). Flag the root with `data-occluded` so app.css pauses those animations.
	// This ONLY suspends visual repaint: the process, all event handling, tab switching,
	// and the active-session input rAF pump keep running. Cleared the instant the active
	// tab leaves the native session (home/another tab) or the session ends.
	$effect(() => {
		const s = sm.sessions.find((x) => x.tabId === sm.activeTab);
		const occluded = !!s && !!s.native && s.phase === 'active' && !!s.ready;
		document.documentElement.toggleAttribute('data-occluded', occluded);
	});

	// Give the whole app a gaming look (cyan accent) while a game-stream session is
	// the active tab; revert as soon as it's left.
	$effect(() => {
		const s = sm.sessions.find((x) => x.tabId === sm.activeTab);
		document.documentElement.toggleAttribute('data-gaming', !!s && s.mode === 'game');
	});

	// Suppress the webview's native right-click menu (the browser-like context menu
	// looks out of place in a desktop app), but keep it on editable fields so the
	// user can still cut/copy/paste in inputs.
	$effect(() => {
		const onCtx = (e: MouseEvent) => {
			const el = e.target as HTMLElement | null;
			if (el && (el.isContentEditable || el.closest('input, textarea'))) return;
			e.preventDefault();
		};
		document.addEventListener('contextmenu', onCtx);
		return () => document.removeEventListener('contextmenu', onCtx);
	});

	// Host: kick a connected client.
	function kickPeer(peer: string) {
		api.disconnectPeer(peer).catch(() => {});
		hostSessions = hostSessions.filter((s) => s.peer !== peer);
	}

	// Keep the core's published game list in sync so connecting clients can see it.
	$effect(() => {
		if (isPopup) return;
		api.publishGames($state.snapshot(gameStore.games)).catch(() => {});
	});

	// Keep the core's host stream settings in sync (resolution/fps/bitrate/encoder).
	$effect(() => {
		if (isPopup) return;
		const res = gameStore.host.resolution;
		const [width, height] = res === '4K' ? [3840, 2160] : res === '1080p' ? [1920, 1080] : [2560, 1440];
		api
			.setStreamSettings({
				width,
				height,
				fps: gameStore.host.fps,
				bitrate_kbps: gameStore.host.bitrate * 1000,
				encoder: ui.encoder
			})
			.catch(() => {});
	});
</script>

{#if approveReq}
	<Approve id={approveReq.id} peer={approveReq.peer} pw={approveReq.pw} />
{:else if connReq}
	<Connections />
{:else}
	<HostChat />
	<div class="desktop">
	<div class="window" class:fullscreen={sm.fullscreen}>
		{#if splashOn}
			<div class="splash" class:gone={!booting} aria-hidden="true">
				<div class="splash-mark"><PulsarMark size={76} /></div>
				<div class="splash-word">Pulsar</div>
			</div>
		{/if}
		{#if !sm.fullscreen}
			<Chrome
				title={sm.activeTab === 'home' ? t('nav.' + view) : ''}
				dark={theme.dark}
				onToggleTheme={toggleTheme}
			/>
			{#if sm.sessions.length}
				<Tabs
					sessions={sm.sessions}
					activeTab={sm.activeTab}
					onSelect={(tab) => (sm.activeTab = tab)}
					onEnd={sm.endSession}
					onRename={sm.renameTab}
				/>
			{/if}
		{/if}

		<div class="stage">
		<div class="layer" class:hidden={sm.activeTab !== 'home'}>
			<HomeView
				nav={NAV}
				{view}
				{mode}
				{selfId}
				{selfPw}
				{online}
				{connecting}
				{connError}
				unattended={config?.unattended_access ?? false}
				connectErr={sm.connectErr}
				{hostSessions}
				{activity}
				onView={(v) => (view = v)}
				onGoOnline={goOnline}
				onMode={(m) => (mode = m)}
				onRefreshPw={refreshPw}
				onDisconnect={kickPeer}
				onConnect={sm.startConnect}
				onStream={sm.startHostSession}
				onClearConnectErr={() => (sm.connectErr = '')}
				onAuthDone={closePwFor}
			/>
		</div>

		{#each sm.sessions as s (s.tabId)}
			<div class="layer" class:hidden={sm.activeTab !== s.tabId}>
				{#if s.phase === 'connecting'}
					<Connecting
						target={s.target}
						mode={s.mode}
						awaitingApproval={!!pwPrompt}
						preparing={s.playId >= 0}
						onCancel={() => sm.endSession(s.tabId)}
					/>
				{:else}
					<SessionView
						playId={s.playId}
						target={s.target}
						mode={s.mode}
						conn={s.conn}
						wsPort={s.wsPort}
						audioWsPort={s.audioWsPort ?? 0}
						native={s.native ?? false}
						embedded={s.embedded ?? false}
						hostCodecs={s.hostCodecs ?? []}
						hostEncoders={s.hostEncoders ?? []}
						{selfId}
						active={sm.activeTab === s.tabId}
						fullscreen={sm.fullscreen}
						onToggleFullscreen={sm.toggleFullscreen}
						onEnd={() => sm.endSession(s.tabId)}
					/>
				{/if}
			</div>
		{/each}
		</div>

		{#if pwPrompt}
			<PasswordModal
				bind:pwInput
				{pwError}
				{pwChecking}
				onSubmit={submitPw}
				onCancel={cancelPw}
			/>
		{/if}
	</div>
	</div>
{/if}

<style>
	.stage {
		position: relative;
		flex: 1;
		min-height: 0;
	}
	.layer {
		position: absolute;
		inset: 0;
		display: flex;
	}
	.layer.hidden {
		display: none;
	}
</style>
