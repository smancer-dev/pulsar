/**
 * prefs.js — client-only UI/playback preferences that the rust config doesn't own.
 *
 * Kept OUT of the config store (config.js) on purpose: those fields round-trip
 * through `get_config`/`set_config`, which would drop any key the rust side
 * doesn't return. These are pure local prefs (localStorage), read directly by the
 * feature that uses them (the HUD, the audio sink).
 *
 * Storage key: 'pulsar.prefs.v1'.
 */

const KEY = 'pulsar.prefs.v1';

const DEFAULTS = {
	hudVisible: true,        // show the in-session performance HUD strip
	overlayButton: true,     // show the floating overlay button (FAB); else 3-finger tap
	playHostAudio: true,     // play the host's streamed audio (false = muted)
	frameRate: 'auto',       // stream fps: 'auto' (=display refresh) | 'unlimited' | 30|60|120|144|168|244
	resolution: 'auto',      // 'auto' (host default) | '720' | '1080' | '1440' | '2160'
	hdr: false,              // request HDR (HEVC Main10 / PQ) from the host
	gamepadTargets: {},      // controller name → host emulation: 'auto' | 'xbox' | 'ds4'
};

/** Host emulation target for a controller name ('auto' | 'xbox' | 'ds4'). */
export function gamepadTarget(name) {
	const m = getPref('gamepadTargets');
	return (m && m[name]) || 'auto';
}

/** Set the host emulation target for a controller name. */
export function setGamepadTarget(name, target) {
	const m = { ...(getPref('gamepadTargets') || {}) };
	m[name] = target;
	setPref('gamepadTargets', m);
}

function load() {
	try {
		const v = JSON.parse(localStorage.getItem(KEY) || '{}');
		return { ...DEFAULTS, ...(v && typeof v === 'object' ? v : {}) };
	} catch (_) {
		return { ...DEFAULTS };
	}
}

/** @param {keyof typeof DEFAULTS} k */
export function getPref(k) {
	const v = load()[k];
	return v === undefined ? DEFAULTS[k] : v;
}

/** @param {keyof typeof DEFAULTS} k */
export function setPref(k, v) {
	const map = load();
	map[k] = v;
	try {
		localStorage.setItem(KEY, JSON.stringify(map));
	} catch (_) {
		/* quota / private mode — non-fatal */
	}
}
