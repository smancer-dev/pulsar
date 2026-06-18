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
	/** epoch ms of the last successful REMOTE-desktop connect, or null if never. Drives
	 * the remote Home recents. */
	lastConnected: number | null;
	/** epoch ms of the last successful GAME-streaming connect, or null if never. A
	 * SEPARATE timeline from `lastConnected` so the gaming-mode home shows its own
	 * recents without mixing in remote-desktop connections (the maintainer wants the
	 * gaming list distinct from Devices / remote recents). */
	gameConnected?: number | null;
	/** The peer's LAST-SEEN identity image (data URL, pushed over the session) —
	 * cached so recents / LAN rows / devices show a face without a live session. */
	avatar?: string;
	/** USER-CHOSEN device image (add-device modal): either `icon:<name>` for one of
	 * the built-in line icons, or a small data URL for an uploaded picture. Takes
	 * precedence over the pushed `avatar` when displaying a saved device. */
	image?: string;
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
			// Keep the most recent of EACH timeline independently when merging duplicate
			// ids (a peer connected in both modes carries both stamps).
			prev.gameConnected = Math.max(prev.gameConnected ?? 0, p.gameConnected ?? 0) || null;
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

/** REMOTE connection history — every peer ever connected to in remote-desktop mode,
 * most-recent first. Separate from the saved Devices list (a connection alone never
 * saves a device) and from the GAME history (`gameHistoryPeers`). */
export function historyPeers(n?: number): Peer[] {
	const sorted = peers
		.filter((p) => p.lastConnected != null)
		.sort((a, b) => (b.lastConnected ?? 0) - (a.lastConnected ?? 0));
	return n == null ? sorted : sorted.slice(0, n);
}

/** GAME connection history — every host ever connected to in game-streaming mode,
 * most-recent first. Drives the gaming-mode home's recents (kept distinct from the
 * remote recents and the Devices address book). */
export function gameHistoryPeers(n?: number): Peer[] {
	const sorted = peers
		.filter((p) => p.gameConnected != null)
		.sort((a, b) => (b.gameConnected ?? 0) - (a.gameConnected ?? 0));
	return n == null ? sorted : sorted.slice(0, n);
}

export function recentPeers(n = 3): Peer[] {
	return historyPeers(n);
}

/** Record an actual connection — stamps history. Does NOT save the device to the
 * address book (Devices); call `addPeer` for that. History is capped at
 * `HISTORY_MAX`: beyond it the oldest entries fall out (saved devices stay in the
 * address book, only their history stamp is cleared). */
export function recordConnection(
	id: string,
	name: string,
	cat: PeerCategory = 'pc',
	kind: 'remote' | 'game' = 'remote'
) {
	id = normalizeId(id);
	const now = Date.now();
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		if (kind === 'game') existing.gameConnected = now;
		else existing.lastConnected = now;
		// The caller's name is just the tab label — a generic "Uzak Cihaz" for manual
		// connects, the GAME title for game connects. Only fill in a real name when
		// all we have is the id-derived placeholder; never rename a known peer.
		if (name && (!existing.name || existing.name === fmtPeerId(existing.id))) existing.name = name;
	} else {
		peers.push({
			id,
			name: name || fmtPeerId(id),
			cat,
			fav: false,
			saved: false,
			lastConnected: kind === 'game' ? null : now,
			gameConnected: kind === 'game' ? now : null
		});
	}
	// Cap each timeline independently so neither history grows without bound. An
	// over-cap entry loses only the relevant stamp; it is spliced only when it is
	// unsaved AND has no remaining stamp on the OTHER timeline.
	const capList = kind === 'game' ? gameHistoryPeers() : historyPeers();
	for (const p of capList.slice(HISTORY_MAX)) {
		if (kind === 'game') p.gameConnected = null;
		else p.lastConnected = null;
		if (!p.saved && p.lastConnected == null && p.gameConnected == null)
			peers.splice(peers.indexOf(p), 1);
	}
	persist();
}

