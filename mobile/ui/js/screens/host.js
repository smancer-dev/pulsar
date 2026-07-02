/**
 * screens/host.js — Cihazım (My Device / host) screen
 *
 * Full W3-host implementation per §2.7 + DT-host design spec.
 *
 * Features:
 *   - Online / offline toggle with live status indicator
 *   - Large stable device ID with copy + share
 *   - OTP chip with copy + rotate
 *   - Unattended-access toggle (reads cfg, no inline edit here — see settings)
 *   - Live connected-peers list with per-peer kick
 *   - Touch-first incoming-connection approval bottom-sheet with 30s countdown
 *   - Accessibility permission entry
 *
 * Events listened:
 *   host-password        { password }
 *   session-request      { reqId, peer, hasPassword }
 *   host-peer-connected  { sid, peer }
 *   host-peer-disconnected { sid }
 *
 * Commands called:
 *   go_online, go_offline, new_password,
 *   respond_request, disconnect_session,
 *   open_a11y_settings, a11y_enabled
 *
 * Design: indigo accent, large mono ID, bottom-sheet approval, ≥44px tap targets.
 */

import { registerScreen } from '../router.js';
import { invoke, listen, hasTauri, clipboard, share } from '../tauri.js';
import { t } from '../i18n.js';
import { getConfig } from '../store/config.js';

// ── State ─────────────────────────────────────────────────────────────────────

/** @type {{ sid: number, peer: string, elapsed: number }[]} */
let activePeers = [];

/** @type {{ reqId: number, peer: string, hasPassword: boolean } | null} */
let pendingRequest = null;

/** Auto-deny countdown timer id */
let countdownTimer = null;
let countdownRemaining = 30;

/** Whether we are currently online */
let isOnline = false;

/** Current device id (u32) or null */
let myId = null;

/** Current OTP */
let currentOtp = '';

/** Whether a go_online call is in flight */
let going = false;

/** Elapsed seconds counter for peer rows */
let elapsedInterval = null;

// ── DOM helpers ───────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);

function qs(sel, ctx) {
	return (ctx || document).querySelector(sel);
}

// ── Toast ─────────────────────────────────────────────────────────────────────

let toastTimer = null;

function showToast(msg, durationMs = 2200) {
	let el = $('host-toast');
	if (!el) return;
	el.textContent = msg;
	el.classList.add('show');
	clearTimeout(toastTimer);
	toastTimer = setTimeout(() => el && el.classList.remove('show'), durationMs);
}

// ── ID / OTP formatting ───────────────────────────────────────────────────────

function fmtId(id) {
	// 9-digit → "123 456 789"
	return String(id).padStart(9, '0').replace(/(\d{3})(\d{3})(\d{3})/, '$1 $2 $3');
}

// ── Elapsed time label ────────────────────────────────────────────────────────

function fmtElapsed(sec) {
	if (sec < 60)  return t('host.elapsedSec', { n: sec });
	if (sec < 3600) return t('host.elapsedMin', { n: Math.floor(sec / 60) });
	return t('host.elapsedHour', { h: Math.floor(sec / 3600), m: Math.floor((sec % 3600) / 60) });
}

// ── Render helpers ────────────────────────────────────────────────────────────

/** Render the status pill + online/offline toggle button. */
function renderStatus() {
	const statusEl = $('h-status');
	const onlineBtn = $('h-online-btn');
	const idSection = $('h-id-section');
	const pwSection = $('h-pw-section');
	const peerSection = $('h-peer-section');

	if (!statusEl) return;

	// Connected-devices list is only meaningful while online — hide it when offline
	// (you can't have peers connected to an offline host).
	if (peerSection) peerSection.style.display = isOnline ? '' : 'none';

	if (isOnline) {
		statusEl.className = 'statusline live';
		statusEl.innerHTML = `<span class="d"></span><span>${t('status.online')}</span>`;
		if (onlineBtn) {
			onlineBtn.textContent = t('m.host.goOffline');
			onlineBtn.className = 'btn btn-danger full';
			onlineBtn.disabled = false;
		}
		if (idSection) idSection.style.display = '';
		if (pwSection) pwSection.style.display = '';
		showLocalIp();
		applyUnattendedHost();
	} else {
		const lip = $('h-local-ip');
		if (lip) lip.style.display = 'none';
		statusEl.className = 'statusline';
		statusEl.innerHTML = `<span class="d"></span><span>${t('status.offline')}</span>`;
		if (onlineBtn) {
			onlineBtn.textContent = going ? t('status.connecting') : t('m.host.goOnline');
			onlineBtn.className = 'btn btn-primary';
			onlineBtn.disabled = going;
		}
		if (idSection) idSection.style.display = going ? '' : 'none';
		if (pwSection) pwSection.style.display = going ? '' : 'none';
	}
}

