/**
 * screens/connect.js — Bağlan (connect) screen
 *
 * W2-connect implementation:
 *  - Ports connectTarget.ts (isAddr / fmtTarget / ipRe / canConnectTarget)
 *  - Stops forcing numeric-only input; allows digits + . + : for IP targets
 *  - Pre-connect mode toggle wiring (remote / game personalities)
 *  - Pre-connect quality presets (Auto / Data-saver / Balanced / Performance)
 *  - Friendly localised error table via t() (connErr.*)
 *  - Re-enables connect button + clears in-session state on reject
 *  - Reads data-mode + config quality and passes them into connect_host
 *  - Listens to conn-phase (routing to connecting overlay) and play-ended
 *
 * Contract (§2, §3):
 *  - Calls invoke('connect_host', { relay, target, password, slot, netmode,
 *      name, mode, codec, fps, bitrateKbps, width, height, quality })
 *    (W2-rust renames `id` → `target` in the Rust command; §2.6)
 *  - Exports doConnect(target, slot, mode?) for use by history/devices screens
 *  - Exports renderHistory() so the history tab can call it
 *  - Does NOT write body[data-mode] directly — delegates to bus.emit('mode-changed', m)
 *    which is processed by router.js (sole writer of data-mode per §1.2)
 */

import { registerScreen }                                  from '../router.js';
import { invoke, listen, hasTauri }                        from '../tauri.js';
import { t }                                               from '../i18n.js';
import { relay, netmode, deviceName, codec, quality, getConfig } from '../store/config.js';
import { isSaved, savedPeers }                              from '../store/peers.js';
import { getPeerMeta, setPeerName, setPeerImage }          from '../store/peerMeta.js';
import { getPref }                                         from '../store/prefs.js';
import { start as startConnecting, cancel as cancelConnecting } from './connecting.js';
import { toast }                                           from '../toast.js';

// Slot → connected device id (set in doConnect) so the host-identity events below
// cache under the right id.
const _slotTarget = {};

// The remote HOST pushes its display name + avatar over the session (peer-name /
// peer-avatar, surfaced by the rust read loop). Cache them per device id so the
// recents + "on your network" rows show the host's real name and picture even for
// devices the user never explicitly saved. Registered once at module load.
if (hasTauri) {
	listen('peer-name', ({ slot, name }) => {
		const id = _slotTarget[slot];
		if (id && name) { setPeerName(id, name); try { renderRecentQuick(); renderLanSection(); } catch (_) {} }
	});
	listen('peer-avatar', ({ slot, dataUrl }) => {
		const id = _slotTarget[slot];
		if (id && dataUrl) { setPeerImage(id, dataUrl); try { renderRecentQuick(); renderLanSection(); } catch (_) {} }
	});
}

// bus is accessed via window to avoid circular import (app.js → connect.js ← app.js)
const getBus = () => window.__pulsarBus || null;

// ---------------------------------------------------------------------------
// connectTarget.ts port — isAddr / fmtTarget / ipRe / canConnectTarget
// ---------------------------------------------------------------------------

/**
 * Returns true if the value looks like an IP address or IP:port (contains
 * '.' or ':').  Used to branch between relay-ID and direct-address paths.
 * @param {string} v
 * @returns {boolean}
 */
export const isAddr = (v) => /[.:]/.test(v);

/**
 * Canonical input formatting:
 *  - Address (has '.' or ':'): keep only digits / dots / colons, max 21 chars
 *    (covers "255.255.255.255:65535").
 *  - Relay ID (digits only): strip non-digits, cap at 9, group in threes.
 * @param {string} v
 * @returns {string}
 */
export const fmtTarget = (v) =>
	isAddr(v)
		? v.replace(/[^0-9.:]/g, '').slice(0, 21)
		: v
				.replace(/\D/g, '')
				.slice(0, 9)
				.replace(/(\d{3})(?=\d)/g, '$1 ')
				.trim();

/**
 * IPv4 (optionally :port) regex.  Used by canConnectTarget.
 * @type {RegExp}
 */
export const ipRe = /^\d{1,3}(\.\d{1,3}){3}(:\d{1,5})?$/;

/**
 * Returns true if the value is a connectable target:
 *  - Full IPv4 (optionally :port), OR
 *  - At least 6 relay-ID digits (partial ID the relay can resolve).
 * @param {string} v
 * @returns {boolean}
 */
export const canConnectTarget = (v) =>
	isAddr(v) ? ipRe.test(v.trim()) : v.replace(/\D/g, '').length >= 6;

// ---------------------------------------------------------------------------
// Quality preset definitions
// Pre-connect quality presets (Auto / Data-saver / Balanced / Performance)
// map to (width, height, fps, bitrateKbps, quality) for StreamReq.
// These are sent to connect_host at connect time; live restream controls
// land in W3-quality-js.
// ---------------------------------------------------------------------------

/**
 * @typedef {{ width:number, height:number, fps:number, bitrateKbps:number, quality:string }} QParams
 */

/** @type {Record<string, QParams>} */
const QUALITY_PRESETS = {
	auto:          { width: 1920, height: 1080, fps: 60,  bitrateKbps: 0,     quality: 'latency'  },
	'data-saver':  { width: 1280, height: 720,  fps: 30,  bitrateKbps: 3000,  quality: 'latency'  },
	balanced:      { width: 1920, height: 1080, fps: 60,  bitrateKbps: 8000,  quality: 'balanced' },
	performance:   { width: 1920, height: 1080, fps: 120, bitrateKbps: 20000, quality: 'latency'  },
};

