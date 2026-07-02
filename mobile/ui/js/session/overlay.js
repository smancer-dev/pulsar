/**
 * overlay.js — Mode-aware in-session overlay dock + registerCard registry
 * (W3-overlay)
 *
 * Central enforcement of the remote-vs-game feature split:
 *   - A card with modes:['remote'] is NEVER mounted/shown in game mode.
 *   - A card with modes:['game']   is NEVER mounted/shown in remote mode.
 *   - A card with modes:['remote','game'] shows in both.
 *
 * Architecture (§1.1 + §1.2 contract):
 *   - registerCard(spec)  — called at import time by feature modules.
 *   - open()  / close()   — open or close the dock.
 *   - toggle()            — flip open/closed state.
 *
 * The overlay is opened by:
 *   (a) Tapping the FAB (floating action button) over the video surface, OR
 *   (b) Tapping the status pill in the control bar (#bar .stat).
 *
 * Layout sections (per DT-overlay + §5 W3-overlay brief):
 *   Remote layout : Stream / Display / Audio / Tools
 *   Game layout   : Stream / Gauges / Controllers
 *
 * Each section renders only the cards whose section field matches and whose
 * modes[] includes the current body[data-mode].
 *
 * Exports:
 *   registerCard(spec)    — register a card; spec = { id, modes, section, order, mount }
 *   open()                — open the dock
 *   close()               — close the dock
 *   toggle()              — toggle open/closed
 *   isOpen()              — boolean
 *
 * Listens (JS bus):
 *   session-started       — { slot, id, codec, mode, transport }
 *   session-ended         — { slot }
 *   mode-changed          — 'remote' | 'game'
 *
 * Does NOT call any Tauri commands directly — it is a pure host surface.
 * Feature panels (quality.js, hud.js, etc.) invoke commands from their own cards.
 *
 * Touch-first design (DT-overlay):
 *   - Large 44px+ tap targets everywhere.
 *   - Bottom sheet with safe-area-inset-bottom padding.
 *   - Section pill-row for quick section switching on small screens.
 *   - Cards expand vertically; wide screens get a 2-col grid.
 *   - Indigo (remote) / cyan (game) themed.
 *   - Glass dark surface over the transparent video.
 */

import { t } from '../i18n.js';

// ---- Registry -------------------------------------------------------------------

/**
 * @typedef {Object} CardSpec
 * @property {string}               id        — unique card id
 * @property {Array<'remote'|'game'>} modes   — which personalities show this card
 * @property {'stream'|'display'|'audio'|'tools'|'gauges'|'controllers'} section
 * @property {number}               [order]   — sort order within the section (default 50)
 * @property {(el: HTMLElement) => void} mount — called once when the card DOM is inserted
 */

/** @type {CardSpec[]} */
const _cards = [];

/** @type {Map<string, boolean>} — tracks which card ids have been mounted */
// id -> the card's mounted DOM element. Cards are mounted ONCE, then this live
// element is re-attached on every _renderDock() (which clears the dock's innerHTML).
// The previous `_mounted` boolean map left re-created card divs empty from the 2nd
// render on (reopen / tab switch), because the guard skipped re-mounting but the
// DOM had been wiped.
const _cardEls = new Map();

/**
 * Register a card with the overlay.
 * Called at module import time by feature panels (quality.js, hud.js, etc.).
 * Safe to call before DOMContentLoaded.
 *
 * @param {CardSpec} spec
 */
export function registerCard(spec) {
	if (!spec || !spec.id) return;
	// Avoid duplicate registration
	if (_cards.some((c) => c.id === spec.id)) return;
	_cards.push({
		order: 50,
		...spec,
	});
	// If the dock is already rendered, refresh it to include the new card
	if (_dockEl) _renderDock();
}

// ---- Section metadata -----------------------------------------------------------

/**
 * Section definitions — order + label key for each section.
 * Used to build the section pill-row and the section headers.
 */
const SECTIONS_REMOTE = [
	{ id: 'stream',      labelKey: 'm.overlay.sec.stream'   },
	{ id: 'display',     labelKey: 'm.overlay.sec.display'  },
	{ id: 'audio',       labelKey: 'm.overlay.sec.audio'    },
	{ id: 'tools',       labelKey: 'm.overlay.sec.tools'    },
];
const SECTIONS_GAME = [
	{ id: 'stream',      labelKey: 'm.overlay.sec.stream'    },
	{ id: 'gauges',      labelKey: 'm.overlay.sec.gauges'    },
	{ id: 'controllers', labelKey: 'm.overlay.sec.controllers'},
];

