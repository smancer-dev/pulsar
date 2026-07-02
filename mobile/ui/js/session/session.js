/**
 * session.js — In-session lifecycle controller (W2-session + W5-session-js)
 *
 * Owns:
 *   - body.in-session lifecycle (via router enterSession/exitSession)
 *   - Per-slot session registry
 *   - play-ended handler: drops session, detaches surface, shows disconnect card
 *   - play-stall / play-firstframe state updates
 *   - auth-prompt bottom sheet (DT-client-authprompt)
 *   - btn-end: invokes end_session before detaching
 *
 * W5 additions (DT-multisession-split):
 *   - Touch session switcher: horizontal pill row over the session bar when
 *     multiple sessions are active; swipe-down expands to a full session sheet.
 *   - setActivePane(slot): routes input + audio to the foregrounded slot.
 *   - Per-session rename: long-press a pill OR tap "Yeniden Adlandır" in the
 *     session sheet to open an inline rename sheet.
 *   - Active-pane indication: the focused pill is branded (indigo/cyan), others
 *     are dimmed; the focused slot drives body[data-mode].
 *   - Slot focus is propagated via bus 'active-pane-changed' so input.js,
 *     audio.js, and any future routing can follow the active slot.
 *   - setActivePane also calls Rust set_active_pane (W5-rust-session command)
 *     to reroute native audio output; best-effort (no-op if Tauri not ready).
 *
 * Exports:
 *   registry          — array of { slot, id, codec, mode, label, transport,
 *                                   stalled, firstFrame }
 *   startSession(opts)— registers a slot, enters in-session, wires bar meta
 *   endSession(slot)  — programmatic teardown (calls end_session + cleanup)
 *   setActivePane(slot)— marks which pane receives input + audio; updates UI
 *   renameSession(slot, label) — update the label for a slot; re-renders switcher
 *
 * Listens (Tauri events):
 *   play-ended       — { slot, reason } — read-loop exited
 *   play-stall       — { slot, stalled:bool } — video stall state
 *   play-firstframe  — { slot } — first decoded frame arrived
 *   auth-prompt      — { slot, peer:string } — host needs OTP
 *   conn-phase       — { slot, phase, transport? } — connecting phase updates
 *
 * Calls:
 *   end_session      — { slot }
 *   submit_password  — { slot, password }
 *   set_active_pane  — { slot }  (W5; best-effort)
 *   plugin:pulsar-video|detach — (no args)
 *
 * Design (DT-multisession-split):
 *   Touch-first, portrait + landscape phone. Pill row: 44px+ tap targets,
 *   horizontally scrollable, indigo(remote)/cyan(game) accent on focused pill,
 *   dimmed on background sessions, stall/stall-dot indicator per pill.
 *   Swipe-down handle on the pill row opens the session sheet (bottom sheet,
 *   safe-area aware) showing each session as a card with rename + end controls.
 *   Rename: inline bottom sheet with a text input, JetBrains Mono, 16px.
 */

import { enterSession, exitSession } from '../router.js';
import { invoke, listen, hasTauri } from '../tauri.js';
import { t } from '../i18n.js';
import { doConnect } from '../screens/connect.js';

// ---- JS→JS bus (lazy — avoids circular import with app.js) -------------------

const getBus = () => window.__pulsarBus || null;
const busOn  = (name, cb) => {
	const b = getBus();
	if (b) { b.on(name, cb); } else {
		window.addEventListener('load', () => { const b2 = getBus(); if (b2) b2.on(name, cb); }, { once: true });
	}
};

// ---- Session registry --------------------------------------------------------

/**
 * Per-slot session descriptor.
 * @typedef {{ slot:number, id:string, codec:string, mode:'remote'|'game',
 *             label:string, transport:string, stalled:boolean, firstFrame:boolean }} SessionEntry
 */

/** @type {SessionEntry[]} */
export const registry = [];

/** Currently focused pane for input + audio routing (W5 multi-session). */
let _activeSlot = 0;

// ---- DOM helpers -------------------------------------------------------------

const $ = (id) => document.getElementById(id);
const MOUNT = () => $('mount-session');

// ---- Pill row & session sheet IDs -------------------------------------------

const PILL_ROW_ID     = 'session-pill-row';
const SESS_SHEET_ID   = 'session-sheet';
const SESS_BACKDROP_ID= 'session-sheet-backdrop';

// ---- Session switcher pill row (W5) -----------------------------------------

/**
 * Build/re-render the horizontal pill row above (or inside) the in-session bar.
 * Shown only when registry.length >= 2. Hidden (removed) when ≤1 session.
 *
 * Layout:
 *   [ #session-pill-row ]  — a scrollable flex row appended to #bar-pill-area
 *   Each pill:
 *     [ mode-dot ] [ label ] [ stall-dot? ] [ × end btn ]
 *
 * The "swipe down" handle is a thin pill (visual affordance) — tapping it opens
 * the full session sheet. A touch drag down on the pill row also opens the sheet.
 */
function renderPillRow() {
	// Remove existing pill row first
	const existing = $(PILL_ROW_ID);
	if (existing) existing.remove();

	if (registry.length < 2) return; // not needed for a single session

	const area = $('bar-pill-area');
	if (!area) return;

	const row = document.createElement('div');
	row.id        = PILL_ROW_ID;
	row.className = 'session-pill-row';
	row.setAttribute('role', 'tablist');
	row.setAttribute('aria-label', t('m.session.switcher.label'));

	// Sheet-open handle (thin horizontal bar affordance — tap or swipe down)
	const handle = document.createElement('button');
	handle.className = 'session-pill-handle';
	handle.setAttribute('aria-label', t('m.session.switcher.open'));
	handle.setAttribute('type', 'button');
	handle.innerHTML = `<span class="session-pill-handle-bar"></span>`;
	handle.addEventListener('click', () => openSessionSheet());
	row.appendChild(handle);

	// Pill scroller
	const scroller = document.createElement('div');
	scroller.className = 'session-pill-scroller';

	for (const entry of registry) {
		const pill = _buildPill(entry);
		scroller.appendChild(pill);
	}

	row.appendChild(scroller);

	// Swipe-down gesture to open sheet
	_wireSwipeDown(row, () => openSessionSheet());

	area.appendChild(row);
}

/**
 * Build one session pill element.
 * @param {SessionEntry} entry
 */