// Game mode overrides: always force latency quality regardless of preset
// (per §2.6 and the two-modes product rule in CLAUDE.md)
/** @type {Record<string, QParams>} */
const GAME_QUALITY_OVERRIDES = Object.fromEntries(
	Object.entries(QUALITY_PRESETS).map(([k, v]) => [k, { ...v, quality: 'latency' }])
);

// ---------------------------------------------------------------------------
// Friendly connect-error mapper — port of sessions.svelte.ts::friendlyConnectError
// Maps raw Rust ConnError strings → localised copy via t(connErr.*)
// ---------------------------------------------------------------------------

const CONN_TIMEOUT_MARKER = 'connect-timed-out';

/**
 * Map a raw error string (from a Tauri invoke rejection or ConnectResult.detail)
 * to a friendly i18n string.
 * @param {string} raw
 * @returns {string}
 */
function friendlyConnectError(raw) {
	const m = String(raw).toLowerCase();
	if (m.includes(CONN_TIMEOUT_MARKER))                      return t('connErr.timeout');
	if (m.includes('relay did not respond'))                  return t('connErr.relayDown');
	if (m.includes('could not be reached via the relay'))     return t('connErr.peerUnreachable');
	if (m.includes('not registered with a relay yet'))        return t('connErr.notOnline');
	if (m.includes('p2p connection failed'))                  return t('connErr.p2pFailed');
	if (m.includes('incompatible version')
	    || m.includes('requires a newer version'))            return t('connErr.incompatibleVersion');
	// Fallback: for address targets use 'unreachable'; for ID targets use 'peerUnreachable'
	if (isAddr(raw))                                          return t('connErr.unreachable');
	return t('connErr.peerUnreachable');
}

// ---------------------------------------------------------------------------
// localStorage (LS) helper — for history only.  Config values come from
// store/config.js (which is the authoritative Rust-backed store).
// ---------------------------------------------------------------------------

const LS = {
	get: (k, d) => {
		try { return JSON.parse(localStorage.getItem('pulsar.' + k)) ?? d; }
		catch { return localStorage.getItem('pulsar.' + k) ?? d; }
	},
	set: (k, v) => localStorage.setItem('pulsar.' + k, typeof v === 'string' ? v : JSON.stringify(v)),
};

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);

function say(text, isErr) {
	const msg = $('msg');
	if (!msg) return;
	msg.textContent = text || '';
	msg.classList.toggle('err', !!isErr);
}

/**
 * Return the raw (unformatted) target value from the input:
 * - For addresses: return as-is (keep dots/colons)
 * - For relay IDs: strip spaces / formatting
 * @returns {string}
 */