/** Fetch + show this device's local LAN IP (so peers on the same network can
 *  connect by address). Best-effort; hidden if unavailable. */
async function showLocalIp() {
	const box = $('h-local-ip');
	const val = $('h-local-ip-val');
	if (!box || !val) return;
	try {
		const ip = await invoke('local_ip');
		if (ip) {
			// Append the node's bound port so peers can reach this device by IP:port.
			let txt = ip;
			try { const p = await invoke('node_port'); if (p && p > 0) txt = `${ip}:${p}`; } catch (_) {}
			val.textContent = txt; box.style.display = '';
		} else box.style.display = 'none';
	} catch (_) { box.style.display = 'none'; }
}

/** When unattended access is on the host issues no one-time password — show a
 *  warning banner in place of the OTP row (desktop SelfCard parity). */
async function applyUnattendedHost() {
	let on = false;
	try { on = !!(await getConfig()).unattendedAccess; } catch (_) {}
	const info = $('h-unattended-info');
	const row  = $('h-otp-row');
	if (info) info.style.display = on ? '' : 'none';
	if (row)  row.style.display  = on ? 'none' : 'flex';
}

/** Render the device ID block. */
function renderId() {
	const idEl = $('h-id');
	if (!idEl) return;
	if (myId != null) {
		idEl.textContent = fmtId(myId);
		idEl.classList.remove('ph');
	} else {
		idEl.textContent = '--- --- ---';
		idEl.classList.add('ph');
	}
}

/** Render the OTP chip. */
function renderOtp() {
	const otpEl = $('h-otp');
	if (otpEl) otpEl.textContent = currentOtp || '----';
}

/** Render the peer list. */
function renderPeers() {
	const listEl = $('h-peer-list');
	const emptyEl = $('h-peer-empty');
	if (!listEl) return;

	if (activePeers.length === 0) {
		listEl.innerHTML = '';
		if (emptyEl) emptyEl.style.display = '';
		return;
	}

	if (emptyEl) emptyEl.style.display = 'none';

	listEl.innerHTML = activePeers.map(p => `
		<div class="item h-peer-row" data-sid="${p.sid}">
			<div class="ic">
				<svg viewBox="0 0 24 24" fill="none" width="20" height="20">
					<rect x="6" y="3" width="12" height="18" rx="2.5"
						stroke="currentColor" stroke-width="2"/>
					<path d="M11 18h2" stroke="currentColor"
						stroke-width="2" stroke-linecap="round"/>
				</svg>
			</div>
			<div class="meta">
				<div class="id">${p.peer}</div>
				<div class="when">${fmtElapsed(p.elapsed)}</div>
			</div>
			<button
				class="btn btn-ghost h-kick-btn"
				data-sid="${p.sid}"
				aria-label="${t('home.kick')} — ${p.peer}"
				style="padding:10px 14px;font-size:13px;"
			>${t('home.kickLabel')}</button>
		</div>
	`).join('');

	// Attach kick handlers.
	listEl.querySelectorAll('.h-kick-btn').forEach(btn => {
		btn.addEventListener('click', (e) => {
			e.stopPropagation();
			const sid = Number(btn.dataset.sid);
			kickPeer(sid);
		});
	});
}

// ── Approval sheet ────────────────────────────────────────────────────────────