function _getSections(mode) {
	return mode === 'game' ? SECTIONS_GAME : SECTIONS_REMOTE;
}

// ---- Translation fallbacks (overlay-specific keys not in i18n catalog) ----------

const _LABEL_FALLBACKS = {
	'm.overlay.sec.stream':      { tr: 'Akış',       en: 'Stream'      },
	'm.overlay.sec.display':     { tr: 'Görüntü',    en: 'Display'     },
	'm.overlay.sec.audio':       { tr: 'Ses',        en: 'Audio'       },
	'm.overlay.sec.tools':       { tr: 'Araçlar',    en: 'Tools'       },
	'm.overlay.sec.gauges':      { tr: 'Göstergeler',en: 'Gauges'      },
	'm.overlay.sec.controllers': { tr: 'Kontrolcüler',en: 'Controllers' },
	'm.overlay.title.remote':    { tr: 'Oturum Kontrolleri', en: 'Session Controls' },
	'm.overlay.title.game':      { tr: 'Oyun Menüsü',        en: 'Game Menu'        },
	'm.overlay.close':           { tr: 'Kapat',      en: 'Close'       },
};

/**
 * Translate with overlay-specific fallbacks.
 * @param {string} key
 * @returns {string}
 */
function _t(key) {
	try {
		const v = t(key);
		// t() returns the key itself when not found — fall through to our table
		if (v !== key) return v;
	} catch (_) {}
	const fb = _LABEL_FALLBACKS[key];
	if (!fb) return key;
	// Detect current lang from the html element or default to 'tr'
	const htmlLang = (document.documentElement.lang || 'tr').toLowerCase().slice(0, 2);
	return fb[htmlLang] || fb.tr || key;
}

// ---- State ----------------------------------------------------------------------

let _open     = false;
let _mode     = document.body.dataset.mode || 'remote'; // 'remote' | 'game'
let _slot     = 0;
let _section  = null; // currently selected section id (null = auto = first)

/** @type {HTMLElement|null} */
let _dockEl   = null;

/** @type {HTMLElement|null} */
let _backdropEl = null;

/** @type {HTMLElement|null} */
let _fabEl    = null;

// ---- Public API -----------------------------------------------------------------

/** Open the overlay dock. */
export function open() {
	if (!_dockEl) _buildDOM();
	_renderDock();
	_open = true;
	// Disable input forwarding (input.js checks this) + hide the FAB so it doesn't
	// float over the dock.
	document.body.classList.add('overlay-open');
	if (_fabEl) _fabEl.style.display = 'none';
	requestAnimationFrame(() => {
		_dockEl?.classList.add('open');
		_backdropEl?.classList.add('open');
		// Announce to screen readers
		_dockEl?.setAttribute('aria-hidden', 'false');
	});
}

/** Close the overlay dock. */
export function close() {
	_open = false;
	document.body.classList.remove('overlay-open');
	_dockEl?.classList.remove('open');
	_backdropEl?.classList.remove('open');
	_dockEl?.setAttribute('aria-hidden', 'true');
	// Restore the FAB unless the user disabled it in Settings.
	if (_fabEl) _fabEl.style.display = _overlayBtnPref() === false ? 'none' : '';
}

/** Toggle the overlay dock open/closed. */
export function toggle() {
	if (_open) close(); else open();
}

/** @returns {boolean} */
export function isOpen() {
	return _open;
}

// ---- DOM build ------------------------------------------------------------------

/**
 * Build the overlay DOM elements (dock + backdrop + FAB) and insert them
 * into the document. Called lazily on first open().
 *
 * The dock uses a two-layer layout:
 *   1. A scrollable section pill-row at the top.
 *   2. A card container area below.
 */
