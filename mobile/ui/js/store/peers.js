/**
 * store/peers.js — Pulsar Mobile
 *
 * Port of desktop src/lib/peers.svelte.ts to a plain ES module (no bundler,
 * no Svelte $state). Reactive listeners receive a 'change' event on the
 * exported `peerEvents` EventTarget whenever the list mutates.
 *
 * Storage key: 'pulsar.peers.v1' (compatible with the desktop store).
 *
 * Exports (§3 contract):
 *   normalizeId, fmtPeerId,
 *   savedPeers, historyPeers, gameHistoryPeers,
 *   recordConnection, addPeer, updatePeer, removePeer,
 *   removeFromHistory, toggleFav, clearHistory,
 *   (plus: allPeers, isSaved, setPeerIdentity, avatarFor,
 *    removeFromGameHistory, renameGamePeer, setGamePeerImage, peerEvents, _reset)
 */

const KEY = 'pulsar.peers.v1';
const HISTORY_MAX = 20;
const IDENTITY_MAX = 20;

/** EventTarget that fires a 'change' event after every mutation. */
export const peerEvents = new EventTarget();

// ---------------------------------------------------------------------------
// ID helpers
// ---------------------------------------------------------------------------

/** Canonical stored form of a peer id: 9-digit relay IDs are stripped of all
 * whitespace; addresses (IP / IP:port) pass through verbatim. */
export function normalizeId(id) {
  const despaced = id.replace(/\s/g, '');
  return /^\d{9}$/.test(despaced) ? despaced : id.trim();
}

/** Display form: a 9-digit relay ID grouped in threes ("641 724 395");
 * anything else (IP/IP:port) as-is. */
export function fmtPeerId(id) {
  const n = normalizeId(id);
  return /^\d{9}$/.test(n) ? `${n.slice(0, 3)} ${n.slice(3, 6)} ${n.slice(6)}` : n;
}

// ---------------------------------------------------------------------------
// Internal state (plain array — no Svelte reactivity needed)
// ---------------------------------------------------------------------------

/** @type {Array<{
 *   id: string,
 *   name: string,
 *   cat: 'pc'|'server'|'console',
 *   fav: boolean,
 *   saved: boolean,
 *   lastConnected: number|null,
 *   gameConnected?: number|null,
 *   avatar?: string,
 *   image?: string,
 * }>} */
let peers = [];

// Lazy-load once on first import so the module can be required in a test
// environment that sets up localStorage before importing.
(function load() {
  if (typeof localStorage === 'undefined') return;
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return;
    /** @type {any[]} */
    const list = JSON.parse(raw);

    // Migration: pre-`saved` entries
    for (const p of list) {
      if (typeof p.saved !== 'boolean') p.saved = p.lastConnected == null;
    }

    // Migration: normalize ids and merge duplicates
    const byId = new Map();
    for (const p of list) {
      p.id = normalizeId(p.id);
      const prev = byId.get(p.id);
      if (!prev) {
        byId.set(p.id, p);
        continue;
      }
      prev.saved = prev.saved || p.saved;
      prev.fav = prev.fav || p.fav;
      prev.gameConnected =
        Math.max(prev.gameConnected ?? 0, p.gameConnected ?? 0) || null;
      if ((p.lastConnected ?? 0) > (prev.lastConnected ?? 0)) {
        prev.lastConnected = p.lastConnected;
        if (p.name) prev.name = p.name;
      }
    }
    peers = [...byId.values()];
  } catch {
    peers = [];
  }
})();

function persist() {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(KEY, JSON.stringify(peers));
  } catch {
    // Quota exceeded (store carries base64 avatars) — swallow silently so a
    // successful connect() is never torn down by a storage write failure.
  }
  peerEvents.dispatchEvent(new Event('change'));
}

// ---------------------------------------------------------------------------
// Read-only views
// ---------------------------------------------------------------------------

/** All peers (address book + history). */
export function allPeers() {
  return peers.slice();
}

