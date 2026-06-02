<script lang="ts">
	import { onMount } from 'svelte';
	import PulsarMark from '$lib/PulsarMark.svelte';
	import Icon from '$lib/Icon.svelte';
	import { api, windowControl, onSessionEvent, onAuthPrompt, setFullscreen } from '$lib/api';
	import { recordConnection } from '$lib/peers.svelte';
	import { gameStore, type Game } from '$lib/games.svelte';
	import { ui } from '$lib/settings.svelte';
	import { t, i18n, cycleLang, LANGS } from '$lib/i18n.svelte';
	import type { Config } from '$lib/types';
	import Home from '$lib/screens/Home.svelte';
	import Devices from '$lib/screens/Devices.svelte';
	import Settings from '$lib/screens/Settings.svelte';
	import Games from '$lib/screens/Games.svelte';
	import Connecting from '$lib/screens/Connecting.svelte';
	import SessionView from '$lib/screens/Session.svelte';
	import Approve from '$lib/screens/Approve.svelte';
	import HostChat from '$lib/screens/HostChat.svelte';

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

	type View = 'home' | 'devices' | 'gaming' | 'settings';
	type Target = { name: string; id: string };
	type Session = {
		tabId: number;
		playId: number; // Rust play id (-1 until active / local host session)
		phase: 'connecting' | 'active';
		target: Target;
		mode: 'remote' | 'game';
		conn: 'direct' | 'relay';
		wsPort: number;
		local: boolean; // host is this same machine → control disabled
	};

	const NAV: { id: View; icon: string }[] = [
		{ id: 'home', icon: 'connect' },
		{ id: 'devices', icon: 'devices' },
		{ id: 'gaming', icon: 'gaming' },
		{ id: 'settings', icon: 'settings' }
	];
	// Short code (TR/EN) shown on the language toggle button.
	const langShort = $derived(LANGS.find((l) => l.value === i18n.lang)?.short ?? 'EN');

	let view = $state<View>('home');
	let mode = $state<'remote' | 'game'>('remote');
	let dark = $state(false);
	let selfId = $state('—');
	let selfPw = $state('');
	let online = $state(false);
	let config = $state<Config | null>(null);
	// Multiple concurrent host connections, each a tab. `activeTab` is 'home' or a
	// session's tabId. Fullscreen hides all chrome/tabs (only the active host).
	let sessions = $state<Session[]>([]);
	let activeTab = $state<'home' | number>('home');
	let nextTab = 0;
	let activeGame = $state<Game | null>(null);
	let fullscreen = $state(false);
	let connecting = $state(false);
	let connError = $state('');
	let connectErr = $state('');
	// Client-side password prompt — driven by the host's `auth-prompt` event, so it
	// appears at the same time as the host's Allow/Deny popup. Replying with the
	// password OR the host clicking Allow (whichever first) completes the connect.
	let pwPrompt = $state<{ req: number; peer: string } | null>(null);
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
	function closePw() {
		pwPrompt = null;
		pwInput = '';
		pwError = '';
		pwChecking = false;
	}
	// Host-side activity: who's connected + a recent event log.
	let hostSessions = $state<{ peer: string; since: number }[]>([]);
	let activity = $state<string[]>([]);
	const STREAM_PORT = 9000;

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
			connError = e instanceof Error ? e.message : String(e);
		} finally {
			connecting = false;
		}
	}

	// Roll a fresh one-time password (host side).
	async function refreshPw() {
		try {
			selfPw = await api.newPassword();
		} catch {
			/* keep the current password */
		}
	}

	onMount(async () => {
		if (approveReq) return; // approval popup: nothing to bootstrap
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
		// A host is asking us for a password — show the prompt (a re-fire means the
		// previous password was wrong).
		await onAuthPrompt((e) => {
			if (pwPrompt && pwChecking) pwError = t('pw.error');
			pwPrompt = { req: e.req, peer: e.peer };
			pwInput = '';
			pwChecking = false;
		});
	});

	$effect(() => {
		document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
	});

	// Keep <html lang> in sync so CSS text-transform uses the right casing rules
	// (Turkish lowercase i uppercases to a dotted İ — wrong for English copy).
	$effect(() => {
		document.documentElement.lang = i18n.lang;
	});

	function patchSession(tabId: number, patch: Partial<Session>) {
		sessions = sessions.map((s) => (s.tabId === tabId ? { ...s, ...patch } : s));
	}

	async function startConnect(target: Target, m?: 'remote' | 'game', gameId = '') {
		const useMode = m ?? mode;
		connectErr = '';
		const tabId = nextTab++;
		sessions = [
			...sessions,
			{ tabId, playId: -1, phase: 'connecting', target, mode: useMode, conn: 'direct', wsPort: 0, local: false }
		];
		activeTab = tabId;
		// Auth (password prompt + host Allow/Deny) happens during this call, driven
		// by events — no password is passed up front.
		try {
			const info = await api.startRemotePlay(
				target.id,
				gameId,
				STREAM_PORT,
				ui.codec,
				ui.decoder,
				useMode === 'game'
			);
			patchSession(tabId, {
				playId: info.id,
				phase: 'active',
				conn: info.transport === 'relay' ? 'relay' : 'direct',
				wsPort: info.ws_port,
				local: info.local
			});
			recordConnection(target.id, target.name, useMode === 'game' ? 'console' : 'pc');
		} catch (e) {
			connectErr = e instanceof Error ? e.message : String(e);
			removeTab(tabId);
		} finally {
			closePw(); // this connection's prompt (if any) is done
		}
	}

	function removeTab(tabId: number) {
		sessions = sessions.filter((s) => s.tabId !== tabId);
		if (activeTab === tabId) activeTab = sessions.length ? sessions[sessions.length - 1].tabId : 'home';
	}

	// Close a tab: stop its stream (host sees a disconnect) and drop it.
	function endSession(tabId: number) {
		const s = sessions.find((x) => x.tabId === tabId);
		if (s) {
			if (s.playId >= 0) api.stopStream(s.playId).catch(() => {});
			if (activeGame?.cmdStop && s.mode === 'game') api.runCommand(activeGame.cmdStop).catch(() => {});
		}
		removeTab(tabId);
		if (fullscreen) toggleFullscreen();
	}

	function toggleFullscreen() {
		fullscreen = !fullscreen;
		setFullscreen(fullscreen).catch(() => {});
	}

	// Host: kick a connected client.
	function kickPeer(peer: string) {
		api.disconnectPeer(peer).catch(() => {});
		hostSessions = hostSessions.filter((s) => s.peer !== peer);
	}

	// Host launches one of its own games — a local tab (no remote peer / video).
	function startHostSession(game: Game) {
		activeGame = game;
		const tabId = nextTab++;
		sessions = [
			...sessions,
			{
				tabId,
				playId: -1,
				phase: 'active',
				target: { name: game.title, id: t('host.local') },
				mode: 'game',
				conn: 'direct',
				wsPort: 0,
				local: true
			}
		];
		activeTab = tabId;
	}

	// Keep the core's published game list in sync so connecting clients can see it.
	$effect(() => {
		if (approveReq) return;
		api.publishGames($state.snapshot(gameStore.games)).catch(() => {});
	});

	// Keep the core's host stream settings in sync (resolution/fps/bitrate/encoder).
	$effect(() => {
		if (approveReq) return;
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
{:else}
	<HostChat />
	<div class="desktop">
	<div class="window">
		{#if !fullscreen}
		<!-- frameless titlebar -->
		<div class="chrome" data-tauri-drag-region>
			<div class="traffic">
				<button class="tl r" type="button" aria-label={t('chrome.close')} onclick={() => windowControl('close')}
				></button>
				<button class="tl y" type="button" aria-label={t('chrome.minimize')} onclick={() => windowControl('minimize')}
				></button>
				<button
					class="tl g"
					type="button"
					aria-label={t('chrome.maximize')}
					onclick={() => windowControl('maximize')}
				></button>
			</div>
			<div class="ctitle">Pulsar{activeTab === 'home' ? ` — ${t('nav.' + view)}` : ''}</div>
			<div class="cright">
				<button
					class="lang-btn"
					title={t('chrome.language')}
					aria-label={t('chrome.languageToggle')}
					onclick={cycleLang}
				>
					<Icon name="globe" size={15} /><span class="lang-code mono">{langShort}</span>
				</button>
				<button
					class="icon-btn"
					title={t('chrome.theme')}
					aria-label={t('chrome.themeToggle')}
					onclick={() => (dark = !dark)}
				>
					<Icon name={dark ? 'sun' : 'monitor'} size={16} />
				</button>
				<span class="mono ver">Pulsar v1.0</span>
			</div>
		</div>
		{#if sessions.length}
			<div class="tabs">
				<button class="tab" class:on={activeTab === 'home'} onclick={() => (activeTab = 'home')}>
					<Icon name="home" size={15} />{t('tab.home')}
				</button>
				{#each sessions as s (s.tabId)}
					<div
						class="tab"
						class:on={activeTab === s.tabId}
						role="button"
						tabindex="0"
						onclick={() => (activeTab = s.tabId)}
						onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (activeTab = s.tabId)}
					>
						<span class="tdot" class:live={s.phase === 'active'}></span>
						<span class="tname">{s.target.name}</span>
						<button
							class="tx"
							aria-label={t('tab.close')}
							onclick={(e) => {
								e.stopPropagation();
								endSession(s.tabId);
							}}
						>
							<Icon name="x" size={11} />
						</button>
					</div>
				{/each}
			</div>
		{/if}
		{/if}

		<div class="stage">
		<div class="layer" class:hidden={activeTab !== 'home'}>
		<div class="body">
			<!-- sidebar -->
			<aside class="sidebar">
				<div class="brand">
					<PulsarMark size={26} />
					<span class="nm">Pulsar</span>
				</div>
				<nav class="nav">
					{#each NAV as n (n.id)}
						<button
							class="navlink"
							class:on={view === n.id}
							onclick={() => (view = n.id)}
						>
							<Icon name={n.icon} size={19} />
							{t('nav.' + n.id)}
						</button>
					{/each}
				</nav>
				<div class="sidefoot">
					<div class="idcard">
						<div class="idlab mono">{t('sidebar.idLabel')}</div>
						<div class="idval mono">{selfId}</div>
					</div>
					<div class="me">
						<div class="meavatar">{t('sidebar.me')}</div>
						<div>
							<div class="mename">{t('sidebar.thisDevice')}</div>
							<div class="mestatus" class:off={!online}>
								<span class="dot"></span>
								{#if connecting}{t('status.connecting')}{:else if online}{t('status.online')}{:else}{t('status.offline')}{/if}
							</div>
							{#if !online && !connecting}
								<button class="reconnect" onclick={goOnline} title={connError}>{t('status.goOnline')}</button>
								{#if connError}<div class="connerr">{connError}</div>{/if}
							{/if}
						</div>
					</div>
				</div>
			</aside>

			<!-- content -->
			<main class="content">
				{#if connectErr}
					<div class="flash" role="alert">
						<Icon name="shield" size={16} />
						<span>{connectErr}</span>
						<button class="flashx" aria-label={t('flash.close')} onclick={() => (connectErr = '')}>
							<Icon name="x" size={14} />
						</button>
					</div>
				{/if}
				{#if view === 'home'}
					<Home
						{selfId}
						{selfPw}
						{mode}
						{hostSessions}
						{activity}
						debug={ui.debug}
						onMode={(m) => (mode = m)}
						onRefreshPw={refreshPw}
						onDisconnect={kickPeer}
						onConnect={startConnect}
					/>
				{:else if view === 'devices'}
					<Devices onConnect={startConnect} />
				{:else if view === 'gaming'}
					<Games onStream={startHostSession} />
				{:else if view === 'settings'}
					<Settings onReconnect={goOnline} />
				{/if}

			</main>
		</div>
		</div>

		{#each sessions as s (s.tabId)}
			<div class="layer" class:hidden={activeTab !== s.tabId}>
				{#if s.phase === 'connecting'}
					<Connecting target={s.target} mode={s.mode} onCancel={() => endSession(s.tabId)} />
				{:else}
					<SessionView
						playId={s.playId}
						target={s.target}
						mode={s.mode}
						conn={s.conn}
						wsPort={s.wsPort}
						local={s.local}
						{fullscreen}
						onToggleFullscreen={toggleFullscreen}
						onEnd={() => endSession(s.tabId)}
					/>
				{/if}
			</div>
		{/each}
		</div>

		{#if pwPrompt}
			<div class="pwmodal">
				<div class="pwcard">
					<div class="pwhdr"><Icon name="shield" size={18} /><span>{t('pw.title')}</span></div>
					<!-- eslint-disable-next-line svelte/no-at-html-tags -->
					<p class="pwlead">{@html t('pw.lead')}</p>
					{#if pwError}<div class="pwerr">{pwError}</div>{/if}
					<!-- svelte-ignore a11y_autofocus -->
					<input
						class="pwfield mono"
						type="text"
						bind:value={pwInput}
						disabled={pwChecking}
						onkeydown={(e) => e.key === 'Enter' && submitPw()}
						placeholder={t('pw.placeholder')}
						aria-label={t('pw.aria')}
						autofocus
					/>
					<div class="pwact">
						<button class="pwbtn ghost" onclick={cancelPw}>{t('pw.cancel')}</button>
						<button class="pwbtn primary" disabled={pwChecking} onclick={submitPw}>
							{pwChecking ? t('pw.checking') : t('pw.submit')}
						</button>
					</div>
				</div>
			</div>
		{/if}
	</div>
	</div>
{/if}

<style>
	.flash {
		display: flex;
		align-items: center;
		gap: 9px;
		margin-bottom: 16px;
		padding: 11px 14px;
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--danger) 12%, var(--surface));
		border: 1px solid color-mix(in oklch, var(--danger) 40%, var(--border));
		color: var(--danger);
		font-size: 13px;
		line-height: 1.4;
	}
	.flash span {
		flex: 1;
		word-break: break-word;
	}
	.flashx {
		flex: none;
		border: none;
		background: transparent;
		color: var(--danger);
		cursor: pointer;
		padding: 2px;
		display: grid;
		place-items: center;
		border-radius: 4px;
	}
	.flashx:hover {
		background: color-mix(in oklch, var(--danger) 18%, transparent);
	}
	.pwmodal {
		position: absolute;
		inset: 0;
		z-index: 20;
		display: grid;
		place-items: center;
		background: oklch(0.2 0.01 265 / 0.45);
		backdrop-filter: blur(3px);
	}
	.pwcard {
		width: 340px;
		max-width: calc(100% - 40px);
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r);
		box-shadow: var(--shadow-lg);
		padding: 20px;
	}
	.pwhdr {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: 16px;
		font-weight: 700;
		color: var(--accent-press);
	}
	.pwlead {
		margin: 9px 0 14px;
		font-size: 13px;
		color: var(--text-muted);
		line-height: 1.5;
	}
	.pwerr {
		margin-bottom: 10px;
		font-size: 12.5px;
		color: var(--danger);
		font-weight: 600;
	}
	.pwfield {
		width: 100%;
		box-sizing: border-box;
		padding: 11px 12px;
		font-size: 17px;
		letter-spacing: 0.04em;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text);
	}
	.pwfield:focus {
		outline: none;
		border-color: var(--accent);
	}
	.pwact {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 10px;
		margin-top: 16px;
	}
	.pwbtn {
		padding: 10px 0;
		border-radius: var(--r-sm);
		font-weight: 600;
		font-size: 14px;
		cursor: pointer;
		border: 1px solid var(--border);
	}
	.pwbtn.ghost {
		background: var(--surface-2);
		color: var(--text);
	}
	.pwbtn.ghost:hover {
		background: var(--surface-3);
	}
	.pwbtn.primary {
		background: var(--accent);
		color: #fff;
		border-color: transparent;
	}
	.pwbtn.primary:hover {
		background: var(--accent-press);
	}
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
	/* tab strip (home + connected hosts) */
	.tabs {
		flex: none;
		display: flex;
		align-items: stretch;
		gap: 2px;
		height: 36px;
		padding: 0 8px;
		background: var(--surface-2);
		border-bottom: 1px solid var(--border);
		overflow-x: auto;
	}
	.tab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		max-width: 200px;
		padding: 0 10px;
		border: none;
		border-bottom: 2px solid transparent;
		background: transparent;
		color: var(--text-muted);
		font-size: 13px;
		font-weight: 500;
		cursor: pointer;
		white-space: nowrap;
	}
	.tab:hover {
		background: var(--surface-3);
	}
	.tab.on {
		color: var(--accent-press);
		border-bottom-color: var(--accent);
		background: var(--surface);
	}
	.tname {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.tdot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--border-strong);
		flex: none;
	}
	.tdot.live {
		background: var(--ok);
	}
	.tx {
		display: grid;
		place-items: center;
		width: 18px;
		height: 18px;
		border: none;
		border-radius: 5px;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
	}
	.tx:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.chrome {
		height: 44px;
		flex: none;
		display: flex;
		align-items: center;
		padding: 0 14px;
		border-bottom: 1px solid var(--border);
		background: var(--surface);
		user-select: none;
		position: relative;
	}
	.traffic {
		display: flex;
		gap: 8px;
	}
	.tl {
		width: 12px;
		height: 12px;
		border-radius: 50%;
		border: none;
		padding: 0;
		cursor: pointer;
		transition: filter var(--dur) var(--ease);
	}
	.tl:hover {
		filter: brightness(0.9);
	}
	.tl.r {
		background: #ed6a5e;
	}
	.tl.y {
		background: #f4bf4f;
	}
	.tl.g {
		background: #61c554;
	}
	.ctitle {
		position: absolute;
		left: 0;
		right: 0;
		text-align: center;
		font-size: 13px;
		font-weight: 600;
		color: var(--text-muted);
		pointer-events: none;
	}
	.cright {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 10px;
		z-index: 1;
	}
	.cright .ver {
		font-size: 11.5px;
		color: var(--text-faint);
	}
	.lang-btn {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		height: 28px;
		padding: 0 9px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		transition:
			background var(--dur) var(--ease),
			color var(--dur) var(--ease);
	}
	.lang-btn:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.lang-code {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.04em;
	}
	.body {
		display: flex;
		flex: 1;
		min-height: 0;
		width: 100%;
	}
	.sidebar {
		width: 224px;
		flex: none;
		background: var(--surface-2);
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		padding: 14px 12px;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 8px 16px;
	}
	.brand .nm {
		font-family: var(--font-display);
		font-weight: 600;
		font-size: 18px;
		letter-spacing: -0.03em;
	}
	.nav {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.navlink {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 10px 11px;
		border: none;
		border-radius: var(--r-sm);
		cursor: pointer;
		text-align: left;
		font-family: var(--font-sans);
		font-size: 14.5px;
		font-weight: 500;
		color: var(--text-muted);
		background: transparent;
		transition: all var(--dur) var(--ease);
	}
	.navlink:hover {
		background: var(--surface-3);
	}
	.navlink.on {
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
	}
	.sidefoot {
		margin-top: auto;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.idcard {
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r);
		padding: 11px 12px;
	}
	.idlab {
		font-size: 10px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.idval {
		font-size: 16px;
		font-weight: 500;
		letter-spacing: 0.04em;
		margin-top: 4px;
	}
	.me {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 4px 6px;
	}
	.meavatar {
		width: 32px;
		height: 32px;
		border-radius: 8px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 11px;
		font-family: var(--font-display);
	}
	.mename {
		font-size: 13px;
		font-weight: 600;
	}
	.mestatus {
		font-size: 11.5px;
		color: var(--ok);
		display: flex;
		align-items: center;
		gap: 5px;
	}
	.mestatus .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--ok);
	}
	.mestatus.off {
		color: var(--text-faint);
	}
	.mestatus.off .dot {
		background: var(--border-strong);
	}
	.reconnect {
		margin-top: 6px;
		font-size: 11.5px;
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
		border: 1px solid var(--accent-soft-2);
		border-radius: var(--r-sm);
		padding: 4px 9px;
		cursor: pointer;
	}
	.reconnect:hover {
		background: var(--accent-soft-2);
	}
	.connerr {
		margin-top: 5px;
		font-size: 10.5px;
		color: var(--danger);
		max-width: 180px;
		line-height: 1.35;
		word-break: break-word;
	}
</style>
