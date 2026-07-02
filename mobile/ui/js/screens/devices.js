/**
 * screens/devices.js — Cihazlar / Geçmiş screen  (W4-devices)
 *
 * Touch-first saved-devices + connection-history screen built over store/peers.js.
 *
 * Features (per DT-devices, §5 W4-devices brief):
 *  - Two-section list: "Kaydedilenler" (address book, sorted fav-first) and
 *    "Son Bağlantılar" (remote/game history tabs).
 *  - Each device row: category icon + name + grouped ID + online dot + last-seen.
 *  - Tap row → doConnect(id, 0) — remote mode by default.
 *  - Long-press row → action bottom-sheet (Bağlan / Oyun olarak bağlan / Düzenle /
 *    Favori / Kaydet / Geçmişten kaldır / Unut).
 *  - "+ Cihaz ekle" FAB / button → add-device bottom-sheet
 *    (name + ID/IP field + 4 icon chips + Ekle / Vazgeç).
 *  - Edit sheet (for saved devices): change name, id, icon.
 *  - Clear history action (for history sections).
 *  - Remote vs. game timeline split in history: chip filter.
 *  - LAN presence section (W5-stub — listens to 'lan-presence' bus event and shows
 *    a stub row; degrades gracefully to empty if W5-lan-presence is not present).
 *
 * Owned files (W4-devices): screens/devices.js, app.js (import already present).
 * Never touches: index.html, router.js, peers.js, connect.js — only reads their APIs.
 *
 * Contract deps:
 *  - store/peers.js  → savedPeers, historyPeers, gameHistoryPeers, addPeer,
 *                       updatePeer, removePeer, removeFromHistory, toggleFav,
 *                       clearHistory, fmtPeerId, normalizeId, peerEvents
 *  - screens/connect.js → doConnect (tap-to-connect)
 *  - js/router.js   → registerScreen
 *  - js/i18n.js     → t
 *  - Tauri bus      → lan-presence (W5; stubbed)
 */

import { registerScreen }  from '../router.js';
import { t }               from '../i18n.js';

