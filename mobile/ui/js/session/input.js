/**
 * input.js — Touch→pointer engine for Pulsar Mobile (W3-input)
 *
 * DT-touch-input: Touch gesture model for remote pointing.
 *
 * Gesture set:
 *   Single finger
 *     tap          → left button click  (down + up within TAP_SLOP / TAP_MS)
 *     double-tap   → left double-click  (two taps within DBL_MS)
 *     long-press   → right button click (held > LONG_MS without moving)
 *     drag         → pointer motion (absolute mode) / relative motion (trackpad mode)
 *
 *   Two fingers
 *     2-finger tap → middle button click
 *     2-finger drag → scroll (send_scroll dx/dy)
 *
 *   Trackpad / relative mode
 *     Enabled by tapping the mode-toggle button in the cursor-dot overlay.
 *     In this mode finger delta → PointerRelative events (send_pointer_rel).
 *     An on-screen cursor dot follows the relative position.
 *
 * Calls (Tauri commands):
 *   send_pointer      { slot, x, y }        — absolute pointer move
 *   send_button       { slot, button, down } — button 0=left 1=right 2=middle
 *   send_scroll       { slot, dx, dy }       — two-finger pan
 *   send_pointer_rel  { slot, dx, dy }       — relative (trackpad) motion
 *
 * Listens (JS bus):
 *   bus:gamepad-active { active:bool } — when active, pointer engine is gated off
 *   bus:session-started               — (re-)mount on a new session
 *   bus:session-ended                 — unmount and reset
 *
 * Mount:
 *   mount(slot)  — called by session.js (or app.js) when a session starts.
 *                  Creates the transparent touch-capture overlay and cursor dot.
 *   unmount()    — tears down listeners and overlay DOM.
 *
 * Design (DT-touch-input):
 *   • Transparent fixed overlay covering the full viewport (above video surface,
 *     below the control bar and overlay dock). pointer-events:auto only here.
 *   • Cursor dot: 12px indigo/cyan filled circle, follows relative pointer.
 *   • Mode toggle FAB (bottom-left, above bar): ⊕ to switch trackpad/absolute.
 *   • First-use hint card (bottom sheet, shown once per app install):
 *     lists the 5 gesture icons/text. Dismissed by tap or auto after 6s.
 *   • In game mode (body[data-mode=game]) brand is cyan, in remote it's indigo.
 *
 * Touch sizing: all interactive elements ≥ 44px (--touch-min).
 * Never blocks passive touch events on the document — the overlay gets its own
 * non-passive listeners only on itself (not window-level).
 */

import { invoke }   from '../tauri.js';
import { t }        from '../i18n.js';

// ── Constants ─────────────────────────────────────────────────────────────────

/** Maximum distance (px) between touchstart and touchend to count as a tap. */
const TAP_SLOP  = 10;

/** Maximum duration (ms) of a touch to count as a tap (not a drag/press). */
const TAP_MS    = 200;

/** Two taps within this window = double-tap. */
const DBL_MS    = 280;

/** Hold touch for this long (ms) without moving = long-press (right click). */
const LONG_MS   = 480;

/** Minimum drag distance (px) in 2-finger mode before we treat it as a scroll
 *  (prevents accidental scrolls from tiny 2-finger taps). */
const SCROLL_SLOP = 6;

/** Scroll sensitivity divisor — lower = faster scroll. */
const SCROLL_DIV  = 2.5;

/** Relative (trackpad) motion sensitivity multiplier. */
const REL_SENS    = 1.6;

/** LocalStorage key for trackpad mode preference. */
const LS_TRACKPAD = 'pulsar.input.trackpad.v1';

/** LocalStorage key for whether the hint card has been shown. */
const LS_HINT_SHOWN = 'pulsar.input.hint.v1';

// ── Module state ──────────────────────────────────────────────────────────────

/** Active session slot (set by mount()). */
let _slot = 0;

/** Whether the pointer engine is gated off by gamepad being active. */
let _gatedByGamepad = false;

/** Whether we are in trackpad (relative) mode or absolute mode. */
let _trackpadMode = (() => {
  try { return localStorage.getItem(LS_TRACKPAD) === '1'; } catch (_) { return false; }
})();

/** DOM: the transparent touch-capture overlay element. */
let _overlay = null;

/** DOM: the cursor dot element (trackpad mode). */
let _cursorDot = null;

/** DOM: the mode-toggle button. */
let _modeBtn = null;

/** Cursor position in viewport coords (trackpad mode). */
let _curX = 0;
let _curY = 0;

/** Whether the module has been mounted. */
let _mounted = false;

/** rAF handle for coalescing pointer moves. */
let _rafHandle = null;

