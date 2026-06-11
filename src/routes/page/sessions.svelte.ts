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
	conn: 'direct' | 'relay';
	wsPort: number;
	audioWsPort?: number;
	native?: boolean;
	embedded?: boolean;
	/** Host's validated stream caps (empty = unknown) — gates the session-menu options. */
	hostCodecs?: string[];
	hostEncoders?: string[];
	// For a local host-game tab: the game's stop command, run when this tab closes.
	// Per-session (not a shared global) so closing one tab never stops another game.
	stopCmd?: string;
};

const STREAM_PORT = 9000;

// Known core connect errors arrive as raw English Rust strings (ConnError) — map them
// to friendly Turkish copy for the connect flash (substring match so wrapped/formatted
// variants still hit). Unknown/already-Turkish messages fall through verbatim.
function friendlyConnectError(raw: string): string {
	const m = raw.toLowerCase();
	if (m.includes('relay did not respond'))
		return 'Aktarıcı sunucuya ulaşılamadı — internet bağlantınızı ve aktarıcı ayarını kontrol edin.';
	if (m.includes('could not be reached via the relay'))
		return 'Cihaza ulaşılamadı — çevrimdışı olabilir ya da kimlik hatalı.';
	if (m.includes('not registered with a relay yet'))
		return 'Henüz çevrimiçi değilsiniz — önce çevrimiçi olun.';
	if (m.includes('p2p connection failed'))
		return 'Doğrudan bağlantı kurulamadı ve aktarıcı kullanımı kapalı (Ağ ayarlarına bakın).';
	if (m.includes('network is unreachable') || m.includes('connection refused') || m.includes('timed out'))
		return 'Hedefe bağlanılamadı — adresi ve ağ bağlantınızı kontrol edin.';
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
		const tabId = this.#nextTab++;
		this.sessions = [
			...this.sessions,
			{ tabId, playId: -1, phase: 'connecting', target, mode: useMode, conn: 'direct', wsPort: 0 }
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
		try {
			const info = await api.startRemotePlay(
				target.id,
				gameId,
				STREAM_PORT,
				ui.codec,
				ui.encoder,
				useMode === 'game',
				useMode === 'game'
			);
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
				hostEncoders: info.host_encoders ?? []
			});
			setTimeout(() => this.#activateByPlayId(pid), 10_000);
			recordConnection(target.id, target.name, useMode === 'game' ? 'console' : 'pc');
		} catch (e) {
			this.connectErr = friendlyConnectError(e instanceof Error ? e.message : String(e));
			this.removeTab(tabId);
		} finally {
			this.#deps.onAuthDone(target.id); // this connection's prompt (if any) is done
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
