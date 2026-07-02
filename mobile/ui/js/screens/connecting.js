/**
 * screens/connecting.js — Bağlanıyor (connecting) full-screen overlay
 *
 * W2-connecting lane — Wave 2 implementation.
 *
 * Exported API (consumed by W2-shell-glue / session.js):
 *   start(target, mode, slot)  — show the overlay; begins listening to conn-phase
 *   cancel()                   — programmatic cancel (e.g. on play-ended before done)
 *
 * Behaviour:
 *   - Full-screen overlay mounted into #mount-connecting (added by W2-shell-glue)
 *   - Listens to the Tauri 'conn-phase' event emitted by client.rs
 *   - Phase progression:
 *       reaching   → step 1 active (Relay'e bağlanılıyor)
 *       transport  → step 2 done (P2P/Relay bağlantı kuruldu)
 *       auth       → step 3 active (Yetkilendirme)
 *       awaiting   → step 3 sub-label (Host onayı bekleniyor)
 *       preparing  → step 4 active (Akış hazırlanıyor)
 *   - After 12 s of no phase advancement from 'reaching', shows a slow-host hint
 *   - Cancel button → invoke('end_session', { slot }) + hide overlay
 *   - Indigo (remote) / cyan (game) themed via --brand token (body[data-mode])
 *   - Hides automatically when session.js receives 'play-firstframe' or 'play-ended'
 *     (session.js calls cancel() on those events)
 *   - Does NOT register a bottom-nav screen — it is an overlay, not a tab
 *
 * Contract §2.6 events consumed:
 *   conn-phase { slot, phase, transport? }
 *     phase values: 'reaching' | 'transport' | 'auth' | 'awaiting' | 'preparing'
 *     transport:    'direct' | 'relay'  (present when phase === 'transport')
 *
 * Contract §2.5 commands:
 *   end_session { slot }
 */

import { invoke, listen } from '../tauri.js';
import { t }              from '../i18n.js';

// ── Constants ─────────────────────────────────────────────────────────────────

/** Milliseconds before we show the "slow host" hint. */
const SLOW_HINT_MS = 12_000;

// ── Internal state ────────────────────────────────────────────────────────────

let _overlay  = null;  // the overlay root <div>
let _unlisten = null;  // Tauri unlisten fn for conn-phase
let _slot     = 0;
let _slowTimer = null;
let _mounted  = false;

// ── DOM helpers ───────────────────────────────────────────────────────────────

/**
 * Build and inject the overlay HTML into #mount-connecting.
 * Safe to call multiple times — only injects once.
 */
function ensureOverlay() {
  if (_mounted) return;
  _mounted = true;

  const mount = document.getElementById('mount-connecting');
  if (!mount) {
    console.warn('[connecting] #mount-connecting not found — overlay cannot render');
    return;
  }

  // Create overlay root
  const el = document.createElement('div');
  el.id = 'connecting-overlay';
  el.setAttribute('role', 'dialog');
  el.setAttribute('aria-modal', 'true');
  el.setAttribute('aria-label', t('status.connecting'));
  el.innerHTML = _html();

  // Inject scoped styles
  const style = document.createElement('style');
  style.textContent = _css();
  el.appendChild(style);

  mount.appendChild(el);
  _overlay = el;

  // Wire cancel button
  el.querySelector('#conn-cancel')?.addEventListener('click', () => {
    cancel(true);
  });
}

/** Generate the overlay inner HTML. */
function _html() {
  return `
<div class="conn-backdrop"></div>
<div class="conn-body" role="status" aria-live="polite">

  <!-- Pulse-ring animation -->
  <div class="conn-ring-wrap" aria-hidden="true">
    <div class="conn-ring conn-ring-3"></div>
    <div class="conn-ring conn-ring-2"></div>
    <div class="conn-ring conn-ring-1"></div>
    <div class="conn-ring-core"></div>
  </div>

  <!-- Target info -->
  <div class="conn-target" id="conn-target-display">
    <span class="conn-target-id mono" id="conn-target-id"></span>
    <span class="conn-mode-badge" id="conn-mode-badge"></span>
  </div>

  <!-- Step list -->
  <ol class="conn-steps" aria-label="${t('status.connecting')}" id="conn-steps">
    <li class="conn-step" id="conn-step-1" data-state="pending">
      <span class="conn-step-icon" aria-hidden="true"></span>
      <span class="conn-step-text">${t('connecting.step1')}</span>
    </li>
    <li class="conn-step" id="conn-step-2" data-state="pending">
      <span class="conn-step-icon" aria-hidden="true"></span>
      <span class="conn-step-text" id="conn-step-2-text">${t('connecting.step2')}</span>
    </li>
    <li class="conn-step" id="conn-step-3" data-state="pending">
      <span class="conn-step-icon" aria-hidden="true"></span>
      <span class="conn-step-text" id="conn-step-3-text">${t('connecting.stepAuth')}</span>
    </li>
    <li class="conn-step" id="conn-step-4" data-state="pending">
      <span class="conn-step-icon" aria-hidden="true"></span>
      <span class="conn-step-text">${t('connecting.preparing')}</span>
    </li>
  </ol>

  <!-- Slow-host hint (visible only after 12 s) -->
  <p class="conn-slow-hint" id="conn-slow-hint" aria-live="assertive">
    ${t('connecting.slowHint')}
  </p>

  <!-- Cancel button — large touch target -->
  <button class="btn conn-cancel-btn" id="conn-cancel" type="button">
    ${t('connecting.cancel')}
  </button>
</div>
`;
}

