// Host-side "Oyunlar" (games/apps) state, persisted locally.
//
// A "game" is anything the host can launch for a streaming session: a program
// (executable + args), a raw command, or just a custom cover image (a desktop /
// "do nothing but stream" entry). Each can run a command when the session
// starts and when it ends (Sunshine-style prep commands). Streaming quality is a
// single host-level setting, not per-game.

export type GameType = 'program' | 'command' | 'image';

export interface Game {
	id: string;
	title: string;
	type: GameType;
	/** executable or image path */
	path: string;
	/** launch args (program) */
	args: string;
	/** raw command (type === 'command') */
	command: string;
	/** cover image path/url (optional) */
	image: string;
	/** run on session start */
	cmdStart: string;
	/** run on session end */
	cmdStop: string;
}

export interface HostSettings {
	resolution: string;
	fps: number;
	bitrate: number;
}

export interface ScanConfig {
	folders: string[];
	autoScan: boolean;
	intervalMin: number;
}

interface Store {
	games: Game[];
	host: HostSettings;
	scan: ScanConfig;
}

const KEY = 'pulsar.games.v1';
const hasLS = typeof localStorage !== 'undefined';

function defaults(): Store {
	return {
		games: [],
		host: { resolution: '1440p', fps: 60, bitrate: 40 },
		scan: { folders: [], autoScan: false, intervalMin: 30 }
	};
}

function load(): Store {
	if (!hasLS) return defaults();
	try {
		const raw = localStorage.getItem(KEY);
		return raw ? { ...defaults(), ...(JSON.parse(raw) as Partial<Store>) } : defaults();
	} catch {
		return defaults();
	}
}

export const gameStore = $state<Store>(load());

function uid(): string {
	if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID();
	return `g-${gameStore.games.length}-${gameStore.games.reduce((n, g) => n + g.title.length, 1)}`;
}

export function saveGames() {
	if (hasLS) localStorage.setItem(KEY, JSON.stringify($state.snapshot(gameStore)));
}

export function addGame(g: Omit<Game, 'id'>): Game {
	const game: Game = { ...g, id: uid() };
	gameStore.games.push(game);
	saveGames();
	return game;
}

export function updateGame(id: string, patch: Partial<Game>) {
	const g = gameStore.games.find((x) => x.id === id);
	if (g) {
		Object.assign(g, patch);
		saveGames();
	}
}

export function removeGame(id: string) {
	const i = gameStore.games.findIndex((x) => x.id === id);
	if (i >= 0) {
		gameStore.games.splice(i, 1);
		saveGames();
	}
}

/** Add a program discovered by a folder scan (deduped by path). */
export function addScanned(title: string, path: string): boolean {
	if (gameStore.games.some((g) => g.path === path)) return false;
	addGame({ title, type: 'program', path, args: '', command: '', image: '', cmdStart: '', cmdStop: '' });
	return true;
}

export function addFolder(p: string) {
	const path = p.trim();
	if (path && !gameStore.scan.folders.includes(path)) {
		gameStore.scan.folders.push(path);
		saveGames();
	}
}

export function removeFolder(p: string) {
	const i = gameStore.scan.folders.indexOf(p);
	if (i >= 0) {
		gameStore.scan.folders.splice(i, 1);
		saveGames();
	}
}

/** Test helper. */
export function _reset() {
	const d = defaults();
	gameStore.games.splice(0, gameStore.games.length);
	gameStore.scan.folders.splice(0, gameStore.scan.folders.length);
	gameStore.host = d.host;
	gameStore.scan.autoScan = false;
	gameStore.scan.intervalMin = 30;
	if (hasLS) localStorage.removeItem(KEY);
}