// ---------------------------------------------------------------------------
// Module-scoped CSS (injected once)
// Follows §4 contract tokens; touch targets ≥ var(--touch-min, 44px).
// ---------------------------------------------------------------------------
(function injectStyles() {
  if (document.getElementById('devices-css')) return;
  const s = document.createElement('style');
  s.id = 'devices-css';
  s.textContent = `
/* ── Devices screen ───────────────────────────────────────────────── */
#t-history { padding: 0; }

.devices-screen {
  display: flex;
  flex-direction: column;
  padding: 0 0 calc(var(--nav-h, 64px) + var(--safe-bottom, env(safe-area-inset-bottom, 0px)) + 16px);
}

.devices-hdr {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;            /* button drops below the heading on narrow widths */
  padding: 20px 16px 12px;
}
.devices-hdr > div { flex: 1 1 auto; min-width: 0; }  /* heading shrinks, never overflows the button off-screen */
.devices-hdr .title { font-size: 22px; margin: 0 0 2px; }
.devices-hdr .sub   { font-size: 13px; color: var(--text-faint); margin: 0; line-height: 1.4; }
.devices-hdr .add-btn {
  margin-top: 4px;
  flex-shrink: 0;
  font-size: 13px;
  padding: 0 14px;
  height: 38px;
  border-radius: var(--r-pill);
}

/* ── Section labels ──────────────────────────────────────────────── */
.devices-screen .sect-label {
  padding: 10px 16px 4px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-faint);
}

/* ── Device rows ─────────────────────────────────────────────────── */
.row-list.device-list { padding: 0 8px; }

.device-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 8px;
  min-height: var(--touch-min, 44px);
  border-radius: var(--r);
  cursor: pointer;
  user-select: none;
  -webkit-user-select: none;
  transition: background 120ms var(--ease-out, ease);
  outline: none;
}
.device-row:hover,
.device-row:focus-visible { background: var(--surface-2); }
.device-row:active        { background: var(--surface-3); }
.device-row.fav .device-name { color: var(--brand, var(--accent)); }

.device-icon {
  flex-shrink: 0;
  width: 40px; height: 40px;
  border-radius: var(--r-sm);
  background: var(--accent-soft);
  display: flex; align-items: center; justify-content: center;
  color: var(--accent);
}
[data-mode=game] .device-icon { background: var(--cyan-soft); color: var(--cyan); }
.device-icon { overflow: hidden; }
.device-img { width: 100%; height: 100%; object-fit: cover; display: block; }
/* Add-device photo picker */
.add-avatar-btn {
  flex-shrink: 0; width: 52px; height: 52px;
  border-radius: var(--r); border: 1px dashed var(--border-strong);
  background: var(--surface-2); color: var(--text-faint);
  display: flex; align-items: center; justify-content: center;
  cursor: pointer; overflow: hidden; position: relative;
}
.add-avatar-img { display: none; position: absolute; inset: 0; background-size: cover; background-position: center; }

.device-meta   { flex: 1; min-width: 0; }
.device-name   { font-size: 15px; font-weight: 500; color: var(--text); line-height: 1.3; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.device-id     { font-size: 12px; color: var(--text-muted); margin-top: 2px; display: flex; align-items: center; gap: 6px; }
.device-when   { font-size: 11px; color: var(--text-faint); margin-top: 1px; }
.fav-star      { color: var(--warn); font-size: 12px; }

.online-dot {
  display: inline-block;
  width: 7px; height: 7px;
  border-radius: 50%;
  background: var(--ok);
  flex-shrink: 0;
}

.device-more {
  flex-shrink: 0;
  width: 36px; height: 44px;
  border-radius: var(--r-sm);
  color: var(--text-faint);
  display: flex; align-items: center; justify-content: center;
}
.device-more:hover { color: var(--text-muted); background: var(--surface-3); }

/* ── History header with mode chips ─────────────────────────────── */
.devices-history-hdr {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-right: 8px;
}
.history-mode-chips {
  display: flex;
  gap: 6px;
  padding: 6px 8px 6px 0;
}
.history-mode-chips .chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 5px 12px;
  border-radius: var(--r-pill);
  font-size: 12px;
  font-weight: 500;
  border: 1.5px solid var(--border);
  background: transparent;
  color: var(--text-muted);
  min-height: var(--touch-min, 44px);
  cursor: pointer;
  transition: background 140ms, color 140ms, border-color 140ms;
}
.history-mode-chips .chip.active {
  background: var(--accent-soft);
  border-color: var(--accent-soft-2);
  color: var(--accent);
}
[data-mode=game] .history-mode-chips .chip.active {
  background: var(--cyan-soft);
  border-color: var(--cyan);
  color: var(--cyan);
}

/* ── Clear history button ────────────────────────────────────────── */
.clear-history-btn {
  margin: 6px 16px 0;
  font-size: 13px;
  color: var(--danger);
  padding: 0 12px;
  height: 40px;
  border-radius: var(--r);
  border: none;
  background: none;
}
.clear-history-btn:hover { background: var(--danger-soft); }

/* ── Empty state ─────────────────────────────────────────────────── */
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 64px 32px;
  color: var(--text-faint);
}
.empty-state .empty { font-size: 16px; font-weight: 500; margin-bottom: 8px; }
.empty-state .sub   { font-size: 14px; line-height: 1.5; color: var(--text-muted); }

/* ── Icon chips in the add/edit sheet ───────────────────────────── */
.icon-chips {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  padding: 4px 0;
}
.icon-chip {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 5px;
  padding: 12px 16px;
  border-radius: var(--r);
  border: 1.5px solid var(--border);
  background: var(--surface);
  color: var(--text-muted);
  font-size: 11px;
  min-width: 72px;
  min-height: var(--touch-min, 44px);
  cursor: pointer;
  transition: background 120ms, border-color 120ms, color 120ms;
}
.icon-chip.selected {
  border-color: var(--accent);
  background: var(--accent-soft);
  color: var(--accent);
}
[data-mode=game] .icon-chip.selected {
  border-color: var(--cyan);
  background: var(--cyan-soft);
  color: var(--cyan);
}

/* ── Sheet overlay ───────────────────────────────────────────────── */
/* base .sheet + .sheet-overlay are defined in components.css (W3-overlay);
   we only add devices-specific overrides here. */
.sheet-overlay {
  position: fixed; inset: 0;
  background: oklch(0.15 0.02 270 / 0.45);
  z-index: 300;
  display: flex; align-items: flex-end;
  opacity: 0; transition: opacity 200ms var(--ease-out, ease);
}
.sheet-overlay.open { opacity: 1; }

.sheet {
  width: 100%;
  background: var(--surface);
  border-radius: var(--r-xl) var(--r-xl) 0 0;
  padding-bottom: calc(var(--safe-bottom, env(safe-area-inset-bottom, 0px)) + 16px);
  transform: translateY(100%);
  transition: transform 260ms var(--ease-out, ease);
  max-height: 80vh;
  overflow-y: auto;
  overscroll-behavior: contain;
  box-shadow: 0 -4px 32px oklch(0.3 0.04 270 / 0.12);
}
.sheet.open { transform: translateY(0); }

.sheet-handle {
  width: 36px; height: 4px;
  background: var(--border-strong);
  border-radius: 2px;
  margin: 12px auto 4px;
}
.sheet-title {
  font-size: 17px; font-weight: 600;
  padding: 12px 20px 4px;
  color: var(--text);
}
.sheet-form { padding: 4px 16px; }
.sheet-form .field { margin-bottom: 14px; }
.sheet-form .field label { font-size: 12px; font-weight: 500; color: var(--text-muted); margin-bottom: 6px; display: block; }

.sheet-actions        { display: flex; flex-direction: column; gap: 2px; padding: 4px 12px; }
.sheet-actions.row    { flex-direction: row; gap: 8px; padding: 8px 16px 4px; }
.sheet-action {
  display: flex; align-items: center; gap: 14px;
  padding: 0 16px;
  height: 54px;
  border-radius: var(--r);
  font-size: 15px; font-weight: 500;
  text-align: left;
  background: transparent;
  border: none;
  color: var(--text);
  cursor: pointer;
  transition: background 120ms;
}
.sheet-action:hover    { background: var(--surface-2); }
.sheet-action.primary  { color: var(--brand, var(--accent)); font-weight: 600; }
.sheet-action.danger   { color: var(--danger); }
.sheet-action-icon     { flex-shrink: 0; display: flex; color: inherit; }

.sheet-cancel {
  display: block;
  width: calc(100% - 32px);
  margin: 8px 16px 0;
  height: 52px;
  border-radius: var(--r);
  font-size: 16px; font-weight: 500;
  background: var(--surface-2);
  border: none;
  color: var(--text-muted);
  cursor: pointer;
}
.sheet-cancel:hover { background: var(--surface-3); }
  `;
  document.head.appendChild(s);
})();
import {
  savedPeers, historyPeers, gameHistoryPeers,
  addPeer, updatePeer, removePeer,
  removeFromHistory, removeFromGameHistory,
  toggleFav, clearHistory,
  fmtPeerId, normalizeId,
  peerEvents,
} from '../store/peers.js';
import { doConnect } from './connect.js';

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/** @type {'remote'|'game'} */
let _historyMode = 'remote';

