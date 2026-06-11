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
	/** The peer's LAST-SEEN identity image (data URL, pushed over the session) —
	 * cached so recents / LAN rows / devices show a face without a live session. */
	avatar?: string;
}

const KEY = 'pulsar.peers.v1';
const hasLS = typeof localStorage !== 'undefined';
/** History keeps at most this many entries (the address book is uncapped). */
const HISTORY_MAX = 20;
/** Identity-only entries (never connected, not saved — created by inbound peers'
 * name/avatar pushes) keep at most this many; they're invisible in the UI and
 * outside the history cap, so without this every inbound client would grow the
 * store (and its base64 avatars) without bound. */
const IDENTITY_MAX = 20;

/** Canonical stored form of a peer id: a relay ID is plain digits (no grouping —
 * GUI connects pass "641 724 395", CLI passes "641724395"; both must be ONE peer).
 * Addresses (IP/IP:port) pass through verbatim. */
export function normalizeId(id: string): string {
	const despaced = id.replace(/\s/g, '');
	return /^\d{9}$/.test(despaced) ? despaced : id.trim();
}

/** Display form: a 9-digit relay ID grouped in threes ("641 724 395"); anything
 * else (IP/IP:port) as-is. */
export function fmtPeerId(id: string): string {
	const n = normalizeId(id);
	return /^\d{9}$/.test(n) ? `${n.slice(0, 3)} ${n.slice(3, 6)} ${n.slice(6)}` : n;
}

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
		// Migrate to canonical ids ("641 724 395" and "641724395" were two peers when
		// one connect came from the CLI): normalize, then merge duplicates (keep the
		// most recent connection; saved wins).
		const byId = new Map<string, Peer>();
		for (const p of list) {
			p.id = normalizeId(p.id);
			const prev = byId.get(p.id);
			if (!prev) {
				byId.set(p.id, p);
				continue;
			}
			prev.saved = prev.saved || p.saved;
			prev.fav = prev.fav || p.fav;
			if ((p.lastConnected ?? 0) > (prev.lastConnected ?? 0)) {
				prev.lastConnected = p.lastConnected;
				if (p.name) prev.name = p.name;
			}
		}
		return [...byId.values()];
	} catch {
		return [];
	}
}

const peers = $state<Peer[]>(load());

function persist() {
	if (!hasLS) return;
	try {
		localStorage.setItem(KEY, JSON.stringify($state.snapshot(peers)));
	} catch {
		// Quota exceeded (the store carries base64 avatars) must never propagate —
		// recordConnection runs inside startConnect's try, and a throw there would
		// tear down a successfully established session. Keep the in-memory store.
	}
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
 * address book (Devices); call `addPeer` for that. History is capped at
 * `HISTORY_MAX`: beyond it the oldest entries fall out (saved devices stay in the
 * address book, only their history stamp is cleared). */
export function recordConnection(id: string, name: string, cat: PeerCategory = 'pc') {
	id = normalizeId(id);
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		existing.lastConnected = Date.now();
		// The caller's name is just the tab label — a generic "Uzak Cihaz" for manual
		// connects, the GAME title for game connects. Only fill in a real name when
		// all we have is the id-derived placeholder; never rename a known peer.
		if (name && (!existing.name || existing.name === fmtPeerId(existing.id))) existing.name = name;
	} else {
		peers.push({ id, name: name || fmtPeerId(id), cat, fav: false, saved: false, lastConnected: Date.now() });
	}
	const over = historyPeers().slice(HISTORY_MAX);
	for (const p of over) {
		if (p.saved) p.lastConnected = null;
		else peers.splice(peers.indexOf(p), 1);
	}
	persist();
}

/** Drop one entry from the connection history (the recents ×): a saved device keeps
 * its address-book entry and only loses the history stamp. */
export function removeFromHistory(id: string) {
	const i = peers.findIndex((p) => p.id === normalizeId(id));
	if (i < 0) return;
	if (peers[i].saved) peers[i].lastConnected = null;
	else peers.splice(i, 1);
	persist();
}

/** Manually save a device to the address book (Devices). Marks an existing
 * history-only entry as saved, or adds a new saved one. */
export function addPeer(name: string, id: string, cat: PeerCategory = 'pc'): boolean {
	id = normalizeId(id);
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

/** Cache a peer's pushed identity (name and/or avatar) against its id — creates a
 * history-less entry if the peer is unknown so the decoration isn't lost. Used by
 * the live `peer-name`/`peer-avatar` pushes; recents/LAN/devices all read it. */
export function setPeerIdentity(id: string, patch: { name?: string; avatar?: string }) {
	const nid = normalizeId(id);
	let p = peers.find((x) => x.id === nid);
	if (!p) {
		p = { id: nid, name: patch.name || fmtPeerId(nid), cat: 'pc', fav: false, saved: false, lastConnected: null };
		peers.push(p);
		// Cap the invisible identity-only entries (oldest first — array order is
		// insertion order) so inbound clients can't grow the store unboundedly.
		const ghosts = peers.filter((x) => !x.saved && x.lastConnected == null);
		for (const g of ghosts.slice(0, Math.max(0, ghosts.length - IDENTITY_MAX))) {
			peers.splice(peers.indexOf(g), 1);
		}
	}
	// A saved device keeps its user-chosen name — the peer's pushed OS name only
	// decorates non-saved entries. The avatar always updates (it's never user-set).
	if (patch.name && !p.saved) p.name = patch.name;
	if (patch.avatar) p.avatar = patch.avatar;
	persist();
}

/** The cached identity image for a peer id (recents / LAN / devices chips). */
export function avatarFor(id: string): string | undefined {
	const nid = normalizeId(id);
	return peers.find((p) => p.id === nid)?.avatar;
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
