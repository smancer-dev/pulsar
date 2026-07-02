/**
 * display.js — In-session display controls card (W3-media-native)
 *
 * DT-display: View-fit + orientation + multi-monitor picker.
 * Remote-only — declared with modes: ['remote'].
 *
 * W3 scope (this file):
 *   - Fit / Fill / Stretch segmented control → plugin:pulsar-video|set_aspect
 *   - Orientation toggle (portrait ↔ landscape) → plugin:pulsar-video|set_orientation
 *
 * W4-sidechannels scope (display.js is edited by W4-sidechannels to add):
 *   - Multi-monitor picker (host-displays event → bottom-sheet list)
 *   → set_play_monitor { slot, displayIdx }
 *
 * Registers with the overlay card registry:
 *   section: 'display'
 *   modes:   ['remote']  — hidden in game mode
 *
 * Calls (plugin):
 *   plugin:pulsar-video|set_aspect       { slot, mode: 'fit'|'fill'|'stretch' }
 *   plugin:pulsar-video|set_orientation  { landscape: bool }
 *
 * Listens (Tauri events — W4 addition):
 *   host-displays  { slot, displays: [{idx,name,width,height,primary}] }
 *
 * Design (DT-display):
 *   - Segmented control with three aspect choices, active option highlighted
 *     with brand token (indigo in remote mode)
 *   - Orientation toggle pill with auto-detect / portrait / landscape options
 *   - Monitor picker: shown only when >1 display; name + WxH + 'Ana' primary
 *     badge; current selection highlighted; switching spinner
 *   - All tap targets ≥44px; safe-area aware
 */

import { invoke, listen } from '../tauri.js';
import { t } from '../i18n.js';

// ---- host-displays Tauri event (W4-sidechannels adds this) -------------------

/** Unlisten handle for the host-displays Tauri event. */
let _unlistenHostDisplays = null;

/**
 * Subscribe to the `host-displays` Tauri event.
 * Emitted by W4-rust-client when StreamCaps.displays is captured.
 * Payload: { slot: u8, displays: [{idx, name, width, height, primary}] }
 */
async function _subscribeHostDisplays() {
	if (_unlistenHostDisplays) return; // already subscribed
	try {
		_unlistenHostDisplays = await listen('host-displays', (payload) => {
			if (payload == null) return;
			// Guard: only process events for the active slot
			if (typeof payload.slot === 'number' && payload.slot !== _slot) return;
			const displays = Array.isArray(payload.displays) ? payload.displays : [];
			renderDisplays(displays);
		});
	} catch (e) {
		console.warn('[display] listen host-displays failed:', e);
	}
}

function _unsubscribeHostDisplays() {
	if (typeof _unlistenHostDisplays === 'function') {
		_unlistenHostDisplays();
		_unlistenHostDisplays = null;
	}
}

// ---- State -------------------------------------------------------------------

/** Active session slot. */
let _slot = 0;

/** Current aspect mode: 'fit' | 'fill' | 'stretch' */
let _aspect = 'fit';

/** Current orientation request: 'auto' | 'portrait' | 'landscape' */
let _orientation = 'auto';

/** Current pointer mode: 'mouse' (trackpad/relative) | 'touch' (absolute). */
let _pointerMode = (() => {
	try { return localStorage.getItem('pulsar.input.mode.v1') === 'touch' ? 'touch' : 'mouse'; }
	catch (_) { return 'mouse'; }
})();

/**
 * Available host displays (populated by W4-sidechannels via host-displays event).
 * @type {Array<{idx:number, name:string, width:number, height:number, primary:boolean}>}
 */
let _displays = [];

/** Currently selected display index. */
let _selectedDisplay = null;

/** Spinner active (display switch in progress). */
let _switchingDisplay = false;

// ---- DOM helpers -------------------------------------------------------------

const $ = (id) => document.getElementById(id);

// ---- Card mount --------------------------------------------------------------

const CARD_ID = 'display-card';

/**
 * Mount the display card into the provided container element.
 * Called by overlay.js when the overlay opens the display section.
 * @param {HTMLElement} container
 */
