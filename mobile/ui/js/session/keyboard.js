/**
 * keyboard.js — On-screen keyboard + special-key/modifier bar (W3-keyboard)
 *
 * Design task: DT-keyboard
 *
 * Features:
 *   - A toggle button in the overlay that shows/hides the keyboard panel.
 *   - A hidden <textarea> that, when focused, raises the native soft keyboard.
 *   - A scrollable special-key pill row: Esc · Tab · CapsLock · Delete · ←↑↓→
 *     then F1–F12 (horizontal scroll).
 *   - A sticky modifier chip row: Ctrl · Alt · Shift · Win (latching — one press
 *     arms the modifier, the next key sends it, then it releases).
 *   - A one-tap "Ctrl+Alt+Del" combo button.
 *   - visualViewport-aware positioning: the bar floats above the soft keyboard.
 *   - Remote-mode only: registerCard({ modes: ['remote'] }).
 *   - Calls send_char (for printable input) and send_key (for special / modifier).
 *
 * Contract (§2.4 / §3):
 *   Calls:
 *     send_char  { slot: u8, ch: String }    → InputEvent::Char
 *     send_key   { slot: u8, code: u32, down: bool } → InputEvent::Key (evdev)
 *
 *   Registers:
 *     registerCard({ id: 'keyboard', modes: ['remote'], section: 'tools', order: 10, mount })
 *
 * Evdev codes come from linux/input-event-codes.h (same table as desktop keymap.ts).
 * The card button (shown in the overlay dock) toggles the full keyboard panel.
 *
 * Self-contained styles are injected into <head> once at module load. The module
 * does NOT edit components.css (owned by W3-overlay) or overlay.js.
 */

import { invoke }        from '../tauri.js';
import { t }             from '../i18n.js';

// ── i18n additions (keyboard-specific keys added inline; parent catalog may
//    not have them yet in W3) ──────────────────────────────────────────────────
const _KBD_STRINGS = {
  tr: {
    'kbd.toggle':  'Klavye',
    'kbd.cad':     'Ctrl+Alt+Del',
    'kbd.type':    'Metin yaz…',
    'kbd.close':   'Klavyeyi kapat',
    'kbd.mods':    'Değiştirici tuşlar',
  },
  en: {
    'kbd.toggle':  'Keyboard',
    'kbd.cad':     'Ctrl+Alt+Del',
    'kbd.type':    'Type here…',
    'kbd.close':   'Close keyboard',
    'kbd.mods':    'Modifier keys',
  },
};

/** Local translate — falls back to the app-wide t() then the key. */
function tk(key) {
  try {
    // Use the loaded lang from i18n if available
    const { lang } = /** @type {any} */ (window.__pulsarI18n || {});
    const l = lang || 'tr';
    return (_KBD_STRINGS[l] && _KBD_STRINGS[l][key])
      || (_KBD_STRINGS.en[key])
      || t(key)
      || key;
  } catch (_) {
    return (_KBD_STRINGS.tr[key]) || key;
  }
}

// ── Evdev keycode table (linux/input-event-codes.h) ──────────────────────────
// Matches desktop keymap.ts exactly. Used by send_key.