/** Scoped CSS for the overlay (pure CSS, no token overrides — uses existing tokens). */
function _css() {
  return `
/* ── Connecting overlay ─────────────────────────────────────── */
#connecting-overlay {
  position: fixed;
  inset: 0;
  z-index: 100;
  display: none;          /* shown by .active class */
  align-items: center;
  justify-content: center;
  pointer-events: none;
}
#connecting-overlay.active {
  display: flex;
  pointer-events: auto;
}

/* Semi-transparent backdrop — lets any native surface bleed through faintly */
#connecting-overlay .conn-backdrop {
  position: absolute;
  inset: 0;
  background: oklch(0.08 0.015 268 / 0.92);
  backdrop-filter: blur(32px) saturate(1.3);
  -webkit-backdrop-filter: blur(32px) saturate(1.3);
}

/* Centred card body */
#connecting-overlay .conn-body {
  position: relative;
  z-index: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 0;
  padding: max(env(safe-area-inset-top, 20px), 20px) 32px
           max(env(safe-area-inset-bottom, 28px), 28px);
  width: 100%;
  max-width: 420px;
  text-align: center;
}

/* ── Pulse rings ──────────────────────────────────────────── */
.conn-ring-wrap {
  position: relative;
  width: 120px;
  height: 120px;
  margin-bottom: 28px;
  flex-shrink: 0;
}
.conn-ring,
.conn-ring-core {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  margin: auto;
}
.conn-ring {
  border: 2px solid var(--brand);
  animation: conn-pulse 2.4s var(--ease-out) infinite;
}
.conn-ring-1 { width: 64px;  height: 64px;  opacity: 0.75; animation-delay: 0s;    }
.conn-ring-2 { width: 88px;  height: 88px;  opacity: 0.45; animation-delay: 0.55s; }
.conn-ring-3 { width: 112px; height: 112px; opacity: 0.22; animation-delay: 1.1s;  }
.conn-ring-core {
  width: 40px; height: 40px;
  background: var(--brand);
  box-shadow: 0 0 24px 6px var(--brand-ring);
  animation: conn-core-beat 2.4s var(--ease-out) infinite;
}

@keyframes conn-pulse {
  0%   { transform: scale(1);    opacity: var(--ring-base-opacity, 0.75); }
  70%  { transform: scale(1.25); opacity: 0; }
  100% { transform: scale(1);    opacity: 0; }
}
@keyframes conn-core-beat {
  0%,100% { transform: scale(1);   }
  50%      { transform: scale(0.9); }
}

/* ── Target display ──────────────────────────────────────── */
.conn-target {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  margin-bottom: 28px;
}
.conn-target-id {
  font-family: var(--font-mono);
  font-size: 28px;
  font-weight: 700;
  letter-spacing: 0.1em;
  color: oklch(0.97 0 0);
}
.conn-mode-badge {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--brand);
  background: oklch(1 0 0 / 0.08);
  border: 1px solid oklch(1 0 0 / 0.15);
  border-radius: var(--r-pill);
  padding: 4px 12px;
}

/* ── Step list ───────────────────────────────────────────── */
.conn-steps {
  list-style: none;
  padding: 0;
  margin: 0 0 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  width: 100%;
  max-width: 300px;
  text-align: left;
}
.conn-step {
  display: flex;
  align-items: center;
  gap: 12px;
  color: oklch(0.97 0 0 / 0.4);
  font-size: 14px;
  font-weight: 500;
  transition: color 0.35s var(--ease-out);
}
/* State: active */
.conn-step[data-state='active'] {
  color: oklch(0.97 0 0 / 0.9);
}
/* State: done */
.conn-step[data-state='done'] {
  color: oklch(0.97 0 0 / 0.6);
}

/* Step icon (circle) */
.conn-step-icon {
  flex-shrink: 0;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 2px solid currentColor;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.3s, border-color 0.3s;
  position: relative;
}
/* Active: pulsing brand circle */
.conn-step[data-state='active'] .conn-step-icon {
  border-color: var(--brand);
  background: var(--brand-ring);
}
.conn-step[data-state='active'] .conn-step-icon::after {
  content: '';
  width: 8px; height: 8px;
  border-radius: 50%;
  background: var(--brand);
  animation: conn-dot-blink 1.2s var(--ease-out) infinite;
}
@keyframes conn-dot-blink {
  0%,100% { opacity: 1; }
  50%      { opacity: 0.3; }
}
/* Done: checkmark via CSS border trick */
.conn-step[data-state='done'] .conn-step-icon {
  border-color: var(--ok);
  background: oklch(0.63 0.15 158 / 0.2);
}
.conn-step[data-state='done'] .conn-step-icon::after {
  content: '';
  width: 5px; height: 9px;
  border: 2px solid var(--ok);
  border-top: 0;
  border-left: 0;
  transform: rotate(42deg) translateY(-1px);
}

/* ── Slow-host hint ──────────────────────────────────────── */
.conn-slow-hint {
  max-width: 280px;
  font-size: 12.5px;
  color: oklch(0.97 0 0 / 0.5);
  line-height: 1.5;
  margin: 0 0 24px;
  opacity: 0;
  transform: translateY(6px);
  transition: opacity 0.4s var(--ease-out), transform 0.4s var(--ease-out);
  pointer-events: none;
}
.conn-slow-hint.visible {
  opacity: 1;
  transform: none;
  pointer-events: auto;
}

/* ── Cancel button ───────────────────────────────────────── */
.conn-cancel-btn {
  min-width: 180px;
  min-height: 52px;
  padding: 14px 32px;
  border-radius: var(--r-pill);
  background: oklch(1 0 0 / 0.08);
  border: 1px solid oklch(1 0 0 / 0.2);
  color: oklch(0.97 0 0);
  font-size: 16px;
  font-weight: 600;
  font-family: var(--font-sans);
  cursor: pointer;
  transition: background 0.2s, transform 0.15s var(--ease-out), border-color 0.2s;
  margin-top: 4px;
}
.conn-cancel-btn:active { transform: scale(0.96); background: oklch(1 0 0 / 0.15); }
`;
}

