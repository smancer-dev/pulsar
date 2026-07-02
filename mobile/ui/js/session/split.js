/**
 * session/split.js — Multi-session split layout chooser (W5-split-js)
 *
 * DT-multisession-split (split half):
 *   Bottom-sheet layout chooser for split-screen sessions.
 *   Layouts: h2 (side-by-side landscape default), v2 (stacked portrait),
 *            grid4 (2×2 tablet, only offered on wide screens ≥600px).
 *   Also provides:
 *     - Per-pane distinct-target picker sheet (connect a 2nd host to slot 1)
 *     - Focused-pane indicator (highlight border on the active pane)
 *     - Exit-split control (collapse back to single-pane)
 *
 * Contract (§2.5, §2.6, §3):
 *   registerCard({ id:'split', modes:['remote','game'],
 *                  section:'tools', order:90, mount })
 *
 * Calls:
 *   invoke('connect_host', {...})   — connect a new target to slot 1
 *   invoke('set_play_resolution', { slot, width, height })
 *                                  — per-pane reduced res (720p) for the 2nd pane
 *   invoke('end_session', { slot }) — exit split by ending slot 1
 *
 * Reads from session registry (imported from session.js):
 *   registry — to know which slots are active and their mode/label
 *
 * Emits on JS bus:
 *   split-layout-changed  { layout: 'h2'|'v2'|'grid4' }
 *   split-active-pane     { slot: number }            (re-emits setActivePane)
 *   split-exited          {}                           (after slot 1 teardown)
 *
 * Touch-first design (DT-multisession-split):
 *   - Large ≥44px tap targets everywhere
 *   - Bottom sheet (visualViewport-aware) slides up from below
 *   - Layout options shown as visual icon tiles (indigo/cyan branded)
 *   - Per-pane target picker re-uses the bottom-sheet pattern
 *   - Safe-area-inset-bottom padding
 *   - Landscape h2 is default (most useful on a horizontal phone)
 */

import { invoke }        from '../tauri.js';
import { t }             from '../i18n.js';
import { registerCard }  from './overlay.js';
import { registry, endSession, setActivePane } from './session.js';
import { doConnect }     from '../screens/connect.js';

// ---------------------------------------------------------------------------
// JS bus accessor (lazy — avoids circular imports with app.js)
// ---------------------------------------------------------------------------

const getBus = () => window.__pulsarBus || null;
const busEmit = (name, detail) => {
	const b = getBus();
	if (b) b.emit(name, detail);
};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/** @type {'h2'|'v2'|'grid4'} */
let _layout = 'h2';

/** Whether the split layout sheet is currently open. */
let _sheetOpen = false;

/** Whether the per-pane target picker is open. */
let _pickerOpen = false;

/** The slot the target picker is operating on (usually 1). */
let _pickerSlot = 1;

// ---------------------------------------------------------------------------
// i18n keys (split-specific — added in this module; fall back to key if
// not yet in the main catalog). The t() helper falls back to key on miss.
// ---------------------------------------------------------------------------

/** Short helper that formats a slot number for display. */
const fmtSlotLabel = (slot) => t('m.session.slot', { n: slot + 1 });

// ---------------------------------------------------------------------------
// Layout descriptors
// ---------------------------------------------------------------------------

/**
 * @typedef {{ id:'h2'|'v2'|'grid4', labelTr:string, labelEn:string,
 *             icon:string, minWidth?:number }} LayoutDesc
 */

/** @type {LayoutDesc[]} */
const LAYOUTS = [
	{
		id:       'h2',
		labelTr:  'Yan yana',
		labelEn:  'Side by side',
		icon: `<svg width="36" height="28" viewBox="0 0 36 28" fill="none" aria-hidden="true">
			<rect x="1" y="1" width="15" height="26" rx="3" stroke="currentColor" stroke-width="1.75"/>
			<rect x="20" y="1" width="15" height="26" rx="3" stroke="currentColor" stroke-width="1.75"/>
		</svg>`,
	},
	{
		id:       'v2',
		labelTr:  'Üst alta',
		labelEn:  'Stacked',
		icon: `<svg width="28" height="36" viewBox="0 0 28 36" fill="none" aria-hidden="true">
			<rect x="1" y="1" width="26" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
			<rect x="1" y="20" width="26" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
		</svg>`,
	},
	{
		id:       'grid4',
		labelTr:  '2×2 ızgara',
		labelEn:  '2×2 grid',
		minWidth: 600,
		icon: `<svg width="36" height="36" viewBox="0 0 36 36" fill="none" aria-hidden="true">
			<rect x="1"  y="1"  width="15" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
			<rect x="20" y="1"  width="15" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
			<rect x="1"  y="20" width="15" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
			<rect x="20" y="20" width="15" height="15" rx="3" stroke="currentColor" stroke-width="1.75"/>
		</svg>`,
	},
];