function _buildDOM() {
	if (_dockEl) return;

	// ---- Overlay dock -----------------------------------------------------------
	_dockEl = document.createElement('div');
	_dockEl.className  = 'overlay-dock';
	_dockEl.id         = 'overlay-dock';
	_dockEl.setAttribute('role', 'dialog');
	_dockEl.setAttribute('aria-modal', 'true');
	_dockEl.setAttribute('aria-hidden', 'true');
	_dockEl.setAttribute('aria-label', _t('m.overlay.title.remote'));

	// ---- Backdrop ---------------------------------------------------------------
	_backdropEl = document.createElement('div');
	_backdropEl.className = 'overlay-backdrop';
	_backdropEl.id        = 'overlay-backdrop';
	_backdropEl.setAttribute('aria-hidden', 'true');
	_backdropEl.addEventListener('click', () => close());

	// ---- FAB (shown in-session, opens the dock) ---------------------------------
	_fabEl = document.createElement('button');
	_fabEl.className   = 'fab overlay-fab';
	_fabEl.id          = 'overlay-fab';
	_fabEl.type        = 'button';
	_fabEl.setAttribute('aria-label', _t('m.overlay.title.remote'));
	_fabEl.setAttribute('aria-controls', 'overlay-dock');
	_fabEl.innerHTML   = _pulseMarkSVG();
	_wireFabDrag(_fabEl);

	// Swipe the dock down to dismiss (handle/header drag), like a bottom sheet.
	_wireDockSwipe(_dockEl);

	// Insert into document
	document.body.appendChild(_backdropEl);
	document.body.appendChild(_dockEl);
	document.body.appendChild(_fabEl);

	// Settings → "Overlay button" can hide the FAB; the overlay then opens via a
	// 3-finger tap (below), so it stays reachable.
	if (_overlayBtnPref() === false) _fabEl.style.display = 'none';

	// Restore the FAB's dragged position from a previous session.
	_applyFabPos(_fabEl);

	// Wire the status pill in the control bar so it also opens the overlay
	_wirePillTap();

	// 3-finger tap anywhere toggles the overlay (Moonlight-style) — the fallback
	// open gesture when the FAB is hidden. Capture phase + stopPropagation so the
	// gesture doesn't also forward to the host as remote input.
	document.addEventListener('touchstart', (e) => {
		if (e.touches && e.touches.length >= 3) {
			e.stopPropagation();
			toggle();
		}
	}, { capture: true });

	// Wire the escape key (for accessibility with external keyboard)
	document.addEventListener('keydown', (e) => {
		if (e.key === 'Escape' && _open) {
			e.preventDefault();
			close();
		}
	});
}

/** Read the "Overlay button" pref without importing the prefs store. */
function _overlayBtnPref() {
	try {
		return JSON.parse(localStorage.getItem('pulsar.prefs.v1') || '{}').overlayButton;
	} catch (_) {
		return undefined;
	}
}

// ---- FAB drag-to-move + persisted position --------------------------------------

const FAB_POS_KEY = 'pulsar.fabPos.v1';

function _loadFabPos() {
	try {
		const p = JSON.parse(localStorage.getItem(FAB_POS_KEY) || 'null');
		return p && typeof p.left === 'number' && typeof p.top === 'number' ? p : null;
	} catch (_) { return null; }
}

function _clampFab(left, top) {
	const m = 6, sz = 52;
	return {
		left: Math.max(m, Math.min(window.innerWidth - sz - m, left)),
		top: Math.max(8, Math.min(window.innerHeight - sz - m, top)),
	};
}

function _applyFabPos(el) {
	const p = _loadFabPos();
	if (!p) return;
	const { left, top } = _clampFab(p.left, p.top);
	el.style.left = `${left}px`;
	el.style.top = `${top}px`;
	el.style.right = 'auto';
	el.style.bottom = 'auto';
}

/**
 * Make the FAB draggable. A small move past threshold = drag (reposition + persist);
 * a clean press = tap (toggle the overlay). Pointer events are captured + stopped so
 * neither the drag nor the tap forwards to the host as remote input.
 */