function mount(container) {
	container.innerHTML = `
<div class="display-card" id="${CARD_ID}" aria-label="${t('display.cardLabel')}">

  <!-- Aspect ratio segmented control -->
  <div class="display-section">
    <span class="display-section-label">${t('display.aspectLabel')}</span>
    <div class="display-seg" role="group" aria-label="${t('display.aspectLabel')}" id="display-aspect-seg">
      ${_aspectOption('fit',     t('display.fit'),     '◻')}
      ${_aspectOption('fill',    t('display.fill'),    '▣')}
      ${_aspectOption('stretch', t('display.stretch'), '⬛')}
    </div>
    <p class="display-hint" id="display-aspect-hint">${_aspectHint(_aspect)}</p>
  </div>

  <!-- Orientation control -->
  <div class="display-section display-section-orient">
    <span class="display-section-label">${t('display.orientLabel')}</span>
    <div class="display-seg" role="group" aria-label="${t('display.orientLabel')}" id="display-orient-seg">
      ${_orientOption('auto',      t('display.orientAuto'))}
      ${_orientOption('portrait',  t('display.orientPortrait'))}
      ${_orientOption('landscape', t('display.orientLandscape'))}
    </div>
  </div>

  <!-- Pointer mode: mouse (trackpad/relative cursor) vs touch (absolute) -->
  <div class="display-section">
    <span class="display-section-label">${t('display.pointerLabel')}</span>
    <div class="display-seg" role="group" aria-label="${t('display.pointerLabel')}" id="display-pointer-seg">
      ${_pointerOption('mouse', t('display.pointerMouse'))}
      ${_pointerOption('touch', t('display.pointerTouch'))}
    </div>
    <p class="display-hint">${t('display.pointerHint')}</p>
  </div>

  <!-- Monitor picker — empty initially, populated by W4-sidechannels via renderDisplays() -->
  <div class="display-section" id="display-monitor-section"
       style="display:${_displays.length > 1 ? 'block' : 'none'}">
    <span class="display-section-label">${t('display.monitorLabel')}</span>
    <div class="display-monitor-list" id="display-monitor-list" role="listbox"
         aria-label="${t('display.monitorLabel')}">
      ${_renderMonitorList()}
    </div>
  </div>

</div>
`;

	_wireAspect();
	_wireOrientation();
	_wirePointer();
	_wireMonitorList();
}

/** A pointer-mode segmented-control option button. */
function _pointerOption(value, label) {
	const active = _pointerMode === value;
	return `
<button
  class="display-seg-btn ${active ? 'active' : ''}"
  data-pointer="${value}"
  type="button"
  role="radio"
  aria-checked="${active}"
  aria-label="${label}"
>
  <span class="display-seg-label">${label}</span>
</button>`;
}

/** Wire the pointer-mode toggle → persist + notify the input engine (app.js). */
function _wirePointer() {
	const seg = $('display-pointer-seg');
	if (!seg) return;
	seg.addEventListener('click', (e) => {
		const btn = e.target.closest('[data-pointer]');
		if (!btn) return;
		const m = btn.dataset.pointer;
		if (m === _pointerMode) return;
		_pointerMode = m;
		seg.querySelectorAll('.display-seg-btn').forEach((b) => {
			const a = b.dataset.pointer === m;
			b.classList.toggle('active', a);
			b.setAttribute('aria-checked', String(a));
		});
		try { localStorage.setItem('pulsar.input.mode.v1', m); } catch (_) {}
		window.__pulsarBus?.emit('input-mode-changed', m);
	});
}

// ---- Aspect helpers ----------------------------------------------------------

function _aspectOption(value, label, icon) {
	const active = _aspect === value;
	return `
<button
  class="display-seg-btn ${active ? 'active' : ''}"
  data-value="${value}"
  type="button"
  role="radio"
  aria-checked="${active}"
  aria-label="${label}"
>
  <span class="display-seg-icon" aria-hidden="true">${icon}</span>
  <span class="display-seg-label">${label}</span>
</button>`;
}

function _aspectHint(mode) {
	switch (mode) {
		case 'fill':    return t('display.fillHint');
		case 'stretch': return t('display.stretchHint');
		default:        return t('display.fitHint');
	}
}

function _wireAspect() {
	const seg = $('display-aspect-seg');
	if (!seg) return;

	seg.addEventListener('click', async (e) => {
		const btn = e.target.closest('[data-value]');
		if (!btn) return;

		const newMode = btn.dataset.value;
		if (newMode === _aspect) return;
		_aspect = newMode;

		// Update UI immediately (optimistic)
		seg.querySelectorAll('.display-seg-btn').forEach((b) => {
			const active = b.dataset.value === newMode;
			b.classList.toggle('active', active);
			b.setAttribute('aria-checked', String(active));
		});

		const hint = $('display-aspect-hint');
		if (hint) hint.textContent = _aspectHint(newMode);

		try {
			await invoke('plugin:pulsar-video|set_aspect', { slot: _slot, mode: newMode });
		} catch (e) {
			console.warn('[display] setAspect error:', e);
		}
	});
}

// ---- Orientation helpers -----------------------------------------------------