/** Returns available layouts for the current screen width. */
function availableLayouts() {
	const w = window.innerWidth;
	return LAYOUTS.filter((l) => !l.minWidth || w >= l.minWidth);
}

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);

/** Resolve label for a layout synchronously (best-effort). */
function resolveLabel(desc) {
	// We can't use async here, so we pick based on document.documentElement.lang
	const lang = document.documentElement.lang || 'tr';
	return lang === 'en' ? desc.labelEn : desc.labelTr;
}

// ---------------------------------------------------------------------------
// Layout sheet DOM
// ---------------------------------------------------------------------------

const SHEET_ID    = 'split-layout-sheet';
const BACKDROP_ID = 'split-layout-backdrop';

/**
 * Build and open the layout chooser bottom-sheet.
 * If a layout sheet is already open, close it first.
 */
export function openLayoutSheet() {
	if (_sheetOpen) {
		closeLayoutSheet();
		return;
	}

	_sheetOpen = true;

	const avail    = availableLayouts();
	const isInSplit = registry.length > 1;

	// Build the option tiles HTML
	const tilesHtml = avail.map((desc) => {
		const active = _layout === desc.id ? ' split-tile--active' : '';
		const label  = resolveLabel(desc);
		return `
			<button
				class="split-tile${active}"
				data-layout="${desc.id}"
				type="button"
				aria-label="${label}"
				aria-pressed="${_layout === desc.id ? 'true' : 'false'}"
			>
				<span class="split-tile-icon" aria-hidden="true">${desc.icon}</span>
				<span class="split-tile-label">${label}</span>
			</button>
		`;
	}).join('');

	// Pane status rows (one per active slot)
	const paneRowsHtml = registry.length > 0
		? registry.map((entry) => _paneRowHtml(entry)).join('')
		: '';

	// Exit split is only shown when more than one session is active
	const exitHtml = isInSplit ? `
		<button class="btn btn-ghost split-exit-btn" type="button" id="split-exit-btn">
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<path d="M9 3H5a2 2 0 00-2 2v14a2 2 0 002 2h4" stroke="currentColor"
				      stroke-width="2" stroke-linecap="round"/>
				<polyline points="16 17 21 12 16 7" stroke="currentColor" stroke-width="2"
				          stroke-linecap="round" stroke-linejoin="round"/>
				<line x1="21" y1="12" x2="9" y2="12" stroke="currentColor" stroke-width="2"
				      stroke-linecap="round"/>
			</svg>
			${t('m.split.exitSplit')}
		</button>
	` : '';

	// Add-second-pane button (only if only one session is active)
	const addPaneHtml = !isInSplit ? `
		<button class="btn btn-primary split-add-pane-btn" type="button" id="split-add-pane-btn">
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<rect x="3" y="3" width="7" height="18" rx="2" stroke="currentColor" stroke-width="2"/>
				<rect x="14" y="3" width="7" height="18" rx="2" stroke="currentColor" stroke-width="2"/>
				<line x1="17.5" y1="9" x2="17.5" y2="15" stroke="currentColor" stroke-width="2"
				      stroke-linecap="round"/>
				<line x1="14.5" y1="12" x2="20.5" y2="12" stroke="currentColor" stroke-width="2"
				      stroke-linecap="round"/>
			</svg>
			${t('m.split.addPane')}
		</button>
	` : '';

	const html = `
		<div class="sheet-backdrop" id="${BACKDROP_ID}"></div>
		<div class="sheet split-sheet" id="${SHEET_ID}" role="dialog" aria-modal="true"
		     aria-label="${t('m.split.title')}">
			<div class="sheet-handle" aria-hidden="true"></div>
			<h3 class="split-sheet-title">${t('m.split.title')}</h3>

			<section class="split-sheet-section" aria-label="${t('m.split.layoutSection')}">
				<p class="split-sheet-sect-label">${t('m.split.layoutSection')}</p>
				<div class="split-tile-row" role="radiogroup"
				     aria-label="${t('m.split.layoutSection')}">
					${tilesHtml}
				</div>
			</section>

			${paneRowsHtml.length ? `
			<section class="split-sheet-section" aria-label="${t('m.split.panesSection')}">
				<p class="split-sheet-sect-label">${t('m.split.panesSection')}</p>
				<div class="split-pane-list">
					${paneRowsHtml}
				</div>
			</section>
			` : ''}

			<div class="split-sheet-actions">
				${addPaneHtml}
				${exitHtml}
			</div>
		</div>
	`;

	const wrapper = document.createElement('div');
	wrapper.className = 'split-sheet-wrapper';
	wrapper.id = 'split-sheet-wrapper';
	wrapper.innerHTML = html;
	document.body.appendChild(wrapper);

	// Animate in
	requestAnimationFrame(() => {
		$(`${SHEET_ID}`)?.classList.add('open');
		$(`${BACKDROP_ID}`)?.classList.add('open');
	});

	// Wire interactions
	_wireLayoutSheet(wrapper);
}