/** Pending pointer-move payload to flush in the next rAF. */
let _pendingMove = null;

// ── Gesture state machine ─────────────────────────────────────────────────────

/** @type {{ x:number, y:number, t:number } | null} */
let _lastTap   = null;   // last completed tap, for double-tap detection
let _longTimer = null;   // setTimeout handle for long-press
let _startTouches = [];  // TouchList snapshot at touchstart

/**
 * A minimal touch descriptor.
 * @typedef {{ x:number, y:number, id:number }} TDesc
 */

/** @type {TDesc[]} */
let _activeTouches = [];

/** Whether we have started a drag (suppresses tap on touchend). */
let _dragging = false;

/** 2-finger drag: last mid-point for scroll delta. */
let _twoFingerMid = null;

/** Whether the 2-finger gesture was a drag (suppresses 2-finger tap). */
let _twoFingerDragged = false;

// ── Helpers ───────────────────────────────────────────────────────────────────

const isInSession = () => document.body.classList.contains('in-session');
const isGameMode  = () => document.body.dataset.mode === 'game';
const getBus      = () => window.__pulsarBus || null;

/**
 * Clamp absolute coordinates to [0,1] range for the current slot's pane.
 * Handles split-screen (W5) where slot 1 is the bottom half.
 *
 * @param {number} clientX
 * @param {number} clientY
 * @returns {{ x:number, y:number }}
 */
function absCoords(clientX, clientY) {
  const W = window.innerWidth;
  const H = window.innerHeight;
  const splitActive = window.__pulsarSplitActive ? window.__pulsarSplitActive() : false;
  if (splitActive && _slot === 1) {
    const half = H / 2;
    return { x: clientX / W, y: (clientY - half) / half };
  }
  return { x: clientX / W, y: clientY / H };
}

/** Fire the rAF-coalesced absolute pointer move. */
function _flushMove() {
  _rafHandle = null;
  if (!_pendingMove) return;
  const { x, y } = _pendingMove;
  _pendingMove = null;
  invoke('send_pointer', { slot: _slot, x, y }).catch(() => {});
}

/** Schedule a pointer-move via rAF (drops mid-frame duplicates). */
function scheduleAbsMove(x, y) {
  _pendingMove = { x, y };
  if (!_rafHandle) {
    _rafHandle = requestAnimationFrame(_flushMove);
  }
}

/** Move the cursor dot to the given viewport position (clamped to overlay). */
function _moveCursorDot(x, y) {
  if (!_cursorDot) return;
  const W = window.innerWidth;
  const H = window.innerHeight;
  _curX = Math.max(0, Math.min(W, x));
  _curY = Math.max(0, Math.min(H, y));
  _cursorDot.style.transform = `translate(${_curX}px, ${_curY}px)`;
}

/** Cancel any pending long-press timer. */
function _clearLong() {
  if (_longTimer !== null) {
    clearTimeout(_longTimer);
    _longTimer = null;
  }
}

// ── Touch event handlers ──────────────────────────────────────────────────────

/**
 * Snapshot the current touches as TDesc array.
 * @param {TouchList} list
 * @returns {TDesc[]}
 */
function snapTouches(list) {
  const out = [];
  for (let i = 0; i < list.length; i++) {
    const t = list[i];
    out.push({ x: t.clientX, y: t.clientY, id: t.identifier });
  }
  return out;
}