function _orientOption(value, label) {
	const active = _orientation === value;
	return `
<button
  class="display-seg-btn ${active ? 'active' : ''}"
  data-orient="${value}"
  type="button"
  role="radio"
  aria-checked="${active}"
  aria-label="${label}"
>
  <span class="display-seg-label">${label}</span>
</button>`;
}

function _wireOrientation() {
	const seg = $('display-orient-seg');
	if (!seg) return;

	seg.addEventListener('click', async (e) => {
		const btn = e.target.closest('[data-orient]');
		if (!btn) return;

		const newOrient = btn.dataset.orient;
		if (newOrient === _orientation) return;
		_orientation = newOrient;

		// Update UI immediately
		seg.querySelectorAll('.display-seg-btn').forEach((b) => {
			const active = b.dataset.orient === newOrient;
			b.classList.toggle('active', active);
			b.setAttribute('aria-checked', String(active));
		});

		// Map to native: 'auto' → let the OS decide (portrait as default)
		const landscape = newOrient === 'landscape';
		if (newOrient === 'auto') {
			// Nothing to send for auto — let OS handle; best-effort reset
			// by sending portrait=false (sensor-portrait which Android respects)
			try {
				await invoke('plugin:pulsar-video|set_orientation', { landscape: false });
			} catch (e) {
				console.warn('[display] setOrientation (auto) error:', e);
			}
		} else {
			try {
				await invoke('plugin:pulsar-video|set_orientation', { landscape });
			} catch (e) {
				console.warn('[display] setOrientation error:', e);
			}
		}
	});
}

// ---- Monitor list (W4-sidechannels populates _displays) ----------------------

function _renderMonitorList() {
	if (_displays.length === 0) return '';
	return _displays.map((d) => {
		const isSelected = _selectedDisplay === d.idx;
		return `
<button
  class="display-monitor-item ${isSelected ? 'selected' : ''}"
  data-idx="${d.idx}"
  type="button"
  role="option"
  aria-selected="${isSelected}"
>
  <span class="display-monitor-info">
    <span class="display-monitor-name">${d.name || t('display.monitorFallbackName', { idx: d.idx })}</span>
    <span class="display-monitor-res">${d.width}×${d.height}</span>
  </span>
  ${d.primary ? `<span class="display-monitor-badge">${t('display.primary')}</span>` : ''}
  ${isSelected ? `<span class="display-monitor-check" aria-hidden="true">✓</span>` : ''}
  ${_switchingDisplay && isSelected ? `<span class="display-monitor-spinner" aria-hidden="true"></span>` : ''}
</button>`;
	}).join('');
}

function _wireMonitorList() {
	const list = $('display-monitor-list');
	if (!list) return;

	list.addEventListener('click', async (e) => {
		const item = e.target.closest('[data-idx]');
		if (!item) return;

		const idx = Number(item.dataset.idx);
		if (idx === _selectedDisplay || _switchingDisplay) return;

		_selectedDisplay   = idx;
		_switchingDisplay  = true;
		_rerenderMonitorList();

		try {
			await invoke('set_play_monitor', { slot: _slot, displayIdx: idx });
		} catch (e) {
			console.warn('[display] set_play_monitor error:', e);
		} finally {
			_switchingDisplay = false;
			_rerenderMonitorList();
		}
	});
}

function _rerenderMonitorList() {
	const list = $('display-monitor-list');
	if (list) list.innerHTML = _renderMonitorList();
	_wireMonitorList();

	const section = $('display-monitor-section');
	if (section) section.style.display = _displays.length > 1 ? 'block' : 'none';
}

// ---- Public API (W4-sidechannels calls these) --------------------------------

/**
 * Update the display list from the host-displays event.
 * W4-sidechannels calls this after listening to the `host-displays` Tauri event.
 * @param {Array<{idx:number, name:string, width:number, height:number, primary:boolean}>} displays
 */
export function renderDisplays(displays) {
	_displays = displays || [];
	if (_selectedDisplay === null && _displays.length > 0) {
		const primary = _displays.find((d) => d.primary);
		_selectedDisplay = primary ? primary.idx : _displays[0].idx;
	}
	_rerenderMonitorList();
}

// ---- Inline styles -----------------------------------------------------------