/** @type {Array<{id:string, name:string}>} LAN-discovered peers (W5 stub) */
let _lanDevices = [];

/** Container element (set on mount) */
let _root = null;

/** Long-press timer handle */
let _longPressTimer = null;
const LONG_PRESS_MS = 420;

// ---------------------------------------------------------------------------
// Time formatting helpers
// ---------------------------------------------------------------------------

/**
 * @param {number|null|undefined} ts   Unix ms timestamp or null
 * @returns {string}
 */
function ago(ts) {
  if (!ts) return t('devices.never');
  const s = (Date.now() - ts) / 1000;
  if (s < 90)    return t('devices.justNow');
  if (s < 3600)  return t('devices.minAgo',  { n: Math.floor(s / 60) });
  if (s < 86400) return t('devices.hourAgo', { n: Math.floor(s / 3600) });
  return t('devices.dayAgo', { n: Math.floor(s / 86400) });
}

// ---------------------------------------------------------------------------
// Device category icons (SVG strings)
// Each 20×20 viewBox, stroke-based, currentColor.
// ---------------------------------------------------------------------------

const ICONS = {
  pc: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <rect x="3" y="4" width="18" height="13" rx="2" stroke="currentColor" stroke-width="1.8"/>
    <path d="M8 20h8M12 17v3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
  </svg>`,
  server: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <rect x="3" y="4" width="18" height="5.5" rx="1.6" stroke="currentColor" stroke-width="1.8"/>
    <rect x="3" y="11.5" width="18" height="5.5" rx="1.6" stroke="currentColor" stroke-width="1.8"/>
    <circle cx="7.5" cy="6.75" r="1" fill="currentColor"/>
    <circle cx="7.5" cy="14.25" r="1" fill="currentColor"/>
  </svg>`,
  console: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <rect x="3" y="7" width="18" height="10" rx="3" stroke="currentColor" stroke-width="1.8"/>
    <path d="M9 12H7M8 11v2M15 12h2M14 11.5l1.5 1-1.5 1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,
};

const ICON_KEYS = ['pc', 'server', 'console'];

/**
 * @param {'pc'|'server'|'console'} cat
 * @returns {string}
 */
function iconFor(cat) {
  return ICONS[cat] || ICONS.pc;
}

// ---------------------------------------------------------------------------
// Row HTML builder
// ---------------------------------------------------------------------------

/**
 * @param {{
 *   id: string, name: string,
 *   cat?: 'pc'|'server'|'console',
 *   fav?: boolean, saved?: boolean,
 *   lastConnected?: number|null,
 *   gameConnected?: number|null,
 * }} peer
 * @param {string} [context]  — 'saved'|'remote'|'game'|'lan'
 * @returns {string}
 */
function peerRowHtml(peer, context) {
  const cat    = peer.cat || 'pc';
  const name   = peer.name || fmtPeerId(peer.id);
  const idFmt  = fmtPeerId(peer.id);
  const ts     = context === 'game' ? peer.gameConnected : peer.lastConnected;
  const agoCopy = ago(ts);
  const favClass = peer.fav ? ' fav' : '';

  // Online dot: shown only when we have live lan-presence data for this id
  const nid   = normalizeId(peer.id);
  const isLan = _lanDevices.some((d) => normalizeId(d.id) === nid);
  const dotHtml = isLan
    ? '<span class="online-dot" aria-label="' + t('devices.online') + '"></span>'
    : '';

  return `<div class="device-row${favClass}" data-id="${peer.id}" data-ctx="${context || 'saved'}"
               role="button" tabindex="0"
               aria-label="${name}, ${idFmt}">
    <div class="device-icon">${peer.image ? `<img class="device-img" src="${escHtml(peer.image)}" alt="" />` : iconFor(cat)}</div>
    <div class="device-meta">
      <div class="device-name">${escHtml(name)}${peer.fav ? ' <span class="fav-star" aria-hidden="true">★</span>' : ''}</div>
      <div class="device-id mono">${escHtml(idFmt)}${dotHtml}</div>
      <div class="device-when">${agoCopy}</div>
    </div>
    <button class="device-more icon-btn" aria-label="${t('devices.more')}" data-id="${peer.id}" data-ctx="${context || 'saved'}">
      <svg viewBox="0 0 24 24" fill="none" width="20" height="20">
        <circle cx="12" cy="5"  r="1.5" fill="currentColor"/>
        <circle cx="12" cy="12" r="1.5" fill="currentColor"/>
        <circle cx="12" cy="19" r="1.5" fill="currentColor"/>
      </svg>
    </button>
  </div>`;
}