function _buildPill(entry) {
	const isActive  = entry.slot === _activeSlot;
	const modeClass = entry.mode === 'game' ? 'pill-game' : 'pill-remote';

	const pill = document.createElement('div');
	pill.className = [
		'session-pill',
		modeClass,
		isActive ? 'pill-active' : 'pill-bg',
		entry.stalled ? 'pill-stalled' : '',
	].filter(Boolean).join(' ');
	pill.setAttribute('role', 'tab');
	pill.setAttribute('aria-selected', String(isActive));
	pill.setAttribute('aria-label',
		t('m.session.slot', { n: entry.slot + 1 }) + ': ' + entry.label);
	pill.dataset.slot = String(entry.slot);

	// Mode dot (small circle, branded colour)
	const modeDot = document.createElement('span');
	modeDot.className = 'pill-mode-dot';
	modeDot.setAttribute('aria-hidden', 'true');

	// Label
	const labelSpan = document.createElement('span');
	labelSpan.className = 'pill-label';
	labelSpan.textContent = entry.label;

	// Stall indicator (shown only when stalled)
	const stallDot = document.createElement('span');
	stallDot.className = 'pill-stall-dot';
	stallDot.setAttribute('aria-hidden', 'true');
	stallDot.style.display = entry.stalled ? '' : 'none';

	// End (×) button
	const endBtn = document.createElement('button');
	endBtn.className = 'pill-end-btn';
	endBtn.type = 'button';
	endBtn.setAttribute('aria-label', t('m.session.endSlot', { n: entry.slot + 1 }));
	endBtn.innerHTML =
		`<svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
		  <path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" stroke-width="1.8"
		        stroke-linecap="round"/>
		</svg>`;

	endBtn.addEventListener('click', (e) => {
		e.stopPropagation();
		endSession(entry.slot).catch(() => {});
	});

	pill.appendChild(modeDot);
	pill.appendChild(labelSpan);
	pill.appendChild(stallDot);
	pill.appendChild(endBtn);

	// Tap the pill body (not the × button) → set active
	pill.addEventListener('click', (e) => {
		if (e.target === endBtn || endBtn.contains(e.target)) return;
		setActivePane(entry.slot);
	});

	return pill;
}

/**
 * Wire a swipe-down gesture on an element to trigger a callback.
 * Threshold: 40px downward drag.
 * @param {HTMLElement} el
 * @param {Function} cb
 */
function _wireSwipeDown(el, cb) {
	let startY = null;
	el.addEventListener('touchstart', (e) => {
		if (e.touches.length === 1) startY = e.touches[0].clientY;
	}, { passive: true });
	el.addEventListener('touchend', (e) => {
		if (startY === null) return;
		const dy = (e.changedTouches[0]?.clientY ?? 0) - startY;
		startY = null;
		if (dy > 40) cb();
	}, { passive: true });
}

// ---- Session sheet (swipe-down full list) ------------------------------------

/**
 * Open the session management sheet.
 * Shows each active session as a card with: label, slot badge, codec/transport,
 * "Odaklan" (focus/switch), "Yeniden Adlandır", "Bitir".
 */
function openSessionSheet() {
	// Prevent duplicate sheets
	if ($(SESS_SHEET_ID)) return;

	const backdrop = document.createElement('div');
	backdrop.id        = SESS_BACKDROP_ID;
	backdrop.className = 'sheet-backdrop';
	backdrop.addEventListener('click', closeSessionSheet);

	const sheet = document.createElement('div');
	sheet.id        = SESS_SHEET_ID;
	sheet.className = 'sheet session-sheet';
	sheet.setAttribute('role', 'dialog');
	sheet.setAttribute('aria-modal', 'true');
	sheet.setAttribute('aria-label', t('m.session.switcher.label'));

	sheet.innerHTML = `
		<div class="sheet-handle" aria-hidden="true"></div>
		<h3 class="session-sheet-title">${t('m.session.switcher.label')}</h3>
		<div class="session-sheet-list" id="session-sheet-list"></div>
		<button class="btn btn-ghost session-sheet-close" type="button"
		        id="${SESS_SHEET_ID}-close">
			${t('m.close')}
		</button>
	`;

	document.body.appendChild(backdrop);
	document.body.appendChild(sheet);

	// Populate list
	_renderSessionSheetList();

	// Animate in
	requestAnimationFrame(() => {
		sheet.classList.add('open');
		backdrop.classList.add('open');
	});

	$(`${SESS_SHEET_ID}-close`)?.addEventListener('click', closeSessionSheet);

	// Swipe-down on the sheet handle closes the sheet
	const handle = sheet.querySelector('.sheet-handle');
	if (handle) _wireSwipeDown(handle, closeSessionSheet);
}

function _renderSessionSheetList() {
	const list = $('session-sheet-list');
	if (!list) return;

	list.innerHTML = '';

	if (registry.length === 0) {
		list.innerHTML = `<p class="session-sheet-empty">${t('m.session.noSessions')}</p>`;
		return;
	}

	for (const entry of registry) {
		const card = _buildSessionCard(entry);
		list.appendChild(card);
	}
}

/**
 * Build a session card element for the session sheet.
 * @param {SessionEntry} entry
 */
function _buildSessionCard(entry) {
	const isActive  = entry.slot === _activeSlot;
	const modeClass = entry.mode === 'game' ? 'scard-game' : 'scard-remote';
	const modeLabelKey = entry.mode === 'game'
		? 'connecting.modeGame'
		: 'connecting.modeRemote';

	const transportLabel =
		entry.transport === 'direct' ? t('m.session.transport.direct') :
		entry.transport === 'relay'  ? t('m.session.transport.relay')  :
		entry.transport || '—';

	const codecLabel = (entry.codec || 'H.264').toUpperCase();

	const card = document.createElement('div');
	card.className = [
		'session-card',
		modeClass,
		isActive ? 'scard-active' : '',
	].filter(Boolean).join(' ');
	card.dataset.slot = String(entry.slot);

	card.innerHTML = `
		<div class="scard-header">
			<div class="scard-badge ${modeClass}-badge">
				${t('m.session.slot', { n: entry.slot + 1 })}
			</div>
			<span class="scard-label" id="scard-label-${entry.slot}">${entry.label}</span>
			${isActive ? `<span class="scard-active-dot" aria-label="${t('m.session.focused')}"></span>` : ''}
		</div>
		<div class="scard-meta">
			<span class="scard-mode">${t(modeLabelKey)}</span>
			<span class="scard-sep">·</span>
			<span class="scard-codec mono">${codecLabel}</span>
			<span class="scard-sep">·</span>
			<span class="scard-transport">${transportLabel}</span>
			${entry.stalled ? `<span class="scard-stall">${t('m.session.stalled')}</span>` : ''}
		</div>
		<div class="scard-id mono">${fmtId(entry.id)}</div>
		<div class="scard-actions">
			${!isActive ? `
			<button class="btn btn-primary scard-btn-focus" type="button"
			        data-action="focus" data-slot="${entry.slot}">
				${t('m.session.focus')}
			</button>
			` : `
			<div class="scard-focused-badge">
				<svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
					<circle cx="7" cy="7" r="6" stroke="currentColor" stroke-width="1.5" opacity=".5"/>
					<circle cx="7" cy="7" r="3" fill="currentColor"/>
				</svg>
				${t('m.session.focused')}
			</div>
			`}
			<button class="btn btn-ghost scard-btn-rename" type="button"
			        data-action="rename" data-slot="${entry.slot}">
				${t('m.session.rename')}
			</button>
			<button class="btn btn-ghost scard-btn-end" type="button"
			        data-action="end" data-slot="${entry.slot}">
				${t('m.session.endSession')}
			</button>
		</div>
	`;

	// Wire actions
	card.querySelectorAll('[data-action]').forEach((btn) => {
		btn.addEventListener('click', (e) => {
			const action = btn.dataset.action;
			const slot   = Number(btn.dataset.slot);
			if (action === 'focus') {
				setActivePane(slot);
				closeSessionSheet();
			} else if (action === 'rename') {
				closeSessionSheet();
				openRenameSheet(slot);
			} else if (action === 'end') {
				closeSessionSheet();
				endSession(slot).catch(() => {});
			}
		});
	});

	return card;
}

