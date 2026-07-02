/**
 * gamepad-nav.js — navigate the whole app UI with a physical controller.
 *
 * Spatial focus model (no manual wiring): D-pad / left stick move focus to the
 * nearest focusable in that direction, A activates, B goes back (closes the
 * topmost sheet/overlay, else switches to the previous tab). A focus ring
 * (`.gp-focus`) marks the current element.
 *
 * Active only when NOT in a streaming session (in-session the pad is forwarded to
 * the host as gameplay input) and while the app is foregrounded. The Web Gamepad
 * API exposes a pad only after the first button press, so navigation begins once
 * the user touches the controller.
 */

const SEL = [
	'a[href]', 'button:not([disabled])', 'input:not([disabled])',
	'select:not([disabled])', 'textarea:not([disabled])',
	'[tabindex]:not([tabindex="-1"])',
	'.item', '.pill-btn', '.seg button', '.toggle', '.fab', '.icon-btn',
].join(',');

// Standard-mapping button indices.
const BTN = { A: 0, B: 1, UP: 12, DOWN: 13, LEFT: 14, RIGHT: 15 };
const STICK = 0.55;       // left-stick deflection that counts as a direction
const REPEAT_FIRST = 380; // ms before a held direction repeats
const REPEAT = 120;       // ms between repeats

let _current = null;
let _prevBtns = [];
let _heldDir = null;
let _nextMoveAt = 0;
let _running = false;

function _now() { return (performance && performance.now) ? performance.now() : Date.now(); }

function _visible(el) {
	// Rendered (not display:none / hidden tab) but NOT required to be in the viewport —
	// off-screen-but-scrollable content stays navigable; _setCurrent scrolls it in.
	if (!el || el.offsetParent === null) return false;
	const r = el.getBoundingClientRect();
	return r.width > 0 && r.height > 0;
}

/** Topmost open modal scope (sheet / overlay dock), or null. */
function _scope() {
	const sheets = [...document.querySelectorAll('.sheet.open, .overlay-dock.open, .sheet-backdrop.open')]
		.filter((e) => e.classList.contains('sheet') || e.classList.contains('overlay-dock'));
	return sheets.length ? sheets[sheets.length - 1] : null;
}

function _focusables() {
	// In a modal → scope to it. Otherwise scope to the active scroll container (the
	// .tab.on lives inside it) — this naturally excludes the fixed bottom nav AND any
	// CLOSED sheet/overlay (those are translated off-screen but still rendered, so a
	// plain document query would pull their "Allow/Deny/Grant permission" buttons into
	// the navigation and make it oscillate). Bottom nav is reached via down-at-the-end.
	const root = _scope()
		|| [...document.querySelectorAll('main.scroll')].find((m) => m.offsetParent !== null)
		|| document;
	return [...root.querySelectorAll(SEL)].filter((el) =>
		_visible(el)
		&& !el.closest('nav.bottom')
		// The .toggle's checkbox is an opacity:0 overlay — focus the visible label instead.
		&& !(el.tagName === 'INPUT' && el.closest('.toggle'))
	);
}

/** The scrollable element to drive with the right stick (sheet list, else screen). */
function _scrollEl() {
	const scope = _scope();
	if (scope) return scope.querySelector('.licenses-list, main.scroll, [data-scroll]') || scope;
	return [...document.querySelectorAll('main.scroll')].find((m) => m.offsetParent !== null)
		|| document.scrollingElement;
}

function _setCurrent(el) {
	if (_current === el) return;
	if (_current) _current.classList.remove('gp-focus');
	_current = el || null;
	if (_current) {
		_current.classList.add('gp-focus');
		try { _current.scrollIntoView({ block: 'nearest', inline: 'nearest' }); } catch (_) {}
	}
}

const _inBottomNav = (el) => !!(el && el.closest && el.closest('nav.bottom'));
const _bottomNavBtns = () => [...document.querySelectorAll('nav.bottom button')].filter(_visible);

/** Spatial-nearest focusable from `from` in `dir`, within `items`. */
function _nearest(dir, from, items) {
	const cr = from.getBoundingClientRect();
	const cx = cr.left + cr.width / 2, cy = cr.top + cr.height / 2;
	let best = null, bestScore = Infinity;
	for (const el of items) {
		if (el === from) continue;
		const r = el.getBoundingClientRect();
		const dx = (r.left + r.width / 2) - cx;
		const dy = (r.top + r.height / 2) - cy;
		let primary, cross;
		if (dir === 'up')        { if (dy >= -4) continue; primary = -dy; cross = Math.abs(dx); }
		else if (dir === 'down') { if (dy <= 4) continue;  primary = dy;  cross = Math.abs(dx); }
		else if (dir === 'left') { if (dx >= -4) continue; primary = -dx; cross = Math.abs(dy); }
		else                     { if (dx <= 4) continue;  primary = dx;  cross = Math.abs(dy); }
		const score = primary + cross * 2.2; // aligned + near wins
		if (score < bestScore) { bestScore = score; best = el; }
	}
	return best;
}

function _toBottomNav() {
	const nav = _bottomNavBtns();
	if (!nav.length) return;
	_setCurrent(nav.find((b) => b.classList.contains('on')) || nav[0]);
}