/** Saved devices — only ones the user explicitly added to the address book. */
export function savedPeers() {
  return peers.filter((p) => p.saved);
}

/** REMOTE connection history, most-recent first.
 * @param {number} [n] — slice to first n entries when given. */
export function historyPeers(n) {
  const sorted = peers
    .filter((p) => p.lastConnected != null)
    .sort((a, b) => (b.lastConnected ?? 0) - (a.lastConnected ?? 0));
  return n == null ? sorted : sorted.slice(0, n);
}

/** GAME connection history, most-recent first.
 * @param {number} [n] — slice to first n entries when given. */
export function gameHistoryPeers(n) {
  const sorted = peers
    .filter((p) => p.gameConnected != null)
    .sort((a, b) => (b.gameConnected ?? 0) - (a.gameConnected ?? 0));
  return n == null ? sorted : sorted.slice(0, n);
}

/** Whether `id` is in the address book. */
export function isSaved(id) {
  const nid = normalizeId(id);
  return peers.some((p) => p.id === nid && p.saved);
}

/** The cached avatar (data URL) for a peer, or undefined. */
export function avatarFor(id) {
  const nid = normalizeId(id);
  return peers.find((p) => p.id === nid)?.avatar;
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/** Record an actual connection — stamps history. Does NOT save to the address
 * book; call addPeer() for that. Each timeline (remote / game) is capped
 * independently at HISTORY_MAX.
 *
 * @param {string} id
 * @param {string} name
 * @param {'pc'|'server'|'console'} [cat]
 * @param {'remote'|'game'} [kind]
 */
export function recordConnection(id, name, cat = 'pc', kind = 'remote') {
  id = normalizeId(id);
  const now = Date.now();
  const existing = peers.find((p) => p.id === id);
  if (existing) {
    if (kind === 'game') existing.gameConnected = now;
    else existing.lastConnected = now;
    // Only fill in a real name when all we have is the id-derived placeholder;
    // never rename a known peer with a generic label.
    if (name && (!existing.name || existing.name === fmtPeerId(existing.id))) {
      existing.name = name;
    }
  } else {
    peers.push({
      id,
      name: name || fmtPeerId(id),
      cat,
      fav: false,
      saved: false,
      lastConnected: kind === 'game' ? null : now,
      gameConnected: kind === 'game' ? now : null,
    });
  }

  // Cap each timeline independently.
  const capList = kind === 'game' ? gameHistoryPeers() : historyPeers();
  for (const p of capList.slice(HISTORY_MAX)) {
    if (kind === 'game') p.gameConnected = null;
    else p.lastConnected = null;
    if (!p.saved && p.lastConnected == null && p.gameConnected == null) {
      peers.splice(peers.indexOf(p), 1);
    }
  }
  persist();
}

/** Drop one entry from the REMOTE connection history. A saved device keeps its
 * address-book entry; an entry still in the game history keeps that stamp. */
export function removeFromHistory(id) {
  const i = peers.findIndex((p) => p.id === normalizeId(id));
  if (i < 0) return;
  peers[i].lastConnected = null;
  if (!peers[i].saved && peers[i].gameConnected == null) peers.splice(i, 1);
  persist();
}

/** Drop one entry from the GAME connection history. Mirror of removeFromHistory. */
export function removeFromGameHistory(id) {
  const i = peers.findIndex((p) => p.id === normalizeId(id));
  if (i < 0) return;
  peers[i].gameConnected = null;
  if (!peers[i].saved && peers[i].lastConnected == null) peers.splice(i, 1);
  persist();
}

/** Rename a game-recent in place. No-op when the id isn't found or name is blank. */
export function renameGamePeer(id, name) {
  const i = peers.findIndex((p) => p.id === normalizeId(id));
  if (i < 0) return;
  const n = name.trim();
  if (!n) return;
  peers[i].name = n;
  persist();
}

/** Set a game-recent's user-chosen image (`icon:<name>` or a data URL). */
export function setGamePeerImage(id, image) {
  const i = peers.findIndex((p) => p.id === normalizeId(id));
  if (i < 0) return;
  peers[i].image = image;
  persist();
}

/** Manually save a device to the address book (Devices screen). Marks an
 * existing history-only entry as saved, or creates a new saved entry.
 * Returns false if the id was already in the address book (no-op).
 *
 * @param {string} name
 * @param {string} id
 * @param {'pc'|'server'|'console'} [cat]
 * @param {string} [image]
 * @returns {boolean}
 */
export function addPeer(name, id, cat = 'pc', image) {
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

/** Edit a saved device in place. A changed id re-keys the entry (merging into
 * an existing entry of that id when one exists — saved wins, history kept).
 *
 * @param {string} id
 * @param {{ name?: string, newId?: string, image?: string }} patch
 * @returns {boolean}
 */
export function updatePeer(id, patch) {
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
        // Don't clobber the existing device's user-chosen name unless it's
        // still the id-derived placeholder.
        if (p.name && (!clash.name || clash.name === fmtPeerId(clash.id))) {
          clash.name = p.name;
        }
        clash.image = clash.image ?? p.image;
        clash.avatar = clash.avatar ?? p.avatar;
        clash.lastConnected =
          Math.max(clash.lastConnected ?? 0, p.lastConnected ?? 0) || null;
        peers.splice(peers.indexOf(p), 1);
      } else {
        p.id = target;
      }
    }
  }
  persist();
  return true;
}