function openApprovalSheet(req) {
	pendingRequest = req;
	countdownRemaining = 30;

	const sheet = $('h-req-sheet');
	const backdrop = $('h-req-backdrop');
	if (!sheet || !backdrop) return;

	// Populate.
	const peerEl = $('h-req-peer');
	const pwEl   = $('h-req-pw-status');
	const cntEl  = $('h-req-countdown');

	if (peerEl) peerEl.textContent = req.peer;
	if (pwEl) {
		pwEl.textContent = req.hasPassword
			? t('approve.pwOk')
			: t('approve.pwNone');
		pwEl.className = 'val-chip ' + (req.hasPassword ? 'ok' : '');
	}
	if (cntEl) cntEl.textContent = countdownRemaining + 's';

	sheet.classList.add('open');
	backdrop.classList.add('open');

	// Start 30s countdown.
	clearInterval(countdownTimer);
	countdownTimer = setInterval(() => {
		countdownRemaining--;
		if (cntEl) cntEl.textContent = countdownRemaining + 's';
		if (countdownRemaining <= 0) {
			clearInterval(countdownTimer);
			countdownTimer = null;
			closeApprovalSheet();
		}
	}, 1000);
}

function closeApprovalSheet() {
	clearInterval(countdownTimer);
	countdownTimer = null;
	pendingRequest = null;

	const sheet = $('h-req-sheet');
	const backdrop = $('h-req-backdrop');
	if (sheet) sheet.classList.remove('open');
	if (backdrop) backdrop.classList.remove('open');
}

// ── Commands ──────────────────────────────────────────────────────────────────

// Tap "Go online": if the remote-control (accessibility) permission is OFF, show a
// warning sheet first — the phone is still shareable (the peer sees the screen)
// without it, but the peer can't TOUCH/control it. The user grants it or continues.
async function goOnline() {
	if (going || isOnline) return;
	let granted = false;
	if (hasTauri) {
		try { granted = await invoke('a11y_enabled', {}); } catch (_) {}
	}
	if (!granted) { openA11ySheet(); return; }
	doGoOnline();
}

function openA11ySheet() {
	const sheet = $('h-a11y-sheet');
	const backdrop = $('h-a11y-backdrop');
	if (!sheet || !backdrop) { doGoOnline(); return; } // sheet missing → don't block
	sheet.classList.add('open');
	backdrop.classList.add('open');
}

function closeA11ySheet() {
	$('h-a11y-sheet')?.classList.remove('open');
	$('h-a11y-backdrop')?.classList.remove('open');
}

// (Swipe-down-to-dismiss is now handled globally for ALL sheets in sheet-swipe.js.)

async function doGoOnline() {
	if (going) return;
	going = true;
	renderStatus();

	try {
		// Read relay/name/netmode from localStorage (set by config screen).
		const relay   = localStorage.getItem('pulsar.relay')   || '';
		const name    = localStorage.getItem('pulsar.name')    || '';
		const netmode = localStorage.getItem('pulsar.netmode') || 'auto';
		const r = await invoke('go_online', { relay, name, netmode });
		if (r && r.ok) {
			isOnline = true;
			myId = r.id;
			currentOtp = r.password;
			renderId();
			renderOtp();
			// Start elapsed counter.
			startElapsedCounter();
		} else {
			showToast(t('m.error'));
		}
	} catch (e) {
		showToast(t('m.error') + ': ' + String(e));
	} finally {
		going = false;
		renderStatus();
	}
}

async function goOffline() {
	try {
		await invoke('go_offline', {});
	} catch (_) {}

	isOnline = false;
	myId = null;
	currentOtp = '';
	activePeers = [];
	stopElapsedCounter();
	renderId();
	renderOtp();
	renderPeers();
	renderStatus();
}

async function rotatePassword() {
	try {
		const r = await invoke('new_password', {});
		if (r && r.password) {
			currentOtp = r.password;
			renderOtp();
			showToast(t('m.host.password') + ': ' + r.password);
		}
	} catch (e) {
		showToast(t('m.error'));
	}
}

async function kickPeer(sid) {
	try {
		await invoke('disconnect_session', { sid });
	} catch (_) {}
	// Optimistic removal (event will confirm).
	activePeers = activePeers.filter(p => p.sid !== sid);
	renderPeers();
}