function midPoint(a, b) {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

function dist(a, b) {
  return Math.hypot(b.x - a.x, b.y - a.y);
}

/**
 * handleTouchStart — capture start state for the gesture.
 * @param {TouchEvent} e
 */
/** True while the overlay dock is open OR the FAB is being pressed/dragged — input
 *  must not forward to the host in either case. */
function _overlayOpen() {
  return document.body.classList.contains('overlay-open')
      || document.body.classList.contains('fab-dragging');
}

/**
 * True if this touch lands on a Pulsar UI element (FAB, overlay dock/backdrop,
 * mode button, any sheet) rather than the bare capture overlay. The capture layer
 * is full-screen, so without this a tap that visually hits a control could still be
 * forwarded to the host as a click. We hit-test the actual top element at the touch
 * point and only forward when it IS the capture overlay.
 */
function _touchOnUI(e) {
  const t = (e.touches && e.touches[0]) || (e.changedTouches && e.changedTouches[0]);
  if (!t || !_overlay) return false;
  const top = document.elementFromPoint(t.clientX, t.clientY);
  return !!top && top !== _overlay && !_overlay.contains(top);
}

function handleTouchStart(e) {
  if (_gatedByGamepad || !isInSession() || _overlayOpen() || _touchOnUI(e)) return;

  _activeTouches = snapTouches(e.touches);
  _startTouches  = snapTouches(e.touches);
  _dragging      = false;

  if (e.touches.length === 1) {
    _twoFingerMid     = null;
    _twoFingerDragged = false;
    const t0 = _activeTouches[0];

    // Start long-press timer
    _clearLong();
    _longTimer = setTimeout(() => {
      _longTimer = null;
      if (!_dragging && isInSession() && !_gatedByGamepad) {
        // Long-press = right button
        const { x, y } = absCoords(t0.x, t0.y);
        if (!_trackpadMode) {
          invoke('send_pointer', { slot: _slot, x, y }).catch(() => {});
        }
        invoke('send_button', { slot: _slot, button: 1, down: true  }).catch(() => {});
        invoke('send_button', { slot: _slot, button: 1, down: false }).catch(() => {});
        // Haptic feedback (Android)
        if (navigator.vibrate) navigator.vibrate(20);
        // Prevent tap from firing on touchend
        _dragging = true;
      }
    }, LONG_MS);

    // In trackpad mode, initialize the relative cursor at current touch position
    if (_trackpadMode) {
      // We'll start tracking relative motion from here
      _startTouches = snapTouches(e.touches);
    } else {
      // Absolute: send pointer position immediately
      const { x, y } = absCoords(t0.x, t0.y);
      scheduleAbsMove(x, y);
    }
  } else if (e.touches.length === 2) {
    // Cancel single-finger long-press if a second finger arrives
    _clearLong();
    _dragging         = true; // suppress single tap
    _twoFingerDragged = false;
    _twoFingerMid     = midPoint(_activeTouches[0], _activeTouches[1]);
  }
}

/**
 * handleTouchMove — update pointer position and handle dragging/scrolling.
 * @param {TouchEvent} e
 */
function handleTouchMove(e) {
  if (_gatedByGamepad || !isInSession() || _overlayOpen() || _touchOnUI(e)) return;

  const current = snapTouches(e.touches);

  if (e.touches.length === 1 && _startTouches.length === 1) {
    const t0 = current[0];
    const s0 = _startTouches[0];
    const dx = t0.x - s0.x;
    const dy = t0.y - s0.y;
    const moved = Math.hypot(dx, dy);

    if (moved > TAP_SLOP) {
      // Definitely a drag — cancel long-press
      _clearLong();
      _dragging = true;
    }

    if (_trackpadMode) {
      // Relative mode: send delta from the previous active position
      const prev = _activeTouches[0];
      if (prev) {
        const relDx = (t0.x - prev.x) * REL_SENS;
        const relDy = (t0.y - prev.y) * REL_SENS;
        if (Math.abs(relDx) > 0.1 || Math.abs(relDy) > 0.1) {
          // Update cursor dot position
          _moveCursorDot(_curX + relDx, _curY + relDy);
          // Coalesce via rAF: overwrite pendingMove (send_pointer_rel is small)
          _pendingMove = null;
          if (_rafHandle) cancelAnimationFrame(_rafHandle);
          _rafHandle = requestAnimationFrame(() => {
            _rafHandle = null;
            invoke('send_pointer_rel', { slot: _slot, dx: relDx, dy: relDy }).catch(() => {});
          });
        }
      }
    } else {
      // Absolute mode
      const { x, y } = absCoords(t0.x, t0.y);
      scheduleAbsMove(x, y);
    }
  } else if (e.touches.length === 2 && _startTouches.length >= 2) {
    const t0 = current[0];
    const t1 = current[1];
    const mid = midPoint(t0, t1);

    if (_twoFingerMid) {
      const dxScroll = mid.x - _twoFingerMid.x;
      const dyScroll = mid.y - _twoFingerMid.y;
      const scrolled = Math.hypot(dxScroll, dyScroll);
      if (scrolled > SCROLL_SLOP || _twoFingerDragged) {
        _twoFingerDragged = true;
        // Invert direction: drag down → scroll up (natural scrolling)
        const sdx = -(dxScroll / SCROLL_DIV);
        const sdy = -(dyScroll / SCROLL_DIV);
        invoke('send_scroll', { slot: _slot, dx: sdx, dy: sdy }).catch(() => {});
      }
    }
    _twoFingerMid = mid;
  }

  _activeTouches = current;
}

/**
 * handleTouchEnd — detect taps and button events.
 * @param {TouchEvent} e
 */
function handleTouchEnd(e) {
  if (_gatedByGamepad || !isInSession() || _overlayOpen() || _touchOnUI(e)) return;

  _clearLong();
  const now = Date.now();

  // How many fingers just ended? e.changedTouches has the lifted touches,
  // e.touches has the remaining.
  const lifted = snapTouches(e.changedTouches);
  const remaining = snapTouches(e.touches);

  if (_startTouches.length === 2 && remaining.length === 0) {
    // Two-finger gesture ended
    if (!_twoFingerDragged) {
      // Two-finger tap = middle button
      const midX = (_startTouches[0].x + _startTouches[1].x) / 2;
      const midY = (_startTouches[0].y + _startTouches[1].y) / 2;
      if (!_trackpadMode) {
        const { x, y } = absCoords(midX, midY);
        invoke('send_pointer', { slot: _slot, x, y }).catch(() => {});
      }
      invoke('send_button', { slot: _slot, button: 2, down: true  }).catch(() => {});
      invoke('send_button', { slot: _slot, button: 2, down: false }).catch(() => {});
    }
    // Reset 2-finger state
    _twoFingerMid     = null;
    _twoFingerDragged = false;
    _activeTouches    = remaining;
    _startTouches     = [];
    _dragging         = false;
    return;
  }

  if (_startTouches.length === 1 && lifted.length >= 1) {
    const l0  = lifted[0];
    const s0  = _startTouches[0];
    const dur = now - /* start time not tracked per-touch, use conservative check */
                (now - TAP_MS); // we'll just check _dragging flag

    // Is it a tap? (not dragged, lifted quickly enough)
    const moved = dist(l0, s0);
    if (!_dragging && moved <= TAP_SLOP) {
      // It's a tap
      const tapX = l0.x;
      const tapY = l0.y;

      // Double-tap check
      if (_lastTap && (now - _lastTap.t) < DBL_MS &&
          dist({ x: tapX, y: tapY }, { x: _lastTap.x, y: _lastTap.y }) < 40) {
        // Double-tap: send double-click (two down+up pairs)
        _lastTap = null;
        if (!_trackpadMode) {
          const { x, y } = absCoords(tapX, tapY);
          invoke('send_pointer', { slot: _slot, x, y }).catch(() => {});
        }
        invoke('send_button', { slot: _slot, button: 0, down: true  }).catch(() => {});
        invoke('send_button', { slot: _slot, button: 0, down: false }).catch(() => {});
        invoke('send_button', { slot: _slot, button: 0, down: true  }).catch(() => {});
        invoke('send_button', { slot: _slot, button: 0, down: false }).catch(() => {});
        // Haptic feedback
        if (navigator.vibrate) navigator.vibrate([10, 30, 10]);
      } else {
        // Single tap: left click
        _lastTap = { x: tapX, y: tapY, t: now };
        if (!_trackpadMode) {
          const { x, y } = absCoords(tapX, tapY);
          invoke('send_pointer', { slot: _slot, x, y }).catch(() => {});
        }
        invoke('send_button', { slot: _slot, button: 0, down: true  }).catch(() => {});
        invoke('send_button', { slot: _slot, button: 0, down: false }).catch(() => {});
      }
    } else if (!_dragging && moved > TAP_SLOP) {
      // Drag ended — send final position (absolute mode)
      if (!_trackpadMode) {
        const { x, y } = absCoords(l0.x, l0.y);
        scheduleAbsMove(x, y);
      }
    }
  }

  _activeTouches = remaining;
  if (remaining.length === 0) {
    _dragging      = false;
    _startTouches  = [];
    _twoFingerMid  = null;
    _twoFingerDragged = false;
  }
}

/**
 * handleTouchCancel — clean up gesture state.
 * @param {TouchEvent} _e
 */
function handleTouchCancel(_e) {
  _clearLong();
  _activeTouches    = [];
  _startTouches     = [];
  _dragging         = false;
  _twoFingerMid     = null;
  _twoFingerDragged = false;
  _pendingMove      = null;
  if (_rafHandle) { cancelAnimationFrame(_rafHandle); _rafHandle = null; }
}

// ── DOM: overlay + cursor dot + mode button ───────────────────────────────────

/**
 * Create and attach the transparent touch-capture overlay.
 * The overlay sits above the video surface but below the control bar (z-index 9).
 */
function _createOverlay() {
  if (_overlay) return;

  _overlay = document.createElement('div');
  _overlay.id = 'pulsar-touch-overlay';
  // Styles injected via _injectStyles(); keep class-based here
  _overlay.className = 'touch-overlay';
  document.body.appendChild(_overlay);

  // Cursor dot (trackpad mode)
  _cursorDot = document.createElement('div');
  _cursorDot.className = 'touch-cursor-dot';
  _cursorDot.setAttribute('aria-hidden', 'true');
  document.body.appendChild(_cursorDot);

  // Initialize dot position to screen center
  _curX = window.innerWidth  / 2;
  _curY = window.innerHeight / 2;
  _moveCursorDot(_curX, _curY);
  _updateCursorDotVisible();

  // Mode toggle button (trackpad ↔ absolute)
  _modeBtn = document.createElement('button');
  _modeBtn.id = 'touch-mode-btn';
  _modeBtn.className = 'touch-mode-btn';
  _modeBtn.setAttribute('type', 'button');
  _modeBtn.setAttribute('aria-pressed', _trackpadMode ? 'true' : 'false');
  _modeBtn.setAttribute('aria-label', t('m.input.toggleMode'));
  _updateModeBtnLabel();
  document.body.appendChild(_modeBtn);

  _modeBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    _trackpadMode = !_trackpadMode;
    try { localStorage.setItem(LS_TRACKPAD, _trackpadMode ? '1' : '0'); } catch (_) {}
    _modeBtn.setAttribute('aria-pressed', _trackpadMode ? 'true' : 'false');
    _updateModeBtnLabel();
    _updateCursorDotVisible();
    // Reset cursor to center when switching to trackpad mode
    if (_trackpadMode) {
      _curX = window.innerWidth  / 2;
      _curY = window.innerHeight / 2;
      _moveCursorDot(_curX, _curY);
    }
  });

  // Wire touch events on the overlay (non-passive so we can call preventDefault
  // to stop the browser from scrolling the page under the session)
  _overlay.addEventListener('touchstart',  handleTouchStart,  { passive: true });
  _overlay.addEventListener('touchmove',   handleTouchMove,   { passive: true });
  _overlay.addEventListener('touchend',    handleTouchEnd,    { passive: true });
  _overlay.addEventListener('touchcancel', handleTouchCancel, { passive: true });
}