function closeSessionSheet() {
	const sheet    = $(SESS_SHEET_ID);
	const backdrop = $(SESS_BACKDROP_ID);
	if (sheet)    sheet.classList.remove('open');
	if (backdrop) backdrop.classList.remove('open');
	setTimeout(() => {
		sheet?.remove();
		backdrop?.remove();
	}, 380);
}

// ---- Rename sheet ------------------------------------------------------------

/**
 * Open a bottom sheet to rename a session slot.
 * @param {number} slot
 */
function openRenameSheet(slot) {
	const entry = registry.find((e) => e.slot === slot);
	if (!entry) return;

	const sheetId    = `rename-sheet-${slot}`;
	const inputId    = `rename-input-${slot}`;
	const submitId   = `rename-submit-${slot}`;
	const cancelId   = `rename-cancel-${slot}`;
	const backdropId = `rename-backdrop-${slot}`;

	// Remove existing rename sheet if any
	$(`rename-wrapper-${slot}`)?.remove();

	const wrapper = document.createElement('div');
	wrapper.className = 'rename-sheet-wrapper';
	wrapper.id        = `rename-wrapper-${slot}`;

	wrapper.innerHTML = `
		<div class="sheet-backdrop" id="${backdropId}"></div>
		<div class="sheet rename-sheet" id="${sheetId}" role="dialog" aria-modal="true"
		     aria-labelledby="rename-title-${slot}">
			<div class="sheet-handle" aria-hidden="true"></div>
			<h3 class="rename-sheet-title" id="rename-title-${slot}">
				${t('m.session.renameTitle')}
			</h3>
			<p class="rename-sheet-sub">
				${t('m.session.slot', { n: slot + 1 })} · ${fmtId(entry.id)}
			</p>
			<div class="field">
				<label for="${inputId}" class="sr-only">${t('m.session.renameLabel')}</label>
				<input
					id="${inputId}"
					class="input rename-sheet-input"
					type="text"
					inputmode="text"
					autocorrect="off"
					autocapitalize="sentences"
					spellcheck="false"
					maxlength="40"
					placeholder="${entry.label}"
					value="${entry.label}"
				/>
			</div>
			<div class="rename-sheet-actions">
				<button class="btn btn-ghost" id="${cancelId}" type="button">
					${t('m.cancel')}
				</button>
				<button class="btn btn-primary" id="${submitId}" type="button">
					${t('m.save')}
				</button>
			</div>
		</div>
	`;

	document.body.appendChild(wrapper);

	const renameSheet = $(`rename-sheet-${slot}`);
	const backdropEl  = $(`rename-backdrop-${slot}`);

	requestAnimationFrame(() => {
		renameSheet?.classList.add('open');
		backdropEl?.classList.add('open');
	});

	// Focus input after animation
	setTimeout(() => $(`rename-input-${slot}`)?.focus(), 80);

	function closeRename() {
		renameSheet?.classList.remove('open');
		backdropEl?.classList.remove('open');
		setTimeout(() => wrapper.remove(), 380);
	}

	function commitRename() {
		const input = $(`rename-input-${slot}`);
		const newLabel = (input?.value || '').trim();
		if (newLabel) {
			renameSession(slot, newLabel);
		}
		closeRename();
	}

	// Enter key
	$(`rename-input-${slot}`)?.addEventListener('keydown', (e) => {
		if (e.key === 'Enter') commitRename();
		if (e.key === 'Escape') closeRename();
	});

	// Select all on focus for easy replacement
	$(`rename-input-${slot}`)?.addEventListener('focus', (e) => {
		e.target.select();
	});

	$(`rename-submit-${slot}`)?.addEventListener('click', commitRename);
	$(`rename-cancel-${slot}`)?.addEventListener('click', closeRename);
	backdropEl?.addEventListener('click', closeRename);
}

// ---- Session card rendering (disconnect) ------------------------------------

/**
 * Render the disconnect card into #mount-session.
 * Provides a "Tekrar Bağlan" button that calls doConnect with the last
 * known id + slot.
 *
 * @param {{ slot:number, id:string, mode:'remote'|'game', reason:string }} opts
 */
function renderDisconnectCard({ slot, id, mode, reason }) {
	const mount = MOUNT();
	if (!mount) return;

	// Friendly reason mapping
	const reasonMap = {
		'connect-timed-out': t('connErr.timeout'),
		'peer-unreachable':  t('connErr.peerUnreachable'),
		'relay-down':        t('connErr.relayDown'),
	};
	const reasonText = reasonMap[reason] || t('session.streamStopped');

	// Mode-aware brand class
	const brandClass = mode === 'game' ? 'brand-game' : 'brand-remote';

	mount.innerHTML = `
		<div class="session-disconnect-backdrop"></div>
		<div class="session-disconnect-card ${brandClass}" role="dialog" aria-modal="true"
		     aria-label="${t('m.session.disconnected')}">
			<div class="sdc-icon" aria-hidden="true">
				<svg width="40" height="40" viewBox="0 0 24 24" fill="none">
					<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="1.75"
					        opacity="0.25"/>
					<path d="M8.5 8.5l7 7M15.5 8.5l-7 7" stroke="currentColor" stroke-width="2.2"
					      stroke-linecap="round"/>
				</svg>
			</div>
			<h3 class="sdc-title">${t('m.session.disconnected')}</h3>
			<p class="sdc-reason">${reasonText}</p>
			<div class="sdc-id mono">${fmtId(id)}</div>
			<button class="btn btn-primary sdc-reconnect" type="button"
			        id="sdc-reconnect-${slot}">
				<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
					<path d="M4 12a8 8 0 0114.93-4" stroke="currentColor" stroke-width="2"
					      stroke-linecap="round"/>
					<polyline points="19 4 18.93 8 15 8" stroke="currentColor" stroke-width="2"
					          stroke-linecap="round" stroke-linejoin="round"/>
				</svg>
				${t('m.session.reconnect')}
			</button>
			<button class="btn btn-ghost sdc-dismiss" type="button" id="sdc-dismiss-${slot}">
				${t('m.cancel')}
			</button>
		</div>
	`;

	// Animate in
	requestAnimationFrame(() => {
		const card = mount.querySelector('.session-disconnect-card');
		if (card) card.classList.add('sdc-visible');
	});

	// Reconnect button
	$(`sdc-reconnect-${slot}`)?.addEventListener('click', () => {
		clearDisconnectCard();
		doConnect(id, slot).catch((e) => console.warn('[session] reconnect error', e));
	});

	// Dismiss — goes back to the connect tab
	$(`sdc-dismiss-${slot}`)?.addEventListener('click', () => {
		clearDisconnectCard();
	});

	// Backdrop tap = dismiss
	mount.querySelector('.session-disconnect-backdrop')?.addEventListener('click', () => {
		clearDisconnectCard();
	});
}