function _wireFabDrag(el) {
	let sx = 0, sy = 0, sl = 0, st = 0, curL = 0, curT = 0, moved = false, active = false;

	const start = (e) => {
		const p = e.touches ? e.touches[0] : e;
		active = true; moved = false;
		const r = el.getBoundingClientRect();
		sx = p.clientX; sy = p.clientY; sl = r.left; st = r.top; curL = sl; curT = st;
		// Gate host input forwarding for the whole interaction (input.js checks this).
		document.body.classList.add('fab-dragging');
	};
	const move = (e) => {
		if (!active) return;
		const p = e.touches ? e.touches[0] : e;
		const dx = p.clientX - sx, dy = p.clientY - sy;
		if (!moved && Math.hypot(dx, dy) > 6) moved = true;
		if (moved) {
			({ left: curL, top: curT } = _clampFab(sl + dx, st + dy));
			el.style.left = `${curL}px`; el.style.top = `${curT}px`;
			el.style.right = 'auto'; el.style.bottom = 'auto';
			if (e.cancelable) e.preventDefault();
		}
	};
	const end = (e) => {
		if (!active) return;
		active = false;
		document.body.classList.remove('fab-dragging');
		if (moved) {
			// Persist the tracked position (getBoundingClientRect is 0 on a hidden FAB).
			try { localStorage.setItem(FAB_POS_KEY, JSON.stringify({ left: curL, top: curT })); } catch (_) {}
		} else {
			// Clean tap → open. preventDefault suppresses the synthesized click that
			// would otherwise hit the (now-covering) backdrop and immediately re-close.
			if (e.cancelable) e.preventDefault();
			toggle();
		}
		moved = false;
	};

	el.addEventListener('touchstart',  start, { passive: true });
	el.addEventListener('touchmove',   move,  { passive: false });
	el.addEventListener('touchend',    end);
	el.addEventListener('touchcancel', () => { active = false; moved = false; document.body.classList.remove('fab-dragging'); });
	// Swallow any stray click so it never forwards as a host tap.
	el.addEventListener('click', (e) => e.stopPropagation());
}

/**
 * Swipe the dock down to dismiss (bottom-sheet style). Starts only from the top
 * grab zone (handle/header) OR when the card area is scrolled to the top, so it
 * never fights with scrolling the card content. Past ~90px down → close().
 */
function _wireDockSwipe(el) {
	let sy = 0, dy = 0, active = false;
	const cardArea = () => el.querySelector('.overlay-card-area');
	const start = (e) => {
		const p = e.touches[0];
		// Only arm from the handle/header, or when the (scrollable) card area is at the top.
		const onGrab = e.target.closest && e.target.closest('.overlay-handle, .overlay-header');
		const a = cardArea();
		const atTop = !a || a.scrollTop <= 0;
		if (!onGrab && !atTop) { active = false; return; }
		sy = p.clientY; dy = 0; active = true;
		el.style.transition = 'none';
	};
	const move = (e) => {
		if (!active) return;
		const d = e.touches[0].clientY - sy;
		if (d <= 0) { dy = 0; el.style.transform = ''; return; } // upward → ignore
		dy = d;
		el.style.transform = `translateY(${dy}px)`;
		if (e.cancelable) e.preventDefault(); // dismissing, not scrolling
	};
	const end = () => {
		if (!active) return;
		active = false;
		el.style.transition = '';
		el.style.transform = '';
		if (dy > 90) close();
		dy = 0;
	};
	el.addEventListener('touchstart',  start, { passive: true });
	el.addEventListener('touchmove',   move,  { passive: false });
	el.addEventListener('touchend',    end);
	el.addEventListener('touchcancel', () => { active = false; el.style.transition = ''; el.style.transform = ''; dy = 0; });
}

/** Wire a tap on #bar .stat to open the overlay. */
function _wirePillTap() {
	// Wait for the bar element to exist (it might not if in-session hasn't started yet)
	const tryWire = () => {
		const barStat = document.querySelector('#bar .stat');
		if (barStat && !barStat.dataset.overlayWired) {
			barStat.dataset.overlayWired = '1';
			barStat.style.cursor = 'pointer';
			barStat.addEventListener('click', (e) => {
				e.stopPropagation();
				toggle();
			});
			// Accessibility
			barStat.setAttribute('role', 'button');
			barStat.setAttribute('aria-label', _t('m.overlay.title.remote'));
			barStat.tabIndex = 0;
			barStat.addEventListener('keydown', (e) => {
				if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); }
			});
		}
	};
	// Try immediately, and again after a short delay (in case bar renders late)
	tryWire();
	setTimeout(tryWire, 300);
	setTimeout(tryWire, 800);
}

// ---- Render logic ---------------------------------------------------------------

/**
 * Re-render the dock content based on the current mode + section.
 * Idempotent — safe to call multiple times.
 */