const EVDEV = {
  Escape: 1,
  Digit1: 2, Digit2: 3, Digit3: 4, Digit4: 5, Digit5: 6,
  Digit6: 7, Digit7: 8, Digit8: 9, Digit9: 10, Digit0: 11,
  Minus: 12, Equal: 13, Backspace: 14, Tab: 15,
  KeyQ: 16, KeyW: 17, KeyE: 18, KeyR: 19, KeyT: 20, KeyY: 21,
  KeyU: 22, KeyI: 23, KeyO: 24, KeyP: 25, BracketLeft: 26, BracketRight: 27,
  Enter: 28, ControlLeft: 29,
  KeyA: 30, KeyS: 31, KeyD: 32, KeyF: 33, KeyG: 34, KeyH: 35,
  KeyJ: 36, KeyK: 37, KeyL: 38, Semicolon: 39, Quote: 40, Backquote: 41,
  ShiftLeft: 42, Backslash: 43,
  KeyZ: 44, KeyX: 45, KeyC: 46, KeyV: 47, KeyB: 48, KeyN: 49, KeyM: 50,
  Comma: 51, Period: 52, Slash: 53, ShiftRight: 54,
  NumpadMultiply: 55, AltLeft: 56, Space: 57, CapsLock: 58,
  F1: 59, F2: 60, F3: 61, F4: 62, F5: 63, F6: 64,
  F7: 65, F8: 66, F9: 67, F10: 68,
  NumLock: 69, ScrollLock: 70,
  Numpad7: 71, Numpad8: 72, Numpad9: 73, NumpadSubtract: 74,
  Numpad4: 75, Numpad5: 76, Numpad6: 77, NumpadAdd: 78,
  Numpad1: 79, Numpad2: 80, Numpad3: 81, Numpad0: 82, NumpadDecimal: 83,
  F11: 87, F12: 88,
  NumpadEnter: 96, ControlRight: 97, NumpadDivide: 98, AltRight: 100,
  Home: 102, ArrowUp: 103, PageUp: 104, ArrowLeft: 105, ArrowRight: 106,
  End: 107, ArrowDown: 108, PageDown: 109, Insert: 110, Delete: 111,
  MetaLeft: 125, MetaRight: 126, ContextMenu: 127,
};

/** @param {string} code @returns {number} */
function evdevCode(code) {
  return EVDEV[code] || 0;
}

// ── Special keys shown in the scrollable pill bar ─────────────────────────────

/**
 * Each entry: { label: string, code: string, wide?: boolean }
 * `code` is a KeyboardEvent.code — mapped to evdev via EVDEV[].
 * `wide` = true gives a 2× width pill (used for Delete, Enter, Backspace, etc.).
 */
const SPECIAL_KEYS = [
  { label: 'Esc',   code: 'Escape',    wide: false },
  { label: 'Tab',   code: 'Tab',       wide: false },
  { label: 'Caps',  code: 'CapsLock',  wide: false },
  { label: '⌫',     code: 'Backspace', wide: false },
  { label: 'Del',   code: 'Delete',    wide: false },
  { label: 'Ins',   code: 'Insert',    wide: false },
  { label: 'Home',  code: 'Home',      wide: false },
  { label: 'End',   code: 'End',       wide: false },
  { label: 'PgUp',  code: 'PageUp',    wide: false },
  { label: 'PgDn',  code: 'PageDown',  wide: false },
  { label: '↑',     code: 'ArrowUp',   wide: false },
  { label: '←',     code: 'ArrowLeft', wide: false },
  { label: '↓',     code: 'ArrowDown', wide: false },
  { label: '→',     code: 'ArrowRight',wide: false },
  { label: 'F1',    code: 'F1',        wide: false },
  { label: 'F2',    code: 'F2',        wide: false },
  { label: 'F3',    code: 'F3',        wide: false },
  { label: 'F4',    code: 'F4',        wide: false },
  { label: 'F5',    code: 'F5',        wide: false },
  { label: 'F6',    code: 'F6',        wide: false },
  { label: 'F7',    code: 'F7',        wide: false },
  { label: 'F8',    code: 'F8',        wide: false },
  { label: 'F9',    code: 'F9',        wide: false },
  { label: 'F10',   code: 'F10',       wide: false },
  { label: 'F11',   code: 'F11',       wide: false },
  { label: 'F12',   code: 'F12',       wide: false },
];

/** Modifier definitions — sticky/latching. */
const MODIFIERS = [
  { label: 'Ctrl',  code: 'ControlLeft',  evdev: EVDEV.ControlLeft },
  { label: 'Alt',   code: 'AltLeft',      evdev: EVDEV.AltLeft },
  { label: 'Shift', code: 'ShiftLeft',    evdev: EVDEV.ShiftLeft },
  { label: 'Win',   code: 'MetaLeft',     evdev: EVDEV.MetaLeft },
];

// ── Module state ─────────────────────────────────────────────────────────────

/** @type {number} current active session slot */
let _slot = 0;

/** @type {Set<string>} currently latched modifier codes */
const _latchedMods = new Set();

/** @type {boolean} is the panel visible */
let _panelOpen = false;