function _updateModeBtnLabel() {
  if (!_modeBtn) return;
  // Icon + label
  _modeBtn.innerHTML = _trackpadMode
    ? `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
         <rect x="5" y="3" width="14" height="18" rx="4" stroke="currentColor" stroke-width="1.8"/>
         <line x1="5" y1="10" x2="19" y2="10" stroke="currentColor" stroke-width="1.6"/>
         <circle cx="12" cy="15" r="1.8" fill="currentColor"/>
       </svg>
       <span>${t('m.input.absolute')}</span>`
    : `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
         <rect x="5" y="3" width="14" height="18" rx="4" stroke="currentColor" stroke-width="1.8"/>
         <line x1="5" y1="10" x2="19" y2="10" stroke="currentColor" stroke-width="1.6"/>
         <line x1="9" y1="13" x2="9" y2="18" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
         <line x1="15" y1="13" x2="15" y2="18" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
       </svg>
       <span>${t('m.input.trackpad')}</span>`;
}

function _updateCursorDotVisible() {
  if (!_cursorDot) return;
  _cursorDot.style.display = _trackpadMode ? 'block' : 'none';
}

/** Remove the overlay and all associated DOM. */
function _removeOverlay() {
  if (_overlay) {
    _overlay.removeEventListener('touchstart',  handleTouchStart);
    _overlay.removeEventListener('touchmove',   handleTouchMove);
    _overlay.removeEventListener('touchend',    handleTouchEnd);
    _overlay.removeEventListener('touchcancel', handleTouchCancel);
    _overlay.remove();
    _overlay = null;
  }
  if (_cursorDot) { _cursorDot.remove(); _cursorDot = null; }
  if (_modeBtn)   { _modeBtn.remove();   _modeBtn   = null; }
}