function clearDisconnectCard() {
	const mount = MOUNT();
	if (!mount) return;
	const card = mount.querySelector('.session-disconnect-card');
	if (card) {
		card.classList.remove('sdc-visible');
		// Remove after transition (350ms)
		const onEnd = () => { mount.innerHTML = ''; card.removeEventListener('transitionend', onEnd); };
		card.addEventListener('transitionend', onEnd);
		setTimeout(() => { mount.innerHTML = ''; }, 450);
	} else {
		mount.innerHTML = '';
	}
}

// ---- Auth-prompt sheet -------------------------------------------------------

/**
 * Show the OTP / password bottom sheet.
 * DT-client-authprompt: big mono input, Gönder/İptal, inline error on retry.
 *
 * @param {{ slot:number, peer:string }} opts
 */
function showAuthPromptSheet({ slot, peer }) {
	const mount = MOUNT();
	if (!mount) return;

	// Remove any existing auth sheet
	hideAuthPromptSheet();

	const sheetId    = `auth-sheet-${slot}`;
	const backdropId = `auth-backdrop-${slot}`;
	const inputId    = `auth-pw-${slot}`;
	const submitId   = `auth-submit-${slot}`;
	const cancelId   = `auth-cancel-${slot}`;
	const errorId    = `auth-error-${slot}`;

	const sheetHtml = `
		<div class="sheet-backdrop" id="${backdropId}"></div>
		<div class="sheet auth-sheet" id="${sheetId}" role="dialog" aria-modal="true"
		     aria-labelledby="auth-sheet-title-${slot}">
			<div class="sheet-handle" aria-hidden="true"></div>
			<h3 class="auth-sheet-title" id="auth-sheet-title-${slot}">${t('pw.title')}</h3>
			<p class="auth-sheet-lead">${t('pw.lead')}</p>
			<div class="field">
				<label for="${inputId}" class="sr-only">${t('pw.aria')}</label>
				<input
					id="${inputId}"
					class="input mono auth-sheet-input"
					type="text"
					inputmode="text"
					autocomplete="one-time-code"
					autocorrect="off"
					autocapitalize="none"
					spellcheck="false"
					placeholder="${t('pw.placeholder')}"
					aria-describedby="${errorId}"
				/>
			</div>
			<p class="auth-sheet-error" id="${errorId}" role="alert" aria-live="polite"
			   style="display:none">${t('pw.error')}</p>
			<div class="auth-sheet-actions">
				<button class="btn btn-ghost" id="${cancelId}" type="button">
					${t('pw.cancel')}
				</button>
				<button class="btn btn-primary" id="${submitId}" type="button">
					${t('pw.submit')}
				</button>
			</div>
		</div>
	`;

	// Append to mount
	const wrapper = document.createElement('div');
	wrapper.className = 'auth-sheet-wrapper';
	wrapper.id = `auth-wrapper-${slot}`;
	wrapper.innerHTML = sheetHtml;
	mount.appendChild(wrapper);

	// Animate sheet in
	const sheet    = $(`auth-sheet-${slot}`);
	const backdrop = $(`auth-backdrop-${slot}`);
	requestAnimationFrame(() => {
		sheet?.classList.add('open');
		backdrop?.classList.add('open');
	});

	// Focus the input
	setTimeout(() => $(`auth-pw-${slot}`)?.focus(), 80);

	let submitting = false;

	async function submitPw() {
		if (submitting) return;
		const pwInput = $(`auth-pw-${slot}`);
		const password = (pwInput?.value || '').trim();
		if (!password) {
			showAuthError(slot);
			pwInput?.focus();
			return;
		}
		submitting = true;
		const submitBtn = $(`auth-submit-${slot}`);
		if (submitBtn) {
			submitBtn.disabled = true;
			submitBtn.textContent = t('pw.checking');
		}
		try {
			await invoke('submit_password', { slot, password });
			// Success — the read loop will continue; hide the sheet
			hideAuthPromptSheet(slot);
		} catch (e) {
			// Wrong password or error — show inline error, re-enable
			showAuthError(slot);
			if (submitBtn) {
				submitBtn.disabled = false;
				submitBtn.textContent = t('pw.submit');
			}
			submitting = false;
		}
	}

	// Enter key on input
	$(`auth-pw-${slot}`)?.addEventListener('keydown', (e) => {
		if (e.key === 'Enter') submitPw();
	});

	$(`auth-submit-${slot}`)?.addEventListener('click', submitPw);

	$(`auth-cancel-${slot}`)?.addEventListener('click', () => {
		hideAuthPromptSheet(slot);
		// Cancel aborts the session
		endSession(slot).catch(() => {});
	});

	$(`auth-backdrop-${slot}`)?.addEventListener('click', () => {
		hideAuthPromptSheet(slot);
		endSession(slot).catch(() => {});
	});
}

function showAuthError(slot) {
	const errEl = $(`auth-error-${slot}`);
	if (errEl) errEl.style.display = '';
	const input = $(`auth-pw-${slot}`);
	if (input) { input.value = ''; input.focus(); }
}

function hideAuthPromptSheet(slot) {
	// If slot not given, remove all auth sheets
	const selector = slot !== undefined ? `#auth-wrapper-${slot}` : '.auth-sheet-wrapper';
	document.querySelectorAll(selector).forEach((wrapper) => {
		const sheet    = wrapper.querySelector('.sheet');
		const backdrop = wrapper.querySelector('.sheet-backdrop');
		sheet?.classList.remove('open');
		backdrop?.classList.remove('open');
		setTimeout(() => wrapper.remove(), 380);
	});
}

// ---- Control bar wiring ------------------------------------------------------

function fmtId(id) {
	return String(id).replace(/(\d{3})(?=\d)/g, '$1 ').trim();
}

/**
 * Update the in-session bar meta text with codec + transport info.
 * @param {{ codec:string, transport:string }} opts
 */
function updateBarMeta({ codec, transport }) {
	const barMeta = $('bar-meta');
	if (!barMeta) return;
	const transportLabel =
		transport === 'direct' ? t('m.session.transport.direct') :
		transport === 'relay'  ? t('m.session.transport.relay')  :
		transport || '—';
	barMeta.textContent = [codec ? codec.toUpperCase() : null, transportLabel]
		.filter(Boolean).join(' · ');
}

// ---- Active pane indicator in bar -------------------------------------------

/**
 * Update the active session label in the in-session bar.
 * Shows "Ekran N · label" when multi-session.
 */
function updateBarActiveLabel() {
	const barLabel = $('bar-session-label');
	if (!barLabel) return;
	const entry = registry.find((e) => e.slot === _activeSlot);
	if (!entry) return;

	if (registry.length > 1) {
		barLabel.textContent =
			t('m.session.slot', { n: entry.slot + 1 }) + ' · ' + entry.label;
		barLabel.style.display = '';
	} else {
		barLabel.textContent = entry.label;
	}
}

// ---- Public API --------------------------------------------------------------