/** @type {HTMLElement|null} */
let _panel = null;

/** @type {HTMLTextAreaElement|null} hidden input element */
let _hiddenInput = null;

/** @type {boolean} whether the soft keyboard is currently raised */
let _softKbdVisible = false;

// ── Slot tracking (updated via JS bus when session changes) ──────────────────

const _getBus = () => window.__pulsarBus || null;

function _onSessionStarted({ slot }) {
  _slot = slot || 0;
}
function _onSessionEnded() {
  _closePanel();
  _latchedMods.clear();
}

// ── Send helpers ─────────────────────────────────────────────────────────────

/**
 * Send a Char input event — for printable text from the hidden textarea.
 * @param {string} ch
 */
async function _sendChar(ch) {
  if (!ch) return;
  try {
    await invoke('send_char', { slot: _slot, ch });
  } catch (e) {
    console.warn('[keyboard] send_char error', e);
  }
}

/**
 * Send a Key event (press + release) optionally preceded by latched modifiers.
 * After the key is sent, latched modifiers are released and cleared.
 *
 * @param {number} evdev — evdev keycode
 * @param {boolean} [withMods=true] — also fire currently latched modifiers
 */
async function _sendKey(evdev, withMods = true) {
  if (!evdev) return;
  try {
    // Press latched modifiers first
    const mods = withMods ? [..._latchedMods] : [];
    for (const code of mods) {
      const ev = evdevCode(code);
      if (ev) await invoke('send_key', { slot: _slot, code: ev, down: true });
    }
    // Press + release the actual key
    await invoke('send_key', { slot: _slot, code: evdev, down: true });
    await invoke('send_key', { slot: _slot, code: evdev, down: false });
    // Release modifiers in reverse
    for (const code of [...mods].reverse()) {
      const ev = evdevCode(code);
      if (ev) await invoke('send_key', { slot: _slot, code: ev, down: false });
    }
    // Clear latched modifiers after use
    if (mods.length > 0) {
      _latchedMods.clear();
      _syncModifierUI();
    }
  } catch (e) {
    console.warn('[keyboard] send_key error', e);
  }
}

/**
 * Send Ctrl+Alt+Del combo.
 */
async function _sendCtrlAltDel() {
  try {
    await invoke('send_key', { slot: _slot, code: EVDEV.ControlLeft, down: true });
    await invoke('send_key', { slot: _slot, code: EVDEV.AltLeft,     down: true });
    await invoke('send_key', { slot: _slot, code: EVDEV.Delete,      down: true });
    await invoke('send_key', { slot: _slot, code: EVDEV.Delete,      down: false });
    await invoke('send_key', { slot: _slot, code: EVDEV.AltLeft,     down: false });
    await invoke('send_key', { slot: _slot, code: EVDEV.ControlLeft, down: false });
  } catch (e) {
    console.warn('[keyboard] Ctrl+Alt+Del error', e);
  }
}

// ── Hidden input (raises native soft keyboard) ───────────────────────────────

