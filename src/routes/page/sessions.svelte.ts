// Session/tab orchestration for the app shell (+page.svelte). Owns the list of
// concurrent host connections (each a tab), the active tab, fullscreen state, and
// the connect/disconnect lifecycle. Kept out of the route component so the shell
// markup stays small; behavior is identical to the inline version it replaced.

import { api, setFullscreen } from '$lib/api';
import { recordConnection } from '$lib/peers.svelte';
import { ui } from '$lib/settings.svelte';
import { t } from '$lib/i18n.svelte';
import type { Game } from '$lib/games.svelte';

export type Target = { name: string; id: string };
export type Session = {
	tabId: number;
	playId: number; // Rust play id (-1 until active / local host session)
	phase: 'connecting' | 'active';
	target: Target;
	mode: 'remote' | 'game';
	conn: 'direct' | 'relay';
	wsPort: number;
	audioWsPort?: number;
	native?: boolean;
	embedded?: boolean;
	// For a local host-game tab: the game's stop command, run when this tab closes.
	// Per-session (not a shared global) so closing one tab never stops another game.
	stopCmd?: string;
};

const STREAM_PORT = 9000;

type Deps = {
	/** Current default connect mode (from the shell's mode toggle). */
	getMode: () => 'remote' | 'game';
	/** Called when a connection attempt finishes (success or fail) so the shell can
	 * dismiss its password prompt. */
	onAuthDone: () => void;
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
			this.#patch(tabId, {
				playId: info.id,
				phase: 'active',
				conn: info.transport === 'relay' ? 'relay' : 'direct',
				wsPort: info.ws_port,
				audioWsPort: info.audio_ws_port,
				native: info.native,
				embedded: info.embedded
			});
			recordConnection(target.id, target.name, useMode === 'game' ? 'console' : 'pc');
		} catch (e) {
			this.connectErr = e instanceof Error ? e.message : String(e);
			this.removeTab(tabId);
		} finally {
			this.#deps.onAuthDone(); // this connection's prompt (if any) is done
		}
	};

	removeTab = (tabId: number) => {
		this.sessions = this.sessions.filter((s) => s.tabId !== tabId);
		if (this.activeTab === tabId)
			this.activeTab = this.sessions.length ? this.sessions[this.sessions.length - 1].tabId : 'home';
	};

	// Close a tab: stop its stream (host sees a disconnect) and drop it.
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