/** Close the layout sheet. */
export function closeLayoutSheet() {
	if (!_sheetOpen) return;
	_sheetOpen = false;

	const sheet    = $(SHEET_ID);
	const backdrop = $(BACKDROP_ID);
	const wrapper  = $('split-sheet-wrapper');

	sheet?.classList.remove('open');
	backdrop?.classList.remove('open');

	const cleanup = () => wrapper?.remove();
	sheet?.addEventListener('transitionend', cleanup, { once: true });
	setTimeout(cleanup, 420);
}

/** Wire all interactivity inside the layout sheet. */
function _wireLayoutSheet(wrapper) {
	// Backdrop tap → close
	$(BACKDROP_ID)?.addEventListener('click', closeLayoutSheet);

	// Layout tiles
	wrapper.querySelectorAll('.split-tile').forEach((btn) => {
		btn.addEventListener('click', () => {
			const layout = /** @type {'h2'|'v2'|'grid4'} */ (btn.dataset.layout);
			if (layout) _applyLayout(layout);
			closeLayoutSheet();
		});
	});

	// Pane target-change buttons
	wrapper.querySelectorAll('.split-pane-change-btn').forEach((btn) => {
		btn.addEventListener('click', () => {
			const slot = parseInt(btn.dataset.slot ?? '1', 10);
			closeLayoutSheet();
			_openTargetPicker(slot);
		});
	});

	// Pane focus buttons
	wrapper.querySelectorAll('.split-pane-focus-btn').forEach((btn) => {
		btn.addEventListener('click', () => {
			const slot = parseInt(btn.dataset.slot ?? '0', 10);
			setActivePane(slot);
			busEmit('split-active-pane', { slot });
			closeLayoutSheet();
		});
	});

	// Exit split button
	$('split-exit-btn')?.addEventListener('click', () => {
		closeLayoutSheet();
		_exitSplit();
	});

	// Add pane button
	$('split-add-pane-btn')?.addEventListener('click', () => {
		closeLayoutSheet();
		_openTargetPicker(1);
	});
}

/** HTML for one pane row in the sheet. */
function _paneRowHtml(entry) {
	const modeIcon = entry.mode === 'game'
		? `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
			<rect x="2" y="6" width="20" height="12" rx="4" stroke="currentColor" stroke-width="2"/>
			<line x1="8" y1="12" x2="12" y2="12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
			<line x1="10" y1="10" x2="10" y2="14" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
			<circle cx="16" cy="10.5" r="1" fill="currentColor"/>
			<circle cx="18" cy="12.5" r="1" fill="currentColor"/>
		</svg>`
		: `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
			<rect x="2" y="3" width="20" height="14" rx="2" stroke="currentColor" stroke-width="2"/>
			<line x1="8" y1="21" x2="16" y2="21" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
			<line x1="12" y1="17" x2="12" y2="21" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
		</svg>`;

	const modeClass = entry.mode === 'game' ? 'pane-mode--game' : 'pane-mode--remote';
	const slotLabel = fmtSlotLabel(entry.slot);

	return `
		<div class="split-pane-row" data-slot="${entry.slot}">
			<button class="split-pane-focus-btn" data-slot="${entry.slot}" type="button"
			        aria-label="${t('m.split.focusPane', { n: entry.slot + 1 })}">
				<span class="split-pane-slot-badge">${slotLabel}</span>
				<span class="split-pane-label">${escHtml(entry.label)}</span>
				<span class="split-pane-mode-icon ${modeClass}" aria-hidden="true">
					${modeIcon}
				</span>
			</button>
			<button class="split-pane-change-btn btn-ghost btn" data-slot="${entry.slot}"
			        type="button" aria-label="${t('m.split.changeTarget', { n: entry.slot + 1 })}">
				${t('m.split.change')}
			</button>
		</div>
	`;
}

// ---------------------------------------------------------------------------
// Layout application
// ---------------------------------------------------------------------------

/**
 * Apply a layout — updates _layout state, emits bus event, and adjusts
 * per-pane resolution for the secondary slot (720p for slot 1).
 *
 * @param {'h2'|'v2'|'grid4'} layout
 */