/**
 * Start tracking a session slot. Called by connect.js after a successful
 * connect_host response.
 *
 * @param {{
 *   slot: number,
 *   id: string,
 *   codec: string,
 *   mode: 'remote'|'game',
 *   transport: string,
 *   label?: string,
 * }} opts
 */
export function startSession({ slot, id, codec, mode, transport, label }) {
	// Remove any existing entry for this slot
	const idx = registry.findIndex((e) => e.slot === slot);
	if (idx >= 0) registry.splice(idx, 1);

	registry.push({
		slot,
		id,
		codec:      codec || 'h264',
		mode:       mode || 'remote',
		transport:  transport || '',
		label:      label || fmtId(id),
		stalled:    false,
		firstFrame: false,
	});

	// Update body mode (router is sole writer of data-mode)
	const bus = getBus();
	if (bus) bus.emit('mode-changed', mode || 'remote');

	// Enter in-session body class (sole writer via router)
	enterSession();

	// Update bar meta
	updateBarMeta({ codec, transport });

	// Game mode is landscape-first (Moonlight-style); a game session forces landscape.
	// Remote sessions leave the OS orientation alone (the display card can override).
	if (hasTauri && (mode || 'remote') === 'game') {
		invoke('plugin:pulsar-video|set_orientation', { landscape: true }).catch(() => {});
	}

	// Emit JS bus event for any listeners (overlay.js, etc.)
	if (bus) bus.emit('session-started', { slot, id, codec, mode, transport });

	// Set active pane to the new slot (it's the most recently started)
	setActivePane(slot);

	// (Re-)render the pill row — will be shown if ≥2 sessions
	renderPillRow();

	// Clear any stale disconnect card
	clearDisconnectCard();
}

/**
 * Programmatically tear down a session slot.
 * Invokes end_session (Rust) → stops the read loop → detaches the video surface.
 *
 * @param {number} slot
 */
export async function endSession(slot) {
	// Only the last live session tears down the shared native pipeline (all panes +
	// audio + service + webview reset) via `detach`. In a split, end_session's Rust
	// teardown releases just this slot's pane (stop_stream(slot)), so a global detach
	// would black out the sibling pane(s).
	const isLast = registry.filter((e) => e.slot !== slot).length === 0;

	// Invoke the Rust end_session command (cancels the read loop, drops Session/Node)
	if (hasTauri) {
		try {
			await invoke('end_session', { slot });
		} catch (e) {
			console.warn('[session] end_session error (slot ' + slot + '):', e);
		}
		// Detach native video surface (only when nothing else is streaming).
		if (isLast) {
			try {
				await invoke('plugin:pulsar-video|detach');
			} catch (e) {
				console.warn('[session] detach error:', e);
			}
		}
	}

	_dropSlot(slot, /* fromRust */ false);
}

/**
 * Mark a slot as the active pane for input + audio routing (W5 multi-session).
 *
 * - Updates _activeSlot
 * - Emits bus 'active-pane-changed' so input.js / audio.js follow focus
 * - Calls Rust set_active_pane (best-effort — no-op if command not yet registered)
 * - Syncs body[data-mode] to the focused slot's mode
 * - Re-renders the pill row to update active indicator
 *
 * @param {number} slot
 */
export function setActivePane(slot) {
	if (_activeSlot === slot && registry.length <= 1) return; // noop if nothing to do

	_activeSlot = slot;

	// Sync body[data-mode] to the focused slot's personality
	const entry = registry.find((e) => e.slot === slot);
	if (entry) {
		const bus = getBus();
		if (bus) bus.emit('mode-changed', entry.mode);
		// Update bar meta for the newly focused slot
		updateBarMeta({ codec: entry.codec, transport: entry.transport });
		updateBarActiveLabel();
	}

	// Emit bus event so input.js / audio.js / overlay.js can follow
	const bus = getBus();
	if (bus) bus.emit('active-pane-changed', slot);

	// Notify Rust to re-route audio output to the active slot (W5-rust-session)
	// Best-effort: the command may not exist yet in earlier waves.
	if (hasTauri) {
		invoke('set_active_pane', { slot }).catch(() => {
			// W5-rust-session may not be present yet — silently ignore
		});
	}

	// Re-render pill row to show updated active indicator
	renderPillRow();
}

/**
 * The slot of the currently focused pane (input/audio routing + the End button).
 * Authoritative across split panes — app.js's own tracker is not kept in sync.
 * @returns {number}
 */
export function activeSlot() { return _activeSlot; }

/**
 * Rename a session slot label. Updates the registry and re-renders the switcher.
 * @param {number} slot
 * @param {string} label — the new display label (max 40 chars, trimmed)
 */
export function renameSession(slot, label) {
	const entry = registry.find((e) => e.slot === slot);
	if (!entry) return;

	entry.label = String(label).trim().slice(0, 40) || fmtId(entry.id);

	// Re-render switcher pill row + active label
	renderPillRow();
	updateBarActiveLabel();

	// If the session sheet is open, refresh its list
	if ($(SESS_SHEET_ID)) {
		_renderSessionSheetList();
	}

	// Persist rename to sessionStorage (survives JS module re-import, not a full reload)
	try {
		const key = `pulsar.session.label.${entry.id}`;
		sessionStorage.setItem(key, entry.label);
	} catch (_) {}
}

// ---- Internal drop logic -----------------------------------------------------

/**
 * Remove a slot from the registry and update UI.
 * @param {number} slot
 * @param {boolean} fromRust  — true when triggered by play-ended event
 * @param {string}  [reason]  — reason string from play-ended
 */
function _dropSlot(slot, fromRust, reason) {
	const idx   = registry.findIndex((e) => e.slot === slot);
	const entry = idx >= 0 ? registry[idx] : null;
	if (idx >= 0) registry.splice(idx, 1);

	// Hide any auth sheet for this slot
	hideAuthPromptSheet(slot);

	// Clear the waiting-for-screen overlay if the session ended before any frame.
	_hideWaitingOverlay();

	// Close the session sheet if open (it may reference the dropped slot)
	closeSessionSheet();

	if (registry.length === 0) {
		// Last session ended — leave in-session mode
		exitSession();

		// Remove the pill row (no sessions left)
		$(PILL_ROW_ID)?.remove();

		const bus = getBus();
		if (bus) bus.emit('session-ended', { slot });

		// Restore portrait so the home / connect screens aren't stuck sideways after
		// a game session forced landscape.
		if (hasTauri) invoke('plugin:pulsar-video|set_orientation', { landscape: false }).catch(() => {});

		if (fromRust && entry) {
			// Show the disconnect + reconnect card
			renderDisconnectCard({
				slot,
				id:     entry.id,
				mode:   entry.mode,
				reason: reason || '',
			});
		} else {
			// User-initiated end — just clear any card
			clearDisconnectCard();
		}
	} else {
		// Other sessions remain — switch to a remaining slot if the active one ended
		if (_activeSlot === slot) {
			const next = registry.find((e) => e.slot !== slot) || registry[0];
			if (next) setActivePane(next.slot);
		} else {
			// Just re-render the pill row (one pill removed)
			renderPillRow();
		}

		const bus = getBus();
		if (bus) bus.emit('session-slot-ended', { slot });
	}
}