function _injectStyles() {
	if (document.getElementById('display-card-styles')) return;
	const style = document.createElement('style');
	style.id = 'display-card-styles';
	style.textContent = `
/* ---- display.js card styles ---- */

.display-card {
  display: flex;
  flex-direction: column;
  gap: 0;
  width: 100%;
  padding: 4px 0;
}

.display-section {
  padding: 12px 16px 10px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.display-section + .display-section {
  border-top: 1px solid var(--border);
}

.display-section-label {
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-faint);
}

/* Segmented control row */
.display-seg {
  display: flex;
  gap: 6px;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--r-sm);
  padding: 4px;
}

.display-seg-btn {
  flex: 1;
  min-height: 44px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 2px;
  border: 1.5px solid transparent;
  border-radius: var(--r-xs);
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
  font-family: var(--font-sans);
  font-size: 13px;
  font-weight: 500;
  transition:
    background var(--dur) var(--ease),
    color var(--dur) var(--ease),
    border-color var(--dur) var(--ease);
  touch-action: manipulation;
}
.display-seg-btn:active { opacity: 0.75; }
.display-seg-btn:focus-visible {
  outline: 2px solid var(--brand, var(--accent));
  outline-offset: 1px;
}
.display-seg-btn.active {
  background: var(--surface);
  color: var(--brand, var(--accent));
  border-color: oklch(from var(--brand, var(--accent)) l c h / 0.25);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.display-seg-icon {
  font-size: 18px;
  line-height: 1;
  pointer-events: none;
}

.display-seg-label {
  font-size: 12px;
  pointer-events: none;
}

/* Aspect hint */
.display-hint {
  font-size: 12px;
  color: var(--text-faint);
  margin: 0;
  line-height: 1.5;
  padding: 0 2px;
}

/* Orientation section — 3-option seg */
.display-section-orient .display-seg-btn {
  flex-direction: row;
  gap: 5px;
  font-size: 12.5px;
}

/* Monitor picker list */
.display-monitor-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.display-monitor-item {
  width: 100%;
  min-height: 52px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border: 1.5px solid var(--border);
  border-radius: var(--r);
  background: var(--surface-2);
  cursor: pointer;
  text-align: left;
  transition:
    background var(--dur) var(--ease),
    border-color var(--dur) var(--ease);
  touch-action: manipulation;
}
.display-monitor-item:active { opacity: 0.8; }
.display-monitor-item.selected {
  background: oklch(from var(--brand, var(--accent)) l c h / 0.06);
  border-color: oklch(from var(--brand, var(--accent)) l c h / 0.35);
}
.display-monitor-item:focus-visible {
  outline: 2px solid var(--brand, var(--accent));
  outline-offset: 2px;
}

.display-monitor-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.display-monitor-name {
  font-size: 14.5px;
  font-weight: 600;
  color: var(--text);
  line-height: 1.3;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.display-monitor-res {
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-faint);
}

.display-monitor-badge {
  flex: none;
  padding: 2px 8px;
  border-radius: var(--r-pill);
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.03em;
}

.display-monitor-check {
  flex: none;
  color: var(--brand, var(--accent));
  font-size: 16px;
  font-weight: 700;
}

/* Spinner for display switching */
.display-monitor-spinner {
  flex: none;
  width: 18px;
  height: 18px;
  border: 2.5px solid var(--border);
  border-top-color: var(--brand, var(--accent));
  border-radius: 50%;
  animation: display-spin 0.7s linear infinite;
}
@keyframes display-spin {
  to { transform: rotate(360deg); }
}
`;
	document.head.appendChild(style);
}

// ---- Bus wiring --------------------------------------------------------------

function _wireBus() {
	const tryBus = () => {
		const bus = window.__pulsarBus;
		if (!bus) return false;

		bus.on('session-started', ({ slot }) => {
			_slot            = slot ?? 0;
			_aspect          = 'fit';
			_orientation     = 'auto';
			_displays        = [];
			_selectedDisplay = null;
			_switchingDisplay = false;
			// Subscribe to host-displays event (W4-sidechannels)
			_subscribeHostDisplays();
		});

		bus.on('session-ended', () => {
			_displays        = [];
			_selectedDisplay = null;
			// Unsubscribe from host-displays event
			_unsubscribeHostDisplays();
		});

		return true;
	};

	if (!tryBus()) {
		window.addEventListener('load', () => tryBus(), { once: true });
	}
}

// ---- Register card -----------------------------------------------------------

function _register() {
	const tryReg = () => {
		const overlay = window.__pulsarOverlay;
		if (!overlay || typeof overlay.registerCard !== 'function') return false;

		overlay.registerCard({
			id:      'display',
			modes:   ['remote'],   // remote only — hidden in game mode
			section: 'display',
			order:   20,
			label:   () => t('display.cardLabel'),
			mount,
		});
		return true;
	};

	if (!tryReg()) {
		window.addEventListener('load', () => tryReg(), { once: true });
	}
}

// ---- Boot --------------------------------------------------------------------

_injectStyles();
_wireBus();
_register();