function _applyLayout(layout) {
	if (_layout === layout) return;
	_layout = layout;

	busEmit('split-layout-changed', { layout });

	// Nudge slot-1 resolution down to 720p when in a split layout to reduce
	// total bitrate (§5 W5-split-js brief: "per-cell reduced resolution 720p").
	const slot1 = registry.find((e) => e.slot === 1);
	if (slot1) {
		const { w, h } = _resForLayout(layout);
		invoke('set_play_resolution', { slot: 1, width: w, height: h })
			.catch((e) => console.warn('[split] set_play_resolution slot 1:', e));
	}
}

/**
 * Resolution to request for the secondary pane based on layout.
 * h2 and v2 → 720p; grid4 → 540p.
 *
 * @param {'h2'|'v2'|'grid4'} layout
 * @returns {{ w:number, h:number }}
 */
function _resForLayout(layout) {
	if (layout === 'grid4') return { w: 960,  h: 540  };
	return                          { w: 1280, h: 720  };
}

// ---------------------------------------------------------------------------
// Exit split
// ---------------------------------------------------------------------------

/**
 * Exit split mode: end the secondary (slot 1) session, then emit split-exited.
 */
async function _exitSplit() {
	// End slot 1 — this will drop it from the registry via session.js's
	// play-ended handler (or directly via endSession).
	const slot1 = registry.find((e) => e.slot === 1);
	if (slot1) {
		try {
			await endSession(1);
		} catch (e) {
			console.warn('[split] exit split — endSession(1) error:', e);
		}
	}

	// Reset layout to single pane default
	_layout = 'h2';
	busEmit('split-exited', {});
}

// ---------------------------------------------------------------------------
// Per-pane target picker sheet
// ---------------------------------------------------------------------------

const PICKER_SHEET_ID    = 'split-picker-sheet';
const PICKER_BACKDROP_ID = 'split-picker-backdrop';

/**
 * Open the target picker sheet for a specific pane slot.
 * This lets the user connect a different host to slot 1 (or change slot 0's
 * target, though the more common use is adding a second pane).
 *
 * @param {number} slot
 */