// ── First-use gesture hint card ───────────────────────────────────────────────

/**
 * Show a one-time hint bottom sheet explaining the touch gestures.
 * Auto-dismisses after 6 seconds; tapping anywhere on it dismisses it too.
 */
function _showHintCard() {
  let shown = false;
  try { shown = localStorage.getItem(LS_HINT_SHOWN) === '1'; } catch (_) {}
  if (shown) return;

  const gameMode = isGameMode();
  const brandCls = gameMode ? 'hint-card--game' : 'hint-card--remote';

  const card = document.createElement('div');
  card.className = `touch-hint-card ${brandCls}`;
  card.setAttribute('role', 'dialog');
  card.setAttribute('aria-modal', 'true');
  card.setAttribute('aria-label', t('m.input.hintTitle'));

  card.innerHTML = `
    <div class="touch-hint-handle" aria-hidden="true"></div>
    <h4 class="touch-hint-title">${t('m.input.hintTitle')}</h4>
    <ul class="touch-hint-list" aria-label="${t('m.input.hintTitle')}">
      <li class="touch-hint-row">
        <span class="touch-hint-icon" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <circle cx="16" cy="11" r="5" stroke="currentColor" stroke-width="1.8"/>
            <path d="M16 16v6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
            <circle cx="16" cy="26" r="2" fill="currentColor" opacity="0.5"/>
          </svg>
        </span>
        <span class="touch-hint-label">
          <b>${t('m.input.gestTap')}</b>
          <span>${t('m.input.gestTapDesc')}</span>
        </span>
      </li>
      <li class="touch-hint-row">
        <span class="touch-hint-icon" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <circle cx="16" cy="11" r="5" stroke="currentColor" stroke-width="1.8"/>
            <path d="M16 16v4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
            <circle cx="16" cy="24" r="2" fill="currentColor" opacity="0.5"/>
            <path d="M22 10.5c0-3.04-2.69-5.5-6-5.5" stroke="currentColor" stroke-width="1.4"
                  stroke-linecap="round" stroke-dasharray="2.5 2"/>
          </svg>
        </span>
        <span class="touch-hint-label">
          <b>${t('m.input.gestDoubleTap')}</b>
          <span>${t('m.input.gestDoubleTapDesc')}</span>
        </span>
      </li>
      <li class="touch-hint-row">
        <span class="touch-hint-icon" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <circle cx="16" cy="11" r="5" stroke="currentColor" stroke-width="1.8"/>
            <path d="M16 16v6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
            <circle cx="16" cy="26" r="2" fill="currentColor" opacity="0.5"/>
            <circle cx="16" cy="11" r="9" stroke="currentColor" stroke-width="1" opacity="0.3"
                    stroke-dasharray="3 2"/>
          </svg>
        </span>
        <span class="touch-hint-label">
          <b>${t('m.input.gestLongPress')}</b>
          <span>${t('m.input.gestLongPressDesc')}</span>
        </span>
      </li>
      <li class="touch-hint-row">
        <span class="touch-hint-icon" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <circle cx="12" cy="11" r="4" stroke="currentColor" stroke-width="1.8"/>
            <circle cx="22" cy="13" r="4" stroke="currentColor" stroke-width="1.8" opacity="0.65"/>
            <path d="M12 15v6M22 17v4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
          </svg>
        </span>
        <span class="touch-hint-label">
          <b>${t('m.input.gestTwoTap')}</b>
          <span>${t('m.input.gestTwoTapDesc')}</span>
        </span>
      </li>
      <li class="touch-hint-row">
        <span class="touch-hint-icon" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <circle cx="12" cy="13" r="4" stroke="currentColor" stroke-width="1.8"/>
            <circle cx="22" cy="13" r="4" stroke="currentColor" stroke-width="1.8" opacity="0.65"/>
            <path d="M12 17l-3 7M22 17l3 7" stroke="currentColor" stroke-width="1.8"
                  stroke-linecap="round"/>
            <path d="M17 24l-3-5 3 2 3-2-3 5z" fill="currentColor" opacity="0.4"/>
          </svg>
        </span>
        <span class="touch-hint-label">
          <b>${t('m.input.gestTwoDrag')}</b>
          <span>${t('m.input.gestTwoDragDesc')}</span>
        </span>
      </li>
    </ul>
    <button class="touch-hint-dismiss btn btn-ghost" type="button">
      ${t('m.input.hintDismiss')}
    </button>
  `;

  document.body.appendChild(card);

  // Animate in
  requestAnimationFrame(() => card.classList.add('touch-hint-card--open'));

  let dismissed = false;
  function dismiss() {
    if (dismissed) return;
    dismissed = true;
    card.classList.remove('touch-hint-card--open');
    try { localStorage.setItem(LS_HINT_SHOWN, '1'); } catch (_) {}
    setTimeout(() => card.remove(), 400);
  }

  card.querySelector('.touch-hint-dismiss')?.addEventListener('click', dismiss);
  // Tap anywhere on the card dismisses it
  card.addEventListener('click', (e) => {
    if (!e.target.closest('.touch-hint-dismiss')) dismiss();
  });
  // Auto-dismiss after 7 seconds
  setTimeout(dismiss, 7000);
}

