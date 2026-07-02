/**
 * app.js — Pulsar Mobile application entry point
 *
 * Exports:
 *   bus  — EventTarget singleton for JS→JS events (bus.emit/on)
 *
 * Boots:
 *   1. i18n (lang detection)
 *   2. config store (initial load from Rust)
 *   3. router (wire bottom-nav + Tauri conn-phase listener for net-pill)
 *   4. screen modules (self-register via registerScreen())
 *   5. session in-session bar wiring
 *   6. touch forwarding (verbatim from original index.html)
 *
 * The import list is the ONLY thing that grows when a new screen or session
 * module is added. No other structural change is needed.
 *
 * JS→JS event bus convention:
 *   bus.emit(name, detail)  — dispatch
 *   bus.on(name, cb)        — subscribe (cb receives detail)
 *   Standard events: 'mode-changed', 'session-started', 'session-ended',
 *                    'session-bg', 'gamepad-active', 'net-transport'
 *
 * W2-shell-glue additions:
 *   - Import screens/connecting.js  (W2-connecting — self-registers)
 *   - Import session/session.js     (W2-session — self-registers + manages lifecycle)
 *   - Pass tauri module to initRouter so it can listen to 'conn-phase'
 *   - After connect_host resolves, emit 'net-transport' so the pill updates
 *   - btnEnd now calls end_session (§2.5) before detach
 */

// Theme first: applies <html data-theme> + native bar style at import, before the
// screens render, so there's no light→dark flash on boot.
import './theme.js';
import { lang, setLang, t, applyI18n } from './i18n.js';
import { getConfig, netmode } from './store/config.js';
import { initRouter, show, premount, setMode, enterSession, exitSession, setNetPill } from './router.js';
import * as tauri            from './tauri.js';
const { invoke, hasTauri, listen } = tauri;

// Screen modules — each calls registerScreen() at import time
import './screens/connect.js';
import './screens/host.js';
import './screens/settings.js';
import './screens/devices.js';

// Global swipe-down-to-dismiss for every bottom sheet (app-wide, self-installing).
import './sheet-swipe.js';
import { startGamepadNav } from './gamepad-nav.js';

// W2 modules — connecting overlay + session lifecycle
// These files are created by W2-connecting and W2-session lanes.
// Guarded with a dynamic import so a missing file degrades gracefully
// rather than blocking the whole boot.
const _importConnecting = import('./screens/connecting.js').catch(() => null);
const _importSession    = import('./session/session.js').catch(() => null);

// Cache the resolved session module so synchronous hot-path code (touch routing)
// can read the live session registry / active slot without re-importing.
let _sessionMod = null;
_importSession.then((m) => { _sessionMod = m; });

// In-session chrome (W3/W5): the overlay menu, the perf HUD / info strip, the
// quality controls and the feature panels. Each self-mounts on import + listens
// for session events — but none of them were ever imported, so the in-session UI
// (overlay, info bar, bandwidth/quality controls) never appeared. overlay.js first
// so its registerCard() is ready when the panels self-register; each guarded so a
// single broken module can't block the session.
const _importOverlay = import('./session/overlay.js').catch(() => null);
_importOverlay.then(() => import('./session/hud.js')).catch(() => null);
_importOverlay.then(() => import('./session/quality.js')).catch(() => null);
_importOverlay.then(() => import('./session/display.js')).catch(() => null);
_importOverlay.then(() => import('./session/audio.js')).catch(() => null);
_importOverlay.then(() => import('./session/sidechannels.js')).catch(() => null);
_importOverlay.then(() => import('./session/files.js')).catch(() => null);
// Global connected-controller monitor (toast on connect + the "Controllers" list).
import('./session/gamepad-monitor.js').catch(() => null);

// ---- Event bus ---------------------------------------------------------------

/**
 * Minimal JS→JS event bus backed by a plain EventTarget.
 * Usage:
 *   bus.emit('mode-changed', 'game')
 *   bus.on('mode-changed', (detail) => …)
 */
const _et = new EventTarget();

export const bus = {
	/**
	 * Dispatch a named event with an optional payload.
	 * @param {string} name
	 * @param {*} [detail]
	 */
	emit(name, detail) {
		_et.dispatchEvent(Object.assign(new Event(name), { detail }));
	},
	/**
	 * Subscribe to a named event.
	 * @param {string} name
	 * @param {(detail: any) => void} cb
	 * @returns {() => void} unsubscribe function
	 */
	on(name, cb) {
		const handler = (e) => cb(e.detail);
		_et.addEventListener(name, handler);
		return () => _et.removeEventListener(name, handler);
	},
	// Expose raw EventTarget for modules that need addEventListener directly
	addEventListener: _et.addEventListener.bind(_et),
	removeEventListener: _et.removeEventListener.bind(_et),
};

// ---- Bootstrap ---------------------------------------------------------------