async function respondRequest(allow) {
	const req = pendingRequest;
	closeApprovalSheet();
	if (!req) return;
	try {
		await invoke('respond_request', { reqId: req.reqId, allow });
	} catch (_) {}
}

async function refreshA11y() {
	if (!hasTauri) return;
	try {
		const on = await invoke('a11y_enabled', {});
		const statusEl = $('ctrl-status');
		const openBtn  = $('h-a11y-btn');
		if (statusEl) {
			statusEl.textContent = on
				? 'Uzaktan kontrol açık — karşı cihaz dokunarak kontrol edebilir.'
				: t('m.host.a11yHint');
		}
		if (openBtn) {
			openBtn.textContent = on
				? 'Kontrol açık ✓'
				: t('m.host.a11yOpen');
			openBtn.disabled = on;
		}
	} catch (_) {}
}

// ── Elapsed counter ───────────────────────────────────────────────────────────

function startElapsedCounter() {
	stopElapsedCounter();
	elapsedInterval = setInterval(() => {
		activePeers.forEach(p => { p.elapsed++; });
		renderPeers();
	}, 1000);
}

function stopElapsedCounter() {
	if (elapsedInterval != null) {
		clearInterval(elapsedInterval);
		elapsedInterval = null;
	}
}

// ── Copy / share helpers ──────────────────────────────────────────────────────

async function copyId() {
	if (myId == null) return;
	await clipboard(fmtId(myId));
	showToast(t('m.copied'));
}

async function shareId() {
	if (myId == null) return;
	const id = fmtId(myId);
	// Copy to the clipboard AND open the system share sheet.
	await clipboard(id);
	showToast(t('m.copied'));
	await share({ title: 'Pulsar', text: 'Pulsar ID: ' + id });
}

async function copyOtp() {
	if (!currentOtp) return;
	await clipboard(currentOtp);
	showToast(t('m.copied'));
}

// ── Mount DOM ─────────────────────────────────────────────────────────────────