/** Minimal HTML escape for user-supplied strings */
function escHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// ---------------------------------------------------------------------------
// Sheet system — generic bottom-sheet stack
// ---------------------------------------------------------------------------

/** Currently open sheet element (only one at a time) */
let _openSheet = null;

/**
 * Open a bottom-sheet with arbitrary HTML content.
 * @param {string} innerHtml
 * @param {(el: HTMLElement) => void} [onMount]
 */
function openSheet(innerHtml, onMount) {
  closeSheet();

  const overlay = document.createElement('div');
  overlay.className = 'sheet-overlay';
  overlay.setAttribute('role', 'dialog');
  overlay.setAttribute('aria-modal', 'true');

  const sheet = document.createElement('div');
  sheet.className = 'sheet';
  sheet.innerHTML = innerHtml;
  overlay.appendChild(sheet);

  // Close on overlay backdrop click
  overlay.addEventListener('pointerdown', (e) => {
    if (e.target === overlay) closeSheet();
  });

  // Close on Escape
  const escFn = (e) => { if (e.key === 'Escape') closeSheet(); };
  document.addEventListener('keydown', escFn);
  overlay._escFn = escFn;

  document.body.appendChild(overlay);
  _openSheet = overlay;

  // Animate in
  requestAnimationFrame(() => {
    overlay.classList.add('open');
    sheet.classList.add('open');
  });

  if (onMount) onMount(sheet);
}

function closeSheet() {
  if (!_openSheet) return;
  const overlay = _openSheet;
  overlay.classList.remove('open');
  const sheet = overlay.querySelector('.sheet');
  if (sheet) sheet.classList.remove('open');
  if (overlay._escFn) document.removeEventListener('keydown', overlay._escFn);
  // Wait for CSS transition before removing
  setTimeout(() => { if (overlay.parentNode) overlay.parentNode.removeChild(overlay); }, 300);
  _openSheet = null;
}

// ---------------------------------------------------------------------------
// Action sheet (long-press / three-dot menu)
// ---------------------------------------------------------------------------

/**
 * @param {string} id
 * @param {'saved'|'remote'|'game'|'lan'} ctx
 */
function openActionSheet(id, ctx) {
  const peer = getAllPeerById(id);
  const name = (peer && peer.name) || fmtPeerId(id);
  const isSavedDevice = peer && peer.saved;
  const isFav = peer && peer.fav;

  const actions = [];

  actions.push({ key: 'connect-remote', icon: remoteIcon(), label: t('devices.connect'), primary: true });
  actions.push({ key: 'connect-game',   icon: gameIcon(),   label: t('devices.play') });

  if (isSavedDevice) {
    actions.push({ key: 'fav',    icon: starIcon(isFav), label: isFav ? t('filter.all') + ' — ★ kaldır' : t('devices.fav') });
    actions.push({ key: 'edit',   icon: editIcon(),      label: t('devices.edit') });
    actions.push({ key: 'forget', icon: trashIcon(),     label: t('devices.remove'), danger: true });
  } else {
    if (ctx === 'remote' || ctx === 'game') {
      actions.push({ key: 'save',    icon: saveIcon(),  label: t('home.saveRecent') });
      actions.push({ key: 'remove',  icon: trashIcon(), label: t('home.removeRecent'), danger: true });
    } else if (ctx === 'lan') {
      actions.push({ key: 'save',    icon: saveIcon(),  label: t('devices.lanSave') });
    }
  }

  const rowsHtml = actions.map((a) => `
    <button class="sheet-action${a.primary ? ' primary' : ''}${a.danger ? ' danger' : ''}"
            data-action="${a.key}" style="min-height:54px;">
      <span class="sheet-action-icon">${a.icon}</span>
      <span>${escHtml(a.label)}</span>
    </button>
  `).join('');

  openSheet(`
    <div class="sheet-handle"></div>
    <div class="sheet-title">${escHtml(name)}</div>
    <div class="sheet-id mono" style="font-size:12px;color:var(--text-faint);padding:0 16px 12px;">${escHtml(fmtPeerId(id))}</div>
    <div class="sheet-actions">${rowsHtml}</div>
    <button class="sheet-cancel" data-action="cancel">${t('devices.cancel')}</button>
  `, (el) => {
    el.querySelectorAll('button[data-action]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const action = btn.dataset.action;
        handleAction(action, id, ctx, peer);
      });
    });
  });
}

/**
 * @param {string} action
 * @param {string} id
 * @param {string} ctx
 * @param {object|null} peer
 */
function handleAction(action, id, ctx, peer) {
  closeSheet();
  switch (action) {
    case 'connect-remote':
      doConnect(id, 0, 'remote');
      break;
    case 'connect-game':
      doConnect(id, 0, 'game');
      break;
    case 'fav':
      toggleFav(id);
      break;
    case 'edit':
      openEditSheet(id);
      return; // openEditSheet manages its own sheet
    case 'forget':
      removePeer(id);
      break;
    case 'save':
      addPeer(fmtPeerId(id), id, peer?.cat || 'pc');
      break;
    case 'remove':
      if (ctx === 'game') removeFromGameHistory(id);
      else removeFromHistory(id);
      break;
    case 'cancel':
      break;
    default:
      break;
  }
  // Refresh list after mutation (peerEvents will also fire render)
}