/** Tap the top "P2P → relay" pill → a brief explanation of the auto connection model. */
function wireNetPillInfo() {
	const pill = document.getElementById('net-pill');
	if (!pill) return;
	pill.style.cursor = 'pointer';
	pill.setAttribute('role', 'button');
	pill.addEventListener('click', () => {
		if (document.getElementById('netpill-sheet')) return;
		// Mode-aware: explain what the CURRENTLY-selected network mode actually does,
		// not just the general P2P→relay model. Mode comes from the live config cache.
		const mode = netmode();
		const bodyKey = mode === 'p2p-only'   ? 'm.netpill.bodyP2p'
		              : mode === 'relay-only' ? 'm.netpill.bodyRelay'
		              : 'm.netpill.bodyAuto';
		const backdrop = document.createElement('div');
		backdrop.className = 'sheet-backdrop';
		const sheet = document.createElement('div');
		sheet.className = 'sheet';
		sheet.id = 'netpill-sheet';
		sheet.setAttribute('role', 'dialog');
		sheet.setAttribute('aria-modal', 'true');
		sheet.innerHTML =
			'<div class="sheet-handle"></div>' +
			'<div style="font-size:17px;font-weight:700;margin:2px 2px 8px;">' + t('m.netpill.title') + '</div>' +
			'<div style="font-size:14px;color:var(--text-muted);line-height:1.5;margin:0 2px 14px;">' + t(bodyKey) + '</div>' +
			'<div style="font-size:12.5px;color:var(--text-faint);line-height:1.4;margin:0 2px 18px;">' + t('m.netpill.change') + '</div>' +
			'<button class="btn btn-primary" id="netpill-ok">' + t('m.common.ok') + '</button>';
		document.body.appendChild(backdrop);
		document.body.appendChild(sheet);
		requestAnimationFrame(() => { backdrop.classList.add('open'); sheet.classList.add('open'); });
		const close = () => {
			backdrop.classList.remove('open'); sheet.classList.remove('open');
			setTimeout(() => { backdrop.remove(); sheet.remove(); }, 280);
		};
		backdrop.addEventListener('click', close);
		sheet.querySelector('#netpill-ok').addEventListener('click', close);
	});
}

async function boot() {
	// 1. i18n — already loaded at module parse time; nothing async needed.

	// 2. Config — kick off async load; screens read the cache synchronously.
	getConfig().catch(() => { /* non-fatal — falls back to localStorage */ });

	// 3. Router — wire bottom-nav + bus listeners + Tauri conn-phase listener.
	// Pass the tauri module so the router can subscribe to 'conn-phase' and
	// update the net-pill with the real transport (Direct / Relay) before auth.
	initRouter(bus, tauri);

	// 3b. Pre-mount the host screen so its incoming-connection listener
	// (session-request → approval modal) + the approval sheet are live from
	// boot, regardless of which tab is active. Without this the modal only
	// works after the user has opened Cihazım at least once.
	premount('host');

	// 3c. Pre-mount the connect screen too. initRouter() only marks the boot
	// tab active (it doesn't call mount() for the already-visible .tab.on), so
	// connect's mount() — which wires the form AND starts the relay-health
	// indicator poll — otherwise wouldn't run until the user navigates away and
	// back. Guarded by router (_mounted) so navigating to connect won't re-wire.
	premount('connect');

	// 3d. Translate the static shell (data-i18n in index.html) to the active
	// language, and keep it in sync when the user switches language in Settings.
	// JS-rendered screens (settings/host/devices) translate via their own t()
	// renders; this covers the hardcoded connect screen + in-session bar.
	applyI18n(document);
	window.addEventListener('langchange', () => applyI18n(document));

	// 3e. Net pill (P2P → relay) tap → explain the automatic connection model.
	wireNetPillInfo();

	// 3f. Controller navigation of the whole app UI (D-pad/stick + A/B).
	startGamepadNav();

	// 4. In-session bar wiring.
	wireSessionBar();

	// 5. Touch forwarding (verbatim from original index.html inline script).
	wireTouchForwarding();

	// 6. Wire Tauri 'play-ended' → bus 'play-ended'. session.js listens to the Tauri
	// 'play-ended' directly and, via _dropSlot, is the SOLE driver of the bus
	// 'session-ended' — which it emits only when the LAST live pane ends. Emitting
	// 'session-ended' here on every per-pane play-ended tore down the in-session
	// chrome (and every module that cleans up on 'session-ended') while a sibling
	// split pane was still live.
	listen('play-ended', (payload) => {
		// payload: {slot, reason}
		bus.emit('play-ended', payload);
	}).catch(() => { /* no Tauri — ok */ });
}

// ---- In-session control bar --------------------------------------------------

/**
 * Update bar-meta with codec + real transport information.
 * Called after connect_host resolves with a result containing transport.
 * @param {{ codec?: string, mos?: boolean, transport?: string }} result
 */