function rawTarget() {
	const v = ($('id')?.value || '').trim();
	return isAddr(v) ? v : v.replace(/\D/g, '');
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

function addHistory(id) {
	const h = LS.get('history', []).filter((x) => x.id !== id);
	h.unshift({ id, ts: Date.now() });
	LS.set('history', h.slice(0, 12));
	renderRecentQuick();
}

/**
 * Format a stored ID/address for display: group-of-3 for digit-only IDs;
 * pass addresses through unchanged.
 * @param {string} id
 * @returns {string}
 */
function fmtDisplay(id) {
	if (/^\d+$/.test(id)) return id.replace(/(\d{3})(?=\d)/g, '$1 ').trim();
	return id;
}

function ago(ts) {
	const s = (Date.now() - ts) / 1000;
	if (s < 60)    return t('devices.justNow');
	if (s < 3600)  return t('devices.minAgo',  { n: Math.floor(s / 60)   });
	if (s < 86400) return t('devices.hourAgo', { n: Math.floor(s / 3600) });
	return t('devices.dayAgo', { n: Math.floor(s / 86400) });
}

function itemHtml(x) {
	// Prefer a real name (user-saved, else the host's pushed PeerName) over the bare
	// id. With a name, the id drops to the secondary line next to the date.
	const nid = String(x.id || '').replace(/\D/g, '');
	const saved = savedPeers().find((p) => String(p.id).replace(/\D/g, '') === nid);
	const meta = getPeerMeta(nid);
	const nm = ((saved && saved.name) || (meta && meta.name) || '').trim();
	const esc = (s) => String(s).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
	const dot = `<span class="rdot ${isLanOnline(x.id) ? 'on' : ''}" title="${isLanOnline(x.id) ? t('home.online') : t('home.offline')}"></span>`;
	const primary = nm ? esc(nm) : fmtDisplay(x.id);
	const sub = nm ? `${fmtDisplay(x.id)} · ${ago(x.ts)}` : ago(x.ts);
	return `<div class="item" data-id="${x.id}">
		<div class="ic">${rowAvatar(x.id, nm || null)}</div>
		<div class="meta"><div class="id">${dot}${primary}</div><div class="when">${sub}</div></div>
		<svg class="go" width="20" height="20" viewBox="0 0 24 24" fill="none"><path d="M9 6l6 6-6 6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
	</div>`;
}

// ── LAN discovery: "on your network" list + presence dots on recents ─────────

let _lanDevices = [];          // [{ id, name, addr }] from invoke('lan_devices')
let _lanIds = new Set();        // normalized (digits-only) ids currently on the LAN
let _lanPollTimer = null;

/** True if a relay id is currently reachable on this LAN (drives the green dot). */
function isLanOnline(id) {
	const n = String(id || '').replace(/\D/g, '');
	return n.length > 0 && _lanIds.has(n);
}

function lanItemHtml(d) {
	const target = d.id ? String(d.id).replace(/\D/g, '') : d.addr;
	const name = (d.name || t('home.remoteDevice')).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
	const sub = d.id ? fmtDisplay(d.id) : d.addr;
	return `<div class="item" data-target="${target}">
		<div class="ic">${rowAvatar(d.id, d.name)}</div>
		<div class="meta"><div class="id"><span class="rdot on" title="${t('home.online')}"></span>${name}</div><div class="when">${sub}</div></div>
		<svg class="go" width="20" height="20" viewBox="0 0 24 24" fill="none"><path d="M9 6l6 6-6 6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
	</div>`;
}

/** Render the "On your network" section from the latest LAN scan. */
function renderLanSection() {
	const el = $('lan-quick');
	if (!el) return;
	if (!_lanDevices.length) { el.innerHTML = ''; return; }
	el.innerHTML =
		`<div class="sect-label" style="margin:18px 2px 8px;">${t('home.onNetwork')}</div>` +
		'<div class="row-list">' + _lanDevices.map(lanItemHtml).join('') + '</div>';
	el.querySelectorAll('.item').forEach((row) => {
		const target = row.dataset.target;
		const dev = _lanDevices.find((d) => (d.id ? String(d.id).replace(/\D/g, '') : d.addr) === target);
		attachRowGestures(row,
			() => doConnect(target, 0),
			() => openDeviceSheet({ id: dev && dev.id, name: dev && dev.name, addr: dev && dev.addr, source: 'lan' }));
	});
}

async function pollLan() {
	if (!hasTauri) return;
	let list = [];
	try { list = await invoke('lan_devices', {}); } catch (_) {}
	_lanDevices = Array.isArray(list) ? list : [];
	_lanIds = new Set(_lanDevices.map((d) => String(d.id || '').replace(/\D/g, '')).filter(Boolean));
	renderLanSection();
	renderRecentQuick(); // refresh the presence dots
}

function startLanPoll() {
	pollLan();
	if (_lanPollTimer) clearInterval(_lanPollTimer);
	_lanPollTimer = setInterval(pollLan, 5000);
}

// ── Long-press → device details sheet (save / connect / remove) ──────────────

const _esc = (v) => String(v).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
let _sheetDev = null; // { id?, name?, addr?, ts?, source: 'recent'|'lan' }

/** Row avatar (desktop parity): a saved custom photo if the device is saved with
 *  one; else initials of its name; else a generic device glyph. */
function rowAvatar(id, name) {
	const nid = String(id || '').replace(/\D/g, '');
	const saved = nid ? savedPeers().find((p) => String(p.id).replace(/\D/g, '') === nid) : null;
	const meta = nid ? getPeerMeta(nid) : null;
	// User-saved image wins; else the host's pushed avatar; else initials; else glyph.
	const img = (saved && saved.image) || (meta && meta.image);
	if (img) {
		return `<img class="row-avatar-img" src="${String(img).replace(/"/g, '&quot;')}" alt="" />`;
	}
	const display = ((saved && saved.name) || name || (meta && meta.name) || '').trim();
	const init = display.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join('').toUpperCase();
	if (init) return `<span class="row-avatar-init">${_esc(init)}</span>`;
	return `<svg width="18" height="18" viewBox="0 0 24 24" fill="none"><rect x="6" y="3" width="12" height="18" rx="2.5" stroke="currentColor" stroke-width="2"/></svg>`;
}

/** Attach tap (connect) + long-press / right-click (details) gestures to a row. */
function attachRowGestures(row, onTap, onHold) {
	let timer = null, held = false, sx = 0, sy = 0;
	const fire = () => { held = true; if (navigator.vibrate) try { navigator.vibrate(25); } catch (_) {} onHold(); };
	const cancel = () => { if (timer) { clearTimeout(timer); timer = null; } };
	row.addEventListener('touchstart', (e) => {
		held = false; sx = e.touches[0].clientX; sy = e.touches[0].clientY;
		cancel(); timer = setTimeout(fire, 500);
	}, { passive: true });
	row.addEventListener('touchmove', (e) => {
		if (Math.abs(e.touches[0].clientX - sx) > 12 || Math.abs(e.touches[0].clientY - sy) > 12) cancel();
	}, { passive: true });
	row.addEventListener('touchend', cancel);
	row.addEventListener('touchcancel', cancel);
	row.addEventListener('click', () => { if (held) { held = false; return; } onTap(); });
	row.addEventListener('contextmenu', (e) => { e.preventDefault(); fire(); });
}

function buildDeviceSheet() {
	if (document.getElementById('dev-sheet')) return;
	const backdrop = document.createElement('div');
	backdrop.id = 'dev-backdrop';
	backdrop.className = 'sheet-backdrop';
	const sheet = document.createElement('div');
	sheet.id = 'dev-sheet';
	sheet.className = 'sheet';
	sheet.setAttribute('role', 'dialog');
	sheet.setAttribute('aria-modal', 'true');
	sheet.innerHTML = `
		<div class="sheet-handle"></div>
		<div>
			<h3 id="dev-title" style="font-family:var(--font-display);font-size:19px;font-weight:700;letter-spacing:-0.02em;margin:0 0 14px;word-break:break-word"></h3>
			<div class="relay-rows" id="dev-rows"></div>
		</div>
		<div style="display:flex;flex-direction:column;gap:10px">
			<button id="dev-connect" class="btn btn-primary" style="font-size:16px;padding:14px"></button>
			<button id="dev-save" class="btn btn-ghost full" style="font-size:15px;padding:13px"></button>
			<button id="dev-remove" class="btn btn-ghost full" style="font-size:15px;padding:13px;color:var(--danger)"></button>
		</div>
	`;
	document.body.appendChild(backdrop);
	document.body.appendChild(sheet);
	backdrop.addEventListener('click', closeDeviceSheet);
	sheet.querySelector('#dev-connect').addEventListener('click', () => {
		const d = _sheetDev; closeDeviceSheet();
		if (d) doConnect(d.id ? String(d.id).replace(/\D/g, '') : d.addr, 0);
	});
	sheet.querySelector('#dev-save').addEventListener('click', () => {
		const d = _sheetDev; if (!d || !d.id) return;
		const idNorm = String(d.id).replace(/\D/g, '');
		closeDeviceSheet();
		// Reuse the Devices tab's add-device popup (name + category picker), pre-filled.
		import('./devices.js').then((m) => m.openAddSheet({ id: idNorm, name: d.name })).catch(() => {});
	});
	sheet.querySelector('#dev-remove').addEventListener('click', () => {
		const d = _sheetDev;
		if (d && d.id) { LS.set('history', LS.get('history', []).filter((x) => x.id !== d.id)); renderRecentQuick(); }
		closeDeviceSheet();
	});
}

function fillDeviceSheet(dev) {
	_sheetDev = dev;
	const idNorm = dev.id ? String(dev.id).replace(/\D/g, '') : '';
	const online = isLanOnline(idNorm);
	document.getElementById('dev-title').textContent =
		dev.name || (idNorm ? fmtDisplay(idNorm) : dev.addr) || t('m.dev.details');
	const rows = [];
	if (idNorm) rows.push([t('home.deviceId'), fmtDisplay(idNorm), true]);
	if (dev.addr) rows.push([t('relay.address'), dev.addr, true]);
	rows.push([t('relay.statusLabel'), online ? t('home.online') : t('home.offline'), false]);
	if (dev.ts) rows.push([t('m.dev.lastSeen'), ago(dev.ts), false]);
	document.getElementById('dev-rows').innerHTML = rows.map(([k, v, mono]) =>
		`<div class="relay-row"><span class="relay-k">${k}</span><span class="relay-v ${mono ? 'mono' : ''}">${_esc(v)}</span></div>`
	).join('');
	document.getElementById('dev-connect').textContent = t('home.connect');
	// Save button: shown ONLY when the device isn't already saved (no "Saved" state).
	const saveBtn = document.getElementById('dev-save');
	if (!idNorm || isSaved(idNorm)) {
		saveBtn.style.display = 'none';
	} else {
		saveBtn.style.display = ''; saveBtn.disabled = false; saveBtn.textContent = t('home.saveRecent');
	}
	document.getElementById('dev-remove').textContent = t('home.removeRecent');
	document.getElementById('dev-remove').style.display = dev.source === 'recent' ? '' : 'none';
}

function openDeviceSheet(dev) {
	buildDeviceSheet();
	fillDeviceSheet(dev);
	document.getElementById('dev-backdrop')?.classList.add('open');
	document.getElementById('dev-sheet')?.classList.add('open');
}

function closeDeviceSheet() {
	document.getElementById('dev-sheet')?.classList.remove('open');
	document.getElementById('dev-backdrop')?.classList.remove('open');
}

/**
 * Render the full history list (used by the history tab / devices screen).
 */
export function renderHistory() {
	const listEl = $('history-list');
	if (!listEl) return;
	const h = LS.get('history', []);
	listEl.innerHTML = h.length
		? h.map(itemHtml).join('')
		: `<div class="empty">${t('home.noRecents')}</div>`;
	listEl.querySelectorAll('.item').forEach((el) => {
		el.addEventListener('click', () => {
			const idEl = $('id');
			if (idEl) idEl.value = fmtDisplay(el.dataset.id);
			document.querySelector('nav.bottom button[data-tab=connect]')?.click();
			doConnect(el.dataset.id, 0);
		});
	});
}

function renderRecentQuick() {
	const el = $('recent-quick');
	if (!el) return;
	const h = LS.get('history', []).slice(0, 2);
	el.innerHTML = h.length
		? `<div class="sect-label" style="margin:18px 2px 8px;">${t('home.recents')}</div>` +
		  '<div class="row-list">' + h.map(itemHtml).join('') + '</div>'
		: '';
	el.querySelectorAll('.item').forEach((row) => {
		const id = row.dataset.id;
		const hist = LS.get('history', []).find((x) => x.id === id);
		attachRowGestures(row,
			() => doConnect(id, 0),
			() => openDeviceSheet({ id, ts: hist && hist.ts, source: 'recent' }));
	});
}

// ---------------------------------------------------------------------------
// Quality preset UI
// ---------------------------------------------------------------------------

let _selectedPreset = 'auto';

function mountQualityPresets() {
	const container = $('quality-presets');
	if (!container) return;

	const presets = [
		{ key: 'auto',        labelKey: 'm.presetAuto'    },
		{ key: 'data-saver',  labelKey: 'm.presetData'    },
		{ key: 'balanced',    labelKey: 'm.presetBalance' },
		{ key: 'performance', labelKey: 'm.presetPerf'    },
	];

	container.innerHTML =
		`<div class="sect-label" style="margin:16px 0 8px;">${t('settings.quality')}</div>` +
		'<div class="seg pill-row" role="group" aria-label="' + t('settings.quality') + '">' +
		presets.map((p) =>
			`<button data-preset="${p.key}" aria-selected="${p.key === _selectedPreset ? 'true' : 'false'}" ` +
			`style="min-height: var(--touch-min, 44px);">${t(p.labelKey)}</button>`
		).join('') +
		'</div>';

	container.querySelectorAll('button[data-preset]').forEach((btn) => {
		btn.addEventListener('click', () => {
			_selectedPreset = btn.dataset.preset;
			container.querySelectorAll('button[data-preset]').forEach((b) => {
				b.setAttribute('aria-selected', String(b.dataset.preset === _selectedPreset));
			});
		});
	});
}

/**
 * Return the quality preset values to merge into connect_host args, taking
 * the active mode into account.  Game mode always forces latency quality.
 * @param {'remote'|'game'} mode
 * @returns {QParams}
 */
function resolveQualityParams(mode) {
	const presetKey = _selectedPreset in QUALITY_PRESETS ? _selectedPreset : 'auto';
	let params = mode === 'game'
		? { ...GAME_QUALITY_OVERRIDES[presetKey] }
		: { ...QUALITY_PRESETS[presetKey] };

	// When preset is 'auto' and mode is remote, honour the persisted config quality
	if (presetKey === 'auto' && mode !== 'game') {
		params = { ...params, quality: quality() || 'balanced' };
	}
	// Frame rate + resolution: the explicit Settings selections are the authority
	// (override the quality preset's fps/size). 'auto' → host default / display refresh.
	const res = resolveResolution();
	params = { ...params, fps: resolveFrameRate(), width: res.width, height: res.height };
	return params;
}

/** Resolve the user's resolution pref into {width,height} (0,0 = host default). */
export function resolveResolution() {
	switch (getPref('resolution')) {
		case '720':  return { width: 1280, height: 720 };
		case '1080': return { width: 1920, height: 1080 };
		case '1440': return { width: 2560, height: 1440 };
		case '2160': return { width: 3840, height: 2160 };
		default:     return { width: 1920, height: 1080 }; // mobile 'auto' → 1080p (host-native 1440p+ is too heavy for a phone link → NVENC overload + lag)
	}
}

/** Resolve the user's frame-rate pref into a connect fps (0 = unlimited/host max). */
export function resolveFrameRate() {
	const fr = getPref('frameRate');
	if (fr === 'unlimited') return 0;
	const n = typeof fr === 'number' ? fr : parseInt(fr, 10);
	if (Number.isFinite(n) && n > 0) return n;
	return _refreshHz || 60; // 'auto' → panel refresh (the raw AU socket now sustains 120fps)
}

// Display refresh rate (Hz) from the native plugin — the 'auto' frame-rate target.
// The WebView's rAF is throttled when idle (~26 Hz) so it can't be trusted; the
// native Display.getRefreshRate is authoritative. 0 until fetched.
let _refreshHz = 0;
export function refreshHz() { return _refreshHz; }
(async function fetchRefreshHz() {
	if (!hasTauri) return;
	try {
		const r = await invoke('plugin:pulsar-video|screen_refresh_rate');
		const hz = parseFloat(r && r.detail);
		if (hz >= 24 && hz <= 360) _refreshHz = Math.round(hz);
	} catch (_) { /* fallback stays 0 → resolveFrameRate uses 60 */ }
})();

// ---------------------------------------------------------------------------
// doConnect — core connect orchestration (exported for devices/history tabs)
// ---------------------------------------------------------------------------

/**
 * Connect to a target (relay ID or IP / IP:port) in the given slot.
 *
 * @param {string} target           — relay 9-digit ID (digits only) or "ip" / "ip:port"
 * @param {number} slot             — 0 or 1
 * @param {'remote'|'game'} [modeOverride]  — if omitted, reads body[data-mode]
 */
export async function doConnect(target, slot, modeOverride) {
	if (!hasTauri) { say('window.__TAURI__ yok', true); return; }

	// Normalise target
	const normTarget = isAddr(String(target).trim())
		? String(target).trim()
		: String(target).replace(/\D/g, '');

	// Validate. A tap can come from a Devices-tab row (e.g. a saved entry with a
	// malformed ID) where the Connect-screen `say()` line isn't visible — surface a
	// toast too so the tap isn't silently ignored.
	if (!canConnectTarget(normTarget)) {
		say(t('home.idOrIp'), true);
		toast(t('home.idOrIp'));
		return;
	}

	// Remember which device this slot is connected to, so the host's pushed
	// identity (peer-name / peer-avatar events) can be cached under the right id.
	_slotTarget[slot] = normTarget;

	const connectBtn = $('connect');
	if (connectBtn) connectBtn.disabled = true;
	say(t('status.connecting'));

	// Read mode from body[data-mode] (set by router), or use override
	const mode = modeOverride || document.body.dataset.mode || 'remote';

	// Show the connecting overlay (phase popup: reaching → transport → auth →
	// preparing) for the WHOLE connection process — no matter where the connect was
	// triggered (the Connect form OR a tap on a Devices-tab row). It subscribes to
	// the conn-phase events and is hidden again on success/failure below.
	try { startConnecting(normTarget, /** @type {'remote'|'game'} */(mode), slot); } catch (_) {}

	// Resolve quality preset
	const qParams = resolveQualityParams(/** @type {'remote'|'game'} */(mode));

	// Config values (cached synchronously after boot getConfig())
	const relayAddr = relay();
	const nm        = netmode();
	const name      = deviceName();
	const codecPref = codec();

	try {
		const r = await invoke('connect_host', {
			relay:       relayAddr,
			target:      normTarget,
			// Always start with no password; if the host requires one it replies
			// NeedPassword → 'auth-prompt' event → session.js shows the OTP sheet.
			password:    '',
			slot,
			netmode:     nm,
			name,
			mode,
			codec:       codecPref,
			fps:         qParams.fps,
			bitrateKbps: qParams.bitrateKbps,
			width:       qParams.width,
			height:      qParams.height,
			quality:     qParams.quality,
			hdr:         getPref('hdr') === true,
		});

		if (connectBtn) connectBtn.disabled = false;

		if (r && r.ok) {
			// Update transport pill to actual (not configured) transport
			const barMeta = $('bar-meta');
			if (barMeta) {
				const transport = r.transport === 'direct'
					? t('m.session.transport.direct')
					: t('m.session.transport.relay');
				barMeta.textContent = (r.codec || '?').toUpperCase() + ' · ' + transport;
			}
			// Register the session slot via startSession() — NOT a bare
			// bus.emit('session-started'). startSession pushes the entry into the
			// session `registry` and THEN emits 'session-started' itself, so every
			// existing reactor (router, overlay, bar) still fires. Emitting the bus
			// event directly skipped the registry.push, leaving `registry` empty —
			// and play-firstframe/-stall/-ended all early-return on a missing entry
			// (`if (!entry) return`). The net symptom: the "Host görüntüyü hazırlıyor"
			// waiting overlay never cleared even though video was decoding (its hide is
			// inside the play-firstframe handler, past that guard). Dynamic import
			// mirrors the config.js pattern below and dodges the session.js↔connect.js
			// import cycle (session.js imports doConnect from here).
			const bus = getBus();
			if (bus) {
				const { startSession } = await import('../session/session.js');
				startSession({ slot, id: normTarget, codec: r.codec, mode, transport: r.transport });
			} else {
				// Fallback for test/browser environments without the bus yet
				document.body.classList.add('in-session');
			}
			// Session UI now owns the screen → hide the connecting overlay.
			try { cancelConnecting(false); } catch (_) {}
			// Record in history for primary slot only
			if (slot === 0) addHistory(normTarget);
			say('');
		} else {
			// ok=false without throw: host rejected cleanly
			if (connectBtn) connectBtn.disabled = false;
			// Clear any in-session state that may have been set optimistically
			document.body.classList.remove('in-session');
			try { cancelConnecting(false); } catch (_) {}
			const msg = friendlyConnectError(r?.detail || 'peer rejected');
			say(msg, true);
			toast(msg); // visible regardless of which tab the connect was started from
		}
	} catch (e) {
		if (connectBtn) connectBtn.disabled = false;
		// Clear in-session state so the user can try again
		document.body.classList.remove('in-session');
		try { cancelConnecting(false); } catch (_) {}
		const raw = e && e.message ? e.message : String(e);
		const msg = friendlyConnectError(raw);
		say(msg, true);
		toast(msg); // visible regardless of which tab the connect was started from
	}
}

// ---------------------------------------------------------------------------
// Segmented-control helper
// ---------------------------------------------------------------------------

function seg(el, initVal, onPick) {
	if (!el) return;
	const apply = (v) => el.querySelectorAll('button').forEach(
		(b) => b.setAttribute('aria-selected', String(b.dataset.v === v))
	);
	apply(initVal);
	el.querySelectorAll('button').forEach((b) =>
		b.addEventListener('click', () => { apply(b.dataset.v); onPick(b.dataset.v); })
	);
}

// ---------------------------------------------------------------------------
// Mount (called once by router when the tab is first shown)
// ---------------------------------------------------------------------------

function mount() {
	const idEl       = $('id');
	const pasteBtn   = $('paste-id');
	const connectBtn = $('connect');

	// --- Input: accept digits + dots + colons (IDs and IP addresses) ---
	if (idEl) {
		// Switch to generic text inputmode so '.' and ':' are accessible on
		// soft keyboards; the fmtTarget formatter keeps the canonical form.
		idEl.setAttribute('inputmode', 'url');
		idEl.setAttribute('aria-label', t('home.targetAria'));
		// Keep the short "000 000 000" placeholder; the longer "ID veya IP …"
		// guidance lives in the small hint line below the field (desktop parity).
		const hintEl = $('id-hint');
		if (hintEl) hintEl.textContent = t('home.idOrIp');

		idEl.addEventListener('input', () => {
			const raw       = idEl.value;
			const formatted = fmtTarget(raw);
			if (raw === formatted) return;
			// Preserve the caret across re-grouping. The "000 000 000" spaces are
			// inserted by the formatter, so restoring the raw index makes the caret
			// jump whenever a space shifts in/out before it (the reported bug).
			// Instead, map the caret by how many NON-SPACE chars precede it and
			// re-place it after that many non-space chars in the formatted value.
			const caret      = idEl.selectionEnd ?? raw.length;
			const keepBefore = raw.slice(0, caret).replace(/ /g, '').length;
			idEl.value = formatted;
			let pos = 0;
			if (keepBefore > 0) {
				let seen = 0;
				pos = formatted.length;
				for (let i = 0; i < formatted.length; i++) {
					if (formatted[i] !== ' ') {
						seen++;
						if (seen === keepBefore) { pos = i + 1; break; }
					}
				}
			}
			try { idEl.setSelectionRange(pos, pos); } catch (_) {}
		});
	}

	// --- Connect button ---
	connectBtn?.addEventListener('click', () => doConnect(rawTarget(), 0));

	// --- Paste ---
	pasteBtn?.addEventListener('click', async () => {
		try {
			// The Android WebView denies navigator.clipboard.readText() ("Read
			// permission denied"), so go through the native plugin (text in `detail`).
			let txt;
			if (hasTauri) {
				const res = await invoke('plugin:pulsar-video|read_clipboard', {});
				txt = (res && res.detail) || '';
			} else {
				txt = await navigator.clipboard.readText();
			}
			if (!txt) { say(t('session.clipboardEmpty'), true); return; }
			const clean = fmtTarget(txt.trim());
			const norm  = isAddr(clean) ? clean : clean.replace(/\D/g, '');
			if (canConnectTarget(norm)) {
				if (idEl) idEl.value = clean;
				say('');
			} else {
				say(t('home.idOrIp'), true);
			}
		} catch (_) { say(t('session.clipboardError'), true); }
	});

	// --- Mode toggle buttons —
	// bus.emit so router.js (sole writer of data-mode) handles the class swap.
	$('m-remote')?.addEventListener('click', () => {
		const b = getBus();
		if (b) b.emit('mode-changed', 'remote');
	});
	$('m-game')?.addEventListener('click', () => {
		const b = getBus();
		if (b) b.emit('mode-changed', 'game');
	});

	// --- Settings (netmode, relay, deviceName, codec) wired to config store ---
	const relayEl = $('relay');
	if (relayEl) {
		relayEl.value = relay();
		relayEl.addEventListener('change', () => {
			import('../store/config.js').then(({ setConfig }) =>
				setConfig({ relay: relayEl.value.trim() }).catch(() => {})
			);
		});
	}
	const devnameEl = $('devname');
	if (devnameEl) {
		devnameEl.value = deviceName();
		devnameEl.addEventListener('change', () => {
			import('../store/config.js').then(({ setConfig }) =>
				setConfig({ deviceName: devnameEl.value.trim() }).catch(() => {})
			);
		});
	}

	const netPillLabels = {
		auto:         'P2P → relay',
		'p2p-only':   t('settings.modeP2p'),
		'relay-only':  t('settings.modeRelay'),
	};
	const netPillEl = $('net-pill');
	if (netPillEl) netPillEl.textContent = netPillLabels[netmode()] || netPillLabels.auto;

	seg($('netmode'), netmode(), (v) => {
		import('../store/config.js').then(({ setConfig }) =>
			setConfig({ networkMode: v }).catch(() => {})
		);
		if (netPillEl) netPillEl.textContent = netPillLabels[v] || netPillLabels.auto;
	});
	seg($('codecpref'), codec(), (v) => {
		import('../store/config.js').then(({ setConfig }) =>
			setConfig({ codecPref: v }).catch(() => {})
		);
	});

	// --- Quality presets ---
	mountQualityPresets();

	// --- Listen: play-ended — re-enable the button + clear in-session on drop ---
	// W2-session (session.js) handles the full teardown; this is a safety net
	// so that if session.js is not yet loaded the button is still unblocked.
	listen('play-ended', (payload) => {
		const btn = $('connect');
		if (btn) btn.disabled = false;
		// Only remove in-session if this is the primary slot (slot 0) or
		// we have no active multi-session (session.js will handle multi-slot).
		if (!payload || payload.slot === 0) {
			document.body.classList.remove('in-session');
		}
		// Show a brief "disconnected" hint in the msg div if it was empty
		const msg = $('msg');
		if (msg && !msg.textContent) {
			say(t('m.session.disconnected'), false);
		}
	}).catch(() => {});

	// --- Listen: conn-phase — forward to connecting overlay (W2-connecting) ---
	// We don't consume conn-phase here directly; the connecting overlay screen
	// (js/screens/connecting.js) subscribes to it.  We wire it here so the
	// connecting overlay has been imported and is listening before we invoke.
	// (No-op if connecting.js is not yet imported — bus degrades gracefully.)

	// --- Initial renders ---
	renderRecentQuick();
	startRelayPoll();
	startLanPoll();
	// Re-translate the relay-status text live when the language changes.
	window.addEventListener('langchange', pollRelayHealth);
	// Tap the relay pill → open the connection-status detail sheet.
	const relayPill = document.getElementById('relay-status');
	if (relayPill) {
		relayPill.setAttribute('role', 'button');
		relayPill.setAttribute('tabindex', '0');
		relayPill.style.cursor = 'pointer';
		relayPill.addEventListener('click', openRelaySheet);
		relayPill.addEventListener('keydown', (e) => {
			if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openRelaySheet(); }
		});
	}
}