/** Drop one entry from the REMOTE connection history (the recents ×): a saved device
 * keeps its address-book entry; an entry still in the game history keeps that stamp.
 * Only fully-orphaned unsaved entries are removed. */
export function removeFromHistory(id: string) {
	const i = peers.findIndex((p) => p.id === normalizeId(id));
	if (i < 0) return;
	peers[i].lastConnected = null;
	if (!peers[i].saved && peers[i].gameConnected == null) peers.splice(i, 1);
	persist();
}

/** Drop one entry from the GAME connection history (gaming-mode recents ×). Mirror of
 * `removeFromHistory` for the game timeline. */
export function removeFromGameHistory(id: string) {
	const i = peers.findIndex((p) => p.id === normalizeId(id));
	if (i < 0) return;
	peers[i].gameConnected = null;
	if (!peers[i].saved && peers[i].lastConnected == null) peers.splice(i, 1);
	persist();
}

/** Manually save a device to the address book (Devices). Marks an existing
 * history-only entry as saved, or adds a new saved one. */
export function addPeer(name: string, id: string, cat: PeerCategory = 'pc', image?: string): boolean {
	id = normalizeId(id);
	const existing = peers.find((p) => p.id === id);
	if (existing) {
		if (existing.saved) return false; // already in the address book
		existing.saved = true;
		if (name) existing.name = name;
		if (image) existing.image = image;
		persist();
		return true;
	}
	peers.push({ id, name: name || id, cat, fav: false, saved: true, lastConnected: null, image });
	persist();
	return true;
}

/** Edit a saved device in place. A changed id re-keys the entry (merging into an
 * existing entry of that id if one exists — saved wins, history stamp kept). */
export function updatePeer(
	id: string,
	patch: { name?: string; newId?: string; image?: string }
): boolean {
	const nid = normalizeId(id);
	const p = peers.find((x) => x.id === nid);
	if (!p) return false;
	if (patch.name !== undefined && patch.name.trim()) p.name = patch.name.trim();
	if (patch.image !== undefined) p.image = patch.image;
	if (patch.newId !== undefined) {
		const target = normalizeId(patch.newId);
		if (target && target !== nid) {
			const clash = peers.find((x) => x.id === target);
			if (clash) {
				clash.saved = clash.saved || p.saved;
				clash.fav = clash.fav || p.fav;
				// Don't clobber the existing device's user-chosen name with this entry's:
				// only fill in when the clash has no real name (still the id placeholder).
				if (p.name && (!clash.name || clash.name === fmtPeerId(clash.id))) clash.name = p.name;
				clash.image = clash.image ?? p.image;
				clash.avatar = clash.avatar ?? p.avatar;
				clash.lastConnected = Math.max(clash.lastConnected ?? 0, p.lastConnected ?? 0) || null;
				peers.splice(peers.indexOf(p), 1);
			} else {
				p.id = target;
			}
		}
	}
	persist();
	return true;
}

/** Whether `id` is already in the address book — reactive (reads the $state list),
 * so Save buttons can hide themselves the moment a device is saved. */
export function isSaved(id: string): boolean {
	const nid = normalizeId(id);
	return peers.some((p) => p.id === nid && p.saved);
}

/** Clear ALL connection history, both timelines (drops history-only entries; keeps
 * saved devices, only clearing their stamps). */
export function clearHistory() {
	for (let i = peers.length - 1; i >= 0; i--) {
		if (!peers[i].saved) peers.splice(i, 1);
		else {
			peers[i].lastConnected = null;
			peers[i].gameConnected = null;
		}
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
		// Exclude the just-pushed entry from the cap so it can never be the one
		// spliced out at the IDENTITY_MAX boundary (its name/avatar set below).
		const ghosts = peers.filter((x) => x !== p && !x.saved && x.lastConnected == null);
		for (const g of ghosts.slice(0, Math.max(0, ghosts.length - (IDENTITY_MAX - 1)))) {
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