function _openTargetPicker(slot) {
	if (_pickerOpen) return;
	_pickerOpen = true;
	_pickerSlot = slot;

	const currentEntry = registry.find((e) => e.slot === slot);
	const slotLabel    = fmtSlotLabel(slot);
	const inputId      = `split-picker-target-${slot}`;
	const modeRemoteId = `split-picker-mode-remote-${slot}`;
	const modeGameId   = `split-picker-mode-game-${slot}`;

	// Detect which mode the primary pane is in (slot 0) so the picker
	// defaults to the same mode for consistency.
	const primaryEntry = registry.find((e) => e.slot === 0);
	const defaultMode  = currentEntry?.mode ?? primaryEntry?.mode ?? 'remote';

	const pickerHtml = `
		<div class="sheet-backdrop" id="${PICKER_BACKDROP_ID}"></div>
		<div class="sheet split-picker-sheet" id="${PICKER_SHEET_ID}" role="dialog"
		     aria-modal="true" aria-label="${t('m.split.pickerTitle', { n: slot + 1 })}">
			<div class="sheet-handle" aria-hidden="true"></div>
			<h3 class="split-picker-title">
				${t('m.split.pickerTitle', { n: slot + 1 })}
				<span class="split-picker-slot-badge">${slotLabel}</span>
			</h3>
			<p class="split-picker-lead">${t('m.split.pickerLead')}</p>

			<div class="field">
				<label for="${inputId}" class="split-picker-field-label">
					${t('home.idOrIp')}
				</label>
				<input
					id="${inputId}"
					class="input mono split-picker-input"
					type="text"
					inputmode="text"
					autocomplete="off"
					autocorrect="off"
					autocapitalize="none"
					spellcheck="false"
					placeholder="${t('home.idOrIp')}"
					value="${escHtml(currentEntry?.id ?? '')}"
				/>
			</div>

			<div class="split-picker-mode-row">
				<label class="split-picker-mode-label">${t('m.split.modeLabel')}</label>
				<div class="seg split-picker-seg" role="group"
				     aria-label="${t('m.split.modeLabel')}">
					<button class="seg-btn${defaultMode === 'remote' ? ' active' : ''}"
					        id="${modeRemoteId}" type="button"
					        data-mode="remote" aria-pressed="${defaultMode === 'remote' ? 'true' : 'false'}">
						${t('home.modeRemote')}
					</button>
					<button class="seg-btn${defaultMode === 'game' ? ' active' : ''}"
					        id="${modeGameId}" type="button"
					        data-mode="game" aria-pressed="${defaultMode === 'game' ? 'true' : 'false'}">
						${t('home.modeGame')}
					</button>
				</div>
			</div>

			<p class="split-picker-error" id="split-picker-error-${slot}"
			   role="alert" aria-live="polite" style="display:none">
				${t('m.split.targetRequired')}
			</p>

			<div class="split-picker-actions">
				<button class="btn btn-ghost" type="button" id="split-picker-cancel">
					${t('m.cancel')}
				</button>
				<button class="btn btn-primary" type="button" id="split-picker-connect">
					${currentEntry ? t('m.split.reconnect') : t('m.split.connect')}
				</button>
			</div>
		</div>
	`;

	const wrapper = document.createElement('div');
	wrapper.className = 'split-picker-wrapper';
	wrapper.id = 'split-picker-wrapper';
	wrapper.innerHTML = pickerHtml;
	document.body.appendChild(wrapper);

	// Animate in
	requestAnimationFrame(() => {
		$(PICKER_SHEET_ID)?.classList.add('open');
		$(PICKER_BACKDROP_ID)?.classList.add('open');
	});

	// Auto-focus the target input
	setTimeout(() => $(`split-picker-target-${slot}`)?.focus(), 80);

	// Track selected mode
	let selectedMode = defaultMode;

	// Wire mode seg buttons
	wrapper.querySelectorAll('.split-picker-seg .seg-btn').forEach((btn) => {
		btn.addEventListener('click', () => {
			selectedMode = btn.dataset.mode ?? 'remote';
			wrapper.querySelectorAll('.split-picker-seg .seg-btn').forEach((b) => {
				b.classList.toggle('active', b === btn);
				b.setAttribute('aria-pressed', b === btn ? 'true' : 'false');
			});
		});
	});

	// Cancel
	$('split-picker-cancel')?.addEventListener('click', _closeTargetPicker);
	$(PICKER_BACKDROP_ID)?.addEventListener('click',   _closeTargetPicker);

	// Connect
	$('split-picker-connect')?.addEventListener('click', async () => {
		const input  = /** @type {HTMLInputElement|null} */ ($(`split-picker-target-${slot}`));
		const target = (input?.value ?? '').trim().replace(/\s/g, '');
		const errEl  = $(`split-picker-error-${slot}`);

		if (!target) {
			if (errEl) errEl.style.display = '';
			input?.focus();
			return;
		}
		if (errEl) errEl.style.display = 'none';

		const connectBtn = /** @type {HTMLButtonElement|null} */ ($('split-picker-connect'));
		if (connectBtn) {
			connectBtn.disabled    = true;
			connectBtn.textContent = t('status.connecting');
		}

		_closeTargetPicker();

		// If there's already a session on this slot, end it first
		if (registry.find((e) => e.slot === slot)) {
			try { await endSession(slot); } catch { /* ignore */ }
		}

		// Connect the new target — doConnect handles connect_host + startSession
		try {
			await doConnect(target, slot, selectedMode);
		} catch (e) {
			console.warn('[split] picker connect error:', e);
		}

		// Reduce secondary pane resolution to 720p
		if (slot !== 0) {
			const { w, h } = _resForLayout(_layout);
			invoke('set_play_resolution', { slot, width: w, height: h })
				.catch((e2) => console.warn('[split] set_play_resolution slot', slot, e2));
		}
	});

	// Enter key on input
	$(`split-picker-target-${slot}`)?.addEventListener('keydown', (e) => {
		if (e.key === 'Enter') $('split-picker-connect')?.click();
	});
}

/** Close the target picker sheet. */
function _closeTargetPicker() {
	if (!_pickerOpen) return;
	_pickerOpen = false;

	const sheet    = $(PICKER_SHEET_ID);
	const backdrop = $(PICKER_BACKDROP_ID);
	const wrapper  = $('split-picker-wrapper');

	sheet?.classList.remove('open');
	backdrop?.classList.remove('open');

	const cleanup = () => wrapper?.remove();
	sheet?.addEventListener('transitionend', cleanup, { once: true });
	setTimeout(cleanup, 420);
}

// ---------------------------------------------------------------------------
// Escape helper
// ---------------------------------------------------------------------------