let _relayPollTimer = null;
let _relayState = null; // last probe result: { healthy, latencyMs } | null while first check pending

/** Probe relay reachability and reflect it in the #relay-status pill. */
async function pollRelayHealth() {
	const el = document.getElementById('relay-status');
	const txt = document.getElementById('relay-status-text');
	if (!el || !txt) return;
	if (!hasTauri) { el.style.display = 'none'; return; }
	el.className = 'relay-status checking';
	txt.textContent = t('relay.checking');
	// Mirror the "checking" state into the open detail sheet so an in-sheet refresh
	// visibly does something even when the relay state is unchanged (the status text
	// would otherwise stay "Ready" the whole probe and look like nothing happened).
	if (document.getElementById('relay-sheet')?.classList.contains('open')) {
		const sv0 = document.getElementById('relay-v-status');
		const lv0 = document.getElementById('relay-v-latency');
		if (sv0) sv0.innerHTML = `<span class="rs-dot" style="background:var(--warn)"></span>${t('relay.checking')}`;
		if (lv0) lv0.textContent = '—';
	}
	let r = null;
	const _t0 = Date.now();
	// Pass '' so Rust probes its persisted config relay — authoritative and already
	// current at boot, unlike the JS relay() getter which is empty/stale until the
	// async config store finishes loading (that produced a false "unreachable").
	try { r = await invoke('relay_health', { relay: '' }); } catch (_) {}
	// A LAN relay answers in a few ms, so the "checking" state would vanish before
	// it's visible — a manual refresh then looks like a no-op. Hold "checking" for a
	// brief minimum so the dot visibly cycles (top pill + detail sheet).
	const _dt = Date.now() - _t0;
	if (_dt < 350) await new Promise((res) => setTimeout(res, 350 - _dt));
	_relayState = (r && r.healthy) ? { healthy: true, latencyMs: r.latencyMs } : { healthy: false, latencyMs: null };
	if (_relayState.healthy) {
		el.className = 'relay-status ok';
		txt.textContent = `${t('relay.healthy')} · ${_relayState.latencyMs} ms`;
	} else {
		el.className = 'relay-status down';
		txt.textContent = t('relay.unreachable');
	}
	// Keep the detail sheet live if it's open.
	if (document.getElementById('relay-sheet')?.classList.contains('open')) fillRelaySheet();
}