// ---------------------------------------------------------------------------
// Edit sheet (saved devices)
// ---------------------------------------------------------------------------

/** @param {string} id */
function openEditSheet(id) {
  const peer = getAllPeerById(id);
  if (!peer) return;

  const name    = peer.name || '';
  const idVal   = fmtPeerId(peer.id);
  const cat     = peer.cat || 'pc';
  const iconChips = ICON_KEYS.map((k) => `
    <button type="button" class="icon-chip${cat === k ? ' selected' : ''}" data-cat="${k}"
            aria-label="${t('cat.' + k)}" aria-pressed="${String(cat === k)}">
      ${iconFor(k)}
      <span>${t('cat.' + k)}</span>
    </button>
  `).join('');

  openSheet(`
    <div class="sheet-handle"></div>
    <div class="sheet-title">${t('devices.editTitle')}</div>
    <div class="sheet-form">
      <div class="field">
        <label for="edit-name">${t('devices.name')}</label>
        <input id="edit-name" class="input" type="text" inputmode="text"
               value="${escHtml(name)}" placeholder="${t('devices.defaultName')}"
               autocomplete="off" />
      </div>
      <div class="field">
        <label for="edit-id">${t('devices.id')}</label>
        <input id="edit-id" class="input mono" type="text" inputmode="url"
               value="${escHtml(idVal)}" placeholder="${t('devices.idOrIp')}"
               autocomplete="off" />
      </div>
      <div class="field">
        <label>${t('devices.type')}</label>
        <div class="icon-chips">${iconChips}</div>
      </div>
    </div>
    <div class="sheet-actions row">
      <button class="btn btn-ghost" data-action="cancel">${t('devices.cancel')}</button>
      <button class="btn btn-primary" data-action="save">${t('devices.saveBtn')}</button>
    </div>
  `, (el) => {
    let selectedCat = cat;

    el.querySelectorAll('.icon-chip').forEach((chip) => {
      chip.addEventListener('click', () => {
        selectedCat = chip.dataset.cat;
        el.querySelectorAll('.icon-chip').forEach((c) => {
          c.classList.toggle('selected', c.dataset.cat === selectedCat);
          c.setAttribute('aria-pressed', String(c.dataset.cat === selectedCat));
        });
      });
    });

    el.querySelector('[data-action="cancel"]')?.addEventListener('click', closeSheet);
    el.querySelector('[data-action="save"]')?.addEventListener('click', () => {
      const newName = el.querySelector('#edit-name')?.value?.trim() || '';
      const newIdRaw = el.querySelector('#edit-id')?.value?.trim() || '';
      const newId = normalizeId(newIdRaw.replace(/\s/g, ''));

      if (!newId) return; // guard: id must not be blank

      updatePeer(id, {
        name:  newName || undefined,
        newId: newId !== normalizeId(id) ? newId : undefined,
        image: 'icon:' + selectedCat,
      });
      // Also update cat via a dedicated path
      const updated = getAllPeerById(newId || id);
      if (updated) updated.cat = selectedCat;

      closeSheet();
    });
  });
}

// ---------------------------------------------------------------------------
// Add-device sheet
// ---------------------------------------------------------------------------

/**
 * Load an image file, centre-crop it to a `size`×`size` square and return a
 * compressed JPEG data URL — small enough to persist in localStorage as a custom
 * device icon (a raw photo data-URL would be megabytes).
 * @param {File} file @param {number} size @returns {Promise<string>}
 */
function resizeImage(file, size) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      try {
        const canvas = document.createElement('canvas');
        canvas.width = size; canvas.height = size;
        const ctx = canvas.getContext('2d');
        const scale = Math.max(size / img.width, size / img.height);
        const w = img.width * scale, h = img.height * scale;
        ctx.drawImage(img, (size - w) / 2, (size - h) / 2, w, h);
        resolve(canvas.toDataURL('image/jpeg', 0.82));
      } catch (e) { reject(e); } finally { URL.revokeObjectURL(url); }
    };
    img.onerror = (e) => { URL.revokeObjectURL(url); reject(e); };
    img.src = url;
  });
}

/**
 * Open the add-device bottom-sheet. Optionally pre-fill the id/name (used by the
 * connect screen's long-press "Save" → reuses this exact popup so the user can set
 * a name + category before saving).
 * @param {{ id?: string, name?: string }} [prefill]
 */
