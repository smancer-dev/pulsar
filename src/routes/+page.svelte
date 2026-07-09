<script lang="ts">
	import { onMount } from 'svelte';
	import PulsarMark from '$lib/PulsarMark.svelte';
	import { silentUpdateCheck } from '$lib/updater';
	import {
		api,
		isTauri,
		onSessionEvent,
		onAuthPrompt,
		onReverseRequest,
		onFullscreenToggle,
		onLocalCaps,
		onPeerAvatar,
		onPeerName,
		onPeerId,
		onGuideToggle,
		onControllerConnected,
		onNodeId,
		onNodeVersionError,
		onSessionPassword
	} from '$lib/api';
	import { setPeerIdentity } from '$lib/peers.svelte';
	import { gameStore } from '$lib/games.svelte';
	import { ui, configTick, saveUi } from '$lib/settings.svelte';
	import { modalCount } from '$lib/overlayModals.svelte';
	import { initCaps } from '$lib/caps.svelte';
	import { t, i18n, i18nReady } from '$lib/i18n.svelte';
	import { theme, toggleTheme } from '$lib/theme.svelte';
	import type { Config } from '$lib/types';
	import Connecting from '$lib/screens/Connecting.svelte';
	import SessionView from '$lib/screens/Session.svelte';
	import Home from '$lib/screens/Home.svelte';
	import PaneGameConnect from '$lib/screens/Gaming/PaneGameConnect.svelte';
	import InputAssign from '$lib/screens/Gaming/InputAssign.svelte';
	import SplitPicker from './page/SplitPicker.svelte';
	import Approve from '$lib/screens/Approve.svelte';
	import Connections from '$lib/screens/Connections.svelte';
	import FilesWindow from '$lib/screens/FilesWindow.svelte';
	import HostChat from '$lib/screens/HostChat.svelte';
	import Chrome from './page/Chrome.svelte';
	import Icon from '$lib/Icon.svelte';
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

	// When opened as a per-session file-manager window (one per remote-play session,
	// label `files-<id>`), render only the standalone Files screen.
	const filesReq = (() => {
		if (typeof window === 'undefined') return null;
		const inj = (window as unknown as { __FILES__?: { id: number; peer: string } }).__FILES__;
		return inj ? { id: Number(inj.id), peer: String(inj.peer ?? '') } : null;
	})();

	// Any popup window short-circuits the main-app bootstrap (no relay re-register).
	const isPopup = !!approveReq || connReq || !!filesReq;

	type View = 'home' | 'devices' | 'gaming' | 'settings';

	const NAV: { id: View; icon: string }[] = [
		{ id: 'home', icon: 'connect' },
		{ id: 'devices', icon: 'devices' },
		{ id: 'gaming', icon: 'gaming' },
		{ id: 'settings', icon: 'settings' }
	];

	let view = $state<View>('home');
	// App-level personality, persisted in `ui.appMode`. 'remote' = the general
	// remote-desktop app (left sidebar, host capability); 'game' = the controller-first
	// game-streaming client shell (bottom dock, centered ID, no hosting). Toggled from
	// the top bar; the CLI `--mode game` overrides it on launch (set in onMount).
	let mode = $state<'remote' | 'game'>(ui.appMode);

	// Flip the personality and persist the choice so the app reopens in the last mode.
	function toggleMode() {
		const next = mode === 'game' ? 'remote' : 'game';
		// Changing personality ALWAYS leaves split mode (either direction) — the split's panes are
		// tied to the mode it was entered in. Switching INTO gaming also closes any open
		// (remote-mode) sessions/tabs — gaming is a fresh pure-client personality.
		if (sm.splitMode !== 'off') sm.exitSplit();
		if (next === 'game') {
			for (const s of [...sm.sessions]) sm.endSession(s.tabId);
		}
		mode = next;
		ui.appMode = next;
		saveUi();
	}
	// Split mode: the "nasıl bölünsün?" layout chooser is shown when the user presses the
	// chrome split button. The actual split state (layout / panes / focus) lives on the
	// SessionManager (sm.splitMode / sm.panes / sm.focusedPane).
	let showSplitPicker = $state(false);
	// Couch-coop input assignment modal (split + gaming) — `assignPanes` is derived after `sm` below.
	let showInputAssign = $state(false);
	// The personality every pane runs in split mode, captured the moment the picker opens.
	// In SPLIT mode ALL panes are one fixed mode (gaming OR remote) — never a mix. When not
	// yet split we snapshot the app's current `mode`; once split we keep the established
	// `sm.splitSessionMode` so a reshape (e.g. 2→4) never flips the panes' personality.
	let splitEntryMode = $state<'game' | 'remote'>('remote');
	function openSplit() {
		splitEntryMode = sm.splitMode === 'off' ? mode : sm.splitSessionMode;
		showSplitPicker = true;
	}

	let selfId = $state('—');
	let selfPw = $state('');
	let online = $state(false);
	let config = $state<Config | null>(null);
	let connecting = $state(false);
	let connError = $state('');
	// Set to true when the relay rejects us with an incompatible protocol version.
	// Blocks the auto-retry effect so the app stays offline with the "update required"
	// message instead of hammering the relay it can never join.  Cleared only when
	// the user explicitly triggers a new connection (config change → goOnline).
	let versionBlocked = $state(false);
	// Multiple concurrent host connections (tabs), the active tab, fullscreen, and the
	// connect/disconnect lifecycle live in the session manager.
	const sm = new SessionManager({ getMode: () => mode, onAuthDone: (target) => closePwFor(target) });
	// The split panes that have a LIVE session, so each input device can be locked to one (couch
	// co-op). Drives the input-assign button/modal — shown with ≥2 live game panes.
	const assignPanes = $derived(
		sm.panes
			.map((tabId, i) => ({
				index: i,
				playId: tabId != null ? (sm.sessions.find((s) => s.tabId === tabId)?.playId ?? -1) : -1
			}))
			.filter((p) => p.playId >= 0)
	);
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
	// The exact req id we last submitted a password for (the head being checked). A
	// wrong-password re-fire must replace THIS prompt, not any prompt that merely shares
	// the same peer id — two concurrent connects to the same peer would otherwise swap
	// the wrong head. Reset on close.
	let pwCheckingReq = $state<number | null>(null);

	function submitPw() {
		if (!pwPrompt) return;
		// Signal the connect timeout that auth interaction ended — re-arms the post-auth
		// handshake deadline (so only network time counts from here).
		sm.notifyAuthSubmit(pwPrompt.peer);
		api.submitPassword(pwPrompt.req, pwInput).catch(() => {});
		pwChecking = true;
		pwCheckingReq = pwPrompt.req;
		pwError = '';
	}
	function cancelPw() {
		if (pwPrompt) {
			// Resume the timeout: user dismissed auth, so the connect will error from the
			// host (auth refused) or hang — either way let the normal deadline apply.
			sm.notifyAuthSubmit(pwPrompt.peer);
			api.submitPassword(pwPrompt.req, null).catch(() => {});
		}
		closePw();
	}
	// Dequeue the current (head) prompt and re-arm the inputs for the next one, if any.
	function closePw() {
		pwQueue = pwQueue.slice(1);
		pwInput = '';
		pwError = '';
		pwChecking = false;
		pwCheckingReq = null;
	}
	// Does a prompt's reported peer identify the same host as this connect target? The
	// prompt's peer may carry a resolved `ip:port` while the target was typed as a bare ip
	// (or vice-versa), so a bare host bridges to an `ip:port` ONLY when the host segment is
	// EXACTLY equal — NOT a `startsWith` prefix. A prefix test collides distinct hosts
	// (`192.168.1.5` would match `192.168.1.50:…`, and `192.168.1.5` vs `192.168.1.5:9001`
	// could dismiss the wrong concurrent tab); requiring full-segment equality avoids that.
	function peerMatchesTarget(peer: string, targetId: string): boolean {
		const p = peer.replace(/\s/g, '');
		const want = targetId.replace(/\s/g, '');
		if (p === want) return true;
		// Strip the trailing `:port` (the LAST colon — keeps bracketed IPv6 hosts intact)
		// from whichever side carries it, then compare the bare hosts exactly.
		const host = (s: string) => (s.includes(':') ? s.slice(0, s.lastIndexOf(':')) : null);
		return host(p) === want || host(want) === p;
	}
	// A finished connect dismisses only ITS OWN queued prompt: with concurrent tabs,
	// dequeuing the head would remove the prompt the user is typing into whenever an
	// UNRELATED connect settles first — that prompt's req would never be answered and
	// its tab would hang on Connecting. Matched per-host via `peerMatchesTarget`.
	function closePwFor(targetId: string) {
		const i = pwQueue.findIndex((q) => peerMatchesTarget(q.peer, targetId));
		if (i < 0) return;
		pwQueue = pwQueue.filter((_, j) => j !== i);
		// The visible head was removed → re-arm the inputs for the next prompt (if any).
		if (i === 0) {
			pwInput = '';
			pwError = '';
			pwChecking = false;
			pwCheckingReq = null;
		}
	}
	// Does a queued password prompt belong to this connect target? Same per-host matching
	// as closePwFor — used to show "awaiting approval" only on the tab whose connect
	// actually has a pending prompt (not every concurrent Connecting).
	function hasPendingPromptFor(targetId: string): boolean {
		return pwQueue.some((q) => peerMatchesTarget(q.peer, targetId));
	}

	// Split GAMING mode drives the controller-nav bridge here: GamingShell (which normally
	// starts/stops the gilrs→webview `gamepad-nav` bridge) is NOT rendered in split, so without
	// this the panes' PaneGameConnect nav gets no controller input — pad nav looked dead in split.
	// Gaming split only (remote split has no pad nav).
	$effect(() => {
		if (sm.splitMode !== 'off' && sm.splitSessionMode === 'game') {
			api.gamepadNavStart().catch(() => {});
			return () => {
				api.gamepadNavStop().catch(() => {});
			};
		}
	});

	// Host-side activity: who's connected + a recent event log. Keyed by SESSION id (a
	// device can hold several concurrent sessions — couch co-op / split panes), with the
	// peer kept for the label + kick routing.
	let hostSessions = $state<{ sid: number; peer: string; since: number }[]>([]);
	// Peer map-key → the client's own device id (pushed via DataMsg::PeerId). Lets the
	// connect-tab list show a client's ID even on a direct/same-LAN connect (where the
	// session's peer key is the client's ip:port).
	let hostClientIds = $state<Record<string, string>>({});
	// Peer map-key → the connecting client's pushed display name, so the connect-tab list can
	// show "303 036 449 (orangepi)" next to the id/ip.
	let hostNames = $state<Record<string, string>>({});
	let activity = $state<string[]>([]);
	// Controller-connect toast (idle, not in a session): name + battery, bottom-right, ~4.5 s.
	let padToast = $state<{ name: string; battery: number | null } | null>(null);
	let padToastTimer: ReturnType<typeof setTimeout> | undefined;

	// Bind + register with the configured relay. Re-runnable: called on startup,
	// on manual retry, and whenever the relay/network settings change.
	async function goOnline() {
		// A user-initiated or config-driven goOnline call clears the version-blocked
		// guard so the auto-retry effect can resume if the new registration also fails
		// for a transient reason (not a version mismatch).
		versionBlocked = false;
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
			// The relay replied with a version-mismatch error during initial registration
			// (ErrCode::Protocol mapped to ConnError::IncompatibleVersion in node.rs).
			// Show the clean "update required" message and block auto-retry — every retry
			// would fail the same way and overwrite this message with the generic one.
			// (The post-registration path uses the onNodeVersionError event instead.)
			if (msg.includes('incompatible protocol version')) {
				versionBlocked = true;
				connError = t('connErr.incompatibleVersion');
			} else {
				// The sidebar shows this as the offline tooltip; the auto-retry effect below
				// keeps trying, so say so instead of presenting a dead end.
				connError = isTauri ? `${msg} — ${t('status.willRetry')}` : msg;
			}
		} finally {
			connecting = false;
		}
	}

	// Keep the shell's config copy in sync with Settings saves: Settings persists via
	// the core into ITS OWN config snapshot, and this copy (driving the Home screen's
	// unattended-access warning + blanked one-time password) otherwise refreshed only
	// inside goOnline() — a toggle in Settings didn't reach Home until a reconnect.
	// `configTick` bumps ONLY on CORE config saves (not UI-only twiddles like the
	// overlay-button drag or in-session stream controls) — re-fetch on it so the shell
	// reflects unattended/avatar changes without churning IPC on every UI save.
	let lastConfigTick = configTick.n;
	$effect(() => {
		if (configTick.n === lastConfigTick || isPopup || !isTauri) return;
		lastConfigTick = configTick.n;
		api.getConfig().then((c) => {
			const wasUnattended = config?.unattended_access ?? true;
			config = c;
			// Proactively mint an OTP when unattended access is toggled OFF while
			// online: the lazy per-connection mint (host.rs) runs AFTER a client
			// connects, but the client needs the code before it can connect —
			// chicken-and-egg.  Minting here makes the code visible on the Home
			// screen the instant the host is re-secured.
			if (online && wasUnattended && !c.unattended_access && !selfPw) {
				api.newPassword().then((pw) => { selfPw = pw; }).catch(() => {});
			}
		}).catch(() => {});
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
		// Do NOT auto-retry when the relay rejected us with an incompatible protocol
		// version: every attempt would fail the same way and would overwrite the
		// "update required" message with the generic "will retry" text.  The user must
		// update the app; goOnline() clears this flag when explicitly called again.
		if (versionBlocked) return;
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
	// CLI --connect auto-connect: a one-shot, target-scoped password to auto-submit if
	// the kiosk host asks for one. Keyed by the auto-connect target id so it can NEVER
	// answer a later/unrelated host's prompt, and cleared after the first submit so a
	// wrong-password re-fire (or a second connect) falls through to the normal modal.
	let autoPw: { id: string; pw: string } | null = null;
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
		// capability probe + the active language dictionary land (≤5 s cap each), then
		// cross-fade to the UI (so the first real paint is never a flash of raw keys).
		await Promise.all([
			wait(1300),
			Promise.race([capsReady, wait(5000)]),
			Promise.race([i18nReady, wait(5000)])
		]);
		booting = false; // start the opacity fade
		await wait(500);
		splashOn = false; // unmount once faded
		splashGone(); // release the held --connect auto-connect (Connecting screen now visible)
	}

	onMount(async () => {
		initCaps();
		boot();
		if (isPopup) return; // approval popup: nothing else to bootstrap
		// opi5: when GPU compositing is forced on, run the present-keepalive so WebKitGTK/Mali
		// doesn't intermittently freeze the display when the page goes idle (see acKeepalive).
		if (isTauri) {
			api.forceAc()
				.then((on) => {
					if (on) import('$lib/acKeepalive').then((m) => m.startAcKeepalive());
				})
				.catch(() => {});
			// Flag the root when the webview is software-painted (AC off) so the gaming-mode CSS
			// strips expensive blurred shadows only there (cheap on the GPU path; keep the full look).
			api.webviewSwPainted()
				.then((sw) => document.documentElement.toggleAttribute('data-sw-ui', sw))
				.catch(() => {});
		}
		// The one-time password rotates after each successful auth (host-side) — reflect
		// the fresh code in the SelfCard immediately instead of waiting for a re-poll.
		onSessionPassword((pw) => {
			if (!config?.unattended_access) selfPw = pw;
		});
		// The relay restarted and reissued a DIFFERENT 9-digit ID (it lost its
		// pubkey→id map). goOnline() reads the ID once, so without this the Home
		// screen would keep showing the dead old one. Adopt the rotated ID live.
		onNodeId((id) => {
			if (online) selfId = id;
		});
		// The relay was redeployed with a newer protocol version while we were
		// already online. The Rust backend has taken the node offline; mirror that
		// in the UI so the user sees the "update required" message instead of a
		// stale (unreachable) ID. Auto-retry is intentionally NOT triggered here
		// (every re-register would be refused too) — the user must update first.
		onNodeVersionError(() => {
			versionBlocked = true;
			online = false;
			selfId = '—';
			selfPw = '';
			connError = t('connErr.incompatibleVersion');
		});
		// Sync the persisted tray preference into the Rust backend BEFORE going online (it
		// has no dependency on being online) so CloseRequested knows whether to hide-to-tray
		// or quit even while the startup relay registration is still in flight. If the relay
		// is unreachable, goOnline blocks for its full timeout; a close during that window
		// would otherwise hide-to-tray for a user who disabled it, leaving a ghost process.
		api.setTray(ui.tray).catch(() => {});
		await goOnline();
		// Surface incoming connections on the host UI.
		await onSessionEvent((e) => {
			if (e.kind === 'request') {
				activity = [t('activity.wants', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'connected') {
				// Keyed by sid: a 2nd session from the same device adds a row, not replace.
				if (!hostSessions.some((s) => s.sid === e.sid))
					hostSessions = [...hostSessions, { sid: e.sid, peer: e.peer, since: Date.now() }];
				activity = [t('activity.connected', { peer: e.peer }), ...activity].slice(0, 8);
			} else if (e.kind === 'disconnected') {
				hostSessions = hostSessions.filter((s) => s.sid !== e.sid);
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
			hostNames = { ...hostNames, [e.peer]: e.name };
		});
		// A connecting client pushed its OWN device id — show it in the connect-tab list
		// instead of its ip:port (direct/same-LAN connects, where the peer key is an addr).
		await onPeerId((e) => {
			hostClientIds = { ...hostClientIds, [e.peer]: e.id };
		});
		// Controller PS/Guide button (always-on watcher): toggle gaming mode on/off, but
		// ONLY when idle — never while connecting/in a client session or hosting one, and
		// never in the approval popup. In gaming mode this leaves it; from remote it enters.
		await onGuideToggle(() => {
			if (isPopup) return;
			if (sm.sessions.length > 0 || hostSessions.length > 0) return;
			toggleMode();
		});
		// Controller connected → toast (only when idle; in a session it's surfaced in-overlay).
		await onControllerConnected((e) => {
			if (sm.sessions.length > 0) return;
			padToast = e;
			clearTimeout(padToastTimer);
			padToastTimer = setTimeout(() => (padToast = null), 4500);
		});
		// A controlled client asked to reverse direction — connect back to it so the
		// roles swap (it must be online/serving for this to land).
		await onReverseRequest((e) => {
			if (e.id) sm.startConnect({ name: t('home.remoteDevice'), id: e.id }, 'remote');
		});
		// Client (Linux native): Ctrl+Shift+F12 (evdev-captured, so it never reaches the
		// webview as a keydown) — toggle fullscreen (SAME action as the button + F11). The
		// shell F11/Ctrl+Shift+F12 (unengaged app) is a separate $effect window listener below.
		await onFullscreenToggle(() => sm.toggleFullscreen());
		// A host is asking us for a password — show the prompt (a re-fire means the
		// previous password was wrong).
		await onAuthPrompt((e) => {
			// Auto-connect: answer the kiosk host's FIRST password prompt without UI, but
			// only for the CLI target itself (same despaced id/ip:port matching as closePwFor)
			// and only ONCE — clear it so a wrong-password re-fire, a reverse-direction connect,
			// or a manual connect to a different host falls through to the interactive modal.
			if (autoPw) {
				const want = autoPw.id.replace(/\s/g, '');
				const p = e.peer.replace(/\s/g, '');
				if (p === want || p.startsWith(want + ':') || want.startsWith(p + ':')) {
					const pw = autoPw.pw;
					autoPw = null;
					// Auto-submit is instant: no need to pause the timeout (no human delay).
					api.submitPassword(e.req, pw).catch(() => {});
					return;
				}
			}
			// A re-fire for the connection currently being checked (same peer, fresh req) means
			// the previous password was wrong — replace the head's req in place and flag the error.
			// Match on the SPECIFIC req we submitted (pwCheckingReq), not just the peer: two
			// concurrent connects to the same peer id would otherwise swap the wrong head.
			if (pwPrompt && pwChecking && pwPrompt.req === pwCheckingReq && pwPrompt.peer === e.peer) {
				// Wrong-password re-fire: auth is still interactive — keep the pause alive.
				sm.notifyAuthStart(e.peer);
				pwQueue = [{ req: e.req, peer: e.peer }, ...pwQueue.slice(1)];
				pwError = t('pw.error');
				pwChecking = false;
				pwCheckingReq = null;
				return;
			}
			// Otherwise it's a (possibly concurrent) new connection's prompt — queue it. If it
			// becomes the visible head, arm the inputs for it.
			// Pause this connection's timeout: the user now has unlimited time to type the
			// password or wait for the host operator to click Allow/Deny.
			sm.notifyAuthStart(e.peer);
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
			// Appliance/kiosk (CLI --connect): update on BOOT, before connecting. If an
			// update installs, the app relaunches and never reaches startConnect below;
			// otherwise we continue into the session. `timeoutMs` keeps the manifest
			// fetch quick when the endpoint is unreachable; `overallTimeoutMs` bounds the
			// whole flow so a stalled download (flaky Wi-Fi, throttling, half-open TCP)
			// can't hang the boot path before connect. Mid-session is never interrupted
			// (this is the launch path only).
			await silentUpdateCheck({
				timeoutMs: 8000,
				overallTimeoutMs: 60000,
				// startConnect runs only after this resolves, but a peer can connect to
				// this host (incoming control) during the manifest fetch — abort the
				// install so we never tear down a session that began in the gap.
				isBusy: () => sm.sessions.length > 0 || hostSessions.length > 0
			});
			if (ac.pw) autoPw = { id: ac.id, pw: ac.pw };
			const m = ac.mode === 'game' ? 'game' : 'remote';
			// A CLI `--mode game` kiosk launches into the gaming personality (transient —
			// not persisted to ui.appMode, since it's a launch-time override). So when the
			// session ends the user lands on the gaming home, and hosting is refused.
			mode = m;
			const gameId = m === 'game' ? (ac.app || '') : '';
			// Headless --connect: show the splash, THEN the Connecting screen. The session is
			// created now (so the splash fades onto the Connecting screen, not the home view),
			// but `holdConnecting` defers the real network connect until the splash is gone — so
			// the P2P/relay milestone is on-screen instead of a fast connect finishing unseen.
			sm.startConnect({ name: ac.app || t('home.remoteDevice'), id: ac.id }, m, gameId, {
				holdConnecting: splashDone
			});
			// Kiosk / headless start (CLI --connect): begin fullscreen so the host fills
			// the screen with no app chrome. Toggle off with Ctrl+Shift+F12. `--nofullscreen`
			// (ac.nofullscreen) opts out — the session starts windowed (dev test loops / embedded).
			if (!ac.nofullscreen && !sm.fullscreen) sm.toggleFullscreen();
		}
		// Auto-update: only when this launch is IDLE (no CLI --connect into a live
		// session) — the updater must never interrupt a remote session. Deferred a few
		// seconds so it never delays first paint / the splash. Self-swallows all errors.
		if (!(ac && ac.id)) {
			// Restore the gaming-mode fullscreen preference on a normal (non-kiosk) launch:
			// if the user last left game mode fullscreen, reopen fullscreen. The window is
			// already shown (boot()) and goOnline() has completed, so the OS-level
			// fullscreen sticks. Scoped to game mode so remote never reopens fullscreen.
			if (mode === 'game' && ui.gamingFullscreen && !sm.fullscreen) sm.toggleFullscreen();
			setTimeout(() => {
				// Re-check session state at fire time: the user may have connected (client
				// session) or a peer may have connected to control this host within the 3s
				// window. The updater must never tear down a live session.
				if (sm.sessions.length === 0 && hostSessions.length === 0)
					// Re-check at fire time (above) AND inside silentUpdateCheck after the
					// slow manifest fetch: a session can begin during the check→install gap.
					void silentUpdateCheck({
						isBusy: () => sm.sessions.length > 0 || hostSessions.length > 0
					});
			}, 3000);
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
		// In split mode the panes only tile PART of the window (chrome/tabs + any empty
		// PaneConnect cells stay visible), so the webview is never fully occluded — keep
		// repaint live. Single-tab mode is unchanged.
		const s = sm.sessions.find((x) => x.tabId === sm.activeTab);
		const occluded =
			sm.splitMode === 'off' && !!s && !!s.native && s.phase === 'active' && !!s.ready;
		document.documentElement.toggleAttribute('data-occluded', occluded);
	});

	// Give the whole app a gaming look (cyan accent) while the app is in gaming mode OR
	// a game-stream session is the active tab; revert as soon as both are false.
	$effect(() => {
		// Split mode: every pane is the SAME fixed personality, so the gaming look follows
		// `sm.splitSessionMode` (NOT the focused pane). Single-tab mode reads the active tab.
		if (sm.splitMode !== 'off') {
			document.documentElement.toggleAttribute(
				'data-gaming',
				mode === 'game' || sm.splitSessionMode === 'game'
			);
			return;
		}
		const s = sm.sessions.find((x) => x.tabId === sm.activeTab);
		document.documentElement.toggleAttribute(
			'data-gaming',
			mode === 'game' || (!!s && s.mode === 'game')
		);
	});

	// Gaming mode is a pure client: this device may NOT be a host. Tell the core to
	// refuse inbound (the Rust serve loop denies at auth, before any popup), and kick
	// any sessions that are already connected so "nobody is connected in gaming mode"
	// holds. Leaving gaming mode re-enables hosting. The Rust default is "serving", so
	// this only ever needs to flip it off/on.
	$effect(() => {
		if (isPopup || !isTauri) return;
		api.setHostServing(mode !== 'game').catch(() => {});
		if (mode === 'game') {
			// Authoritative kick of EVERY inbound session (covers any the UI list missed).
			api.disconnectAllPeers().catch(() => {});
			// Per-session kick converges the local list (kickPeer filters each out, so it stops
			// writing once empty). Do NOT reassign `hostSessions = []` here — this effect READS
			// hostSessions, so a fresh-array write would re-trigger itself forever (the splash-
			// stuck infinite reactive loop).
			for (const s of hostSessions) kickPeer(s.sid);
		}
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

	// Shell shortcut: F11 or Ctrl+Shift+F12 toggles IMMERSIVE fullscreen (true-fullscreen that also
	// hides the top bar everywhere, incl. the home). Fires only when keys reach the webview — i.e.
	// the unengaged app shell; an engaged session's keys are evdev/hook-captured (handled by
	// onFullscreenToggle). Ignored while typing in a field.
	$effect(() => {
		const onKey = (e: KeyboardEvent) => {
			const el = e.target as HTMLElement | null;
			if (el && (el.isContentEditable || /^(input|textarea|select)$/i.test(el.tagName))) return;
			if (e.key === 'F11' || (e.key === 'F12' && e.ctrlKey && e.shiftKey)) {
				// Guard auto-repeat: holding F11 fired a keydown EVERY frame → toggled fullscreen
				// over and over ("sürekli fullscreen"). One toggle per physical press. Also stop the
				// event so WebKitGTK/the WM never ALSO acts on F11 (its own fullscreen binding fought
				// ours, which is why F11 behaved differently). SAME action as the button.
				e.preventDefault();
				e.stopImmediatePropagation();
				if (e.repeat) return;
				sm.toggleFullscreen();
			}
		};
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	});

	// Split mode: grid-cell placement for a paned session (CSS grid line numbers). Pane
	// order is row-major: h2 = [left,right]; v2 = [top,bottom]; grid4 = [TL,TR,BL,BR].
	function paneCellStyle(i: number): string {
		if (sm.splitMode === 'h2') return `grid-column:${i + 1};grid-row:1`;
		if (sm.splitMode === 'v2') return `grid-row:${i + 1};grid-column:1`;
		// grid4 (2×2)
		const col = (i % 2) + 1;
		const row = Math.floor(i / 2) + 1;
		return `grid-column:${col};grid-row:${row}`;
	}

	// Host: kick a connected SESSION (by sid, so a same-device co-op sibling survives).
	function kickPeer(sid: number) {
		api.disconnectPeer(sid).catch(() => {});
		hostSessions = hostSessions.filter((s) => s.sid !== sid);
	}

	// Keep the core's published game list in sync so connecting clients can see it.
	$effect(() => {
		if (isPopup) return;
		api.publishGames($state.snapshot(gameStore.games)).catch(() => {});
	});

	// Keep the core's host stream settings in sync (resolution/fps/bitrate/encoder/hdr).
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
				encoder: ui.encoder,
				hdr: ui.hdr
			})
			.catch(() => {});
	});
</script>

{#if approveReq}
	<Approve id={approveReq.id} peer={approveReq.peer} pw={approveReq.pw} />
{:else if connReq}
	<Connections />
{:else if filesReq}
	<FilesWindow playId={filesReq.id} peer={filesReq.peer} />
{:else}
	<HostChat />
	{#if padToast}
		<div class="pad-toast" role="status">
			<span class="pt-dot"></span>
			<span class="pt-name">{padToast.name}</span>
			<span class="pt-sub">{t('toast.padConnected')}{#if padToast.battery != null} · {padToast.battery}%{/if}</span>
		</div>
	{/if}
	<div class="desktop">
	<div class="window" class:fullscreen={sm.fullscreen}>
		{#if splashOn}
			<div class="splash" class:gone={!booting} aria-hidden="true">
				<div class="splash-mark"><PulsarMark size={76} /></div>
				<div class="splash-word">Pulsar</div>
			</div>
		{/if}
		{#if !sm.fullscreen || (sm.splitMode === 'off' && sm.activeTab === 'home')}
			<Chrome
				title={sm.activeTab === 'home' && mode !== 'game' ? t('nav.' + view) : ''}
				dark={theme.dark}
				onToggleTheme={toggleTheme}
				gaming={mode === 'game'}
				onToggleMode={toggleMode}
				splitMode={sm.splitMode}
				onSplit={openSplit}
				fullscreen={sm.fullscreen}
				onToggleFullscreen={sm.toggleFullscreen}
			/>
			{#if sm.sessions.length && sm.splitMode === 'off'}
				<Tabs
					sessions={sm.sessions}
					activeTab={sm.activeTab}
					onSelect={(tab) => (sm.activeTab = tab)}
					onEnd={sm.endSession}
					onRename={sm.renameTab}
				/>
			{/if}
		{/if}

		<div class="stage" class:split={sm.splitMode !== 'off'} data-layout={sm.splitMode}>
		<!-- Home is hidden whenever a session tab is active OR split mode is on (split has
		     no "home" pane). Single-tab flow is unchanged when splitMode==='off'. -->
		<div class="layer" class:hidden={sm.activeTab !== 'home' || sm.splitMode !== 'off'}>
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
				{hostClientIds}
				{hostNames}
				{activity}
				active={sm.activeTab === 'home' && sm.splitMode === 'off'}
				fullscreen={sm.fullscreen}
				onToggleFullscreen={sm.toggleFullscreen}
				onView={(v) => (view = v)}
				onGoOnline={goOnline}
				onRefreshPw={refreshPw}
				onDisconnect={kickPeer}
				onConnect={sm.startConnect}
				onStream={sm.startHostSession}
				onClearConnectErr={() => (sm.connectErr = '')}
				onAuthDone={closePwFor}
				splitMode={sm.splitMode}
				onSplit={openSplit}
			/>
		</div>

		{#each sm.sessions as s (s.tabId)}
			{@const paneIdx = sm.splitMode === 'off' ? -1 : sm.panes.indexOf(s.tabId)}
			<!-- Split: a session in a pane becomes a grid cell (placed by paneCellStyle); a
			     session NOT in any pane stays mounted but hidden (background tab). Single: the
			     active tab shows, the rest are CSS-hidden — exactly as before. -->
			<div
				class="layer"
				class:hidden={sm.splitMode === 'off' ? sm.activeTab !== s.tabId : paneIdx < 0}
				class:pane={paneIdx >= 0}
				class:focused={paneIdx >= 0 && paneIdx === sm.focusedPane}
				style={paneIdx >= 0 ? paneCellStyle(paneIdx) : ''}
			>
				{#if s.phase === 'connecting'}
					<Connecting
						target={s.target}
						mode={s.mode}
						awaitingApproval={hasPendingPromptFor(s.target.id)}
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
						hostDisplays={s.hostDisplays ?? []}
						{selfId}
						active={sm.splitMode === 'off'
							? sm.activeTab === s.tabId
							: paneIdx === sm.focusedPane}
						split={sm.splitMode !== 'off'}
						fullscreen={sm.fullscreen}
						onToggleFullscreen={sm.toggleFullscreen}
						onPaneFocus={paneIdx >= 0 ? () => sm.focusPane(paneIdx) : undefined}
						occludeNative={showSplitPicker || modalCount.n > 0 || !!pwPrompt || showInputAssign}
						onEnd={() => sm.endSession(s.tabId)}
					/>
				{/if}
			</div>
		{/each}

		<!-- Split mode: an empty pane (null) shows the FULL normal connect screen for the
		     split's fixed mode — the real Home (remote) or the real gaming connect flow
		     (game), just rendered inside the cell. Clicking it focuses the pane; connecting
		     fills THAT pane via sm.connectIntoPane (which forces the split mode + auto-assigns
		     a free host monitor). The screen is wrapped in an overflow:auto box sized to the
		     cell so a cramped 2×2 scrolls instead of overflowing. -->
		{#if sm.splitMode !== 'off'}
			{#each sm.panes as pane, i (i)}
				{#if pane === null}
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<div
						class="layer pane"
						class:focused={i === sm.focusedPane}
						style={paneCellStyle(i)}
						onpointerdowncapture={() => sm.focusPane(i)}
					>
						<div class="pane-connect-scroll">
							{#if sm.splitSessionMode === 'game'}
								<PaneGameConnect
									index={i}
									focused={i === sm.focusedPane}
									onConnect={sm.connectIntoPane}
									onFetched={closePwFor}
								/>
							{:else}
								<Home
									connectOnly
									{selfId}
									{selfPw}
									{online}
									{connecting}
									unattended={config?.unattended_access ?? false}
									{hostSessions}
									{hostClientIds}
									{hostNames}
									{activity}
									debug={ui.debug}
									onConnect={(t, _m, g) => sm.connectIntoPane(i, t, 'remote', g)}
								/>
							{/if}
						</div>
					</div>
				{/if}
			{/each}
		{/if}
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

		{#if showSplitPicker}
			<SplitPicker
				splitMode={sm.splitMode}
				entryMode={splitEntryMode}
				onPick={(layout) => sm.setLayout(layout, splitEntryMode)}
				onExit={() => sm.exitSplit()}
				onClose={() => (showSplitPicker = false)}
			/>
		{/if}

		<!-- Split bottom bar: in split mode the top chrome is hidden while fullscreen (the kiosk's
		     default), leaving no way to change layout / exit split / drop fullscreen. Show a slim bar
		     BELOW the pane grid — a flex sibling AFTER .stage, so it sits in a video-free strip (the
		     Linux native render fills only the panes above it). Shown only when the top bar is hidden
		     (split + fullscreen) so the two never appear at once. -->
		{#if sm.splitMode !== 'off' && sm.fullscreen}
			<div class="split-bar">
				<button class="sb-btn danger" onclick={() => sm.exitSplit()}>
					<Icon name="x" size={15} />{t('split.exit')}
				</button>
				<button class="sb-btn" onclick={openSplit}>
					<Icon name="split" size={15} />{t('chrome.split')}
				</button>
				<button class="sb-btn" onclick={sm.toggleFullscreen}>
					<Icon name="expand" size={15} />{t('gaming.fullscreen')}
				</button>
				{#if sm.splitSessionMode === 'game' && assignPanes.length >= 2}
					<button class="sb-btn" onclick={() => (showInputAssign = true)}>
						<Icon name="gaming" size={15} />{t('chrome.inputAssign')}
					</button>
				{/if}
			</div>
		{/if}
		{#if showInputAssign}
			<InputAssign panes={assignPanes} onClose={() => (showInputAssign = false)} />
		{/if}
	</div>
	</div>
{/if}

<style>
	/* Split bottom bar — a flex strip BELOW the pane grid (so it never overlaps the panes' native
	   video on Linux), shown only when the top chrome is hidden (split + fullscreen). */
	.split-bar {
		flex: none;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 10px;
		height: 46px;
		padding: 0 14px;
		border-top: 1px solid var(--border);
		background: var(--surface);
		z-index: 2;
	}
	.sb-btn {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		height: 32px;
		padding: 0 14px;
		font-size: 13px;
		font-weight: 600;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text);
		cursor: pointer;
		transition:
			background var(--dur) var(--ease),
			color var(--dur) var(--ease);
	}
	.sb-btn:hover {
		background: var(--surface-3);
	}
	.sb-btn.danger:hover {
		background: #e81123;
		border-color: #e81123;
		color: #fff;
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
	/* ── Split mode ───────────────────────────────────────────────────────────────
	   The stage becomes a CSS grid; each pane (a paned session OR an empty connect screen)
	   is a RELATIVE grid item placed by inline grid-column/grid-row. Background sessions
	   (mounted but not in a pane) keep the absolute+hidden treatment. */
	.stage.split {
		display: grid;
		gap: 2px;
		background: var(--border);
	}
	.stage.split[data-layout='h2'] {
		grid-template-columns: 1fr 1fr;
		grid-template-rows: 1fr;
	}
	.stage.split[data-layout='v2'] {
		grid-template-columns: 1fr;
		grid-template-rows: 1fr 1fr;
	}
	.stage.split[data-layout='grid4'] {
		grid-template-columns: 1fr 1fr;
		grid-template-rows: 1fr 1fr;
	}
	/* A paned cell sits in the grid flow (not absolutely positioned). */
	.stage.split .layer.pane {
		position: relative;
		inset: auto;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
	}
	/* The focused pane gets a subtle accent ring so the user knows where input goes. */
	.stage.split .layer.pane.focused {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
		z-index: 1;
	}
	/* An empty pane renders the FULL connect screen (real Home / gaming flow). Those screens
	   are sized for a whole window, so a 2×2 cell would clip them — this box fills the cell
	   and scrolls (both axes) instead of overflowing. The inner padding gives the remote
	   Home (which has no built-in stage padding) breathing room; the gaming screens carry
	   their own. */
	.pane-connect-scroll {
		flex: 1;
		min-width: 0;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: auto;
		padding: 18px;
		background:
			radial-gradient(120% 80% at 50% -10%, var(--accent-soft) 0%, transparent 55%),
			var(--surface-2);
	}
	/* The gaming flow brings its own padded, scrolling layout — drop the wrapper's padding
	   so it isn't double-padded and let its internal scroll handle overflow. */
	.pane-connect-scroll:has(> :global(.pane-game)) {
		padding: 0;
		overflow: hidden;
	}
	/* Controller-connect toast (idle): bottom-right, brief. */
	.pad-toast {
		position: fixed;
		right: 18px;
		bottom: 18px;
		z-index: 90;
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 10px 14px;
		border-radius: var(--r-md, 10px);
		background: var(--surface-2, var(--surface));
		border: 1px solid var(--accent-soft-2, var(--border));
		box-shadow: var(--shadow-2, 0 14px 34px oklch(0.2 0.03 265 / 0.28));
		font-size: 13px;
		color: var(--text);
	}
	.pad-toast .pt-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--ok);
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--ok) 22%, transparent);
		flex: none;
	}
	.pad-toast .pt-name {
		font-weight: 600;
	}
	.pad-toast .pt-sub {
		color: var(--text-muted);
	}
</style>