function _renderDock() {
	if (!_dockEl) return;

	const mode     = _mode;
	const sections = _getSections(mode);
	const title    = _t(mode === 'game' ? 'm.overlay.title.game' : 'm.overlay.title.remote');

	// Update aria-label
	_dockEl.setAttribute('aria-label', title);
	if (_fabEl) {
		_fabEl.setAttribute('aria-label', title);
		_fabEl.setAttribute('aria-expanded', String(_open));
	}

	// Pick the active section (default = first)
	if (!_section || !sections.find((s) => s.id === _section)) {
		_section = sections[0]?.id || null;
	}

	// Filter cards for this mode + section
	const visibleCards = _cards
		.filter((c) => c.modes.includes(mode))
		.sort((a, b) => (a.order || 50) - (b.order || 50));

	const activeCards = visibleCards.filter((c) => c.section === _section);

	// Build HTML
	_dockEl.innerHTML = '';

	// -- Handle bar -----------------------------------------------------------------
	const handleDiv = document.createElement('div');
	handleDiv.className = 'overlay-handle';
	handleDiv.setAttribute('aria-hidden', 'true');
	_dockEl.appendChild(handleDiv);

	// -- Dock header ----------------------------------------------------------------
	const headerDiv = document.createElement('div');
	headerDiv.className = 'overlay-header';
	headerDiv.innerHTML = `
		<span class="overlay-title">${title}</span>
		<button class="overlay-close icon-btn" type="button"
		        aria-label="${_t('m.overlay.close')}"
		        id="overlay-close-btn">
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<path d="M6 6l12 12M18 6L6 18"
				      stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>
			</svg>
		</button>
	`;
	_dockEl.appendChild(headerDiv);

	// Wire close button
	headerDiv.querySelector('#overlay-close-btn')?.addEventListener('click', () => close());

	// -- Section pill-row -----------------------------------------------------------
	if (sections.length > 1) {
		const pillRow = document.createElement('nav');
		pillRow.className    = 'pill-row overlay-section-row';
		pillRow.setAttribute('role', 'tablist');
		pillRow.setAttribute('aria-label', title);

		sections.forEach((sec) => {
			const cardsInSec = visibleCards.filter((c) => c.section === sec.id);
			if (cardsInSec.length === 0) return; // skip empty sections

			const btn = document.createElement('button');
			btn.className = 'pill-btn' + (_section === sec.id ? ' on' : '');
			btn.type = 'button';
			btn.dataset.section = sec.id;
			btn.setAttribute('role', 'tab');
			btn.setAttribute('aria-selected', String(_section === sec.id));
			btn.textContent = _t(sec.labelKey);

			btn.addEventListener('click', () => {
				_section = sec.id;
				_renderDock();
				// Scroll new section into view if pill-row overflows
				btn.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
			});

			pillRow.appendChild(btn);
		});

		_dockEl.appendChild(pillRow);
	}

	// -- Card area ------------------------------------------------------------------
	const cardArea = document.createElement('div');
	cardArea.className = 'overlay-card-area';

	// Cards needing a first-time mount — mounted AFTER they're in the document so the
	// panels' getElementById-based wiring (e.g. display.js segmented controls) works.
	const toMount = [];
	if (activeCards.length === 0) {
		const empty = document.createElement('p');
		empty.className   = 'overlay-empty';
		empty.textContent = _t('m.overlay.sec.' + (_section || 'stream'));
		cardArea.appendChild(empty);
	} else {
		activeCards.forEach((spec) => {
			// Reuse the card's live element across renders. Mount only the first time;
			// later renders just re-attach it (the dock's innerHTML was cleared above,
			// detaching but not destroying these cached elements).
			let cardEl = _cardEls.get(spec.id);
			if (!cardEl) {
				cardEl = document.createElement('div');
				cardEl.className = 'overlay-card';
				cardEl.id        = 'overlay-card-' + spec.id;
				cardEl.setAttribute('data-card', spec.id);
				_cardEls.set(spec.id, cardEl);
				if (spec.mount) toMount.push([spec, cardEl]);
			}
			cardArea.appendChild(cardEl);
		});
	}

	_dockEl.appendChild(cardArea);

	// Now the cards are attached — mount the fresh ones (their wiring can resolve
	// elements by id and attach listeners).
	toMount.forEach(([spec, cardEl]) => {
		try {
			spec.mount(cardEl, _slot); // panels' mount(el, slot)
		} catch (e) {
			console.warn('[overlay] card mount error:', spec.id, e);
		}
	});

	// -- Footer: End session (the removed bottom bar used to hold End) -------------
	const footer = document.createElement('div');
	footer.className = 'overlay-footer';
	const endBtn = document.createElement('button');
	endBtn.type = 'button';
	endBtn.className = 'btn overlay-end-btn full';
	endBtn.textContent = _t('session.end');
	endBtn.addEventListener('click', () => {
		close();
		// Reuse the (hidden but still wired) bar End button → end_session.
		document.getElementById('btn-end')?.click();
	});
	footer.appendChild(endBtn);
	_dockEl.appendChild(footer);
}