// ── Phase rendering ──────────────────────────────────────────────────────────

/**
 * Set step DOM state.
 * @param {number} n   — 1..4
 * @param {'pending'|'active'|'done'} state
 */
function _setStep(n, state) {
  const el = _overlay?.querySelector(`#conn-step-${n}`);
  if (el) el.dataset.state = state;
}

/**
 * Update the transport step text once we know Direct vs Relay.
 * @param {'direct'|'relay'|null} transport
 */
function _setTransportLabel(transport) {
  const el = _overlay?.querySelector('#conn-step-2-text');
  if (!el) return;
  if (transport === 'direct') {
    el.textContent = t('connecting.stepReachedP2p');
  } else if (transport === 'relay') {
    el.textContent = t('connecting.stepReachedRelay');
  } else {
    el.textContent = t('connecting.step2');
  }
}

/**
 * Apply a phase payload from the conn-phase event.
 * Phase progression is monotone — we never go backwards.
 * @param {{ slot: number, phase: string, transport?: string }} payload
 */
function _applyPhase(payload) {
  if (payload.slot !== _slot) return;  // not our slot

  const phase     = payload.phase;
  const transport = payload.transport || null;

  switch (phase) {
    case 'reaching':
      // Step 1 active, rest pending
      _setStep(1, 'active');
      _setStep(2, 'pending');
      _setStep(3, 'pending');
      _setStep(4, 'pending');
      break;

    case 'transport':
      // Transport established — step 1 done, step 2 done, show transport label
      _clearSlowHintTimer();  // clear slow-hint timer; we made progress
      _setTransportLabel(transport);
      _setStep(1, 'done');
      _setStep(2, 'done');
      _setStep(3, 'active');
      _setStep(4, 'pending');
      break;

    case 'auth':
      // Auth in progress — step 3 active
      _setStep(1, 'done');
      _setStep(2, 'done');
      _setStep(3, 'active');
      _setStep(4, 'pending');
      // Update step 3 label to auth
      {
        const el = _overlay?.querySelector('#conn-step-3-text');
        if (el) el.textContent = t('connecting.stepAuth');
      }
      break;

    case 'awaiting':
      // Waiting for host approval — step 3 still active, update label
      _setStep(1, 'done');
      _setStep(2, 'done');
      _setStep(3, 'active');
      _setStep(4, 'pending');
      {
        const el = _overlay?.querySelector('#conn-step-3-text');
        if (el) el.textContent = t('connecting.awaiting');
      }
      break;

    case 'preparing':
      // Stream being prepared — step 3 done, step 4 active
      _setStep(1, 'done');
      _setStep(2, 'done');
      _setStep(3, 'done');
      _setStep(4, 'active');
      {
        const el = _overlay?.querySelector('#conn-step-3-text');
        if (el) el.textContent = t('connecting.stepAuthDone');
      }
      break;

    default:
      console.warn('[connecting] unknown conn-phase:', phase);
  }
}