export function updateBarMeta(result) {
	const barMeta = document.getElementById('bar-meta');
	if (!barMeta || !result) return;
	const codecPart = result.codec ? result.codec.toUpperCase() : '?';
	let transportPart;
	if (result.transport === 'direct') {
		transportPart = 'P2P';
	} else if (result.transport === 'relay') {
		transportPart = 'relay';
	} else if (result.mos) {
		transportPart = 'oturum';
	} else {
		transportPart = 'p2p';
	}
	barMeta.textContent = codecPart + ' · ' + transportPart;
	// Also update the net-pill to reflect the real transport
	if (result.transport) {
		setNetPill(result.transport);
		bus.emit('net-transport', result.transport);
	}
}

/**
 * Track the active slot for the current session (used by split / end_session).
 * Set by session.js or connect.js when a session starts.
 */
let _activeSlot = 0;

/** @param {number} slot */
export function setActiveSlot(slot) {
	_activeSlot = slot;
}

function wireSessionBar() {
	let splitActive = false;

	const btnSplit = document.getElementById('btn-split');
	const btnEnd   = document.getElementById('btn-end');

	btnSplit?.addEventListener('click', () => {
		splitActive = true;
		// W5-split will fully wire this; for now delegate to connect.js target
		const idEl = document.getElementById('id');
		const idVal = idEl ? idEl.value.replace(/\D/g, '') : '';
		if (idVal.length === 9) {
			invoke('connect_host', {
				relay:   localStorage.getItem('pulsar.relay') || '',
				target:  idVal,
				password: '', // host prompts via 'auth-prompt' → OTP sheet if needed
				slot:    1,
				netmode: localStorage.getItem('pulsar.netmode') || 'auto',
				name:    localStorage.getItem('pulsar.name') || '',
				mode:    document.body.dataset.mode || 'remote',
				codec:   localStorage.getItem('pulsar.codec') || 'auto',
				fps:     0,
				bitrateKbps: 0,
				width:   0,
				height:  0,
				quality: 'balanced',
				hdr:     false,
			}).catch((e) => console.warn('[app] split connect error', e));
		}
	});

	btnEnd?.addEventListener('click', async () => {
		if (!hasTauri) {
			// Dev/browser mode — just clear the in-session state via bus
			splitActive = false;
			bus.emit('session-ended');
			return;
		}
		// Delegate teardown to session.js so the multi-session logic (detach only
		// when it's the last live pane; otherwise release just this slot and switch
		// panes) lives in ONE place. endSession ends session.js's authoritative
		// active slot — app.js's own _activeSlot is not kept in sync across panes.
		const sessionMod = await _importSession;
		if (sessionMod && typeof sessionMod.endSession === 'function') {
			try {
				const slot = sessionMod.activeSlot ? sessionMod.activeSlot() : _activeSlot;
				await sessionMod.endSession(slot);
			} catch (e) {
				console.warn('[app] endSession error', e);
			}
			splitActive = false;
			return;
		}
		// Fallback (session.js unavailable): §2.5 end_session FIRST, then detach.
		try {
			await invoke('end_session', { slot: _activeSlot });
		} catch (e) {
			console.warn('[app] end_session error', e);
		}
		try {
			await invoke('plugin:pulsar-video|detach');
		} catch (e) {
			console.warn('[app] detach error', e);
		}
		splitActive = false;
		bus.emit('session-ended');
	});

	// Expose splitActive getter for touch wiring
	window.__pulsarSplitActive = () => splitActive;
}

// ---- Touch forwarding --------------------------------------------------------
// Verbatim from original index.html inline script; refactored to use
// the splitActive accessor but logic unchanged.