// ---- SVG helpers ----------------------------------------------------------------

function _pulseMarkSVG() {
	return `
		<svg width="24" height="24" viewBox="0 0 52 52" fill="none" aria-hidden="true">
			<circle cx="26" cy="26" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.22"/>
			<circle cx="26" cy="26" r="14" stroke="currentColor" stroke-width="1.75" opacity="0.5"/>
			<circle cx="26" cy="26" r="7" stroke="currentColor" stroke-width="2"/>
			<circle cx="26" cy="26" r="3" fill="currentColor"/>
		</svg>
	`;
}

// ---- Bus wiring -----------------------------------------------------------------

/**
 * Wire the JS bus (window.__pulsarBus) + Tauri events.
 * Called lazily — the bus may not be available at module parse time
 * (app.js sets it synchronously but we may execute before that in some paths).
 */
function _wireBus() {
	const bus = window.__pulsarBus;
	if (!bus) return;

	// Session started — store the active mode + slot; pre-build the DOM
	bus.on('session-started', ({ slot, mode }) => {
		_slot = slot || 0;
		const newMode = mode || 'remote';
		if (newMode !== _mode) {
			_mode    = newMode;
			_section = null; // reset section selection on mode change
		}
		if (!_dockEl) _buildDOM();
		// Re-render if already open
		if (_open) _renderDock();
		// Show the FAB
		if (_fabEl) _fabEl.style.display = '';
	});

	// Session ended — close and hide the FAB
	bus.on('session-ended', () => {
		close();
		if (_fabEl) _fabEl.style.display = 'none';
	});

	// Mode changed mid-session (should not happen per spec, but guard it)
	bus.on('mode-changed', (newMode) => {
		if (newMode === _mode) return;
		_mode    = newMode;
		_section = null;
		if (_open) _renderDock();
	});
}

// ---- Inline styles --------------------------------------------------------------
// Injected into <head> once — self-contained so overlay.js has no dependency on
// components.css being complete. These styles extend/override the base
// .overlay-dock / .overlay-card / .fab rules defined in components.css.