/** Remove a device from the address book AND all history. */
export function removePeer(id) {
  const i = peers.findIndex((p) => p.id === normalizeId(id));
  if (i >= 0) {
    peers.splice(i, 1);
    persist();
  }
}

/** Toggle the favourite flag for a peer. */
export function toggleFav(id) {
  const p = peers.find((x) => x.id === normalizeId(id));
  if (p) {
    p.fav = !p.fav;
    persist();
  }
}

/** Clear ALL connection history (both timelines). Saved devices keep their
 * address-book entry but lose their timestamps. */
export function clearHistory() {
  for (let i = peers.length - 1; i >= 0; i--) {
    if (!peers[i].saved) {
      peers.splice(i, 1);
    } else {
      peers[i].lastConnected = null;
      peers[i].gameConnected = null;
    }
  }
  persist();
}

/** Cache a peer's pushed identity (name and/or avatar). Creates a
 * history-less entry when the peer is unknown so the decoration isn't lost.
 * A saved device keeps its user-chosen name — only the avatar always updates.
 *
 * @param {string} id
 * @param {{ name?: string, avatar?: string }} patch
 */
export function setPeerIdentity(id, patch) {
  const nid = normalizeId(id);
  let p = peers.find((x) => x.id === nid);
  if (!p) {
    p = {
      id: nid,
      name: patch.name || fmtPeerId(nid),
      cat: 'pc',
      fav: false,
      saved: false,
      lastConnected: null,
    };
    peers.push(p);
    // Cap invisible identity-only entries (oldest insertion order first) so
    // inbound clients can't grow the store — and therefore its base64 avatars —
    // without bound.
    const ghosts = peers.filter(
      (x) =>
        x !== p &&
        !x.saved &&
        x.lastConnected == null &&
        x.gameConnected == null
    );
    for (const g of ghosts.slice(0, Math.max(0, ghosts.length - (IDENTITY_MAX - 1)))) {
      peers.splice(peers.indexOf(g), 1);
    }
  }
  if (patch.name && !p.saved) p.name = patch.name;
  if (patch.avatar) p.avatar = patch.avatar;
  persist();
}

// ---------------------------------------------------------------------------
// Test helper
// ---------------------------------------------------------------------------

/** Reset the store to empty — for unit tests only. */
export function _reset() {
  peers.splice(0, peers.length);
  if (typeof localStorage !== 'undefined') localStorage.removeItem(KEY);
}