function wireTouchForwarding() {
	// Skip touches that land on Pulsar UI (control bar, FAB, overlay dock/backdrop,
	// touch-mode button, sheets, bottom nav) — otherwise dragging the FAB or tapping
	// the overlay leaks a click/drag to the host (looked like text selection).
	// B1: the on-screen gamepad layer (game mode) sits over the video; its controls
	// receive pointer events, but the window-level touch listeners below ALSO fire
	// for the same touches and would forward them to the host as mouse clicks. Skip
	// any touch that lands on the pad layer so it never leaks into the host pointer.
	const SKIP_SEL = '.bar, .fab, .overlay-dock, .overlay-backdrop, .touch-mode-btn, .sheet, .sheet-overlay, .sheet-backdrop, nav.bottom, .gamepad-layer, #gamepad-layer';
	const MAX_ZOOM = 4;
	const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
	const W = () => innerWidth, H = () => innerHeight;

	// Pinch-zoom/pan (AnyDesk/RustDesk-style). Local-only: the host stream is
	// untouched; we magnify the native surface and map touches back to frame coords.
	// State is the video's destination rect on screen, in CSS px (slot 0 only). It's
	// aspect-correct: `vid` (the decoded size, from the native `video-size` event)
	// gives the fit rect; pinch scales it about the gesture's anchor + pans. We send
	// the rect NORMALIZED so the CSS-px webview and device-px native surface agree
	// regardless of devicePixelRatio.
	let vid = { vw: 0, vh: 0 }; // decoded video size, 0 = unknown
	let rect = null;            // {x,y,w,h} CSS px, null until video-size known
	let zfac = 1;               // zoom factor over fit (for the badge + pinch math)
	let pinch = null;           // active 2-finger pinch/pan state (for updatePinch)
	let multi = false;          // ≥2 fingers down at any point this gesture

	// ── Pointer mode (overlay-toggleable) ──────────────────────────────────────
	// 'mouse' = trackpad/relative: a local cursor moves by finger delta, tap=click
	//           at the cursor (precise, AnyDesk default). 'touch' = absolute: the
	//           host pointer follows the finger, tap=click where you touch.
	const MODE_KEY = 'pulsar.input.mode.v1';
	let mode = (() => { try { return localStorage.getItem(MODE_KEY) === 'touch' ? 'touch' : 'mouse'; } catch (_) { return 'mouse'; } })();
	let cur = null;             // virtual cursor {x,y} CSS px (mouse mode), kept in rect
	let cursorEl = null;

	// Gesture bookkeeping
	const TAP_SLOP = 10, LONG_MS = 480, DBL_MS = 280, TAP_MAX_MS = 600, DRAG_DBL_MS = 360;
	const PINCH_THRESH = 16, SCROLL_SLOP = 6, SCROLL_DIV = 2.4, REL_SENS = 1.5;
	let g1 = null;             // 1-finger: {sx,sy,x,y,t,moved,dragging}
	let g2 = null;             // 2-finger: {d0, mid0, type, lastMid}
	let longTimer = null;
	let lastTap = null;

	const inSession = () => hasTauri && document.body.classList.contains('in-session');
	// Split touch-routing is active whenever ≥2 sessions are live, regardless of
	// which UI opened the split: the legacy #btn-split set window.__pulsarSplitActive,
	// but the overlay Split card / split.js target picker never did. The native layout
	// stacks the panes top/bottom, so norm() maps the top half → slot 0 and the bottom
	// half → slot 1 to match. Falls back to the legacy flag if the registry isn't up yet.
	const isSplit = () => {
		if (_sessionMod && _sessionMod.registry && _sessionMod.registry.length >= 2) return true;
		return !!(window.__pulsarSplitActive && window.__pulsarSplitActive());
	};
	const uiBlocked = (e) =>
		document.body.classList.contains('overlay-open') ||
		document.body.classList.contains('fab-dragging') ||
		(e.target && e.target.closest && e.target.closest(SKIP_SEL));

	// B1: while the on-screen gamepad is active (game mode), every touch belongs to
	// the pad — forwarding to the host too leaks spurious mouse input and ~doubles
	// the send traffic (send_gamepad + send_pointer per touch). gamepad.js emits
	// 'gamepad-active' on show/hide; gate the whole live touch path on it.
	let gamepadActive = false;
	if (bus && bus.on) bus.on('gamepad-active', (d) => { gamepadActive = !!(d && d.active); });

	// Aspect-fit rect (zoom factor 1) for the current video + screen, CSS px.
	const fitRect = () => {
		if (!vid.vw || !vid.vh) return null;
		const Wd = W(), Hd = H();
		const s = Math.min(Wd / vid.vw, Hd / vid.vh);
		const w = vid.vw * s, h = vid.vh * s;
		return { x: (Wd - w) / 2, y: (Hd - h) / 2, w, h };
	};
	// Base rect for zoom math. Aspect-fit when the video size is known; otherwise a
	// full-screen fallback so pinch still works (slightly off until `vid` is fetched).
	const baseRect = () => fitRect() || { x: 0, y: 0, w: W(), h: H() };
	// Clamp pan so the video covers the screen where it can (no empty borders beyond
	// the fit); on an axis smaller than the screen (letterbox) it stays centered.
	const clampRect = (r) => {
		const Wd = W(), Hd = H();
		r.x = r.w <= Wd ? (Wd - r.w) / 2 : clamp(r.x, Wd - r.w, 0);
		r.y = r.h <= Hd ? (Hd - r.h) / 2 : clamp(r.y, Hd - r.h, 0);
		return r;
	};
	// B2: the OS fires many touchmove events per animation frame; an invoke on each
	// floods the session (~2×+ the needed traffic) and adds latency. Coalesce the
	// high-frequency move + video-transform sends to ONE of each per frame, keeping
	// only the most recent payload.
	let _pendingMove = null; // latest { slot, x, y } for send_pointer
	let _pendingRect = null; // latest { slot, x, y, w, h } for set_video_transform
	let _flushRaf = null;
	const _flushSends = () => {
		_flushRaf = null;
		if (_pendingMove) { const m = _pendingMove; _pendingMove = null; invoke('send_pointer', m).catch(() => {}); }
		if (_pendingRect) { const r = _pendingRect; _pendingRect = null; invoke('set_video_transform', r).catch(() => {}); }
	};
	const _scheduleFlush = () => { if (_flushRaf === null) _flushRaf = requestAnimationFrame(_flushSends); };
	const sendRect = () => {
		if (!rect) return;
		const Wd = W(), Hd = H();
		// B2: queue the latest transform; flushed once per frame by _flushSends.
		_pendingRect = { slot: 0, x: rect.x / Wd, y: rect.y / Hd, w: rect.w / Wd, h: rect.h / Hd };
		_scheduleFlush();
	};
	// Reset to the aspect-fit view (zoom factor 1).
	const setFit = () => { zfac = 1; rect = fitRect(); sendRect(); };

	// A CSS-px screen point → normalized video-content coords, per split slot.
	const norm = (px, py) => {
		if (isSplit()) {
			const Wd = W(), Hd = H(), half = Hd / 2;
			if (py < half) return { slot: 0, x: clamp(px / Wd, 0, 1), y: clamp(py / half, 0, 1) };
			return { slot: 1, x: clamp(px / Wd, 0, 1), y: clamp((py - half) / half, 0, 1) };
		}
		if (rect && rect.w > 0 && rect.h > 0) {
			return { slot: 0, x: clamp((px - rect.x) / rect.w, 0, 1), y: clamp((py - rect.y) / rect.h, 0, 1) };
		}
		return { slot: 0, x: clamp(px / W(), 0, 1), y: clamp(py / H(), 0, 1) };
	};
	// True if a touch lands inside the displayed video rect (so we never drive the
	// host pointer from the letterbox / outside the screen being shared).
	const inRect = (t) => {
		if (!rect || isSplit()) return true;
		return t.clientX >= rect.x && t.clientX <= rect.x + rect.w &&
		       t.clientY >= rect.y && t.clientY <= rect.y + rect.h;
	};
	// B2: queue the move; flushed once per frame by _flushSends (coalesces a burst).
	const sendMoveAt  = (px, py) => { const p = norm(px, py); _pendingMove = { slot: p.slot, x: p.x, y: p.y }; _scheduleFlush(); };
	const sendClickAt = (px, py, button) => {
		const p = norm(px, py);
		// B2: this sends an explicit pointer position immediately; drop any queued
		// coalesced move so a stale move can't flush after the click's button-up.
		_pendingMove = null;
		invoke('send_pointer', { slot: p.slot, x: p.x, y: p.y }).catch(() => {});
		invoke('send_button', { slot: p.slot, button, down: true }).catch(() => {});
		invoke('send_button', { slot: p.slot, button, down: false }).catch(() => {});
	};
	// Press OR release one button at a position (no auto-release) — used for
	// hold-to-drag: button DOWN on drag start, pointer moves while held, button UP
	// on release. `down:true` then a series of moves then `down:false` = a drag.
	const sendButtonAt = (px, py, button, down) => {
		const p = norm(px, py);
		// B2: explicit pointer position — drop any queued coalesced move so it can't
		// flush out of order around this button press/release (e.g. drag start/end).
		_pendingMove = null;
		invoke('send_pointer', { slot: p.slot, x: p.x, y: p.y }).catch(() => {});
		invoke('send_button', { slot: p.slot, button, down }).catch(() => {});
	};

	// ── Virtual cursor (mouse/trackpad mode) ───────────────────────────────────
	const rectCenter  = () => { const r = rect || baseRect(); return { x: r.x + r.w / 2, y: r.y + r.h / 2 }; };
	const clampToRect = (p) => { const r = rect || baseRect(); return { x: clamp(p.x, r.x, r.x + r.w), y: clamp(p.y, r.y, r.y + r.h) }; };
	// One axis of cursor-follow pan: keep the cursor within `[EDGE, screen-EDGE]`;
	// when it pushes past, pan the (zoomed) view by the overflow so the host screen
	// scrolls under it. If the view can't pan further (host edge reached), the
	// leftover pushes the cursor on toward the real screen edge so it can still reach
	// the corner. Returns the new rect-origin + the on-screen cursor position.
	const EDGE_PAN = 44;
	const panAxis = (pos, rpos, size, screen) => {
		// Letterboxed axis (video doesn't fill the screen) → no pan; keep the cursor
		// ON the video so it never enters the black bars / leaves the displayed frame.
		if (size <= screen) return { rpos, cpos: clamp(pos, rpos, rpos + size) };
		const minR = screen - size; // most-negative origin (other extreme is 0)
		if (pos < EDGE_PAN) {
			const ov = EDGE_PAN - pos;
			const np = clamp(rpos + ov, minR, 0);
			return { rpos: np, cpos: clamp(EDGE_PAN - (ov - (np - rpos)), 0, screen) };
		}
		if (pos > screen - EDGE_PAN) {
			const ov = pos - (screen - EDGE_PAN);
			const np = clamp(rpos - ov, minR, 0);
			return { rpos: np, cpos: clamp((screen - EDGE_PAN) + (ov - (rpos - np)), 0, screen) };
		}
		return { rpos, cpos: pos };
	};
	const moveCursor = (p) => {
		if (rect && zfac > 1.001) {
			// Zoomed: cursor stays on-screen, the view pans to follow it (AnyDesk-style).
			const ax = panAxis(p.x, rect.x, rect.w, W());
			const ay = panAxis(p.y, rect.y, rect.h, H());
			const panned = ax.rpos !== rect.x || ay.rpos !== rect.y;
			rect = clampRect({ x: ax.rpos, y: ay.rpos, w: rect.w, h: rect.h });
			cur = { x: ax.cpos, y: ay.cpos };
			if (panned) sendRect();
		} else {
			cur = clampToRect(p);
		}
		if (cursorEl) cursorEl.style.transform = `translate(${cur.x}px, ${cur.y}px)`;
	};
	const ensureCursor = () => {
		if (cursorEl) return;
		cursorEl = document.createElement('div');
		cursorEl.id = 'pulsar-cursor';
		document.body.appendChild(cursorEl);
	};
	// 'cursor-mouse' on <body> gates the cursor's visibility via CSS (hidden when the
	// overlay is open or out of session). Place it at the rect centre when activating.
	const applyMode = () => {
		document.body.classList.toggle('cursor-mouse', mode === 'mouse');
		if (mode === 'mouse') { ensureCursor(); if (!cur) cur = rectCenter(); moveCursor(cur); }
	};

	// Brief on-screen zoom level (AnyDesk-style) — also confirms the pinch is detected.
	let zBadge = null, zTimer = null;
	const showZoom = (s) => {
		if (!zBadge) {
			zBadge = document.createElement('div');
			zBadge.id = 'zoom-badge';
			zBadge.style.cssText = 'position:fixed;top:50%;left:50%;transform:translate(-50%,-50%);z-index:99;background:rgba(0,0,0,0.72);color:#fff;font:600 18px/1 system-ui,sans-serif;padding:9px 14px;border-radius:12px;pointer-events:none;opacity:0;transition:opacity .25s';
			document.body.appendChild(zBadge);
		}
		zBadge.textContent = `🔍 ${s.toFixed(1)}×`;
		zBadge.style.opacity = '1';
		clearTimeout(zTimer);
		zTimer = setTimeout(() => { if (zBadge) zBadge.style.opacity = '0'; }, 800);
	};

	const beginPinch = (touches) => {
		if (!vid.vw) fetchVid(); // safety: learn the size if the session-start poll missed it
		const a = touches[0], b = touches[1];
		const d = Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY) || 1;
		const rect0 = rect ? { ...rect } : baseRect();
		return { d0: d, z0: zfac, rect0, mx: (a.clientX + b.clientX) / 2, my: (a.clientY + b.clientY) / 2 };
	};
	const updatePinch = (touches) => {
		if (!pinch || !pinch.rect0) return;
		const f = baseRect();
		const a = touches[0], b = touches[1];
		const d = Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY) || 1;
		const mx = (a.clientX + b.clientX) / 2, my = (a.clientY + b.clientY) / 2;
		const z = clamp(pinch.z0 * d / pinch.d0, 1, MAX_ZOOM);
		const w = f.w * z, h = f.h * z;
		// Keep the frame point under the gesture-start midpoint pinned, and pan with
		// the current midpoint (two-finger drag = pan).
		const u = (pinch.mx - pinch.rect0.x) / pinch.rect0.w;
		const v = (pinch.my - pinch.rect0.y) / pinch.rect0.h;
		rect = clampRect({ x: mx - u * w, y: my - v * h, w, h });
		zfac = z;
		sendRect();
		showZoom(z);
		// Keep the virtual cursor (mouse mode) on the visible video as the view
		// changes — if zooming would leave it outside, pull it in ("comes with the
		// zoom") and move the host pointer to match so the two don't desync.
		if (cur) {
			const Wd = W(), Hd = H();
			const nx = rect.w <= Wd ? clamp(cur.x, rect.x, rect.x + rect.w) : clamp(cur.x, 0, Wd);
			const ny = rect.h <= Hd ? clamp(cur.y, rect.y, rect.y + rect.h) : clamp(cur.y, 0, Hd);
			if (nx !== cur.x || ny !== cur.y) {
				cur = { x: nx, y: ny };
				if (cursorEl) cursorEl.style.transform = `translate(${cur.x}px, ${cur.y}px)`;
				sendMoveAt(cur.x, cur.y);
			}
		}
	};

	addEventListener('touchstart', (e) => {
		if (!inSession() || gamepadActive || uiBlocked(e)) return; // B1: gate while pad active
		if (e.touches.length >= 2) {
			// Two fingers → pinch-zoom / pan / scroll. Abort any pending 1-finger gesture.
			g1 = null; clearTimeout(longTimer); longTimer = null;
			multi = true;
			if (isSplit()) { g2 = null; pinch = null; return; }
			const a = e.touches[0], b = e.touches[1];
			const mid0 = { x: (a.clientX + b.clientX) / 2, y: (a.clientY + b.clientY) / 2 };
			g2 = { d0: Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY) || 1, mid0, type: 'none', lastMid: mid0 };
			pinch = beginPinch(e.touches);
			return;
		}
		if (multi) return;
		const t = e.touches[0];
		// Touch (absolute) mode: ignore touches outside the shared screen rect.
		if (mode === 'touch' && !inRect(t)) { g1 = null; return; }
		g1 = { sx: t.clientX, sy: t.clientY, x: t.clientX, y: t.clientY, t: Date.now(), moved: false, dragging: false };
		// Tap-and-a-half: a press that closely follows a tap becomes a left-button
		// DRAG as soon as it moves (the standard trackpad gesture; matches the
		// user's "double-click then hold to drag"). No long still-hold needed.
		if (lastTap && (Date.now() - lastTap.t) < DRAG_DBL_MS) g1.afterTap = true;
		// Long-press → right click.
		clearTimeout(longTimer);
		longTimer = setTimeout(() => {
			longTimer = null;
			if (!g1 || g1.dragging || !inSession()) return;
			// Held in place long enough → ARM a press (no click yet). If the finger
			// now MOVES it becomes a left-button DRAG (e.g. move a window); if it
			// RELEASES without moving it's a right-click. Disambiguated in
			// touchmove (drag start) / touchend (right-click) below.
			g1.armed = true;
			g1.dragging = true; // suppress the normal tap on release
			if (navigator.vibrate) navigator.vibrate(18);
		}, LONG_MS);
		// Absolute mode places the pointer under the finger immediately.
		if (mode === 'touch') sendMoveAt(t.clientX, t.clientY);
	}, { passive: true });

	addEventListener('touchmove', (e) => {
		if (!inSession() || gamepadActive) return; // B1: gate while on-screen pad active
		if (multi) {
			if (!g2 || e.touches.length < 2) return;
			const a = e.touches[0], b = e.touches[1];
			const d = Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY) || 1;
			const mid = { x: (a.clientX + b.clientX) / 2, y: (a.clientY + b.clientY) / 2 };
			if (g2.type === 'none') {
				if (Math.abs(d - g2.d0) > PINCH_THRESH) g2.type = 'zoom';
				else if (Math.hypot(mid.x - g2.mid0.x, mid.y - g2.mid0.y) > SCROLL_SLOP) g2.type = (zfac > 1.001 ? 'pan' : 'scroll');
			}
			if (g2.type === 'zoom' || g2.type === 'pan') {
				updatePinch(e.touches); // zoom by distance + pan by midpoint
			} else if (g2.type === 'scroll') {
				const dx = mid.x - g2.lastMid.x, dy = mid.y - g2.lastMid.y;
				invoke('send_scroll', { slot: 0, dx: -(dx / SCROLL_DIV), dy: -(dy / SCROLL_DIV) }).catch(() => {});
			}
			g2.lastMid = mid;
			return;
		}
		if (!g1) return;
		const t = e.touches[0];
		if (!g1.moved && Math.hypot(t.clientX - g1.sx, t.clientY - g1.sy) > TAP_SLOP) {
			g1.moved = true; g1.dragging = true; clearTimeout(longTimer); longTimer = null;
		}
		// Armed hold + first movement → press the LEFT button now and keep it down;
		// the moves below then drag (window move / text select) until release.
		if ((g1.armed || g1.afterTap) && !g1.dragStarted && g1.moved) {
			g1.dragStarted = true;
			if (mode === 'mouse') { const c = cur || rectCenter(); sendButtonAt(c.x, c.y, 0, true); }
			else { sendButtonAt(g1.sx, g1.sy, 0, true); }
		}
		if (mode === 'mouse') {
			// Relative: move the cursor by finger delta (kept inside the rect), drive
			// the host pointer to follow the visible cursor.
			const base = cur || rectCenter();
			moveCursor({ x: base.x + (t.clientX - g1.x) * REL_SENS, y: base.y + (t.clientY - g1.y) * REL_SENS });
			g1.x = t.clientX; g1.y = t.clientY;
			sendMoveAt(cur.x, cur.y);
		} else if (inRect(t)) {
			sendMoveAt(t.clientX, t.clientY);
		}
	}, { passive: true });

	addEventListener('touchend', (e) => {
		if (gamepadActive) return; // B1: gate while on-screen pad active
		if (e.touches.length === 0) {
			clearTimeout(longTimer); longTimer = null;
			if (multi) { multi = false; pinch = null; g2 = null; }
			else if (g1) {
				const now = Date.now();
				if (g1.dragStarted) {
						// End the hold-drag: release the left button at the cursor/finger.
						if (mode === 'mouse') { const c = cur || rectCenter(); sendButtonAt(c.x, c.y, 0, false); }
						else { sendButtonAt(g1.x, g1.y, 0, false); }
					} else if (g1.armed) {
						// Held in place without moving → right click.
						const rx = mode === 'mouse' ? (cur ? cur.x : g1.sx) : g1.sx;
						const ry = mode === 'mouse' ? (cur ? cur.y : g1.sy) : g1.sy;
						sendClickAt(rx, ry, 1);
					} else if (!g1.dragging && (now - g1.t) < TAP_MAX_MS) {
					const cx = mode === 'mouse' ? (cur ? cur.x : g1.sx) : g1.sx;
					const cy = mode === 'mouse' ? (cur ? cur.y : g1.sy) : g1.sy;
					if (lastTap && (now - lastTap.t) < DBL_MS && Math.hypot(cx - lastTap.x, cy - lastTap.y) < 40) {
						lastTap = null; sendClickAt(cx, cy, 0); sendClickAt(cx, cy, 0); // double-click
						if (navigator.vibrate) navigator.vibrate([8, 24, 8]);
					} else {
						lastTap = { x: cx, y: cy, t: now }; sendClickAt(cx, cy, 0); // left click
					}
				}
				g1 = null;
			}
		} else if (multi && e.touches.length < 2) {
			pinch = null; g2 = null; // pinch needs two fingers; keep `multi` until all lift
		}
	}, { passive: true });

	addEventListener('touchcancel', () => {
		if (g1 && g1.dragStarted) { const c = (mode === 'mouse') ? (cur || rectCenter()) : { x: g1.x, y: g1.y }; sendButtonAt(c.x, c.y, 0, false); }
		clearTimeout(longTimer); longTimer = null; g1 = null; g2 = null; pinch = null; multi = false;
	}, { passive: true });

	// Pointer-mode toggle (from the overlay Display card). Persisted + reflected live.
	if (bus && bus.on) bus.on('input-mode-changed', (m) => {
		mode = (m === 'touch') ? 'touch' : 'mouse';
		try { localStorage.setItem(MODE_KEY, mode); } catch (_) {}
		applyMode();
	});
	applyMode();

	// Learn the decoded video size (slot 0) so zoom/touch-mapping is aspect-correct.
	// PULL model: we poll `get_video_size` after a session starts. (The plugin can't
	// push a `video-size` event — Tauri's plugin `registerListener` is not permitted
	// in this build, so neither global `listen` nor `addPluginListener` works.)
	const parseSize = (detail) => { const m = /^(\d+)x(\d+)$/.exec(detail || ''); return m && +m[1] > 0 ? { vw: +m[1], vh: +m[2] } : null; };
	const fetchVid = async () => {
		if (!hasTauri) return false;
		try {
			const r = await invoke('plugin:pulsar-video|get_video_size', { slot: 0 });
			const s = parseSize(r && r.detail);
			if (s) {
				const changed = s.vw !== vid.vw || s.vh !== vid.vh;
				vid = s;
				if (changed && zfac === 1) setFit(); // refit + populate rect for input mapping
				return true;
			}
		} catch (_) { /* command missing / no surface — fall back to full-screen */ }
		return false;
	};
	let _vidPoll = null;
	const startVidPoll = () => {
		clearInterval(_vidPoll);
		let tries = 0;
		_vidPoll = setInterval(async () => { if (await fetchVid() || ++tries > 40) clearInterval(_vidPoll); }, 250);
	};

	// Reset zoom + cursor + (re)learn the video size when a new session starts.
	if (bus && bus.on) bus.on('session-started', () => {
		zfac = 1; rect = null; vid = { vw: 0, vh: 0 }; cur = null;
		// B2: drop any queued move/transform from the previous session + cancel the
		// pending rAF so a stale send can't land in the new session.
		_pendingMove = null; _pendingRect = null;
		if (_flushRaf !== null) { cancelAnimationFrame(_flushRaf); _flushRaf = null; }
		startVidPoll(); applyMode();
	});

	// Rotation / resize → recompute the aspect-fit for the new screen geometry (also
	// keeps the touch→host mapping correct). Debounced; resets an active zoom to fit.
	let _resizeTimer = null;
	const onResize = () => {
		clearTimeout(_resizeTimer);
		_resizeTimer = setTimeout(() => { if (inSession() && vid.vw) setFit(); }, 150);
	};
	addEventListener('resize', onResize, { passive: true });
	addEventListener('orientationchange', onResize, { passive: true });
}

// ---- Expose bus + helpers on window for modules that cannot import app.js ----
// (avoids circular import: app.js → screens → bus → app.js)
window.__pulsarBus = bus;

/**
 * updateBarMeta and setActiveSlot are exposed on window so that connect.js
 * (owned by W2-connect) can call them after connect_host resolves, without
 * creating a circular import back into app.js.
 */
window.__pulsarUpdateBarMeta = updateBarMeta;
window.__pulsarSetActiveSlot = setActiveSlot;

// ---- Run ---------------------------------------------------------------------

if (document.readyState === 'loading') {
	document.addEventListener('DOMContentLoaded', boot);
} else {
	boot();
}