export function openAddSheet(prefill = {}) {
  const iconChips = ICON_KEYS.map((k) => `
    <button type="button" class="icon-chip${k === 'pc' ? ' selected' : ''}" data-cat="${k}"
            aria-label="${t('cat.' + k)}" aria-pressed="${String(k === 'pc')}">
      ${iconFor(k)}
      <span>${t('cat.' + k)}</span>
    </button>
  `).join('');

  openSheet(`
    <div class="sheet-handle"></div>
    <div class="sheet-title">${t('devices.add')}</div>
    <div class="sheet-form">
      <div class="field">
        <label for="add-name">${t('devices.name')}</label>
        <input id="add-name" class="input" type="text" inputmode="text"
               placeholder="${t('devices.defaultName')}" autocomplete="off" />
      </div>
      <div class="field">
        <label for="add-id">${t('devices.id')}</label>
        <input id="add-id" class="input mono" type="text" inputmode="url"
               placeholder="000 000 000 ${t('devices.idOrIp')}" autocomplete="off" />
      </div>
      <div class="field">
        <label>${t('devices.type')}</label>
        <div class="icon-chips">${iconChips}</div>
      </div>
      <div class="field">
        <label>${t('m.dev.photo')}</label>
        <div style="display:flex;align-items:center;gap:14px">
          <button type="button" id="add-avatar-btn" class="add-avatar-btn" aria-label="${t('m.dev.photo')}">
            <span id="add-avatar-img" class="add-avatar-img"></span>
            <svg id="add-avatar-ph" viewBox="0 0 24 24" fill="none" width="20" height="20"><path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
          </button>
          <input id="add-avatar-file" type="file" accept="image/*" style="display:none" />
          <button type="button" id="add-avatar-remove" class="btn btn-ghost" style="display:none;padding:8px 14px;font-size:13px">${t('m.dev.removePhoto')}</button>
        </div>
      </div>
      <div id="add-msg" class="msg" style="display:none;"></div>
    </div>
    <div class="sheet-actions row">
      <button class="btn btn-ghost" data-action="cancel">${t('devices.cancel')}</button>
      <button class="btn btn-primary" data-action="add">${t('devices.addBtn')}</button>
    </div>
  `, (el) => {
    let selectedCat = 'pc';

    // Pre-fill (connect-screen "Save" long-press).
    if (prefill && prefill.id) {
      const idEl = el.querySelector('#add-id');
      if (idEl) idEl.value = prefill.id;
    }
    if (prefill && prefill.name) {
      const nameEl = el.querySelector('#add-name');
      if (nameEl) nameEl.value = prefill.name;
    }

    // Custom device photo (optional) — centre-cropped + compressed to a data URL.
    let selectedImage = null;
    const fileInput = el.querySelector('#add-avatar-file');
    const avatarImg = el.querySelector('#add-avatar-img');
    const avatarPh  = el.querySelector('#add-avatar-ph');
    const removeBtn = el.querySelector('#add-avatar-remove');
    const setPreview = (url) => {
      selectedImage = url;
      if (url) {
        avatarImg.style.backgroundImage = `url("${url}")`;
        avatarImg.style.display = 'block';
        if (avatarPh) avatarPh.style.display = 'none';
        if (removeBtn) removeBtn.style.display = '';
      } else {
        avatarImg.style.backgroundImage = '';
        avatarImg.style.display = 'none';
        if (avatarPh) avatarPh.style.display = '';
        if (removeBtn) removeBtn.style.display = 'none';
        if (fileInput) fileInput.value = '';
      }
    };
    el.querySelector('#add-avatar-btn')?.addEventListener('click', () => fileInput?.click());
    fileInput?.addEventListener('change', async () => {
      const f = fileInput.files && fileInput.files[0];
      if (!f) return;
      try { setPreview(await resizeImage(f, 160)); } catch (_) {}
    });
    removeBtn?.addEventListener('click', () => setPreview(null));

    el.querySelectorAll('.icon-chip').forEach((chip) => {
      chip.addEventListener('click', () => {
        selectedCat = chip.dataset.cat;
        el.querySelectorAll('.icon-chip').forEach((c) => {
          c.classList.toggle('selected', c.dataset.cat === selectedCat);
          c.setAttribute('aria-pressed', String(c.dataset.cat === selectedCat));
        });
      });
    });

    el.querySelector('[data-action="cancel"]')?.addEventListener('click', closeSheet);
    el.querySelector('[data-action="add"]')?.addEventListener('click', () => {
      const nameVal = el.querySelector('#add-name')?.value?.trim() || '';
      const idRaw   = el.querySelector('#add-id')?.value?.trim() || '';
      const idNorm  = normalizeId(idRaw.replace(/\s/g, ''));
      const msgEl   = el.querySelector('#add-msg');

      if (!idNorm) {
        if (msgEl) { msgEl.textContent = t('home.idOrIp'); msgEl.style.display = ''; }
        return;
      }
      if (msgEl) msgEl.style.display = 'none';

      addPeer(nameVal || t('devices.defaultName'), idNorm, selectedCat, selectedImage || undefined);
      closeSheet();
    });
  });
}

// ---------------------------------------------------------------------------
// Peer lookup helper (reads from all peers by id)
// ---------------------------------------------------------------------------

/** @param {string} id @returns {object|null} */
function getAllPeerById(id) {
  const nid = normalizeId(id);
  // Scan saved + history + game lists for a match
  const all = [
    ...savedPeers(),
    ...historyPeers(),
    ...gameHistoryPeers(),
  ];
  return all.find((p) => normalizeId(p.id) === nid) || null;
}

// ---------------------------------------------------------------------------
// Touch / long-press row interaction
// ---------------------------------------------------------------------------

/**
 * Wire touch-first interaction on a device row:
 *  - Tap (no move) → doConnect (primary action)
 *  - Long-press (420ms hold, no move) → action sheet
 *  - Three-dot (…) button → action sheet
 * @param {HTMLElement} rowEl
 */