function buildDOM(root) {
	root.innerHTML = `
<div class="host-screen" style="animation:rise 0.4s var(--ease) both">

  <!-- Title -->
  <h2 class="title">${t('nav.host')}</h2>
  <p class="sub">${t('m.host.sub')}</p>

  <!-- Status card -->
  <div class="card" style="gap:12px">
    <div id="h-status" class="statusline">
      <span class="d"></span>
      <span>${t('status.offline')}</span>
    </div>

    <!-- ID block (hidden until online) -->
    <div id="h-id-section" style="display:none;text-align:center">
      <div class="sect-label" style="margin-bottom:6px">${t('m.host.online')}</div>
      <div id="h-id" class="idbig ph">--- --- ---</div>
      <div id="h-local-ip" style="margin-top:8px;font-size:12.5px;color:var(--text-muted);display:none">
        <span data-i18n="m.host.localAddr">${t('m.host.localAddr')}</span> · <span id="h-local-ip-val" class="mono"></span>
      </div>
      <!-- ID action row -->
      <div style="display:flex;gap:10px;margin-top:12px;justify-content:center">
        <button id="h-copy-id-btn" class="btn btn-ghost"
          aria-label="${t('home.copyId')}" style="flex:1;max-width:140px">
          <svg viewBox="0 0 24 24" fill="none" width="17" height="17">
            <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" stroke-width="1.8"/>
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke="currentColor" stroke-width="1.8"/>
          </svg>
          ${t('m.copy')}
        </button>
        <button id="h-share-id-btn" class="btn btn-ghost"
          aria-label="${t('m.host.share')}" style="flex:1;max-width:140px">
          <svg viewBox="0 0 24 24" fill="none" width="17" height="17">
            <path d="M4 12v7a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7M16 6l-4-4-4 4M12 2v13"
              stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
          ${t('m.host.share')}
        </button>
      </div>
    </div>

    <!-- OTP block -->
    <div id="h-pw-section" style="display:none">
      <div class="sect-label" style="margin-bottom:6px">${t('m.host.password')}</div>
      <div id="h-unattended-info" style="display:none">
        <div class="host-unattended-banner">
          <strong data-i18n="m.host.unattendedOn">${t('m.host.unattendedOn')}</strong>
          <span data-i18n="m.host.unattendedOnHint">${t('m.host.unattendedOnHint')}</span>
        </div>
      </div>
      <div id="h-otp-row" style="display:flex;align-items:center;gap:10px">
        <div id="h-otp" class="pw-chip" style="flex:1;min-width:0;text-align:center;letter-spacing:0.04em;font-size:17px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">
          ----
        </div>
        <button id="h-copy-otp-btn" class="icon-btn"
          title="${t('m.copy')}" aria-label="${t('m.copy')} OTP"
          style="background:var(--accent-soft);color:var(--accent)">
          <svg viewBox="0 0 24 24" fill="none" width="18" height="18">
            <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" stroke-width="1.8"/>
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke="currentColor" stroke-width="1.8"/>
          </svg>
        </button>
        <button id="h-rotate-otp-btn" class="icon-btn"
          title="${t('m.host.rotate')}" aria-label="${t('m.host.rotate')}"
          style="background:var(--accent-soft);color:var(--accent)">
          <svg viewBox="0 0 24 24" fill="none" width="18" height="18">
            <path d="M1 4v6h6M23 20v-6h-6" stroke="currentColor" stroke-width="1.8"
              stroke-linecap="round" stroke-linejoin="round"/>
            <path d="M20.49 9A9 9 0 0 0 5.64 5.64L1 10m22 4-4.64 4.36A9 9 0 0 1 3.51 15"
              stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </button>
      </div>
    </div>

    <!-- Online / offline toggle -->
    <button id="h-online-btn" class="btn btn-primary">
      ${t('m.host.goOnline')}
    </button>
  </div>

  <!-- Connected peers (only meaningful while online) -->
  <div class="card" id="h-peer-section" style="margin-top:14px">
    <div class="sect-label">${t('m.host.peerList')}</div>
    <div id="h-peer-list" class="row-list"></div>
    <div id="h-peer-empty" class="empty" style="padding:20px 0 4px">
      ${t('m.host.noPeers')}
    </div>
  </div>

  <!-- Accessibility permission is no longer a persistent card here; it is asked
       for via the warning sheet (#h-a11y-sheet) when the user taps Go online. -->

</div>

<!-- Toast -->
<div id="host-toast" class="toast" role="status" aria-live="polite"></div>

<!-- ── Approval sheet ───────────────────────────────────────────────── -->
<div id="h-req-backdrop" class="sheet-backdrop"></div>
<div id="h-req-sheet" class="sheet" role="dialog"
     aria-modal="true" aria-labelledby="h-req-title">
  <div class="sheet-handle"></div>

  <!-- Header -->
  <div>
    <h3 id="h-req-title" style="font-family:var(--font-display);font-size:19px;
        font-weight:700;letter-spacing:-0.02em;margin:0 0 4px">
      ${t('m.host.reqTitle')}
    </h3>
    <p style="margin:0;font-size:13.5px;color:var(--text-muted)">
      ${t('approve.lead')}
    </p>
  </div>

  <!-- Peer identity block -->
  <div style="background:var(--accent-soft);border:1px solid var(--accent-soft-2);
      border-radius:var(--r-lg);padding:14px 16px;display:flex;align-items:center;gap:12px">
    <div style="width:40px;height:40px;border-radius:12px;background:var(--accent-soft-2);
        color:var(--accent);display:grid;place-items:center;flex:none">
      <svg viewBox="0 0 24 24" fill="none" width="22" height="22">
        <rect x="6" y="3" width="12" height="18" rx="2.5"
          stroke="currentColor" stroke-width="2"/>
        <path d="M11 18h2" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
      </svg>
    </div>
    <div style="flex:1;min-width:0">
      <div style="font-size:11px;font-weight:700;letter-spacing:0.05em;
          text-transform:uppercase;color:var(--text-faint);margin-bottom:2px">
        ${t('approve.deviceId')}
      </div>
      <div id="h-req-peer" class="mono"
        style="font-size:15px;font-weight:600;color:var(--accent);word-break:break-all">
        —
      </div>
    </div>
  </div>

  <!-- Password status chip -->
  <div id="h-req-pw-status" class="val-chip" style="align-self:flex-start">
    ${t('approve.pwNone')}
  </div>

  <!-- Countdown -->
  <div style="display:flex;align-items:center;justify-content:space-between;
      font-size:12.5px;color:var(--text-faint)">
    <span>${t('approve.autoDeny')}</span>
    <span id="h-req-countdown" style="font-family:var(--font-mono);font-weight:700;
        color:var(--warn);font-size:14px">
      30s
    </span>
  </div>

  <!-- Action buttons — stacked for thumb reach -->
  <div style="display:flex;flex-direction:column;gap:10px">
    <button id="h-req-allow" class="btn btn-primary"
      style="background:var(--ok);box-shadow:none;font-size:17px;padding:16px">
      <svg viewBox="0 0 24 24" fill="none" width="20" height="20">
        <path d="M20 6 9 17l-5-5" stroke="currentColor"
          stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      ${t('m.host.allow')}
    </button>
    <button id="h-req-deny" class="btn btn-ghost full"
      style="border-color:var(--danger);color:var(--danger);font-size:17px;padding:16px">
      <svg viewBox="0 0 24 24" fill="none" width="20" height="20">
        <path d="M18 6 6 18M6 6l12 12" stroke="currentColor"
          stroke-width="2.2" stroke-linecap="round"/>
      </svg>
      ${t('m.host.deny')}
    </button>
  </div>
</div>

<!-- ── Accessibility (remote-control) warning sheet — shown on Go online ── -->
<div id="h-a11y-backdrop" class="sheet-backdrop"></div>
<div id="h-a11y-sheet" class="sheet" role="dialog" aria-modal="true" aria-labelledby="h-a11y-title">
  <div class="sheet-handle"></div>
  <div>
    <h3 id="h-a11y-title" style="font-family:var(--font-display);font-size:19px;
        font-weight:700;letter-spacing:-0.02em;margin:0 0 6px">
      ${t('a11y.warnTitle')}
    </h3>
    <p class="msg" style="text-align:left;margin:0 0 12px">${t('a11y.warnLead')}</p>
    <div style="display:flex;flex-direction:column;gap:8px;margin-bottom:4px">
      <div style="display:flex;align-items:center;gap:10px;font-size:14.5px">
        <span style="color:var(--ok);font-weight:700;font-size:17px">✓</span>
        <span>${t('a11y.warnWorks')}</span>
      </div>
      <div style="display:flex;align-items:center;gap:10px;font-size:14.5px">
        <span style="color:var(--danger);font-weight:700;font-size:17px">✕</span>
        <span>${t('a11y.warnNoControl')}</span>
      </div>
    </div>
  </div>
  <div style="display:flex;flex-direction:column;gap:10px">
    <button id="h-a11y-grant" class="btn btn-primary" style="font-size:16px;padding:15px">
      ${t('a11y.grant')}
    </button>
    <button id="h-a11y-continue" class="btn btn-ghost full" style="font-size:16px;padding:15px">
      ${t('a11y.goAnyway')}
    </button>
  </div>
</div>
`;
}

