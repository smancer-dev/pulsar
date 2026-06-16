// Session/tab orchestration for the app shell (+page.svelte). Owns the list of
// concurrent host connections (each a tab), the active tab, fullscreen state, and
// the connect/disconnect lifecycle. Kept out of the route component so the shell
// markup stays small; behavior is identical to the inline version it replaced.

import { api, isTauri, onPlayEnded, onPlayReady, setFullscreen } from '$lib/api';
import { recordConnection } from '$lib/peers.svelte';
import { ui } from '$lib/settings.svelte';
import { t } from '$lib/i18n.svelte';
import type { Game } from '$lib/games.svelte';

export type Target = { name: string; id: string };
export type Session = {
	tabId: number;
	playId: number; // Rust play id (-1 until active / local host session)
	phase: 'connecting' | 'active';
	/** First decoded frames have landed (`play-ready`) — the native child window is now
	 * actually painting over the webview. Drives the occlusion repaint-suspend. */
	ready?: boolean;
	target: Target;
	/** User-renamed tab title (session-scoped, via the tab's edit button); falls
	 * back to `target.name` when unset. */
	label?: string;
	mode: 'remote' | 'game';
	/** For game-mode sessions: the game id passed to startConnect (empty = none). */
	gameId?: string;
	conn: 'direct' | 'relay';
	wsPort: number;
	audioWsPort?: number;
	native?: boolean;
	embedded?: boolean;
	/** Host's validated stream caps (empty = unknown) — gates the session-menu options. */
	hostCodecs?: string[];
	hostEncoders?: string[];
	/** Host's streamable monitors (primary first); the session menu's screen picker. */
	hostDisplays?: import('$lib/api.types').HostDisplay[];
	// For a local host-game tab: the game's stop command, run when this tab closes.
	// Per-session (not a shared global) so closing one tab never stops another game.
	stopCmd?: string;
};

const STREAM_PORT = 9000;

// A connect attempt that neither resolves nor rejects (e.g. the host accepted the
// transport but never answers the auth/caps step, or the link dropped mid-handshake
// after the await began) would otherwise leave the Connecting screen spinning forever
// — the core's auth recv loop has no timeout. Fail the attempt after this deadline so
// the user lands back on Home with a friendly error instead of an endless spinner.
const CONNECT_TIMEOUT_MS = 45_000;

/** Marker thrown when the connect attempt blows past `CONNECT_TIMEOUT_MS`. */
const CONNECT_TIMEOUT = 'connect-timed-out';

// How long to wait after auth resumes (password submitted / prompt closed) before
// timing out the post-auth handshake. Separate from CONNECT_TIMEOUT_MS because the
// full 45 s is generous for a network handshake but auth itself is unbounded.
const POST_AUTH_TIMEOUT_MS = 30_000;

// Known core connect errors arrive as raw English Rust strings (ConnError) — map them
// to friendly copy IN THE ACTIVE UI LANGUAGE for the connect flash (substring match
// so wrapped/formatted variants still hit). Unknown messages fall through verbatim.
function friendlyConnectError(raw: string): string {
	const m = raw.toLowerCase();
	if (m.includes(CONNECT_TIMEOUT)) return t('connErr.timeout');
	if (m.includes('relay did not respond')) return t('connErr.relayDown');
	if (m.includes('could not be reached via the relay')) return t('connErr.peerUnreachable');
	if (m.includes('not registered with a relay yet')) return t('connErr.notOnline');
	if (m.includes('p2p connection failed')) return t('connErr.p2pFailed');
	if (m.includes('network is unreachable') || m.includes('connection refused') || m.includes('timed out'))
		return t('connErr.unreachable');
	return raw;
}

type Deps = {
	/** Current default connect mode (from the shell's mode toggle). */
	getMode: () => 'remote' | 'game';
	/** Called when a connection attempt finishes (success or fail) so the shell can
	 * dismiss THAT target's password prompt (others may still be pending). */
	onAuthDone: (targetId: string) => void;
};

export class SessionManager {
	// Multiple concurrent host connections, each a tab. `activeTab` is 'home' or a
	// session's tabId. Fullscreen hides all chrome/tabs (only the active host).
	sessions = $state<Session[]>([]);
	activeTab = $state<'home' | number>('home');
	fullscreen = $state(false);
	connectErr = $state('');

	#nextTab = 0;
	#deps: Deps;
	// Targets (despaced peer id) for which interactive auth (password prompt or
	// host Allow/Deny) is currently in progress. The connect timeout is paused
	// while the target is in this set — auth time must not count against the
	// handshake deadline, since auth is a human-interaction step.
	#authInProgress = new Set<string>();
	// When the timeout fires while auth is in progress it parks {reject, arm} here
	// (keyed by despaced peer id) rather than immediately rejecting. Once the user
	// submits a password (notifyAuthSubmit) we re-arm for POST_AUTH_TIMEOUT_MS.
	// Once the connect finishes (finally block) or the tab closes, the entry is
	// deleted — any parked arm that lost its race just becomes a no-op.
	#pendingTimeoutReject = new Map<string, { reject: (e: Error) => void; arm: (ms: number) => void }>();