function wireRowInteraction(rowEl) {
  const id  = rowEl.dataset.id;
  const ctx = rowEl.dataset.ctx;
  if (!id) return;

  let startX = 0, startY = 0, moved = false;

  // Tap / long-press on the row itself (not on the ... button)
  rowEl.addEventListener('pointerdown', (e) => {
    if (e.target.closest('.device-more')) return; // handled separately
    startX = e.clientX; startY = e.clientY; moved = false;
    _longPressTimer = setTimeout(() => {
      if (!moved) openActionSheet(id, ctx);
    }, LONG_PRESS_MS);
  }, { passive: true });

  rowEl.addEventListener('pointermove', (e) => {
    if (Math.abs(e.clientX - startX) > 8 || Math.abs(e.clientY - startY) > 8) {
      moved = true;
      clearTimeout(_longPressTimer);
    }
  }, { passive: true });

  rowEl.addEventListener('pointerup', (e) => {
    clearTimeout(_longPressTimer);
    if (e.target.closest('.device-more')) return;
    if (!moved) {
      // Primary tap → connect in the contextual mode
      const mode = ctx === 'game' ? 'game' : 'remote';
      doConnect(id, 0, mode);
    }
  });

  rowEl.addEventListener('pointercancel', () => clearTimeout(_longPressTimer));

  // Keyboard: Enter / Space
  rowEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      const mode = ctx === 'game' ? 'game' : 'remote';
      doConnect(id, 0, mode);
    }
  });

  // Three-dot (…) button
  const moreBtn = rowEl.querySelector('.device-more');
  if (moreBtn) {
    moreBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      openActionSheet(id, ctx);
    });
  }
}

// ---------------------------------------------------------------------------
// Section HTML builders
// ---------------------------------------------------------------------------

/**
 * Build the "Kaydedilenler" (address book) section HTML.
 * @returns {string}
 */
function buildSavedSection() {
  const list = savedPeers().sort((a, b) => {
    // Favourites first, then alphabetical
    if (a.fav !== b.fav) return a.fav ? -1 : 1;
    return (a.name || '').localeCompare(b.name || '');
  });

  if (list.length === 0) return '';

  return `
    <div class="sect-label">${t('devices.savedSection')}</div>
    <div class="row-list device-list" data-section="saved">
      ${list.map((p) => peerRowHtml(p, 'saved')).join('')}
    </div>
  `;
}

/**
 * Build the "Son Bağlantılar" (history) section with a remote/game mode filter.
 * @returns {string}
 */
function buildHistorySection() {
  const remoteList = historyPeers(20);
  const gameList   = gameHistoryPeers(20);
  if (remoteList.length === 0 && gameList.length === 0) return '';

  const activeList = _historyMode === 'game' ? gameList : remoteList;
  const ctx        = _historyMode;

  const itemsHtml = activeList.length
    ? activeList.map((p) => peerRowHtml(p, ctx)).join('')
    : `<div class="empty">${t('home.noRecents')}</div>`;

  return `
    <div class="devices-history-hdr">
      <div class="sect-label">${t('home.recents')}</div>
      <div class="history-mode-chips">
        <button class="chip${_historyMode === 'remote' ? ' active' : ''}" data-hm="remote">
          ${remoteIcon(16)} ${t('home.modeRemote')}
        </button>
        <button class="chip${_historyMode === 'game' ? ' active' : ''}" data-hm="game">
          ${gameIcon(16)} ${t('home.modeGame')}
        </button>
      </div>
    </div>
    <div class="row-list device-list" data-section="history">
      ${itemsHtml}
    </div>
    ${activeList.length > 0 ? `
      <button class="btn btn-ghost clear-history-btn" data-hm="${ctx}">
        ${t('home.removeRecent')}…
      </button>
    ` : ''}
  `;
}

/**
 * Build the LAN presence section (W5 stub — shows only if _lanDevices is non-empty).
 * @returns {string}
 */
function buildLanSection() {
  if (_lanDevices.length === 0) return '';

  const rowsHtml = _lanDevices.map((d) => `
    <div class="device-row" data-id="${escHtml(d.id)}" data-ctx="lan"
         role="button" tabindex="0">
      <div class="device-icon">${iconFor(d.cat || 'pc')}</div>
      <div class="device-meta">
        <div class="device-name">${escHtml(d.name || fmtPeerId(d.id))}</div>
        <div class="device-id mono">${escHtml(fmtPeerId(d.id))}
          <span class="online-dot" aria-label="${t('devices.online')}"></span>
        </div>
        <div class="device-when">${t('devices.online')}</div>
      </div>
      <button class="device-more icon-btn" aria-label="${t('devices.more')}" data-id="${escHtml(d.id)}" data-ctx="lan">
        <svg viewBox="0 0 24 24" fill="none" width="20" height="20">
          <circle cx="12" cy="5"  r="1.5" fill="currentColor"/>
          <circle cx="12" cy="12" r="1.5" fill="currentColor"/>
          <circle cx="12" cy="19" r="1.5" fill="currentColor"/>
        </svg>
      </button>
    </div>
  `).join('');

  return `
    <div class="sect-label">${t('devices.lanTitle')}</div>
    <div class="row-list device-list" data-section="lan">${rowsHtml}</div>
  `;
}

// ---------------------------------------------------------------------------
// Inline SVG helpers for action icons
// ---------------------------------------------------------------------------