function _injectStyles() {
	if (document.getElementById('overlay-module-styles')) return;
	const style = document.createElement('style');
	style.id = 'overlay-module-styles';
	style.textContent = `
/* ---- Overlay backdrop ---- */
.overlay-backdrop {
	position: fixed;
	inset: 0;
	z-index: 14;
	background: oklch(0 0 0 / 0.38);
	opacity: 0;
	pointer-events: none;
	transition: opacity 0.3s var(--ease, cubic-bezier(0.16,1,0.3,1));
}
.overlay-backdrop.open {
	opacity: 1;
	pointer-events: auto;
}

/* ---- Overlay dock refinements ---- */
#overlay-dock {
	display: flex;
	flex-direction: column;
	gap: 0;
	max-height: 80dvh;
	overflow: hidden;
	border-top-left-radius: var(--r-xl, 26px);
	border-top-right-radius: var(--r-xl, 26px);
}
.overlay-handle {
	width: 38px; height: 4px;
	border-radius: 2px;
	background: oklch(1 0 0 / 0.22);
	margin: 12px auto 4px;
	flex: none;
}
.overlay-header {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 4px 16px 8px;
	flex: none;
}
.overlay-title {
	font-family: var(--font-display, 'Space Grotesk', sans-serif);
	font-size: 16px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: oklch(0.97 0 0);
}
.overlay-header .overlay-close {
	background: oklch(1 0 0 / 0.08);
	color: oklch(0.88 0 0);
	flex: none;
}
.overlay-header .overlay-close:hover {
	background: oklch(1 0 0 / 0.16);
}

/* ---- Section pill-row inside the overlay (dark surface override) ---- */
.overlay-section-row {
	padding: 0 16px 8px;
	flex: none;
}
.overlay-section-row .pill-btn {
	background: oklch(1 0 0 / 0.08);
	border-color: oklch(1 0 0 / 0.12);
	color: oklch(0.82 0 0);
	font-size: 12.5px;
	padding: 7px 14px;
}
.overlay-section-row .pill-btn.on {
	background: var(--brand, oklch(0.555 0.205 272));
	border-color: transparent;
	color: oklch(0.99 0 0);
}

/* ---- Card area scrolls within the dock ---- */
.overlay-card-area {
	overflow-y: auto;
	-webkit-overflow-scrolling: touch;
	padding: 0 12px calc(8px + var(--safe-bottom, env(safe-area-inset-bottom, 0px)));
	flex: 1;
	display: flex;
	flex-direction: column;
	gap: 10px;
}
/* Two-column layout on wide screens (landscape phone / tablet) */
@media (min-width: 600px) {
	.overlay-card-area {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		align-content: start;
	}
}

/* ---- Empty state in card area ---- */
.overlay-empty {
	font-size: 13px;
	color: oklch(0.65 0 0);
	text-align: center;
	padding: 24px 8px;
	margin: 0;
}

/* ---- overlay-card refinements (dark glass surface) ---- */
.overlay-card {
	/* components.css provides the base; we refine here */
	display: flex;
	flex-direction: column;
	gap: 12px;
}

/* ---- FAB visibility: hidden until in-session ---- */
.overlay-fab {
	display: none;
	/* Above the touch-capture overlay (z:9) so a tap opens the dock, not forwards. */
	z-index: 16;
}
body.in-session .overlay-fab {
	display: flex;
}
/* When the overlay is open, FAB gets a pressed state */
body.in-session .overlay-fab[aria-expanded="true"] {
	background: oklch(0.35 0.04 268);
	transform: scale(0.92);
}

/* ---- Game mode: cyan FAB + section pill accent ---- */
[data-mode="game"] body.in-session .overlay-fab,
body.in-session[data-mode="game"] .overlay-fab {
	background: var(--cyan, oklch(0.62 0.15 215));
}
[data-mode="game"] .overlay-section-row .pill-btn.on,
body[data-mode="game"] .overlay-section-row .pill-btn.on {
	background: var(--cyan, oklch(0.62 0.15 215));
}

/* ---- Pill in the status bar shows pointer to hint it's tappable ---- */
#bar .stat[role="button"] {
	cursor: pointer;
	border-radius: var(--r-pill, 999px);
	padding: 4px 10px;
	margin: -4px -10px;
	transition: background 0.18s;
	-webkit-tap-highlight-color: transparent;
}
#bar .stat[role="button"]:active {
	background: oklch(1 0 0 / 0.1);
}

/* ---- Card label / section heading inside the overlay card ---- */
.overlay-card-label {
	font-size: 11px;
	font-weight: 700;
	letter-spacing: 0.06em;
	text-transform: uppercase;
	color: oklch(0.55 0 0);
	margin: 0 0 8px;
}

/* ---- Row layout helper inside overlay cards ---- */
.overlay-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	min-height: var(--touch-min, 44px);
}
.overlay-row .label {
	font-size: 14px;
	font-weight: 500;
	color: oklch(0.92 0 0);
	flex: 1;
}
.overlay-row .hint {
	font-size: 12px;
	color: oklch(0.62 0 0);
}

/* ---- Value chips inside overlay (codec / res / fps) ---- */
.overlay-chip {
	font-family: var(--font-mono, 'JetBrains Mono', monospace);
	font-size: 12px;
	font-weight: 600;
	color: oklch(0.88 0 0);
	background: oklch(1 0 0 / 0.1);
	border: 1px solid oklch(1 0 0 / 0.14);
	border-radius: var(--r-sm, 9px);
	padding: 4px 10px;
	white-space: nowrap;
}

/* ---- Seg (segmented control) inside overlay: dark variant ---- */
.overlay-card .seg {
	background: oklch(1 0 0 / 0.06);
	border-color: oklch(1 0 0 / 0.12);
}
.overlay-card .seg button {
	color: oklch(0.72 0 0);
	font-size: 12px;
}
.overlay-card .seg button[aria-selected="true"] {
	background: var(--brand, oklch(0.555 0.205 272));
	color: oklch(0.99 0 0);
}
body[data-mode="game"] .overlay-card .seg button[aria-selected="true"] {
	background: var(--cyan, oklch(0.62 0.15 215));
}

/* ---- Slider (range) inside overlay ---- */
.overlay-card input[type="range"] {
	-webkit-appearance: none;
	appearance: none;
	width: 100%;
	height: 4px;
	border-radius: 2px;
	background: oklch(1 0 0 / 0.18);
	outline: none;
	cursor: pointer;
}
.overlay-card input[type="range"]::-webkit-slider-thumb {
	-webkit-appearance: none;
	appearance: none;
	width: 22px; height: 22px;
	border-radius: 50%;
	background: var(--brand, oklch(0.555 0.205 272));
	border: 2px solid oklch(0.99 0 0 / 0.8);
	box-shadow: 0 2px 8px oklch(0 0 0 / 0.35);
	cursor: pointer;
}
body[data-mode="game"] .overlay-card input[type="range"]::-webkit-slider-thumb {
	background: var(--cyan, oklch(0.62 0.15 215));
}

/* ---- Toggle (checkbox) inside overlay ---- */
.overlay-toggle {
	position: relative;
	width: 46px; height: 26px;
	flex: none;
}
.overlay-toggle input[type="checkbox"] {
	opacity: 0;
	position: absolute;
	inset: 0;
	width: 100%; height: 100%;
	cursor: pointer;
	z-index: 1;
	margin: 0;
}
.overlay-toggle .track {
	position: absolute;
	inset: 0;
	border-radius: 13px;
	background: oklch(1 0 0 / 0.18);
	transition: background 0.2s;
	pointer-events: none;
}
.overlay-toggle .thumb {
	position: absolute;
	top: 3px; left: 3px;
	width: 20px; height: 20px;
	border-radius: 50%;
	background: oklch(0.82 0 0);
	transition: transform 0.2s var(--ease, cubic-bezier(0.16,1,0.3,1)), background 0.2s;
	pointer-events: none;
}
.overlay-toggle input:checked ~ .track {
	background: var(--brand, oklch(0.555 0.205 272));
}
body[data-mode="game"] .overlay-toggle input:checked ~ .track {
	background: var(--cyan, oklch(0.62 0.15 215));
}
.overlay-toggle input:checked ~ .thumb {
	transform: translateX(20px);
	background: oklch(0.99 0 0);
}
`;
	document.head.appendChild(style);
}