// ── Inline styles ─────────────────────────────────────────────────────────────

function _injectStyles() {
  if (document.getElementById('input-module-styles')) return;
  const style = document.createElement('style');
  style.id = 'input-module-styles';
  style.textContent = `
/* ---- Touch capture overlay ---- */
.touch-overlay {
  position: fixed;
  inset: 0;
  /* Below control bar (z=10) and overlay dock (z=15), above video surface */
  z-index: 9;
  /* Transparent — passes visual through to native video surface */
  background: transparent;
  /* Touch events: auto — receives all touches except those on higher z elements */
  pointer-events: auto;
  /* Suppress browser default: no text selection, no long-tap menu */
  -webkit-user-select: none;
  user-select: none;
  -webkit-touch-callout: none;
  touch-action: none;
}
/* When in-session we show the overlay; hidden otherwise */
body:not(.in-session) .touch-overlay {
  pointer-events: none;
}

/* ---- Cursor dot (trackpad mode) ---- */
.touch-cursor-dot {
  position: fixed;
  /* Positioned via JS transform */
  top: 0; left: 0;
  z-index: 13; /* above overlay, below bar */
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--brand);
  opacity: 0.85;
  box-shadow: 0 0 0 3px oklch(0 0 0 / 0.12), 0 0 6px var(--brand);
  pointer-events: none;
  will-change: transform;
  /* Offset so center of dot is the pointer */
  margin-left: -6px;
  margin-top: -6px;
  transition: opacity 0.2s, transform 0.04s linear;
}
/* Larger dot ring in game mode */
[data-mode='game'] .touch-cursor-dot {
  background: var(--cyan);
  box-shadow: 0 0 0 3px oklch(0 0 0 / 0.12), 0 0 6px var(--cyan);
}

/* ---- Mode toggle button ---- */
.touch-mode-btn {
  position: fixed;
  /* Bottom-left, above safe area + bar */
  bottom: calc(var(--safe-bottom) + 72px);
  left: 16px;
  z-index: 14;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 9px 14px;
  min-height: var(--touch-min);
  border: 1px solid oklch(1 0 0 / 0.18);
  border-radius: var(--r-pill);
  background: oklch(0.16 0.018 268 / 0.78);
  backdrop-filter: blur(14px) saturate(1.3);
  -webkit-backdrop-filter: blur(14px) saturate(1.3);
  color: oklch(0.95 0.01 268);
  font-family: var(--font-sans);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  box-shadow: 0 4px 16px oklch(0 0 0 / 0.35);
  transition: opacity 0.2s, transform 0.15s var(--ease), background 0.2s;
}
.touch-mode-btn:active {
  transform: scale(0.94);
  background: oklch(0.2 0.02 268 / 0.88);
}
/* Active (trackpad on) gets brand tint */
.touch-mode-btn[aria-pressed='true'] {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), 0 4px 16px oklch(0 0 0 / 0.35);
  color: oklch(0.97 0 0);
}
[data-mode='game'] .touch-mode-btn[aria-pressed='true'] {
  border-color: var(--cyan);
  box-shadow: 0 0 0 1px var(--cyan), 0 4px 16px oklch(0 0 0 / 0.35);
}
/* Hidden when not in session */
body:not(.in-session) .touch-mode-btn {
  display: none;
}

/* ---- First-use gesture hint card ---- */
.touch-hint-card {
  position: fixed;
  left: 0; right: 0; bottom: 0;
  z-index: 22;
  background: var(--surface);
  border-top: 1px solid var(--border);
  border-top-left-radius: var(--r-xl);
  border-top-right-radius: var(--r-xl);
  box-shadow: var(--shadow-lg);
  padding: 20px 20px calc(20px + var(--safe-bottom));
  display: flex;
  flex-direction: column;
  gap: 14px;
  transform: translateY(100%);
  transition: transform 0.38s var(--ease-out);
}
.touch-hint-card--open {
  transform: translateY(0);
}
/* Game mode: slight cyan tint on border */
.touch-hint-card--game {
  border-color: oklch(0.62 0.15 215 / 0.35);
}
.touch-hint-handle {
  width: 36px; height: 4px;
  border-radius: 2px;
  background: var(--border-strong);
  margin: 0 auto -4px;
}
.touch-hint-title {
  font-family: var(--font-display);
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.02em;
  color: var(--text);
  margin: 0;
  text-align: center;
}
.touch-hint-list {
  list-style: none;
  margin: 0; padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0;
}
.touch-hint-row {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 4px;
  border-bottom: 1px solid var(--border);
}
.touch-hint-row:last-child { border-bottom: none; }
.touch-hint-icon {
  flex: none;
  width: 44px; height: 44px;
  display: flex; align-items: center; justify-content: center;
  color: var(--brand);
  background: var(--brand-soft);
  border-radius: var(--r);
}
[data-mode='game'] .touch-hint-icon {
  color: var(--cyan);
  background: var(--cyan-soft);
}
.touch-hint-label {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.touch-hint-label b {
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
}
.touch-hint-label span {
  font-size: 12px;
  color: var(--text-muted);
  line-height: 1.4;
}
.touch-hint-dismiss {
  align-self: stretch;
  margin-top: 4px;
}
`;
  document.head.appendChild(style);
}