// ── Slow-hint timer ──────────────────────────────────────────────────────────

function _clearSlowHintTimer() {
  if (_slowTimer != null) {
    clearTimeout(_slowTimer);
    _slowTimer = null;
  }
}

function _startSlowHintTimer() {
  _clearSlowHintTimer();
  _slowTimer = setTimeout(() => {
    const hint = _overlay?.querySelector('#conn-slow-hint');
    if (hint) hint.classList.add('visible');
  }, SLOW_HINT_MS);
}

function _hideSlowHint() {
  _clearSlowHintTimer();
  const hint = _overlay?.querySelector('#conn-slow-hint');
  if (hint) hint.classList.remove('visible');
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Show the connecting overlay and start listening for conn-phase events.
 *
 * @param {string} target  — the ID or IP being connected to (display only)
 * @param {'remote'|'game'} mode  — determines the mode badge label
 * @param {number} [slot=0]  — session slot (matched against conn-phase payloads)
 */
export async function start(target, mode, slot = 0) {
  _slot = slot;

  ensureOverlay();
  if (!_overlay) return;

  // Reset all steps to pending
  _setStep(1, 'pending');
  _setStep(2, 'pending');
  _setStep(3, 'pending');
  _setStep(4, 'pending');

  // Reset step labels to defaults
  const step2Text = _overlay.querySelector('#conn-step-2-text');
  if (step2Text) step2Text.textContent = t('connecting.step2');
  const step3Text = _overlay.querySelector('#conn-step-3-text');
  if (step3Text) step3Text.textContent = t('connecting.stepAuth');

  // Set target display
  const targetEl = _overlay.querySelector('#conn-target-id');
  if (targetEl) {
    // Format as grouped ID if purely numeric 9 digits, else show as-is
    const digits = String(target).replace(/\D/g, '');
    targetEl.textContent = (digits.length === 9)
      ? digits.replace(/(\d{3})(?=\d)/g, '$1 ')
      : target;
  }

  // Set mode badge
  const badgeEl = _overlay.querySelector('#conn-mode-badge');
  if (badgeEl) {
    badgeEl.textContent = (mode === 'game')
      ? t('connecting.modeGame')
      : t('connecting.modeRemote');
  }

  // Hide slow hint
  _hideSlowHint();

  // Activate step 1 immediately (reaching phase)
  _setStep(1, 'active');

  // Start the slow-hint timer
  _startSlowHintTimer();

  // Subscribe to conn-phase (cleanup any prior subscription)
  if (_unlisten) {
    const prev = _unlisten;
    _unlisten = null;
    try { await prev(); } catch (_) {}
  }
  _unlisten = await listen('conn-phase', (payload) => {
    _applyPhase(payload);
  });

  // Show overlay
  _overlay.classList.add('active');
  _overlay.setAttribute('aria-label', t('status.connecting'));

  // Focus the cancel button for accessibility
  requestAnimationFrame(() => {
    _overlay?.querySelector('#conn-cancel')?.focus({ preventScroll: true });
  });
}

/**
 * Hide the connecting overlay and clean up listeners.
 *
 * @param {boolean} [doEndSession=false]
 *   When true (user tapped Cancel), invoke end_session before hiding.
 */
export async function cancel(doEndSession = false) {
  if (!_overlay) return;

  // Stop slow-hint timer
  _hideSlowHint();

  // Unlisten from conn-phase
  if (_unlisten) {
    const fn = _unlisten;
    _unlisten = null;
    try { await fn(); } catch (_) {}
  }

  // If triggered by Cancel button — call end_session first, best-effort
  if (doEndSession) {
    try {
      await invoke('end_session', { slot: _slot });
    } catch (e) {
      console.warn('[connecting] end_session error:', e);
    }
  }

  // Hide the overlay
  _overlay.classList.remove('active');
}
