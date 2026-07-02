/**
 * gamepad-monitor.js — global connected-controller tracker.
 *
 * The Web Gamepad API (`gamepadconnected` / `navigator.getGamepads()`) only
 * exposes a pad AFTER the user presses a button (a gesture requirement) and is
 * unreliable in the Android WebView — a connected PS5 pad showed up as nothing.
 * So detection is driven by the NATIVE `gamepad_battery` command (Android
 * InputDevice enumeration), which sees connected controllers immediately and
 * carries the battery level. We poll it while the app is foregrounded and also
 * refresh on the Web gamepad events for immediacy.
 *
 *   - toast on connect/disconnect (name + battery),
 *   - live list via connectedPads(),
 *   - a "Controllers" overlay card (game mode).
 */

import { toast } from '../toast.js';
import { t } from '../i18n.js';
import { invoke, hasTauri } from '../tauri.js';

/** Latest native list: [{ name, level }] (level 0..100, or -1 if unknown). */
let _list = [];
let _names = new Set();
let _first = true; // don't toast for pads already connected at startup

/** Strip the verbose "(STANDARD GAMEPAD Vendor: … Product: …)" suffix. */
function shortName(id) {
	const n = String(id || '').replace(/\s*\(.*\)\s*$/, '').trim();
	return n || t('m.gamepad.generic');
}

async function refresh() {
	if (!hasTauri) return;
	let list = [];
	try {
		const resp = await invoke('plugin:pulsar-video|gamepad_battery');
		list = JSON.parse((resp && resp.detail) || '[]');
		if (!Array.isArray(list)) list = [];
	} catch (_) {
		return;
	}
	const names = new Set(list.map((d) => d.name));

	if (_first) {
		_first = false;
	} else {
		for (const d of list) {
			if (!_names.has(d.name)) {
				const bat = d.level >= 0 ? ` · %${d.level}` : '';
				toast(`🎮 ${shortName(d.name)} ${t('m.gamepad.connected')}${bat}`);
				if (navigator.vibrate) { try { navigator.vibrate(20); } catch (_) {} }
			}
		}
		for (const old of _names) {
			if (!names.has(old)) toast(`🎮 ${shortName(old)} ${t('m.gamepad.disconnected')}`);
		}
	}
	_list = list;
	_names = names;
	window.dispatchEvent(new CustomEvent('pulsar-pads-changed'));
}

/** @returns {{name:string,battery:number}[]} */
export function connectedPads() {
	return _list.map((d) => ({
		name: shortName(d.name),
		battery: typeof d.level === 'number' ? d.level : -1,
	}));
}

// ── Polling (foreground only) + Web-event immediacy ───────────────────────────

let _timer = null;
function startPoll() {
	if (_timer) return;
	refresh();
	_timer = setInterval(refresh, 2500);
}
function stopPoll() {
	if (_timer) { clearInterval(_timer); _timer = null; }
}

if (typeof document !== 'undefined') {
	if (document.visibilityState !== 'hidden') startPoll();
	document.addEventListener('visibilitychange', () => {
		if (document.visibilityState === 'hidden') stopPoll(); else startPoll();
	});
}
if (typeof window !== 'undefined') {
	// A button press fires these — refresh immediately so we don't wait for the poll.
	window.addEventListener('gamepadconnected', refresh);
	window.addEventListener('gamepaddisconnected', refresh);
}

// ── Overlay "Controllers" card: the persistent list ───────────────────────────

function _renderCard(el) {
	const pads = connectedPads();
	if (!pads.length) {
		el.innerHTML = `<div class="pad-empty">${t('m.gamepad.none')}</div>`;
		return;
	}
	el.innerHTML = pads.map((p) => {
		const bat = p.battery >= 0 ? `<span class="pad-bat">🔋 %${p.battery}</span>` : '';
		const name = String(p.name).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
		return `<div class="pad-row"><span class="pad-dot"></span><span class="pad-name">${name}</span>${bat}</div>`;
	}).join('');
}

function _register() {
	const reg = window.__pulsarRegisterCard;
	if (typeof reg !== 'function') return false;
	reg({
		id: 'connected-pads',
		modes: ['game'],
		section: 'controllers',
		order: 5,
		mount: (el) => {
			_renderCard(el);
			window.addEventListener('pulsar-pads-changed', () => _renderCard(el));
		},
	});
	return true;
}

if (!_register()) {
	window.addEventListener('overlay-ready', _register);
	if (typeof document !== 'undefined') {
		if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', _register);
		else setTimeout(_register, 0);
	}
}