// ---- Boot -----------------------------------------------------------------------

_injectStyles();

// Wire the bus: try immediately (app.js may have already set __pulsarBus), then
// retry after DOMContentLoaded in case we are evaluating before app.js runs.
if (window.__pulsarBus) {
	_wireBus();
} else {
	const _tryWire = () => {
		if (window.__pulsarBus) {
			_wireBus();
		} else {
			// Final fallback: listen for first DOMContentLoaded + a tick
			if (document.readyState !== 'loading') {
				setTimeout(_wireBus, 0);
			}
		}
	};
	if (document.readyState === 'loading') {
		document.addEventListener('DOMContentLoaded', _tryWire);
	} else {
		_tryWire();
	}
}

// Hide FAB by default (shown when in-session fires)
// Handled purely via CSS: .overlay-fab { display: none } / body.in-session .overlay-fab { display: flex }
// so no JS needed at boot time.

// ── Registration hooks for feature panels ─────────────────────────────────────
// The feature panels avoid a static import of overlay.js (circular-dependency
// risk) and instead register their cards through globals: quality.js/hud.js call
// `window.__pulsarRegisterCard`; display.js/audio.js/sidechannels.js use
// `window.__pulsarOverlay.registerCard`; all also listen for an `overlay-ready`
// event as a deferred fallback. overlay.js never published any of these, so NO
// card ever registered and the overlay opened empty. Publish them at import (this
// module loads before the panels — see app.js's `_importOverlay.then(...)`).
if (typeof window !== 'undefined') {
	window.__pulsarRegisterCard = registerCard;
	window.__pulsarOverlay = { registerCard, open, close, toggle, isOpen, cards: () => _cards.slice() };
	window.dispatchEvent(new CustomEvent('overlay-ready'));
}