/** @param {number} [sz=20] */
const remoteIcon = (sz = 20) => `<svg width="${sz}" height="${sz}" viewBox="0 0 24 24" fill="none">
  <rect x="3" y="4" width="18" height="13" rx="2" stroke="currentColor" stroke-width="1.8"/>
  <path d="M8 20h8M12 17v3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
</svg>`;

/** @param {number} [sz=20] */
const gameIcon = (sz = 20) => `<svg width="${sz}" height="${sz}" viewBox="0 0 24 24" fill="none">
  <rect x="2" y="8" width="20" height="8" rx="3" stroke="currentColor" stroke-width="1.8"/>
  <path d="M8 12H6M7 11v2M16 12h2M15 11.5l1.5 1-1.5 1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

/** @param {boolean} isFav */
const starIcon = (isFav) => `<svg width="20" height="20" viewBox="0 0 24 24" fill="${isFav ? 'currentColor' : 'none'}">
  <path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z" stroke="currentColor" stroke-width="1.8"/>
</svg>`;

const editIcon = () => `<svg width="20" height="20" viewBox="0 0 24 24" fill="none">
  <path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

const trashIcon = () => `<svg width="20" height="20" viewBox="0 0 24 24" fill="none">
  <polyline points="3 6 5 6 21 6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M19 6l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M10 11v6M14 11v6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

const saveIcon = () => `<svg width="20" height="20" viewBox="0 0 24 24" fill="none">
  <path d="M19 21H5a2 2 0 01-2-2V5a2 2 0 012-2h11l5 5v11a2 2 0 01-2 2z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  <polyline points="17 21 17 13 7 13 7 21" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  <polyline points="7 3 7 8 15 8" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

// ---------------------------------------------------------------------------
// Main render
// ---------------------------------------------------------------------------

/** Full screen render — called on mount + on peerEvents 'change' */
function render() {
  if (!_root) return;

  // Devices is the saved-devices address book ONLY — recents + "on your network"
  // live on the Connect screen now, so we don't render history/LAN sections here.
  const saved   = buildSavedSection();
  const showEmpty = !saved;

  _root.innerHTML = `
    <div class="devices-screen">
      <div class="devices-hdr">
        <div>
          <h2 class="title">${t('devices.title')}</h2>
          <p class="sub">${t('devices.sub')}</p>
        </div>
        <button class="btn btn-primary add-btn" id="devices-add-btn"
                style="white-space:nowrap; flex-shrink:0;" aria-label="${t('devices.add')}">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" style="margin-right:5px;vertical-align:middle;">
            <path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>
          </svg>${t('devices.add')}
        </button>
      </div>

      ${showEmpty ? `
        <div class="empty-state">
          <svg width="52" height="52" viewBox="0 0 24 24" fill="none" style="opacity:.3;margin-bottom:16px;">
            <rect x="3" y="4" width="18" height="13" rx="2" stroke="currentColor" stroke-width="1.5"/>
            <path d="M8 20h8M12 17v3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          <div class="empty">${t('devices.empty')}</div>
        </div>
      ` : saved}
    </div>
  `;

  // Wire add button
  _root.querySelector('#devices-add-btn')?.addEventListener('click', () => openAddSheet());

  // Wire history mode chips
  _root.querySelectorAll('.chip[data-hm]').forEach((chip) => {
    chip.addEventListener('click', () => {
      const hm = chip.dataset.hm;
      if (hm === 'remote' || hm === 'game') {
        _historyMode = hm;
        render();
      }
    });
  });

  // Wire clear history
  _root.querySelectorAll('.clear-history-btn[data-hm]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const hm = btn.dataset.hm;
      if (hm === 'game') {
        // Clear game history: nullify gameConnected for all non-saved peers
        const gpeers = gameHistoryPeers();
        for (const p of gpeers) {
          removeFromGameHistory(p.id);
        }
      } else {
        clearHistory();
      }
      render();
    });
  });

  // Wire device rows (both saved + history + lan sections)
  _root.querySelectorAll('.device-row').forEach(wireRowInteraction);
}

// ---------------------------------------------------------------------------
// Mount (called once by router when screen is first shown)
// ---------------------------------------------------------------------------

function mount() {
  // The section DOM is `#t-history` (existing index.html).
  // devices.js registers under id 'history' to match that DOM id.
  _root = document.getElementById('t-history');
  if (!_root) return;

  // Listen for peer store changes → re-render
  peerEvents.addEventListener('change', render);

  // Listen for W5 lan-presence bus event (stub: degrades gracefully)
  const bus = window.__pulsarBus;
  if (bus) {
    bus.on('lan-presence', (devices) => {
      _lanDevices = Array.isArray(devices) ? devices : [];
      render();
    });
  }

  render();
}

function onShow() {
  render();
}

// ---------------------------------------------------------------------------
// Register
// ---------------------------------------------------------------------------

registerScreen({
  id:          'history',
  navIcon: `<svg viewBox="0 0 24 24" fill="none">
    <rect x="3" y="4" width="18" height="13" rx="2" stroke="currentColor" stroke-width="2"/>
    <path d="M8 20h8M12 17v3" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
  </svg>`,
  navLabel:    t('nav.devices'),
  navLabelKey: 'nav.devices',
  mount,
  onShow,
});