// ---- Tauri event listeners ---------------------------------------------------

async function init() {
	// play-ended: the Rust read-loop exited (Ok(None) / error / cancel)
	await listen('play-ended', ({ slot, reason }) => {
		// If we still have this slot, detach the surface then drop it.
		const entry = registry.find((e) => e.slot === slot);
		if (!entry) return; // already dropped by user action

		// The Rust read-loop teardown already released THIS slot's pane per-slot
		// (stop_stream(slot)). Only run the global `detach` — which releases ALL
		// panes + the shared audio pipeline + foreground service and resets the
		// webview background — when this was the last live session; otherwise a
		// sibling split pane would go permanently black.
		const isLast = registry.filter((e) => e.slot !== slot).length === 0;
		if (hasTauri && isLast) {
			invoke('plugin:pulsar-video|detach').catch(() => {});
		}

		_dropSlot(slot, /* fromRust */ true, reason);
	});

	// play-stall: video stall state change
	await listen('play-stall', ({ slot, stalled }) => {
		const entry = registry.find((e) => e.slot === slot);
		if (!entry) return;
		entry.stalled = stalled;
		const bus = getBus();
		if (bus) bus.emit('session-stall', { slot, stalled });
		// Update bar status indicator
		_updateBarStall(stalled);
		// Refresh pill to show/hide stall dot for this slot
		renderPillRow();
	});

	// play-firstframe: first decoded video frame
	await listen('play-firstframe', ({ slot }) => {
		const entry = registry.find((e) => e.slot === slot);
		if (!entry) return;
		entry.firstFrame = true;
		entry.stalled    = false;
		_hideWaitingOverlay();
		const bus = getBus();
		if (bus) bus.emit('session-firstframe', { slot });
		_updateBarStall(false);
		renderPillRow();
	});

	// auth-prompt: host is asking for a one-time password
	await listen('auth-prompt', ({ slot, peer }) => {
		showAuthPromptSheet({ slot, peer });
	});

	// auth-ok: host approved WITHOUT a password (clicked "Allow") — dismiss the
	// OTP prompt so it doesn't linger while the session proceeds.
	await listen('auth-ok', ({ slot }) => {
		hideAuthPromptSheet(slot);
	});

	// conn-phase: update bar meta during connection phases
	await listen('conn-phase', ({ slot, phase, transport }) => {
		if (transport) {
			const entry = registry.find((e) => e.slot === slot);
			if (entry) {
				entry.transport = transport;
				if (slot === _activeSlot) {
					updateBarMeta({ codec: entry.codec, transport });
				}
			}
		}
		const bus = getBus();
		if (bus) bus.emit('session-conn-phase', { slot, phase, transport });
	});

	// Wire the btn-end button (invokes end_session before detaching)
	_wireEndButton();

	// JS-bus: listen for mode-changed so we can keep the focused pane in sync
	const bus = getBus();
	if (bus) {
		bus.on('session-started', ({ slot, id, codec, mode, transport }) => {
			// Idempotent — startSession already added to registry directly.
			// Show the "waiting for host to share their screen" overlay until the
			// first decoded frame arrives (play-firstframe hides it).
			_showWaitingOverlay();
		});
	}
}

// ---- Bar status helpers ------------------------------------------------------

function _updateBarStall(stalled) {
	const statEl = document.querySelector('.bar .stat');
	if (!statEl) return;
	if (stalled) {
		statEl.innerHTML =
			'<span class="live stall-dot"></span>' + t('m.session.stalled');
	} else {
		statEl.innerHTML = '<span class="live"></span>' + t('session.activeNow');
	}
}

// ---- End button wiring -------------------------------------------------------

let _endBtnWired = false;

function _wireEndButton() {
	if (_endBtnWired) return;
	const btnEnd = $('btn-end');
	if (!btnEnd) return;
	_endBtnWired = true;

	btnEnd.addEventListener('click', async (e) => {
		e.stopPropagation();

		if (registry.length > 1) {
			// Multi-session: show the session sheet so the user can choose which to end
			openSessionSheet();
			return;
		}

		// Single session: end it directly
		const slots = registry.map((e) => e.slot);
		if (slots.length === 0) {
			// Fallback: just remove in-session class
			exitSession();
			return;
		}
		for (const slot of slots) {
			await endSession(slot);
		}
	});
}

// ---- Inline styles -----------------------------------------------------------
// Injected once into <head> so this module is self-contained and doesn't
// require changes to components.css (which is W3-overlay-owned in W3).

// ---- "Waiting for host to share screen" overlay (C) --------------------------
// Shown from session-started until the first decoded frame (play-firstframe).
// Covers the gap where the host is answering the OS screen-share picker (e.g. the
// Wayland XDG portal dialog), so the client shows guidance instead of a blank /
// (on some emulators) green native surface. hud.js's first-frame loader is not
// imported in this build, so the wait UI lives here in the always-loaded session module.
function _showWaitingOverlay() {
	if (document.getElementById('ps-wait')) return;
	const el = document.createElement('div');
	el.id = 'ps-wait';
	el.className = 'ps-wait';
	el.setAttribute('role', 'status');
	el.innerHTML =
		'<div class="ps-wait-rings" aria-hidden="true"><span></span><span></span><span></span></div>' +
		'<p class="ps-wait-text">' + t('session.waiting') + '</p>';
	document.body.appendChild(el);
}

function _hideWaitingOverlay() {
	const el = document.getElementById('ps-wait');
	if (!el) return;
	el.classList.add('ps-wait-out');
	setTimeout(() => el.remove(), 350);
}