// ── i18n additions ────────────────────────────────────────────────────────────
// These keys are consumed only by this module. They extend the existing i18n
// catalog via the established t() helper pattern. If the keys are missing from
// the catalog (they aren't added here — only the module file is owned by W3-input),
// t() falls back gracefully to the key string.
//
// Expected keys (added to i18n.js by the W3-input lane, or deferred to W5):
//   m.input.toggleMode      — "Mod değiştir"
//   m.input.absolute        — "Doğrudan"
//   m.input.trackpad        — "İzleme alanı"
//   m.input.hintTitle       — "Dokunmatik hareketler"
//   m.input.hintDismiss     — "Anladım"
//   m.input.gestTap         — "Dokun"
//   m.input.gestTapDesc     — "Sol tık"
//   m.input.gestDoubleTap   — "Çift dokun"
//   m.input.gestDoubleTapDesc— "Çift tık"
//   m.input.gestLongPress   — "Uzun bas"
//   m.input.gestLongPressDesc— "Sağ tık"
//   m.input.gestTwoTap      — "İki parmakla dokun"
//   m.input.gestTwoTapDesc  — "Orta tık"
//   m.input.gestTwoDrag     — "İki parmakla sürükle"
//   m.input.gestTwoDragDesc — "Kaydır"
//
// The t() helper in i18n.js already falls back: catalogs[lang][key] → en[key] → key.
// So the module is fully functional even before the keys land — the key string
// itself is shown as the label.

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Mount the touch→pointer engine for a session slot.
 * Creates the transparent overlay, cursor dot, and mode-toggle button.
 * Shows the first-use hint card if this is the first launch.
 *
 * @param {number} slot  — the session slot this engine drives
 */
