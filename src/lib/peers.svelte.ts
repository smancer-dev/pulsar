// Real, locally-persisted peer list (recents + address book). Empty by default —
// entries appear only when you actually connect to an ID or add a device, so the
// UI never shows connections that didn't happen.

export type PeerCategory = 'pc' | 'server' | 'console';

export interface Peer {
	id: string;
	name: string;
	cat: PeerCategory;
	fav: boolean;
	/** epoch ms of the last successful connect, or null if never connected. */
	lastConnected: number | null;
}

const KEY = 'pulsar.peers.v1';
const hasLS = typeof localStorage !== 'undefined';

function load(): Peer[] {
	if (!hasLS) return [];
	try {
		const raw = localStorage.getItem(KEY);
		return raw ? (JSON.parse(raw) as Peer[]) : [];
	} catch {
		return [];
	}
}

const peers = $state<Peer[]>(load());

function persist() {
	if (hasLS) localStorage.setItem(KEY, JSON.stringify($state.snapshot(peers)));
}

export function allPeers(): Peer[] {
	return peers;
}

export function recentPeers(n = 3): Peer[] {
	return peers
		.filter((p) => p.lastConnected != null)
		.sort((a, b) => (b.lastConnected ?? 0) - (a.lastConnected ?? 0))
		.slice(0, n);
}

/** Record an actual connection — upserts the peer and stamps lastConnected. */
export function recordConnection(id: string, name: string, cat: PeerCategory = 'pc') {
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		existing.lastConnected = Date.now();
		if (name) existing.name = name;
	} else {
		peers.push({ id, name: name || id, cat, fav: false, lastConnected: Date.now() });
	}
	persist();
}

/** Manually add a saved device (address book) without connecting. */
export function addPeer(name: string, id: string, cat: PeerCategory = 'pc'): boolean {
	if (peers.some((p) => p.id === id)) return false;
	peers.push({ id, name: name || id, cat, fav: false, lastConnected: null });
	persist();
	return true;
}

export function removePeer(id: string) {
	const i = peers.findIndex((p) => p.id === id);
	if (i >= 0) {
		peers.splice(i, 1);
		persist();
	}
}

export function toggleFav(id: string) {
	const p = peers.find((x) => x.id === id);
	if (p) {
		p.fav = !p.fav;
		persist();
	}
}

/** Test helper. */
export function _reset() {
	peers.splice(0, peers.length);
	if (hasLS) localStorage.removeItem(KEY);
}