function _injectStyles() {
	if (document.getElementById('session-module-styles')) return;
	const style = document.createElement('style');
	style.id = 'session-module-styles';
	style.textContent = `
/* ============================================================
   Session module styles (W2-session + W5-session-js)
   Touch-first: 44px tap targets, safe-area insets, indigo/cyan
   ============================================================ */

/* ---- Waiting-for-screen overlay (C) ---- */
.ps-wait {
	position: fixed;
	inset: 0;
	z-index: 45;
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 20px;
	padding: 0 32px;
	text-align: center;
	background: oklch(0.10 0.012 268 / 0.92);
	backdrop-filter: blur(6px);
	-webkit-backdrop-filter: blur(6px);
	opacity: 1;
	transition: opacity 0.3s ease;
}
.ps-wait.ps-wait-out { opacity: 0; pointer-events: none; }
.ps-wait-text {
	font-family: var(--font-sans, system-ui, sans-serif);
	font-size: 15px;
	font-weight: 600;
	color: oklch(0.95 0 0);
	margin: 0;
	max-width: 320px;
	line-height: 1.45;
}
.ps-wait-rings { position: relative; width: 64px; height: 64px; }
.ps-wait-rings span {
	position: absolute;
	inset: 0;
	border: 2px solid var(--brand, #6366f1);
	border-radius: 50%;
	opacity: 0;
	animation: ps-wait-pulse 2.2s ease-out infinite;
}
.ps-wait-rings span:nth-child(2) { animation-delay: 0.55s; }
.ps-wait-rings span:nth-child(3) { animation-delay: 1.1s; }
@keyframes ps-wait-pulse {
	0%   { transform: scale(0.5); opacity: 0.7; }
	70%  { transform: scale(1.25); opacity: 0; }
	100% { transform: scale(0.5); opacity: 0; }
}

/* ---- Disconnect card ---- */
.session-disconnect-backdrop {
	position: fixed;
	inset: 0;
	z-index: 24;
	background: oklch(0 0 0 / 0.55);
	backdrop-filter: blur(4px);
	-webkit-backdrop-filter: blur(4px);
}
.session-disconnect-card {
	position: fixed;
	left: 50%;
	bottom: 0;
	transform: translate(-50%, 100%);
	z-index: 25;
	width: min(420px, calc(100vw - 24px));
	background: var(--surface);
	border-top-left-radius: var(--r-xl);
	border-top-right-radius: var(--r-xl);
	border-top: 1px solid var(--border);
	box-shadow: var(--shadow-lg);
	padding: 28px 24px calc(28px + var(--safe-bottom, env(safe-area-inset-bottom, 0px)));
	display: flex;
	flex-direction: column;
	align-items: center;
	gap: 12px;
	text-align: center;
	transition: transform 0.38s var(--ease-out, cubic-bezier(0.16,1,0.3,1));
}
.session-disconnect-card.sdc-visible {
	transform: translate(-50%, 0);
}
.sdc-icon {
	width: 64px;
	height: 64px;
	border-radius: 50%;
	background: oklch(0.575 0.205 25 / 0.1);
	color: oklch(0.575 0.205 25);
	display: flex;
	align-items: center;
	justify-content: center;
	margin-bottom: 4px;
}
/* In game mode: use cyan tint for the icon */
.session-disconnect-card.brand-game .sdc-icon {
	background: oklch(0.62 0.15 215 / 0.12);
	color: var(--cyan);
}
.sdc-title {
	font-family: var(--font-display);
	font-size: 20px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
}
.sdc-reason {
	font-size: 14px;
	color: var(--text-muted);
	margin: 0 0 4px;
	line-height: 1.5;
	max-width: 30ch;
}
.sdc-id {
	font-family: var(--font-mono);
	font-weight: 600;
	font-size: 18px;
	letter-spacing: 0.1em;
	color: var(--text-faint);
	margin-bottom: 4px;
}
.sdc-reconnect {
	width: 100%;
	max-width: 280px;
	border-radius: var(--r-pill);
	gap: 10px;
}
.sdc-dismiss {
	width: 100%;
	max-width: 280px;
	border-radius: var(--r-pill);
}

/* ---- Auth prompt sheet ---- */
.auth-sheet-wrapper { position: fixed; inset: 0; z-index: 26; pointer-events: none; }
.auth-sheet-wrapper .sheet-backdrop,
.auth-sheet-wrapper .sheet { pointer-events: auto; }
.auth-sheet {
	max-height: 80dvh;
}
.auth-sheet-title {
	font-family: var(--font-display);
	font-size: 18px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
}
.auth-sheet-lead {
	font-size: 13.5px;
	color: var(--text-muted);
	line-height: 1.55;
	margin: 0;
}
.auth-sheet-lead b { color: var(--text); font-weight: 600; }
.auth-sheet-input {
	font-family: var(--font-mono);
	font-size: 20px;
	font-weight: 600;
	letter-spacing: 0.12em;
	text-align: center;
	padding: 16px 14px;
}
.auth-sheet-error {
	font-size: 13px;
	color: var(--danger);
	text-align: center;
	margin: -4px 0 0;
}
.auth-sheet-actions {
	display: grid;
	grid-template-columns: 1fr 2fr;
	gap: 10px;
}

/* Stall dot override — orange instead of green */
.bar .stat .stall-dot {
	background: var(--warn);
	animation: live 1.2s var(--ease, cubic-bezier(0.22,1,0.36,1)) infinite;
}

/* Screen-reader-only utility */
.sr-only {
	position: absolute;
	width: 1px; height: 1px;
	padding: 0; margin: -1px;
	overflow: hidden;
	clip: rect(0,0,0,0);
	white-space: nowrap;
	border: 0;
}

/* ============================================================
   W5 — Session switcher pill row
   Sits in #bar-pill-area (above the in-session bar), shown
   only when ≥2 sessions are active.
   ============================================================ */

/* Pill row container */
.session-pill-row {
	display: flex;
	flex-direction: column;
	gap: 0;
	width: 100%;
	background: var(--surface);
	border-bottom: 1px solid var(--border);
	/* Respect safe-area on notch devices */
	padding-top: env(safe-area-inset-top, 0px);
}

/* Sheet-open handle affordance */
.session-pill-handle {
	all: unset;
	display: flex;
	justify-content: center;
	align-items: center;
	height: 20px;
	width: 100%;
	cursor: pointer;
	touch-action: pan-y;
}
.session-pill-handle-bar {
	display: block;
	width: 32px;
	height: 4px;
	border-radius: var(--r-pill);
	background: var(--border-strong);
}
.session-pill-handle:active .session-pill-handle-bar {
	background: var(--text-faint);
}

/* Horizontally scrollable pill scroller */
.session-pill-scroller {
	display: flex;
	flex-direction: row;
	gap: 8px;
	overflow-x: auto;
	overflow-y: hidden;
	padding: 6px 12px 10px;
	scroll-snap-type: x proximity;
	-webkit-overflow-scrolling: touch;
	scrollbar-width: none; /* Firefox */
}
.session-pill-scroller::-webkit-scrollbar { display: none; }

/* Individual session pill */
.session-pill {
	display: inline-flex;
	align-items: center;
	gap: 7px;
	height: 44px; /* meets --touch-min */
	padding: 0 14px 0 12px;
	border-radius: var(--r-pill);
	border: 1.5px solid transparent;
	background: var(--surface-2);
	cursor: pointer;
	user-select: none;
	touch-action: manipulation;
	scroll-snap-align: start;
	flex-shrink: 0;
	transition:
		background 180ms var(--ease, cubic-bezier(0.22,1,0.36,1)),
		border-color 180ms var(--ease, cubic-bezier(0.22,1,0.36,1)),
		box-shadow 180ms var(--ease, cubic-bezier(0.22,1,0.36,1));
	-webkit-tap-highlight-color: transparent;
}
.session-pill:active {
	transform: scale(0.96);
}

/* Active (focused) pill — indigo in remote mode, cyan in game mode */
.session-pill.pill-active.pill-remote {
	background: var(--accent-soft);
	border-color: var(--accent);
	box-shadow: 0 0 0 3px var(--accent-ring, oklch(0.555 0.205 272 / 0.2));
}
.session-pill.pill-active.pill-game {
	background: var(--cyan-soft);
	border-color: var(--cyan);
	box-shadow: 0 0 0 3px oklch(0.62 0.15 215 / 0.2);
}

/* Background (non-focused) pills are dimmed */
.session-pill.pill-bg {
	opacity: 0.65;
}
.session-pill.pill-bg:hover,
.session-pill.pill-bg:focus-visible {
	opacity: 1;
}

/* Stalled pill — amber warning border */
.session-pill.pill-stalled {
	border-color: var(--warn);
}

/* Mode dot — small colored circle */
.pill-mode-dot {
	width: 8px;
	height: 8px;
	border-radius: 50%;
	flex-shrink: 0;
	background: var(--text-faint);
}
.session-pill.pill-remote.pill-active .pill-mode-dot {
	background: var(--accent);
}
.session-pill.pill-game.pill-active .pill-mode-dot {
	background: var(--cyan);
}
.session-pill.pill-remote.pill-bg .pill-mode-dot {
	background: var(--accent);
	opacity: 0.55;
}
.session-pill.pill-game.pill-bg .pill-mode-dot {
	background: var(--cyan);
	opacity: 0.55;
}

/* Pill label text */
.pill-label {
	font-family: var(--font-sans);
	font-size: 13.5px;
	font-weight: 600;
	letter-spacing: -0.01em;
	color: var(--text);
	white-space: nowrap;
	max-width: 120px;
	overflow: hidden;
	text-overflow: ellipsis;
}
.session-pill.pill-active.pill-remote .pill-label { color: var(--accent); }
.session-pill.pill-active.pill-game   .pill-label { color: var(--cyan); }

/* Stall dot inside pill — blinking amber dot */
.pill-stall-dot {
	width: 6px;
	height: 6px;
	border-radius: 50%;
	background: var(--warn);
	flex-shrink: 0;
	animation: live 1.2s ease infinite;
}

/* Pill ×-end button */
.pill-end-btn {
	all: unset;
	display: flex;
	align-items: center;
	justify-content: center;
	width: 22px;
	height: 22px;
	min-width: 22px;
	border-radius: 50%;
	background: var(--surface-3);
	color: var(--text-muted);
	cursor: pointer;
	transition: background 140ms, color 140ms;
	-webkit-tap-highlight-color: transparent;
}
.pill-end-btn:hover,
.pill-end-btn:active {
	background: var(--danger-soft);
	color: var(--danger);
}

/* ============================================================
   W5 — Session management sheet (swipe-down from pill row)
   ============================================================ */

.session-sheet {
	max-height: 90dvh;
	overflow-y: auto;
	-webkit-overflow-scrolling: touch;
}
.session-sheet-title {
	font-family: var(--font-display);
	font-size: 17px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
}
.session-sheet-list {
	display: flex;
	flex-direction: column;
	gap: 12px;
	width: 100%;
}
.session-sheet-empty {
	font-size: 14px;
	color: var(--text-faint);
	text-align: center;
	padding: 16px 0;
	margin: 0;
}
.session-sheet-close {
	width: 100%;
	border-radius: var(--r-pill);
}

/* ---- Session card in the sheet ---- */
.session-card {
	border: 1.5px solid var(--border);
	border-radius: var(--r-lg);
	background: var(--surface);
	padding: 16px;
	display: flex;
	flex-direction: column;
	gap: 10px;
}
.session-card.scard-active {
	border-color: var(--accent);
	background: var(--accent-soft);
}
.session-card.scard-active.scard-game {
	border-color: var(--cyan);
	background: var(--cyan-soft);
}

.scard-header {
	display: flex;
	align-items: center;
	gap: 10px;
	min-height: 28px;
}
.scard-badge {
	font-family: var(--font-mono);
	font-size: 11px;
	font-weight: 600;
	letter-spacing: 0.06em;
	text-transform: uppercase;
	padding: 3px 8px;
	border-radius: var(--r-pill);
	flex-shrink: 0;
}
.scard-remote-badge {
	background: var(--accent-soft-2);
	color: var(--accent);
}
.scard-game-badge {
	background: var(--cyan-soft);
	color: var(--cyan);
}
.scard-label {
	font-family: var(--font-sans);
	font-size: 15px;
	font-weight: 600;
	color: var(--text);
	flex: 1;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.scard-active-dot {
	width: 10px;
	height: 10px;
	border-radius: 50%;
	background: var(--ok);
	flex-shrink: 0;
}
.session-card.scard-game .scard-active-dot {
	background: var(--cyan);
}

.scard-meta {
	display: flex;
	align-items: center;
	gap: 6px;
	font-size: 12.5px;
	color: var(--text-faint);
	flex-wrap: wrap;
}
.scard-sep { color: var(--border-strong); }
.scard-codec {
	font-family: var(--font-mono);
	font-size: 11.5px;
	font-weight: 500;
}
.scard-stall {
	background: var(--warn-soft);
	color: var(--warn);
	border-radius: var(--r-pill);
	font-size: 11px;
	font-weight: 600;
	padding: 1px 7px;
}

.scard-id {
	font-family: var(--font-mono);
	font-size: 13px;
	font-weight: 500;
	letter-spacing: 0.1em;
	color: var(--text-faint);
}

.scard-actions {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
	align-items: center;
}
.scard-btn-focus,
.scard-btn-rename,
.scard-btn-end {
	height: 44px; /* --touch-min */
	font-size: 14px;
	border-radius: var(--r-sm);
	padding: 0 16px;
	flex: 1;
	min-width: 80px;
}
.scard-btn-end {
	color: var(--danger);
	border-color: var(--danger-soft);
}
.scard-btn-end:hover,
.scard-btn-end:active {
	background: var(--danger-soft);
	border-color: var(--danger);
}
.scard-focused-badge {
	display: inline-flex;
	align-items: center;
	gap: 6px;
	font-size: 13px;
	font-weight: 600;
	color: var(--ok);
	padding: 0 4px;
	flex: 1;
}
.session-card.scard-game .scard-focused-badge {
	color: var(--cyan);
}

/* ============================================================
   W5 — Rename sheet
   ============================================================ */

.rename-sheet-wrapper {
	position: fixed;
	inset: 0;
	z-index: 40;
	pointer-events: none;
}
.rename-sheet-wrapper .sheet-backdrop,
.rename-sheet-wrapper .sheet {
	pointer-events: auto;
}
.rename-sheet {
	max-height: 80dvh;
}
.rename-sheet-title {
	font-family: var(--font-display);
	font-size: 18px;
	font-weight: 700;
	letter-spacing: -0.02em;
	color: var(--text);
	margin: 0;
}
.rename-sheet-sub {
	font-family: var(--font-mono);
	font-size: 12.5px;
	color: var(--text-faint);
	letter-spacing: 0.08em;
	margin: 0;
}
.rename-sheet-input {
	font-size: 16px; /* --input-size — never below 16px on iOS/Android */
	font-weight: 500;
	padding: 14px 16px;
	border-radius: var(--r);
	width: 100%;
}
.rename-sheet-actions {
	display: grid;
	grid-template-columns: 1fr 2fr;
	gap: 10px;
}
`;
	document.head.appendChild(style);
}

// ---- Boot --------------------------------------------------------------------

_injectStyles();

// Init runs when the module is first imported. In a plain browser (no Tauri)
// listen() is a no-op, so the module parses and loads safely.
if (document.readyState === 'loading') {
	document.addEventListener('DOMContentLoaded', init);
} else {
	init();
}