function _ensureHiddenInput() {
  if (_hiddenInput) return _hiddenInput;

  const ta = document.createElement('textarea');
  ta.id = 'pulsar-kbd-hidden-input';
  ta.setAttribute('aria-hidden', 'true');
  ta.setAttribute('autocomplete', 'off');
  ta.setAttribute('autocorrect', 'off');
  ta.setAttribute('autocapitalize', 'none');
  ta.setAttribute('spellcheck', 'false');
  ta.style.cssText = [
    'position:fixed',
    'left:-9999px',
    'top:-9999px',
    'width:1px',
    'height:1px',
    'opacity:0',
    'pointer-events:none',
    'font-size:16px', /* must be ≥16px to prevent iOS auto-zoom */
    'transform:scale(0)',
  ].join(';');
  document.body.appendChild(ta);
  _hiddenInput = ta;

  // Drain text as InputEvent::Char one character at a time
  ta.addEventListener('input', () => {
    const text = ta.value;
    if (!text) return;
    ta.value = '';
    for (const ch of text) {
      _sendChar(ch);
    }
  });

  // Keydown → handle special keys (Backspace, Enter, arrows, etc.)
  ta.addEventListener('keydown', (e) => {
    const evdev = evdevCode(e.code);
    if (!evdev) return;
    // Printable characters are handled by 'input' event; skip them here.
    // But we do want to handle: Backspace, Enter, Tab, arrows, Escape, Delete, F-keys
    const isSpecial = (
      e.code === 'Backspace' || e.code === 'Delete' ||
      e.code === 'Tab' || e.code === 'Enter' ||
      e.code === 'Escape' ||
      e.code.startsWith('Arrow') ||
      e.code.startsWith('F') ||
      e.code === 'Home' || e.code === 'End' ||
      e.code === 'PageUp' || e.code === 'PageDown' ||
      e.code === 'Insert' || e.code === 'CapsLock'
    );
    if (isSpecial) {
      e.preventDefault();
      // Send key down
      invoke('send_key', { slot: _slot, code: evdev, down: true }).catch(() => {});
    }
  });

  ta.addEventListener('keyup', (e) => {
    const evdev = evdevCode(e.code);
    if (!evdev) return;
    const isSpecial = (
      e.code === 'Backspace' || e.code === 'Delete' ||
      e.code === 'Tab' || e.code === 'Enter' ||
      e.code === 'Escape' ||
      e.code.startsWith('Arrow') ||
      e.code.startsWith('F') ||
      e.code === 'Home' || e.code === 'End' ||
      e.code === 'PageUp' || e.code === 'PageDown' ||
      e.code === 'Insert' || e.code === 'CapsLock'
    );
    if (isSpecial) {
      invoke('send_key', { slot: _slot, code: evdev, down: false }).catch(() => {});
    }
  });

  // Track soft-kbd visibility via focus/blur
  ta.addEventListener('focus', () => { _softKbdVisible = true; });
  ta.addEventListener('blur',  () => { _softKbdVisible = false; });

  return ta;
}

/** Focus the hidden textarea to raise the native soft keyboard. */
function _raiseSoftKbd() {
  const inp = _ensureHiddenInput();
  inp.focus({ preventScroll: true });
}

/** Blur the hidden textarea to dismiss the native soft keyboard. */
function _dismissSoftKbd() {
  _hiddenInput?.blur();
}

// ── Panel (the full keyboard card rendered over the session surface) ──────────