	constructor(deps: Deps) {
		this.#deps = deps;
		// The Connecting screen holds until the stream is REALLY up (first decoded
		// frames — `play-ready`), so the user never lands on a black session.
		onPlayReady((playId) => this.#activateByPlayId(playId));
		// A session can die during the connecting-hold window (after start_remote_play
		// resolves but before play-ready) — Session.svelte isn't mounted yet to see
		// `play-ended` then, so close the tab here regardless of phase. (Local host-game
		// tabs carry playId -1 and can never match.)
		onPlayEnded((playId) => {
			const s = this.sessions.find((x) => x.playId === playId);
			if (s) this.endSession(s.tabId);
		});
	}

	#activateByPlayId(playId: number) {
		this.sessions = this.sessions.map((s) =>
			s.playId === playId ? { ...s, phase: 'active', ready: true } : s
		);
	}

	#patch(tabId: number, patch: Partial<Session>) {
		this.sessions = this.sessions.map((s) => (s.tabId === tabId ? { ...s, ...patch } : s));
	}

	startConnect = async (
		target: Target,
		m?: 'remote' | 'game',
		gameId = '',
		opts: { holdConnecting?: Promise<unknown> } = {}
	) => {
		const useMode = m ?? this.#deps.getMode();
		this.connectErr = '';
		// Dedupe: a repeated connect to the same peer (e.g. a re-fired reverse-direction
		// `reverse-request`, or a double-click) must not open a second tab to that device —
		// that would spin up duplicate viewer ports + a second native renderer and confuse the
		// tab strip. If an identical session (same id + same mode + same gameId) is already
		// connecting/active, focus it instead. Matched on despaced id; local host-game tabs
		// (no real peer id) never collide. A blank target id is treated as unknown and never
		// deduped. NOTE: different mode (remote vs game) or different gameId are NOT deduped —
		// those are legitimately distinct sessions to the same host.
		const want = target.id.replace(/\s/g, '');
		if (want) {
			const existing = this.sessions.find(
				(s) =>
					s.target.id.replace(/\s/g, '') === want &&
					s.mode === useMode &&
					(useMode !== 'game' || (s.gameId ?? '') === (gameId ?? ''))
			);
			if (existing) {
				this.activeTab = existing.tabId;
				return;
			}
		}
		const tabId = this.#nextTab++;
		this.sessions = [
			...this.sessions,
			{ tabId, playId: -1, phase: 'connecting', target, mode: useMode, gameId: gameId || undefined, conn: 'direct', wsPort: 0 }
		];
		this.activeTab = tabId;
		// Headless --connect: hold the actual network connect until the launch splash is gone,
		// so the Connecting screen (and its real P2P/relay status) is on-screen for the whole
		// connection instead of being hidden behind the splash. No-op for manual connects.
		if (opts.holdConnecting) {
			await opts.holdConnecting;
			// The user may have cancelled (closed the connecting tab) during the splash hold.
			if (!this.sessions.some((s) => s.tabId === tabId)) return;
		}
		// Auth (password prompt + host Allow/Deny) happens during this call, driven
		// by events — no password is passed up front.
		// Hoisted so the finally block can clean up auth-pause state regardless of
		// whether the try body threw before or after peerKey was used.
		const peerKey = target.id.replace(/\s/g, '');
		try {
			// Guard the connect with a deadline: if it neither resolves nor rejects in
			// time, abort with a timeout error (routed through friendlyConnectError) so
			// the Connecting screen can't spin forever. Keep the underlying promise so a
			// late resolution (after the timeout fired) can still be torn down rather than
			// leaking its viewer ports + native renderer + the held host session.
			const play = api.startRemotePlay(
				target.id,
				gameId,
				STREAM_PORT,
				ui.codec,
				ui.encoder,
				useMode === 'game',
				useMode === 'game',
				ui.quality
			);
			let timer: ReturnType<typeof setTimeout> | undefined;
			let timedOut = false;
			// Capture `this` for use in closures below (class private fields can't be
			// accessed via a free `this` alias in all TS targets, so we assign early).
			const self = this;
			const timeout = new Promise<never>((_, reject) => {
				// Reschedule when auth is in progress so human interaction time (typing a
				// password, waiting for Allow/Deny) does not count against the deadline.
				// Each reschedule grants POST_AUTH_TIMEOUT_MS for the post-auth handshake.
				function arm(ms: number) {
					timer = setTimeout(() => {
						if (peerKey && self.#authInProgress.has(peerKey)) {
							// Auth is active — park the reject+arm until notifyAuthSubmit
							// re-arms or notifyAuthEnd clears the pause.
							self.#pendingTimeoutReject.set(peerKey, { reject, arm });
							return;
						}
						timedOut = true;
						reject(new Error(CONNECT_TIMEOUT));
					}, ms);
				}
				arm(CONNECT_TIMEOUT_MS);
			});
			// If the connect wins the race, stop the timeout from later rejecting (and
			// leaking the timer). If the timeout wins, a late-resolving connect must still
			// stop its stream so we don't strand a half-open host session.
			play.then(
				(info) => {
					if (timedOut) api.stopStream(info.id).catch(() => {});
				},
				() => {}
			);
			const info = await Promise.race([play, timeout]).finally(() => clearTimeout(timer));
			// The user may have cancelled (closed the connecting tab) while the auth/connect
			// was in flight. If so the tab is gone, so #patch would be a no-op and the UI would
			// never learn the play id → tear down the session we just created instead of leaking
			// its viewer ports + native renderer + the held host session.
			if (!this.sessions.some((s) => s.tabId === tabId)) {
				api.stopStream(info.id).catch(() => {});
				return;
			}
			// Connected, but HOLD the Connecting screen until first frames arrive
			// (`play-ready`, video+audio pipelines start together host-side). A
			// fallback timer activates anyway so a stats-less edge case can't hang.
			// Browser dev (mock api): Tauri events never fire (no play-ready, no
			// play-ended) and the mock always returns id 0 — substitute the tab id so
			// concurrent mock tabs don't share a play id, and activate immediately
			// instead of sitting on Connecting for the full fallback window.
			const pid = isTauri ? info.id : tabId;
			this.#patch(tabId, {
				playId: pid,
				phase: isTauri ? 'connecting' : 'active',
				conn: info.transport === 'relay' ? 'relay' : 'direct',
				wsPort: info.ws_port,
				audioWsPort: info.audio_ws_port,
				native: info.native,
				embedded: info.embedded,
				hostCodecs: info.host_codecs ?? [],
				hostEncoders: info.host_encoders ?? [],
				hostDisplays: info.host_displays ?? []
			});
			setTimeout(() => this.#activateByPlayId(pid), 10_000);
			recordConnection(target.id, target.name, useMode === 'game' ? 'console' : 'pc');
		} catch (e) {
			this.connectErr = friendlyConnectError(e instanceof Error ? e.message : String(e));
			this.removeTab(tabId);
		} finally {
			// Clean up auth-pause state so a stale entry can't affect future connects.
			if (peerKey) {
				this.#authInProgress.delete(peerKey);
				this.#pendingTimeoutReject.delete(peerKey);
			}
			this.#deps.onAuthDone(target.id); // this connection's prompt (if any) is done
		}
	};

	/** Called when an auth prompt (password / Allow-Deny) becomes active for a peer.
	 * Pauses the connect timeout so human-interaction time doesn't count. */
	notifyAuthStart = (peerId: string) => {
		const key = peerId.replace(/\s/g, '');
		if (key) this.#authInProgress.add(key);
	};

	/** Called when the user submits a password (or the prompt is dismissed) for a peer.
	 * Resumes the timeout — granting POST_AUTH_TIMEOUT_MS for the post-auth handshake. */
	notifyAuthSubmit = (peerId: string) => {
		const key = peerId.replace(/\s/g, '');
		if (!key) return;
		this.#authInProgress.delete(key);
		// If the timeout fired while auth was paused, it parked itself here — re-arm
		// it now so the post-auth handshake is still bounded.
		const parked = this.#pendingTimeoutReject.get(key);
		if (parked) {
			this.#pendingTimeoutReject.delete(key);
			parked.arm(POST_AUTH_TIMEOUT_MS);
		}
	};

	removeTab = (tabId: number) => {
		this.sessions = this.sessions.filter((s) => s.tabId !== tabId);
		if (this.activeTab === tabId)
			this.activeTab = this.sessions.length ? this.sessions[this.sessions.length - 1].tabId : 'home';
	};

	// Close a tab: stop its stream (host sees a disconnect) and drop it.
	/** Rename a session tab for this session only (empty name restores the default). */
	renameTab = (tabId: number, name: string) => {
		const label = name.trim();
		this.#patch(tabId, { label: label || undefined });
	};

	endSession = (tabId: number) => {
		const s = this.sessions.find((x) => x.tabId === tabId);
		if (s) {
			if (s.playId >= 0) api.stopStream(s.playId).catch(() => {});
			// Run only THIS tab's own stop command (set for local host-game tabs); remote
			// game tabs carry no stopCmd, so closing them never touches a local game.
			if (s.stopCmd) api.runCommand(s.stopCmd).catch(() => {});
		}
		this.removeTab(tabId);
		if (this.fullscreen) this.toggleFullscreen();
	};

	toggleFullscreen = () => {
		this.fullscreen = !this.fullscreen;
		setFullscreen(this.fullscreen).catch(() => {});
	};

	// Host launches one of its own games — a local tab (no remote peer / video).
	startHostSession = (game: Game) => {
		const tabId = this.#nextTab++;
		this.sessions = [
			...this.sessions,
			{
				tabId,
				playId: -1,
				phase: 'active',
				target: { name: game.title, id: t('host.local') },
				mode: 'game',
				conn: 'direct',
				wsPort: 0,
				stopCmd: game.cmdStop || undefined
			}
		];
		this.activeTab = tabId;
	};
}