// ── Wire up events ────────────────────────────────────────────────────────────

function wireButtons() {
	// Online / offline toggle.
	$('h-online-btn')?.addEventListener('click', () => {
		if (isOnline) {
			goOffline();
		} else {
			goOnline();
		}
	});

	// Copy + share ID.
	$('h-copy-id-btn')?.addEventListener('click', copyId);
	$('h-share-id-btn')?.addEventListener('click', shareId);

	// Copy + rotate OTP.
	$('h-copy-otp-btn')?.addEventListener('click', copyOtp);
	$('h-rotate-otp-btn')?.addEventListener('click', rotatePassword);

	// Accessibility warning sheet (shown on Go online when control permission is off).
	$('h-a11y-grant')?.addEventListener('click', async () => {
		closeA11ySheet();
		try { await invoke('open_a11y_settings', {}); } catch (_) {}
		// User grants in system settings, returns, and taps Go online again — now
		// the gate sees it granted and goes online directly.
	});
	$('h-a11y-continue')?.addEventListener('click', () => {
		closeA11ySheet();
		doGoOnline();
	});
	$('h-a11y-backdrop')?.addEventListener('click', closeA11ySheet);

	// Approval sheet.
	$('h-req-allow')?.addEventListener('click', () => respondRequest(true));
	$('h-req-deny')?.addEventListener('click',  () => respondRequest(false));
	$('h-req-backdrop')?.addEventListener('click', () => respondRequest(false));
}