function _buildPanel() {
  const panel = document.createElement('div');
  panel.id = 'pulsar-kbd-panel';
  panel.className = 'pulsar-kbd-panel';
  panel.setAttribute('role', 'toolbar');
  panel.setAttribute('aria-label', tk('kbd.toggle'));

  // ── Row 1: Modifier chips + Ctrl+Alt+Del ────────────────────────────────
  const modRow = document.createElement('div');
  modRow.className = 'pulsar-kbd-mod-row';

  for (const mod of MODIFIERS) {
    const btn = document.createElement('button');
    btn.className = 'pulsar-kbd-mod';
    btn.type = 'button';
    btn.dataset.code = mod.code;
    btn.textContent = mod.label;
    btn.setAttribute('aria-pressed', 'false');
    btn.setAttribute('aria-label', mod.label);

    btn.addEventListener('click', () => {
      if (_latchedMods.has(mod.code)) {
        _latchedMods.delete(mod.code);
      } else {
        _latchedMods.add(mod.code);
      }
      _syncModifierUI();
      // Re-focus the hidden input after tapping a modifier (keeps kbd open)
      _raiseSoftKbd();
    });

    modRow.appendChild(btn);
  }

  // Ctrl+Alt+Del button
  const cadBtn = document.createElement('button');
  cadBtn.className = 'pulsar-kbd-cad';
  cadBtn.type = 'button';
  cadBtn.textContent = 'C+A+D';
  cadBtn.setAttribute('aria-label', tk('kbd.cad'));
  cadBtn.title = tk('kbd.cad');
  cadBtn.addEventListener('click', () => {
    _latchedMods.clear();
    _syncModifierUI();
    _sendCtrlAltDel();
  });
  modRow.appendChild(cadBtn);

  panel.appendChild(modRow);

  // ── Row 2: Scrollable special-key pills ─────────────────────────────────
  const specialRow = document.createElement('div');
  specialRow.className = 'pulsar-kbd-special-row';

  for (const key of SPECIAL_KEYS) {
    const btn = document.createElement('button');
    btn.className = 'pulsar-kbd-special' + (key.wide ? ' wide' : '');
    btn.type = 'button';
    btn.textContent = key.label;
    btn.setAttribute('aria-label', key.label);

    btn.addEventListener('click', () => {
      const ev = evdevCode(key.code);
      if (ev) _sendKey(ev, true);
      // Keep focus on hidden input
      _raiseSoftKbd();
    });

    specialRow.appendChild(btn);
  }

  panel.appendChild(specialRow);

  // ── Row 3: Type prompt + close button ───────────────────────────────────
  const typeRow = document.createElement('div');
  typeRow.className = 'pulsar-kbd-type-row';

  const typePill = document.createElement('button');
  typePill.className = 'pulsar-kbd-type-pill';
  typePill.type = 'button';
  typePill.setAttribute('aria-label', tk('kbd.type'));

  // Keyboard icon
  typePill.innerHTML =
    '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">' +
      '<rect x="2" y="5" width="20" height="14" rx="3" stroke="currentColor" stroke-width="1.75"/>' +
      '<path d="M6 9h.01M9 9h.01M12 9h.01M15 9h.01M18 9h.01M6 12h.01M9 12h.01M12 12h.01M15 12h.01M18 12h.01M8 15h8" stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>' +
    '</svg>' +
    '<span class="pulsar-kbd-type-label">' + tk('kbd.type') + '</span>';

  typePill.addEventListener('click', () => {
    _raiseSoftKbd();
  });
  typeRow.appendChild(typePill);

  const closeBtn = document.createElement('button');
  closeBtn.className = 'pulsar-kbd-close';
  closeBtn.type = 'button';
  closeBtn.setAttribute('aria-label', tk('kbd.close'));
  closeBtn.innerHTML =
    '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" aria-hidden="true">' +
      '<path d="M6 18L18 6M6 6l12 12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>' +
    '</svg>';
  closeBtn.addEventListener('click', () => {
    _closePanel();
  });
  typeRow.appendChild(closeBtn);

  panel.appendChild(typeRow);

  return panel;
}

// ── Modifier UI sync ─────────────────────────────────────────────────────────

function _syncModifierUI() {
  if (!_panel) return;
  _panel.querySelectorAll('.pulsar-kbd-mod').forEach((btn) => {
    const code = /** @type {HTMLElement} */ (btn).dataset.code;
    const active = _latchedMods.has(code);
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-pressed', String(active));
  });
}

// ── Panel open / close ───────────────────────────────────────────────────────

function _openPanel() {
  if (_panelOpen) {
    // Already open — re-focus the hidden input to re-raise soft kbd
    _raiseSoftKbd();
    return;
  }
  _panelOpen = true;

  if (!_panel) {
    _panel = _buildPanel();
    document.body.appendChild(_panel);
  }

  // Make sure the hidden input exists
  _ensureHiddenInput();

  // Position panel above current keyboard / bar (visualViewport-aware)
  _updatePanelPosition();

  // Animate in
  requestAnimationFrame(() => {
    if (_panel) _panel.classList.add('open');
  });

  // Raise soft keyboard
  setTimeout(() => _raiseSoftKbd(), 80);

  // Start tracking visualViewport changes
  _startViewportTracking();

  // Update the toggle button state in the overlay card
  _updateToggleBtnState();
}

function _closePanel() {
  if (!_panelOpen) return;
  _panelOpen = false;

  _dismissSoftKbd();

  if (_panel) {
    _panel.classList.remove('open');
    // Optionally: remove from DOM after transition
    // (keep it for fast re-open)
  }

  _stopViewportTracking();
  _updateToggleBtnState();
}

// ── visualViewport tracking ───────────────────────────────────────────────────
// Position the panel directly above the native soft keyboard using visualViewport.

/** @type {(() => void)|null} */
let _stopViewportTracking = () => {};

