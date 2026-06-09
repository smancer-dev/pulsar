// Real, locally-persisted peer list (recents + address book). Empty by default —
// entries appear only when you actually connect to an ID or add a device, so the
// UI never shows connections that didn't happen.

export type PeerCategory = 'pc' | 'server' | 'console';

export interface Peer {
	id: string;
	name: string;
	cat: PeerCategory;
	fav: boolean;
	/** True only if the user explicitly SAVED this device (address book / Devices).
	 * A plain connection records history but does NOT save the device. */
	saved: boolean;
	/** epoch ms of the last successful connect, or null if never connected. */
	lastConnected: number | null;
}

const KEY = 'pulsar.peers.v1';
const hasLS = typeof localStorage !== 'undefined';

function load(): Peer[] {
	if (!hasLS) return [];
	try {
		const raw = localStorage.getItem(KEY);
		if (!raw) return [];
		const list = JSON.parse(raw) as Peer[];
		// Migrate pre-`saved` entries: treat never-connected ones as explicitly saved
		// (they could only have come from "add device"), connected ones as history.
		for (const p of list) {
			if (typeof p.saved !== 'boolean') p.saved = p.lastConnected == null;
		}
		return list;
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

/** Saved devices (the Devices address book) — only ones the user explicitly added. */
export function savedPeers(): Peer[] {
	return peers.filter((p) => p.saved);
}

/** Connection history — every peer ever connected to, most-recent first. Separate
 * from the saved Devices list (a connection alone never saves a device). */
export function historyPeers(n?: number): Peer[] {
	const sorted = peers
		.filter((p) => p.lastConnected != null)
		.sort((a, b) => (b.lastConnected ?? 0) - (a.lastConnected ?? 0));
	return n == null ? sorted : sorted.slice(0, n);
}

export function recentPeers(n = 3): Peer[] {
	return historyPeers(n);
}

/** Record an actual connection — stamps history. Does NOT save the device to the
 * address book (Devices); call `addPeer` for that. */
export function recordConnection(id: string, name: string, cat: PeerCategory = 'pc') {
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		existing.lastConnected = Date.now();
		if (name) existing.name = name;
	} else {
		peers.push({ id, name: name || id, cat, fav: false, saved: false, lastConnected: Date.now() });
	}
	persist();
}

/** Manually save a device to the address book (Devices). Marks an existing
 * history-only entry as saved, or adds a new saved one. */
export function addPeer(name: string, id: string, cat: PeerCategory = 'pc'): boolean {
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		if (existing.saved) return false; // already in the address book
		existing.saved = true;
		if (name) existing.name = name;
		persist();
		return true;
	}
	peers.push({ id, name: name || id, cat, fav: false, saved: true, lastConnected: null });
	persist();
	return true;
}

/** Clear connection history (drops history-only entries; keeps saved devices). */
export function clearHistory() {
	for (let i = peers.length - 1; i >= 0; i--) {
		if (!peers[i].saved) peers.splice(i, 1);
		else peers[i].lastConnected = null;
	}
	persist();
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
