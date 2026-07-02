/**
 * peerMeta.js — cache of identity a remote HOST pushes during a session
 * (its display name via `DataMsg::PeerName` and image via `DataMsg::Avatar`,
 * surfaced by the rust read loop as `peer-name` / `peer-avatar` events).
 *
 * Keyed by the normalised device id, so recents + "on your network" rows can
 * show the host's real name and picture even for devices the user never saved.
 * Distinct from the saved-peers store (user-curated); this is auto-captured.
 *
 * Storage key: 'pulsar.peerMeta.v1'. Shape: { [id]: { name?, image? } }.
 */

const KEY = 'pulsar.peerMeta.v1';

const norm = (id) => String(id || '').replace(/\D/g, '');

function load() {
	try {
		const v = JSON.parse(localStorage.getItem(KEY) || '{}');
		return v && typeof v === 'object' ? v : {};
	} catch (_) {
		return {};
	}
}

function save(map) {
	try {
		localStorage.setItem(KEY, JSON.stringify(map));
	} catch (_) {
		/* quota / private mode — non-fatal */
	}
}

/** @returns {{name?:string,image?:string}|null} */
export function getPeerMeta(id) {
	const n = norm(id);
	return n ? load()[n] || null : null;
}

/** Merge the host's pushed display name for `id`. */
export function setPeerName(id, name) {
	const n = norm(id);
	if (!n || !name) return;
	const map = load();
	map[n] = { ...(map[n] || {}), name };
	save(map);
}

/** Merge the host's pushed image (data URL) for `id`. */
export function setPeerImage(id, image) {
	const n = norm(id);
	if (!n || !image) return;
	const map = load();
	map[n] = { ...(map[n] || {}), image };
	save(map);
}