function _startViewportTracking() {
  if (typeof window.visualViewport === 'undefined') return;

  const vv = window.visualViewport;

  const update = () => _updatePanelPosition();

  vv.addEventListener('resize', update);
  vv.addEventListener('scroll', update);

  // Return a stop function
  _stopViewportTracking = () => {
    vv.removeEventListener('resize', update);
    vv.removeEventListener('scroll', update);
    _stopViewportTracking = () => {};
  };

  // Immediate update
  _updatePanelPosition();
}

function _updatePanelPosition() {
  if (!_panel) return;
  const vv = window.visualViewport;
  if (!vv) return;

  // visualViewport gives us the visible area excluding the soft keyboard.
  // Bottom of the panel should sit at: vv.offsetTop + vv.height
  // (i.e., the top of the keyboard). We also add a small gap from the bar.
  const panelBottom = window.innerHeight - (vv.offsetTop + vv.height);

  // Apply as CSS custom property so the panel can use it
  _panel.style.setProperty('--kbd-panel-bottom', panelBottom + 'px');
}

// ── Overlay card (the entry point registered with overlay.js) ─────────────────

/**
 * Mount the keyboard overlay card into the given container element.
 * The card contains a single large toggle button that opens/closes the panel.
 *
 * @param {HTMLElement} container
 */
function _mountCard(container) {
  // Bus wiring — track session slot
  const bus = _getBus();
  if (bus) {
    bus.on('session-started', _onSessionStarted);
    bus.on('session-ended', _onSessionEnded);
  }

  container.innerHTML = `
    <div class="pulsar-kbd-card">
      <div class="pulsar-kbd-card-header">
        <span class="pulsar-kbd-card-icon" aria-hidden="true">
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none">
            <rect x="2" y="5" width="20" height="14" rx="3"
                  stroke="currentColor" stroke-width="1.75"/>
            <path d="M6 9h.01M9 9h.01M12 9h.01M15 9h.01M18 9h.01
                     M6 12h.01M9 12h.01M12 12h.01M15 12h.01M18 12h.01M8 15h8"
                  stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>
          </svg>
        </span>
        <span class="pulsar-kbd-card-label">${tk('kbd.toggle')}</span>
      </div>
      <button class="pulsar-kbd-toggle btn btn-ghost" id="pulsar-kbd-toggle-btn"
              type="button" aria-pressed="false">
        <svg class="pulsar-kbd-toggle-icon-show" width="18" height="18"
             viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <rect x="2" y="5" width="20" height="14" rx="3"
                stroke="currentColor" stroke-width="1.75"/>
          <path d="M6 9h.01M9 9h.01M12 9h.01M15 9h.01M18 9h.01
                   M6 12h.01M9 12h.01M12 12h.01M15 12h.01M18 12h.01M8 15h8"
                stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>
        </svg>
        <span>${tk('kbd.toggle')}</span>
      </button>
    </div>
  `;

  const toggleBtn = container.querySelector('#pulsar-kbd-toggle-btn');
  toggleBtn?.addEventListener('click', () => {
    if (_panelOpen) {
      _closePanel();
    } else {
      _openPanel();
    }
  });
}

function _updateToggleBtnState() {
  const btn = document.getElementById('pulsar-kbd-toggle-btn');
  if (!btn) return;
  btn.setAttribute('aria-pressed', String(_panelOpen));
  btn.classList.toggle('active', _panelOpen);
}

// ── Styles ────────────────────────────────────────────────────────────────────
// Injected into <head> once; self-contained, no edits to components.css needed.