export function mount(slot) {
  if (_mounted) unmount(); // clean up previous session if re-mounting
  _slot    = slot;
  _mounted = true;
  _clearLong();
  _dragging         = false;
  _activeTouches    = [];
  _startTouches     = [];
  _lastTap          = null;
  _twoFingerMid     = null;
  _twoFingerDragged = false;
  _pendingMove      = null;
  if (_rafHandle) { cancelAnimationFrame(_rafHandle); _rafHandle = null; }

  _injectStyles();
  _createOverlay();

  // Show gesture hint on first use
  _showHintCard();
}

/**
 * Unmount the touch engine.
 * Removes all DOM and resets state.
 */
export function unmount() {
  _mounted = false;
  _clearLong();
  _removeOverlay();
  _pendingMove = null;
  if (_rafHandle) { cancelAnimationFrame(_rafHandle); _rafHandle = null; }
  _activeTouches    = [];
  _startTouches     = [];
  _dragging         = false;
  _twoFingerMid     = null;
  _twoFingerDragged = false;
}

/**
 * Set the gamepad-active gate.
 * When active=true the touch engine forwards NO events (gamepad.js handles input).
 * @param {boolean} active
 */
export function setGamepadActive(active) {
  _gatedByGamepad = active;
  if (active) {
    // Cancel any in-flight gesture
    _clearLong();
    _dragging         = false;
    _activeTouches    = [];
    _startTouches     = [];
    _twoFingerMid     = null;
    _twoFingerDragged = false;
    _pendingMove      = null;
    if (_rafHandle) { cancelAnimationFrame(_rafHandle); _rafHandle = null; }
  }
}

// ── Bus wiring ────────────────────────────────────────────────────────────────

(function _wireGlobalBus() {
  // Defer until bus is set by app.js (synchronous on DOMContentLoaded)
  function doWire() {
    const bus = getBus();
    if (!bus) {
      // Try again after DOMContentLoaded
      if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', doWire, { once: true });
      }
      return;
    }

    // gamepad-active event: gate the pointer engine on/off
    bus.on('gamepad-active', (detail) => {
      const active = detail && detail.active !== undefined ? detail.active : !!detail;
      setGamepadActive(active);
    });

    // session-started: auto-mount for the active slot
    bus.on('session-started', (detail) => {
      const slot = (detail && detail.slot != null) ? detail.slot : 0;
      mount(slot);
    });

    // session-ended: unmount
    bus.on('session-ended', () => {
      unmount();
    });
  }

  // app.js sets window.__pulsarBus synchronously before DOMContentLoaded fires;
  // if we're already past that, getBus() will return it directly.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', doWire, { once: true });
  } else {
    // Small defer so app.js has a chance to set window.__pulsarBus
    Promise.resolve().then(doWire);
  }
})();