function startRelayPoll() {
	pollRelayHealth();
	if (_relayPollTimer) clearInterval(_relayPollTimer);
	_relayPollTimer = setInterval(pollRelayHealth, 20000);
}

// ── Relay status detail sheet (tap the pill) ─────────────────────────────────

/** Lazily build the relay-status bottom sheet + backdrop (appended to <body>). */
function buildRelaySheet() {
	if (document.getElementById('relay-sheet')) return;
	const backdrop = document.createElement('div');
	backdrop.id = 'relay-backdrop';
	backdrop.className = 'sheet-backdrop';
	const sheet = document.createElement('div');
	sheet.id = 'relay-sheet';
	sheet.className = 'sheet';
	sheet.setAttribute('role', 'dialog');
	sheet.setAttribute('aria-modal', 'true');
	sheet.innerHTML = `
		<div class="sheet-handle"></div>
		<div>
			<h3 style="font-family:var(--font-display);font-size:19px;font-weight:700;letter-spacing:-0.02em;margin:0 0 14px">
				${t('relay.title')}
			</h3>
			<div class="relay-rows">
				<div class="relay-row"><span class="relay-k">${t('relay.statusLabel')}</span><span class="relay-v" id="relay-v-status"></span></div>
				<div class="relay-row"><span class="relay-k">${t('relay.latency')}</span><span class="relay-v" id="relay-v-latency">—</span></div>
				<div class="relay-row"><span class="relay-k">${t('relay.address')}</span><span class="relay-v mono" id="relay-v-addr">—</span></div>
			</div>
		</div>
		<button id="relay-refresh" class="btn btn-ghost full" style="font-size:15px;padding:13px;margin-top:4px">
			${t('relay.refresh')}
		</button>
	`;
	document.body.appendChild(backdrop);
	document.body.appendChild(sheet);
	backdrop.addEventListener('click', closeRelaySheet);
	sheet.querySelector('#relay-refresh').addEventListener('click', () => pollRelayHealth());
}