// ── Tauri event subscriptions ─────────────────────────────────────────────────

const unlisteners = [];

async function subscribeEvents() {
	if (!hasTauri) return;

	unlisteners.push(
		await listen('host-password', (payload) => {
			if (payload && payload.password) {
				currentOtp = payload.password;
				renderOtp();
			}
		})
	);

	unlisteners.push(
		await listen('session-request', (payload) => {
			if (!payload) return;
			openApprovalSheet({
				reqId:       payload.reqId || payload.req_id,
				peer:        payload.peer || '?',
				hasPassword: !!(payload.hasPassword || payload.has_password),
			});
		})
	);

	unlisteners.push(
		await listen('host-peer-connected', (payload) => {
			if (!payload) return;
			const sid = payload.sid;
			if (!activePeers.find(p => p.sid === sid)) {
				activePeers.push({ sid, peer: payload.peer || '?', elapsed: 0 });
			}
			renderPeers();
		})
	);

	unlisteners.push(
		await listen('host-peer-disconnected', (payload) => {
			if (!payload) return;
			activePeers = activePeers.filter(p => p.sid !== payload.sid);
			renderPeers();
			// If the approval sheet was for this peer (auto-dismiss).
			if (pendingRequest) closeApprovalSheet();
		})
	);
}

// ── Visibility-change refresh (a11y, etc.) ────────────────────────────────────

function onVisible() {
	if (!document.hidden) refreshA11y();
}

// ── Screen registration ───────────────────────────────────────────────────────

let mounted = false;

function mount() {
	if (mounted) return;
	mounted = true;

	// Inject our DOM into the pre-existing #t-host section from index.html.
	const container = document.getElementById('t-host');
	if (container) {
		// Clear the static HTML stub that index.html contains.
		container.innerHTML = '';
		buildDOM(container);
		// The approval sheet + backdrop are position:fixed viewport overlays.
		// #t-host is a .tab carrying `animation: rise ... both` (transform
		// keyframes) which makes it a containing block for fixed descendants —
		// so left inside the tab the sheet positions relative to the tab, not the
		// viewport, and shows inline at the bottom. Relocate both to <body> so
		// they are true full-screen modals.
		const reqBackdrop = document.getElementById('h-req-backdrop');
		const reqSheet = document.getElementById('h-req-sheet');
		if (reqBackdrop) document.body.appendChild(reqBackdrop);
		if (reqSheet) document.body.appendChild(reqSheet);
		// Same for the accessibility-warning sheet: without relocating to <body> the
		// transformed #t-host is its containing block, so translateY(100%) doesn't
		// hide it and it renders inline at the bottom (behind the nav) on mount.
		const a11yBackdrop = document.getElementById('h-a11y-backdrop');
		const a11ySheet = document.getElementById('h-a11y-sheet');
		if (a11yBackdrop) document.body.appendChild(a11yBackdrop);
		if (a11ySheet) document.body.appendChild(a11ySheet);
	}
	wireButtons();

	// Initial render.
	renderStatus();
	renderId();
	renderOtp();
	renderPeers();

	// Subscribe to Tauri events.
	subscribeEvents();

	// Refresh a11y status on first mount and on re-focus.
	refreshA11y();
	document.addEventListener('visibilitychange', onVisible);
}

function onShow() {
	// Re-check a11y whenever the user navigates to this tab.
	refreshA11y();
	// The unattended toggle lives in Settings — re-sync the OTP/warning on return.
	applyUnattendedHost();
}

registerScreen({
	id:          'host',
	navIcon: `<svg viewBox="0 0 24 24" fill="none">
		<rect x="6" y="3" width="12" height="18" rx="2.5"
			stroke="currentColor" stroke-width="2"/>
		<path d="M11 18h2" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
	</svg>`,
	navLabel:    t('nav.host'),
	navLabelKey: 'nav.host',
	mount,
	onShow,
});