function _injectStyles() {
  if (document.getElementById('pulsar-kbd-styles')) return;
  const style = document.createElement('style');
  style.id = 'pulsar-kbd-styles';
  style.textContent = `
/* ============================================================
   Pulsar keyboard.js — on-screen keyboard + modifier bar
   Theme: indigo (remote-only). Sits above the native soft kb.
   ============================================================ */

/* ── Overlay card entry ────────────────────────────────────── */
.pulsar-kbd-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.pulsar-kbd-card-header {
  display: flex;
  align-items: center;
  gap: 10px;
  color: oklch(0.88 0.02 272);
  font-size: 13px;
  font-weight: 600;
  letter-spacing: 0.01em;
}
.pulsar-kbd-card-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 32px; height: 32px;
  border-radius: 8px;
  background: oklch(0.555 0.205 272 / 0.18);
  color: oklch(0.72 0.18 272);
  flex: none;
}
.pulsar-kbd-toggle {
  width: 100%;
  border-radius: var(--r-pill);
  gap: 8px;
  /* Override ghost to use brand accent when active */
}
.pulsar-kbd-toggle.active {
  background: var(--brand);
  color: var(--text-on-accent);
  border-color: transparent;
  box-shadow: var(--shadow-accent);
}

/* ── Floating panel ────────────────────────────────────────── */
.pulsar-kbd-panel {
  position: fixed;
  left: 0;
  right: 0;
  /* Default bottom — overridden by JS to sit above native keyboard */
  bottom: var(--kbd-panel-bottom, calc(var(--safe-bottom)));
  z-index: 18;  /* above overlay-dock (15) but below modal (30) */
  display: flex;
  flex-direction: column;
  gap: 0;
  background: oklch(0.13 0.015 268 / 0.93);
  backdrop-filter: blur(20px) saturate(1.5);
  -webkit-backdrop-filter: blur(20px) saturate(1.5);
  border-top: 1px solid oklch(1 0 0 / 0.12);
  padding: 10px 10px calc(6px + var(--safe-bottom));
  gap: 8px;

  /* Hidden by default — slides up from the bottom */
  transform: translateY(110%);
  transition: transform 0.32s cubic-bezier(0.16, 1, 0.3, 1);
  will-change: transform;
}
.pulsar-kbd-panel.open {
  transform: translateY(0);
}

/* When the native soft keyboard is visible, don't add safe-area to the
   panel bottom (the keyboard IS the safe area). JS updates --kbd-panel-bottom. */

/* ── Row 1: Modifier chips + CAD ───────────────────────────── */
.pulsar-kbd-mod-row {
  display: flex;
  flex-direction: row;
  align-items: center;
  gap: 6px;
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
  scrollbar-width: none;
  padding: 2px 0;
}
.pulsar-kbd-mod-row::-webkit-scrollbar { display: none; }

.pulsar-kbd-mod {
  flex: 1;
  min-width: 56px;
  min-height: 44px;
  border: 1px solid oklch(1 0 0 / 0.18);
  border-radius: var(--r-sm);
  background: oklch(1 0 0 / 0.07);
  color: oklch(0.9 0.01 268);
  font-family: var(--font-sans);
  font-size: 13px;
  font-weight: 700;
  letter-spacing: 0.02em;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: background 0.15s, color 0.15s, border-color 0.15s, box-shadow 0.15s;
  -webkit-tap-highlight-color: transparent;
}
.pulsar-kbd-mod:active {
  background: oklch(1 0 0 / 0.15);
  transform: scale(0.96);
}
/* Latched (sticky) state — uses indigo accent */
.pulsar-kbd-mod.active {
  background: oklch(0.555 0.205 272 / 0.85);
  color: oklch(0.98 0.01 272);
  border-color: oklch(0.555 0.205 272 / 0.6);
  box-shadow: 0 0 0 3px oklch(0.555 0.205 272 / 0.28);
}

/* Ctrl+Alt+Del button */
.pulsar-kbd-cad {
  flex: none;
  min-width: 72px;
  min-height: 44px;
  border: 1px solid oklch(0.575 0.205 25 / 0.5);
  border-radius: var(--r-sm);
  background: oklch(0.575 0.205 25 / 0.12);
  color: oklch(0.82 0.12 25);
  font-family: var(--font-sans);
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.04em;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: background 0.15s;
  -webkit-tap-highlight-color: transparent;
}
.pulsar-kbd-cad:active {
  background: oklch(0.575 0.205 25 / 0.25);
  transform: scale(0.96);
}

/* ── Row 2: Special-key scrollable pill row ─────────────────── */
.pulsar-kbd-special-row {
  display: flex;
  flex-direction: row;
  gap: 5px;
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
  scrollbar-width: none;
  padding: 2px 0;
}
.pulsar-kbd-special-row::-webkit-scrollbar { display: none; }

.pulsar-kbd-special {
  flex: none;
  min-width: 44px;
  min-height: 44px;
  border: 1px solid oklch(1 0 0 / 0.14);
  border-radius: var(--r-sm);
  background: oklch(1 0 0 / 0.07);
  color: oklch(0.9 0.01 268);
  font-family: var(--font-sans);
  font-size: 12.5px;
  font-weight: 600;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  padding: 0 10px;
  white-space: nowrap;
  transition: background 0.15s, transform 0.12s;
  -webkit-tap-highlight-color: transparent;
}
.pulsar-kbd-special.wide { min-width: 72px; }
.pulsar-kbd-special:active {
  background: oklch(1 0 0 / 0.18);
  transform: scale(0.94);
}

/* Arrow key visual cues */
.pulsar-kbd-special[aria-label="↑"],
.pulsar-kbd-special[aria-label="↓"],
.pulsar-kbd-special[aria-label="←"],
.pulsar-kbd-special[aria-label="→"] {
  font-size: 18px;
  min-width: 50px;
}

/* F-key subtler styling */
.pulsar-kbd-special[aria-label^="F"] {
  color: oklch(0.76 0.06 268);
  font-size: 11.5px;
}

/* ── Row 3: Type-prompt + close ─────────────────────────────── */
.pulsar-kbd-type-row {
  display: flex;
  flex-direction: row;
  align-items: center;
  gap: 8px;
}
.pulsar-kbd-type-pill {
  flex: 1;
  min-height: 44px;
  border: 1px solid oklch(0.555 0.205 272 / 0.35);
  border-radius: var(--r-pill);
  background: oklch(0.555 0.205 272 / 0.12);
  color: oklch(0.78 0.1 272);
  font-family: var(--font-sans);
  font-size: 14px;
  font-weight: 500;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 16px;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s;
  -webkit-tap-highlight-color: transparent;
}
.pulsar-kbd-type-pill:active {
  background: oklch(0.555 0.205 272 / 0.22);
}
.pulsar-kbd-type-label {
  flex: 1;
  text-align: left;
  font-style: italic;
  opacity: 0.7;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.pulsar-kbd-close {
  flex: none;
  width: 44px;
  height: 44px;
  border: 1px solid oklch(1 0 0 / 0.14);
  border-radius: 50%;
  background: oklch(1 0 0 / 0.07);
  color: oklch(0.82 0.01 268);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: background 0.15s, transform 0.12s;
  -webkit-tap-highlight-color: transparent;
}
.pulsar-kbd-close:active {
  background: oklch(1 0 0 / 0.16);
  transform: scale(0.92);
}
`;
  document.head.appendChild(style);
}