/** Populate the open sheet from the last probe + the config relay address. */
async function fillRelaySheet() {
	const sv = document.getElementById('relay-v-status');
	const lv = document.getElementById('relay-v-latency');
	const av = document.getElementById('relay-v-addr');
	if (!sv) return;
	const st = _relayState;
	if (!st) {
		sv.innerHTML = `<span class="rs-dot" style="background:var(--warn)"></span>${t('relay.checking')}`;
		lv.textContent = '—';
	} else if (st.healthy) {
		sv.innerHTML = `<span class="rs-dot" style="background:var(--ok)"></span>${t('relay.healthy')}`;
		lv.textContent = st.latencyMs + ' ms';
	} else {
		sv.innerHTML = `<span class="rs-dot" style="background:var(--danger)"></span>${t('relay.unreachable')}`;
		lv.textContent = '—';
	}
	try { const cfg = await getConfig(); av.textContent = (cfg && cfg.relay) || '—'; } catch (_) {}
}

function openRelaySheet() {
	buildRelaySheet();
	fillRelaySheet();
	document.getElementById('relay-backdrop')?.classList.add('open');
	document.getElementById('relay-sheet')?.classList.add('open');
}

function closeRelaySheet() {
	document.getElementById('relay-sheet')?.classList.remove('open');
	document.getElementById('relay-backdrop')?.classList.remove('open');
}

function onShow() {
	// Refresh recents list on every tab visit
	renderRecentQuick();
	// Re-probe the relay + rescan the LAN so the indicators are fresh.
	pollRelayHealth();
	pollLan();
}

// ---------------------------------------------------------------------------
// Screen registration
// ---------------------------------------------------------------------------

registerScreen({
	id:           'connect',
	navIcon:      '<svg viewBox="0 0 24 24" fill="none"><path d="M13 5l7 7-7 7M4 12h15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
	navLabel:     t('nav.connect'),
	navLabelKey:  'nav.connect',
	mount,
	onShow,
	// Exposed so the history tab (handled by renderHistory) can call it
	_renderHistory: renderHistory,
});