function _move(dir) {
	// Inside the bottom nav: left/right cycle tabs, up returns to the content.
	if (_current && _inBottomNav(_current)) {
		const nav = _bottomNavBtns();
		const idx = nav.indexOf(_current);
		if (dir === 'left' && idx > 0) { _setCurrent(nav[idx - 1]); return; }
		if (dir === 'right' && idx < nav.length - 1) { _setCurrent(nav[idx + 1]); return; }
		if (dir === 'up') {
			const items = _focusables();
			if (items.length) _setCurrent(items[items.length - 1]);
		}
		return;
	}

	const items = _focusables();
	if (!_current || !items.includes(_current) || !_visible(_current)) {
		if (items.length) _setCurrent(items[0]); else _toBottomNav();
		return;
	}
	const best = _nearest(dir, _current, items);
	if (best) { _setCurrent(best); return; }
	// No content in this direction — down at the end drops into the bottom nav.
	if (dir === 'down') _toBottomNav();
}

function _activate() {
	if (!_current) { const f = _focusables(); if (f.length) _setCurrent(f[0]); return; }
	const el = _current;
	if (/^(input|textarea)$/i.test(el.tagName)) { el.focus(); return; }
	if (el.tagName === 'SELECT') { el.focus(); return; }
	el.click();
}

function _back() {
	// An open popup ALWAYS wins — close it instead of navigating pages.
	if (_closeTopPopup()) return;
	// No popup → previous bottom-nav tab.
	const tabs = [...document.querySelectorAll('nav.bottom button')];
	const active = tabs.findIndex((b) => b.classList.contains('on'));
	if (active > 0) tabs[active - 1].click();
}

/**
 * Close the topmost open popup. All sheets/overlays/backdrops carry `.open`, but
 * the backdrop class varies (`.sheet-backdrop`, `.overlay-backdrop`, and
 * `.sheet-overlay` for the devices add-sheet) — missing one made B fall through to
 * page nav. Close order: dismissable backdrop → close/cancel button → strip `.open`.
 * @returns {boolean} true if a popup was closed.
 */
function _closeTopPopup() {
	const openEls = [...document.querySelectorAll(
		'.sheet-backdrop.open, .overlay-backdrop.open, .sheet-overlay.open, .sheet.open, .overlay-dock.open'
	)];
	if (!openEls.length) return false;
	const bd = openEls.find((el) => el.matches('.sheet-backdrop.open, .overlay-backdrop.open, .sheet-overlay.open'));
	if (bd) { bd.click(); }
	const stillOpen = () => document.querySelector('.sheet.open, .overlay-dock.open, .sheet-overlay.open, .sheet-backdrop.open, .overlay-backdrop.open');
	if (bd && !stillOpen()) return true;
	// Backdrop wasn't dismissable (or nothing happened) — try a close/cancel button.
	const top = openEls[openEls.length - 1];
	const sheet = top.closest('.sheet, .overlay-dock') || top;
	const btn = sheet.querySelector(
		'#overlay-close-btn, .overlay-close, [data-action="cancel"], [data-sheet-close], #cf-cancel, #netpill-ok'
	);
	if (btn) { btn.click(); if (!stillOpen()) return true; }
	// Last resort: just hide everything open.
	openEls.forEach((el) => el.classList.remove('open'));
	return true;
}

/** Switch bottom-nav tab by delta (−1 prev, +1 next). */
function _tab(delta) {
	const tabs = [...document.querySelectorAll('nav.bottom button')];
	if (!tabs.length) return;
	let active = tabs.findIndex((b) => b.classList.contains('on'));
	if (active < 0) active = 0;
	const next = Math.min(tabs.length - 1, Math.max(0, active + delta));
	if (next !== active) { tabs[next].click(); _setCurrent(null); }
}

function _pad() {
	if (!navigator.getGamepads) return null;
	const pads = navigator.getGamepads();
	for (const p of pads) if (p && p.connected) return p;
	return null;
}

function _tick() {
	if (!_running) return;
	const inSession = document.body.classList.contains('in-session');
	const foreground = document.visibilityState !== 'hidden';
	const p = (!inSession && foreground) ? _pad() : null;

	if (p) {
		const btns = p.buttons.map((b) => b.pressed);
		const ax = p.axes || [];
		const pressed = (i) => btns[i] && !_prevBtns[i];

		if (pressed(BTN.A)) _activate();
		if (pressed(BTN.B)) _back();

		// Direction from D-pad OR left stick.
		let dir = null;
		if (btns[BTN.UP] || ax[1] < -STICK) dir = 'up';
		else if (btns[BTN.DOWN] || ax[1] > STICK) dir = 'down';
		else if (btns[BTN.LEFT] || ax[0] < -STICK) dir = 'left';
		else if (btns[BTN.RIGHT] || ax[0] > STICK) dir = 'right';

		const now = _now();
		if (dir) {
			if (dir !== _heldDir) { _heldDir = dir; _move(dir); _nextMoveAt = now + REPEAT_FIRST; }
			else if (now >= _nextMoveAt) { _move(dir); _nextMoveAt = now + REPEAT; }
		} else {
			_heldDir = null;
		}

		// Right stick → scroll the active scrollable (content / open sheet list).
		const ry = ax[3] || 0;
		if (Math.abs(ry) > 0.18) {
			const sc = _scrollEl();
			if (sc) sc.scrollBy(0, ry * 18);
		}

		// Shoulder buttons (LB/RB) → previous/next bottom-nav tab.
		if (pressed(4)) _tab(-1);
		if (pressed(5)) _tab(1);

		_prevBtns = btns;
	} else {
		_prevBtns = [];
		_heldDir = null;
		if (_current && !_visible(_current)) _setCurrent(null);
	}
	requestAnimationFrame(_tick);
}

export function startGamepadNav() {
	if (_running) return;
	_running = true;
	requestAnimationFrame(_tick);
}