// ── registerCard ──────────────────────────────────────────────────────────────

/**
 * Register this module as an overlay card.
 * Called at module import time; overlay.js is the host (W3-overlay lane).
 * Falls back gracefully if overlay.js is not yet loaded.
 */
function _register() {
  // overlay.js exposes registerCard() via window.__pulsarRegisterCard after
  // the overlay module is loaded. If it's not there yet, defer until load.
  function doRegister() {
    const rc = window.__pulsarRegisterCard;
    if (typeof rc === 'function') {
      rc({
        id:      'keyboard',
        modes:   ['remote'],   // REMOTE-ONLY per §1.2 and DT-keyboard
        section: 'tools',
        order:   10,
        mount:   _mountCard,
      });
    }
  }

  if (typeof window.__pulsarRegisterCard === 'function') {
    doRegister();
  } else {
    // overlay.js sets this before DOMContentLoaded or on module load;
    // listen for the custom event it dispatches when ready.
    window.addEventListener('pulsar-overlay-ready', doRegister, { once: true });
    // Also try on DOMContentLoaded / load as fallback
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', doRegister, { once: true });
    } else {
      // Schedule microtask so other modules in the same wave finish loading
      Promise.resolve().then(doRegister);
    }
  }
}

// ── Boot ──────────────────────────────────────────────────────────────────────

_injectStyles();
_register();

// Expose session slot updater for external modules if needed
export function setSlot(slot) { _slot = slot; }

// Named exports for testability
export { _sendChar as sendChar, _sendKey as sendKey, _sendCtrlAltDel as sendCtrlAltDel };