/** Basic HTML entity escaping for safe string interpolation. */
function escHtml(str) {
	return String(str)
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;')
		.replace(/"/g, '&quot;');
}

// ---------------------------------------------------------------------------
// Overlay card registration
// ---------------------------------------------------------------------------

/**
 * The split card mount function — renders a compact trigger row inside the
 * overlay dock. The actual sheet is opened via openLayoutSheet().
 *
 * @param {HTMLElement} el
 */
function mountSplitCard(el) {
	el.innerHTML = `
		<div class="split-card-inner">
			<div class="split-card-header">
				<svg width="20" height="20" viewBox="0 0 24 24" fill="none"
				     class="split-card-icon" aria-hidden="true">
					<rect x="2" y="3" width="8" height="18" rx="2"
					      stroke="currentColor" stroke-width="2"/>
					<rect x="14" y="3" width="8" height="18" rx="2"
					      stroke="currentColor" stroke-width="2"/>
				</svg>
				<span class="split-card-title">${t('m.split.title')}</span>
				<span class="split-card-layout-badge" id="split-card-layout-badge">
					${_layoutBadgeLabel(_layout)}
				</span>
			</div>
			<p class="split-card-desc">${t('m.split.cardDesc')}</p>
			<div class="split-card-actions">
				<button class="btn btn-primary split-card-open-btn" type="button"
				        id="split-card-open-btn">
					${registry.length > 1 ? t('m.split.changeLayout') : t('m.split.startSplit')}
				</button>
				${registry.length > 1 ? `
				<button class="btn btn-ghost split-card-exit-btn" type="button"
				        id="split-card-exit-btn">
					${t('m.split.exitSplit')}
				</button>
				` : ''}
			</div>
		</div>
	`;

	// Open the layout sheet
	el.querySelector('#split-card-open-btn')?.addEventListener('click', openLayoutSheet);

	// Exit split (if in split mode)
	el.querySelector('#split-card-exit-btn')?.addEventListener('click', () => {
		_exitSplit();
	});

	// React to layout changes so the badge updates live
	const bus = getBus();
	if (bus) {
		bus.on('split-layout-changed', ({ layout }) => {
			const badge = $('split-card-layout-badge');
			if (badge) badge.textContent = _layoutBadgeLabel(layout);
		});
		bus.on('split-exited', () => {
			// Refresh the card
			const openBtn = /** @type {HTMLButtonElement|null} */ ($('split-card-open-btn'));
			if (openBtn) openBtn.textContent = t('m.split.startSplit');
			const exitBtn = /** @type {HTMLButtonElement|null} */ ($('split-card-exit-btn'));
			exitBtn?.remove();
		});
		bus.on('session-started', () => {
			// Refresh open btn text when new session starts
			const openBtn = /** @type {HTMLButtonElement|null} */ ($('split-card-open-btn'));
			if (openBtn) {
				openBtn.textContent = registry.length > 1
					? t('m.split.changeLayout')
					: t('m.split.startSplit');
			}
		});
	}
}

/** Short text for the layout badge. */
function _layoutBadgeLabel(layout) {
	switch (layout) {
		case 'h2':    return 'H2';
		case 'v2':    return 'V2';
		case 'grid4': return '2×2';
		default:      return '';
	}
}

// ---------------------------------------------------------------------------
// Register with the overlay
// ---------------------------------------------------------------------------

registerCard({
	id:      'split',
	modes:   ['remote', 'game'],
	section: 'tools',
	order:   90,
	mount:   mountSplitCard,
});

// ---------------------------------------------------------------------------
// Inline styles (self-contained so this module needs no components.css edit)
// ---------------------------------------------------------------------------

function _injectStyles() {
	if (document.getElementById('split-module-styles')) return;
	const style = document.createElement('style');
	style.id = 'split-module-styles';
	style.textContent = `
/* ============================================================
   split.js — layout sheet + picker sheet + card styles
   ============================================================ */

/* ---- Sheet wrapper (portal layer) -------------------------------- */
.split-sheet-wrapper,
.split-picker-wrapper {
	position: fixed;
	inset: 0;
	z-index: 28;
	pointer-events: none;
}
.split-sheet-wrapper .sheet-backdrop,
.split-sheet-wrapper .split-sheet,
.split-picker-wrapper .sheet-backdrop,
.split-picker-wrapper .split-picker-sheet {
	pointer-events: auto;
}

/* ---- Layout chooser sheet ---------------------------------------- */
.split-sheet {
	display: flex;
	flex-direction: column;
	gap: 20px;
	padding-bottom: calc(24px + var(--safe-bottom, 0px));
	max-height: 88dvh;
	overflow-y: auto;
	overscroll-behavior: contain;
}

.split-sheet-title {
	font-family: var(--font-display);
	font-size: 18px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
}

.split-sheet-section {
	display: flex;
	flex-direction: column;
	gap: 10px;
}

.split-sheet-sect-label {
	font-size: 12px;
	font-weight: 600;
	letter-spacing: 0.06em;
	text-transform: uppercase;
	color: var(--text-faint);
	margin: 0;
}

/* ---- Layout tiles ------------------------------------------------ */
.split-tile-row {
	display: flex;
	gap: 10px;
	flex-wrap: wrap;
}

.split-tile {
	flex: 1 1 88px;
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 8px;
	min-height: 80px;
	min-width: 80px;
	padding: 14px 10px;
	border: 1.5px solid var(--border);
	border-radius: var(--r-lg);
	background: var(--surface);
	color: var(--text-muted);
	cursor: pointer;
	transition:
		border-color 160ms var(--ease),
		background   160ms var(--ease),
		color        160ms var(--ease),
		box-shadow   160ms var(--ease);
}
.split-tile:active {
	transform: scale(0.97);
}
.split-tile:hover {
	border-color: var(--border-strong);
	background: var(--surface-2);
}
.split-tile--active,
.split-tile--active:hover {
	border-color: var(--brand, var(--accent));
	background: var(--accent-soft);
	color: var(--brand, var(--accent));
	box-shadow: 0 0 0 3px oklch(from var(--brand, var(--accent)) l c h / 0.15);
}
[data-mode='game'] .split-tile--active,
[data-mode='game'] .split-tile--active:hover {
	background: var(--cyan-soft);
}

.split-tile-icon {
	display: flex;
	align-items: center;
	justify-content: center;
	color: inherit;
}
.split-tile-label {
	font-size: 12.5px;
	font-weight: 600;
	color: inherit;
	text-align: center;
	white-space: nowrap;
}

/* ---- Pane list ---------------------------------------------------- */
.split-pane-list {
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.split-pane-row {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 10px 12px;
	border: 1px solid var(--border);
	border-radius: var(--r);
	background: var(--surface);
}

.split-pane-focus-btn {
	flex: 1;
	display: flex;
	align-items: center;
	gap: 8px;
	min-height: var(--touch-min, 44px);
	background: transparent;
	border: none;
	padding: 0;
	cursor: pointer;
	text-align: left;
}
.split-pane-focus-btn:active { opacity: 0.7; }

.split-pane-slot-badge {
	font-family: var(--font-mono);
	font-size: 11px;
	font-weight: 600;
	color: var(--text-on-accent);
	background: var(--brand, var(--accent));
	border-radius: var(--r-pill);
	padding: 2px 8px;
	white-space: nowrap;
	flex-shrink: 0;
}

.split-pane-label {
	font-size: 14px;
	font-weight: 500;
	color: var(--text);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	flex: 1;
}

.split-pane-mode-icon {
	display: flex;
	align-items: center;
	justify-content: center;
	flex-shrink: 0;
}
.split-pane-mode-icon.pane-mode--remote { color: var(--accent); }
.split-pane-mode-icon.pane-mode--game   { color: var(--cyan);   }

.split-pane-change-btn {
	flex-shrink: 0;
	padding: 8px 14px;
	font-size: 13px;
	min-height: var(--touch-min, 44px);
}

/* ---- Sheet actions ------------------------------------------------ */
.split-sheet-actions {
	display: flex;
	flex-direction: column;
	gap: 10px;
	margin-top: 4px;
}

.split-exit-btn {
	width: 100%;
	justify-content: center;
	color: var(--danger);
	border-color: oklch(from var(--danger) l c h / 0.3);
}
.split-exit-btn:hover {
	background: var(--danger-soft);
	border-color: var(--danger);
}

.split-add-pane-btn {
	width: 100%;
	justify-content: center;
}

/* ---- Target picker sheet ----------------------------------------- */
.split-picker-sheet {
	display: flex;
	flex-direction: column;
	gap: 16px;
	padding-bottom: calc(24px + var(--safe-bottom, 0px));
	max-height: 88dvh;
	overflow-y: auto;
	overscroll-behavior: contain;
}

.split-picker-title {
	font-family: var(--font-display);
	font-size: 18px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
	display: flex;
	align-items: center;
	gap: 10px;
}

.split-picker-slot-badge {
	font-family: var(--font-mono);
	font-size: 12px;
	font-weight: 600;
	color: var(--text-on-accent);
	background: var(--brand, var(--accent));
	border-radius: var(--r-pill);
	padding: 3px 10px;
}

.split-picker-lead {
	font-size: 13.5px;
	color: var(--text-muted);
	line-height: 1.55;
	margin: 0;
}

.split-picker-field-label {
	display: block;
	font-size: 13px;
	font-weight: 600;
	color: var(--text-muted);
	margin-bottom: 6px;
}

.split-picker-input {
	font-size: var(--input-size, 16px);
	letter-spacing: 0.04em;
}

.split-picker-mode-row {
	display: flex;
	flex-direction: column;
	gap: 8px;
}
.split-picker-mode-label {
	font-size: 13px;
	font-weight: 600;
	color: var(--text-muted);
}

/* Segmented control (reuse .seg atom) */
.split-picker-seg {
	display: flex;
	border: 1.5px solid var(--border);
	border-radius: var(--r);
	overflow: hidden;
	background: var(--surface-2);
}
.split-picker-seg .seg-btn {
	flex: 1;
	min-height: var(--touch-min, 44px);
	padding: 0 14px;
	font-size: 14px;
	font-weight: 600;
	border: none;
	background: transparent;
	color: var(--text-muted);
	cursor: pointer;
	transition: background 140ms var(--ease), color 140ms var(--ease);
}
.split-picker-seg .seg-btn.active {
	background: var(--surface);
	color: var(--brand, var(--accent));
	box-shadow: inset 0 0 0 1.5px var(--brand, var(--accent));
}
.split-picker-seg .seg-btn:first-child {
	border-radius: calc(var(--r) - 2px) 0 0 calc(var(--r) - 2px);
}
.split-picker-seg .seg-btn:last-child {
	border-radius: 0 calc(var(--r) - 2px) calc(var(--r) - 2px) 0;
}

.split-picker-error {
	font-size: 13px;
	color: var(--danger);
	text-align: center;
	margin: -4px 0 0;
}

.split-picker-actions {
	display: grid;
	grid-template-columns: 1fr 2fr;
	gap: 10px;
}
.split-picker-actions .btn {
	min-height: var(--touch-min, 44px);
	border-radius: var(--r-pill);
	justify-content: center;
}

/* ---- Overlay card ------------------------------------------------- */
.split-card-inner {
	display: flex;
	flex-direction: column;
	gap: 12px;
}

.split-card-header {
	display: flex;
	align-items: center;
	gap: 10px;
}

.split-card-icon {
	flex-shrink: 0;
	color: var(--brand, var(--accent));
}

.split-card-title {
	font-size: 15px;
	font-weight: 700;
	color: var(--text);
	flex: 1;
}

.split-card-layout-badge {
	font-family: var(--font-mono);
	font-size: 11px;
	font-weight: 700;
	color: var(--brand, var(--accent));
	background: var(--accent-soft);
	border-radius: var(--r-pill);
	padding: 3px 9px;
	letter-spacing: 0.05em;
}
[data-mode='game'] .split-card-layout-badge {
	background: var(--cyan-soft);
	color: var(--cyan);
}

.split-card-desc {
	font-size: 13px;
	color: var(--text-muted);
	line-height: 1.55;
	margin: 0;
}

.split-card-actions {
	display: flex;
	flex-direction: column;
	gap: 8px;
}
.split-card-actions .btn {
	min-height: var(--touch-min, 44px);
	justify-content: center;
	border-radius: var(--r-pill);
}
.split-card-exit-btn {
	color: var(--danger);
	border-color: oklch(from var(--danger) l c h / 0.35);
}
.split-card-exit-btn:hover {
	background: var(--danger-soft);
	border-color: var(--danger);
}

/* ---- Focused-pane indicator (applied by body[data-split-active]) --
   The W5-native lane positions actual pane SurfaceViews; this CSS
   shows a highlight ring on the active pane slot badge inside the
   layout sheet when re-opened.                                     */
.split-pane-row[data-slot].split-pane-row--active .split-pane-slot-badge {
	box-shadow: 0 0 0 2px var(--surface), 0 0 0 4px var(--brand, var(--accent));
}
`;
	document.head.appendChild(style);
}

// ---------------------------------------------------------------------------
// Listen for active-pane-changed to update the focused-pane indicator
// ---------------------------------------------------------------------------

function _initBusListeners() {
	const bus = getBus();
	if (!bus) {
		// Retry once after DOMContentLoaded if bus not yet available
		window.addEventListener('load', _initBusListeners, { once: true });
		return;
	}

	bus.on('active-pane-changed', (slot) => {
		// Update visual active indicator inside any open layout sheet
		document.querySelectorAll('.split-pane-row').forEach((row) => {
			const rowSlot = parseInt(/** @type {HTMLElement} */ (row).dataset.slot ?? '-1', 10);
			row.classList.toggle('split-pane-row--active', rowSlot === slot);
		});
	});

	// Close sheets when a session ends (play-ended fires from session.js)
	bus.on('session-ended', () => {
		closeLayoutSheet();
		_closeTargetPicker();
	});
}

// ---------------------------------------------------------------------------
// Public API (re-export convenience — the functions are already exported above)
// ---------------------------------------------------------------------------

// openLayoutSheet and closeLayoutSheet are exported at their declaration sites.

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

_injectStyles();

if (document.readyState === 'loading') {
	document.addEventListener('DOMContentLoaded', _initBusListeners);
} else {
	_initBusListeners();
}
