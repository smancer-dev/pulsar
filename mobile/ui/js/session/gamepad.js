/**
 * gamepad.js — On-screen virtual gamepad + physical controller bridge (W4-gamepad)
 *
 * DT-onscreen-gamepad: Moonlight-class landscape touch controller rendered as a
 * fixed translucent layer over the transparent video surface (game mode only).
 * Also polls navigator.getGamepads() and forwards physical pad state for any
 * Bluetooth/USB controller connected to the phone.
 *
 * ── On-screen layout (Moonlight-style) ──────────────────────────────────────
 *   LEFT side                    CENTER                   RIGHT side
 *   [L2 trigger]  [L1 shoulder]  [Select][Guide][Start]   [R1 shoulder]  [R2 trigger]
 *
 *   [D-pad ↑]                                             [Y]
 *   [D-pad ←] [D-pad →]  [LEFT STICK]  [RIGHT STICK]  [X]   [B]
 *   [D-pad ↓]                                             [A]
 *
 *   LEFT STICK: floating analog disk (joystick zone), tap target ≥ 100 px
 *   RIGHT STICK: same
 *   All buttons: ≥ 44px tap targets, translucent glass style, cyan brand
 *
 * ── Feature set ─────────────────────────────────────────────────────────────
 *   • Multi-touch: each control has its own pointerId; simultaneous presses work.
 *   • Analog sticks: drag within a circular dead-zone ring → axis value −32768…32767
 *     (Y-axis: UP = negative, DOWN = positive, matching XInput convention).
 *   • Triggers (L2/R2): vertical drag within trigger zone → 0…255.
 *   • Shoulder buttons (L1/R1): tap → button bit.
 *   • D-pad, ABXY, Start, Select, Guide: tap → button bit.
 *   • Emits bus:gamepad-active {active:bool} so input.js gates its pointer engine.
 *   • Physical gamepad poll: navigator.getGamepads() at ~60Hz; maps to same
 *     send_gamepad / send_gamepad_disconnect commands. Rumble feedback via
 *     GamepadHapticActuator when the 'rumble' Tauri event arrives.
 *   • Controller status chip in the in-session bar shows detected pad count.
 *   • send_gamepad_disconnect on session end or overlay toggle-off.
 *
 * ── Button bitmask (Xbox / XInput, matches GamepadState on the host) ────────
 *   Bit  0  DPAD_UP        Bit  8  LEFT_THUMB (L3)
 *   Bit  1  DPAD_DOWN      Bit  9  RIGHT_THUMB (R3)
 *   Bit  2  DPAD_LEFT      Bit 10  LEFT_SHOULDER (L1)
 *   Bit  3  DPAD_RIGHT     Bit 11  RIGHT_SHOULDER (R1)
 *   Bit  4  START          Bit 12  A
 *   Bit  5  BACK (SELECT)  Bit 13  B
 *   Bit  6  LEFT_THUMB     Bit 14  X
 *   Bit  7  RIGHT_THUMB    Bit 15  Y
 *   Guide  → button index 16 in the W3C API; we map it to bit 5 (SELECT) or
 *            handle it as a special pulse since the host protocol has no guide bit.
 *
 * ── Send rate ────────────────────────────────────────────────────────────────
 *   On-screen: send on every change (immediately on each touch event).
 *   Physical: rAF loop at ~60 Hz, send-on-change (compare last state).
 *   Keepalive: repeat last state at 1 Hz so the host doesn't timeout the pad.
 *
 * ── Exports ──────────────────────────────────────────────────────────────────
 *   mount(slot)   — start the gamepad layer for this session slot
 *   unmount()     — tear down everything (sends GamepadDisconnect)
 *
 * ── Calls (Tauri commands) ────────────────────────────────────────────────────
 *   send_gamepad            { slot, padIdx, buttons, lx, ly, rx, ry, lt, rt, target }
 *   send_gamepad_disconnect { slot, padIdx }
 *
 *   `slot` is the SESSION slot (which host to route to); `padIdx` is the per-session
 *   PAD index the host emulates (on-screen = pad 0, physical pads = 1, 2, …). The two
 *   are distinct — see input_cmds.rs::send_gamepad.
 *
 * ── Listens (Tauri events) ───────────────────────────────────────────────────
 *   rumble   { slot, large, small } → vibrate physical controller / phone
 *
 * ── Emits (JS bus) ───────────────────────────────────────────────────────────
 *   gamepad-active { active: bool }
 *
 * Self-registration: calls registerCard({modes:['game'], section:'controllers'})
 * so overlay.js mounts the controller toggle card in game mode.
 *
 * Depends on: tauri.js (invoke, listen), i18n.js (t), overlay.js (registerCard)
 */

import { invoke, listen } from '../tauri.js';
import { t }              from '../i18n.js';
import { gamepadTarget }  from '../store/prefs.js';

/** Host emulation target per pad index ('auto' | 'xbox' | 'ds4'). Physical pads
 *  resolve it from the controller name (Settings → Controllers); on-screen = auto. */
const _padTarget = {};
const _padShortName = (id) => String(id || '').replace(/\s*\(.*\)\s*$/, '').trim();

// ── Constants ─────────────────────────────────────────────────────────────────

/** XInput button bit positions (matches GamepadState on the pulsar-core host) */
const BTN = {
	DPAD_UP:         0,
	DPAD_DOWN:       1,
	DPAD_LEFT:       2,
	DPAD_RIGHT:      3,
	START:           4,
	SELECT:          5,
	L3:              6,
	R3:              7,
	LEFT_THUMB:      8,   // same as L3 per XInput enum
	RIGHT_THUMB:     9,   // same as R3
	L1:             10,
	R1:             11,
	A:              12,
	B:              13,
	X:              14,
	Y:              15,
};

/** W3C Gamepad API button indices (standard mapping) */
const W3C = {
	A:         0,
	B:         1,
	X:         2,
	Y:         3,
	L1:        4,
	R1:        5,
	L2:        6,  // also axis 2 on some pads
	R2:        7,  // also axis 3
	SELECT:    8,
	START:     9,
	L3:       10,
	R3:       11,
	DPAD_UP:  12,
	DPAD_DOWN:13,
	DPAD_LEFT:14,
	DPAD_RIGHT:15,
	GUIDE:    16,
};

/** Stick dead-zone (fraction of full range 0..1) */
const DEADZONE = 0.10;

/** Maximum stick drag radius in CSS pixels */
const STICK_RADIUS = 48;

/** Trigger drag distance (px) for full press */
const TRIGGER_FULL_PX = 80;

/** Physical poll interval (ms) — ~60Hz */
const POLL_MS = 16;

/** Keepalive interval (ms) — 1Hz */
const KEEPALIVE_MS = 1000;

/** LocalStorage key for on-screen pad visibility preference */
const LS_VISIBLE = 'pulsar.gamepad.visible.v1';

// ── Module state ──────────────────────────────────────────────────────────────

/** Active session slot */
let _slot = 0;

/** Is the on-screen gamepad visible? */
let _visible = false;

/** Current on-screen gamepad button bitmask */
let _buttons = 0;

/** Current on-screen axis values */
let _lx = 0, _ly = 0, _rx = 0, _ry = 0;

/** Current on-screen trigger values */
let _lt = 0, _rt = 0;

/** Last full state sent (for dedup) */
let _lastSent = { buttons: -1, lx: 0, ly: 0, rx: 0, ry: 0, lt: 0, rt: 0 };

/** Keepalive timer handle */
let _keepaliveTimer = null;

/** Physical pad rAF handle */
let _rafHandle = null;

/** Last physical pad states for send-on-change (indexed by gamepad index) */
const _lastPadState = new Map();

/** Number of physical pads currently detected */
let _physicalPadCount = 0;

/** Gamepad layer DOM element */
let _layerEl = null;

/** Overlay card DOM element (reference) */
let _cardEl = null;

/** Tauri event unlisteners */
const _unlisteners = [];

/** Has the module been mounted? */
let _mounted = false;

// ── Stick tracking (by pointerId) ─────────────────────────────────────────────

/**
 * Per-stick drag state.
 * @typedef {{ active: boolean, pointerId: number, originX: number, originY: number }} StickState
 */

/** @type {StickState} */
const _lstick = { active: false, pointerId: -1, originX: 0, originY: 0 };

/** @type {StickState} */
const _rstick = { active: false, pointerId: -1, originX: 0, originY: 0 };

/** @type {{ active: boolean, pointerId: number, originY: number }} */
const _ltrigger = { active: false, pointerId: -1, originY: 0 };

/** @type {{ active: boolean, pointerId: number, originY: number }} */
const _rtrigger = { active: false, pointerId: -1, originY: 0 };

// ── Self-registration ─────────────────────────────────────────────────────────

/**
 * Called at import time (before DOMContentLoaded).
 * overlay.js exposes registerCard globally so modules can register safely.
 */
function _selfRegister() {
	const registerCard = window.__pulsarRegisterCard;
	if (typeof registerCard === 'function') {
		registerCard({
			id:      'gamepad-controls',
			modes:   ['game'],
			section: 'controllers',
			order:   10,
			mount:   (el) => { _cardEl = el; _renderCard(el); },
		});
	} else {
		// overlay.js not yet loaded — retry after DOMContentLoaded
		const retry = () => {
			if (typeof window.__pulsarRegisterCard === 'function') {
				window.__pulsarRegisterCard({
					id:      'gamepad-controls',
					modes:   ['game'],
					section: 'controllers',
					order:   10,
					mount:   (el) => { _cardEl = el; _renderCard(el); },
				});
			}
		};
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', retry);
		} else {
			setTimeout(retry, 0);
		}
	}
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Start the gamepad system for a session.
 * Called by session.js (or app.js) when a game-mode session starts.
 * @param {number} slot — session slot
 */
export function mount(slot) {
	if (_mounted) unmount();
	_slot = slot;
	_mounted = true;

	// Restore visibility preference
	_visible = localStorage.getItem(LS_VISIBLE) !== 'false';

	// Build the overlay layer
	_buildLayer();

	if (_visible) _showLayer();
	else          _hideLayer();

	// Subscribe to rumble events
	_subscribeRumble();

	// Start the physical pad poll
	_startPhysicalPoll();

	// Keepalive loop
	_keepaliveTimer = setInterval(_sendKeepalive, KEEPALIVE_MS);

	// Emit gamepad-active. B1: reflect the REAL pad visibility (not a hardcoded
	// true) so the touch→mouse engine is only gated when the pad is actually shown;
	// otherwise hiding the pad would silently kill touch-to-mouse forwarding.
	_emitActive(_visible);
}

/**
 * Tear down the gamepad system and release the virtual pad on the host.
 */
export function unmount() {
	if (!_mounted) return;
	_mounted = false;

	// Send disconnect for the on-screen pad (player 0)
	_sendDisconnect(0);

	// Send disconnect for all physical pads (pad index 1, 2, …)
	for (const [idx] of _lastPadState) {
		_sendDisconnect(1 + idx);
	}
	_lastPadState.clear();
	_physicalPadCount = 0;

	// Stop keepalive
	clearInterval(_keepaliveTimer);
	_keepaliveTimer = null;

	// Stop physical poll
	if (_rafHandle !== null) {
		cancelAnimationFrame(_rafHandle);
		_rafHandle = null;
	}

	// Unsubscribe Tauri events
	for (const fn of _unlisteners) { try { fn(); } catch (_) {} }
	_unlisteners.length = 0;

	// Remove layer from DOM
	_layerEl?.remove();
	_layerEl = null;

	// Reset state
	_buttons = 0; _lx = 0; _ly = 0; _rx = 0; _ry = 0; _lt = 0; _rt = 0;
	_lastSent = { buttons: -1, lx: 0, ly: 0, rx: 0, ry: 0, lt: 0, rt: 0 };

	_emitActive(false);
}

// ── Helpers: send ─────────────────────────────────────────────────────────────

/**
 * Send the current on-screen state if it changed.
 */
function _sendOnscreen() {
	const s = _lastSent;
	if (
		s.buttons === _buttons && s.lx === _lx && s.ly === _ly &&
		s.rx === _rx && s.ry === _ry && s.lt === _lt && s.rt === _rt
	) return;
	_lastSent = { buttons: _buttons, lx: _lx, ly: _ly, rx: _rx, ry: _ry, lt: _lt, rt: _rt };
	invoke('send_gamepad', {
		slot:    _slot,   // session slot (host to route to)
		padIdx:  0,       // on-screen pad = player 0
		buttons: _buttons,
		lx:      _lx,
		ly:      _ly,
		rx:      _rx,
		ry:      _ry,
		lt:      _lt,
		rt:      _rt,
		target:  _padTarget[0] || 'auto',
	}).catch((e) => console.warn('[gamepad] send_gamepad error:', e));
}

/**
 * Keepalive: resend last state at 1Hz so the host doesn't drop the virtual pad.
 */
function _sendKeepalive() {
	if (!_mounted) return;
	invoke('send_gamepad', {
		slot:    _slot,   // session slot (host to route to)
		padIdx:  0,       // on-screen pad = player 0
		buttons: _buttons,
		lx:      _lx,
		ly:      _ly,
		rx:      _rx,
		ry:      _ry,
		lt:      _lt,
		rt:      _rt,
		target:  _padTarget[0] || 'auto',
	}).catch(() => {});
}

/**
 * Send GamepadDisconnect for the given pad index on the active session.
 * @param {number} padIdx  — host-side pad index (on-screen = 0, physical = 1, 2, …)
 */
function _sendDisconnect(padIdx) {
	invoke('send_gamepad_disconnect', { slot: _slot, padIdx })
		.catch((e) => console.warn('[gamepad] disconnect error:', e));
}

// ── Helpers: bus ──────────────────────────────────────────────────────────────

function _emitActive(active) {
	const bus = window.__pulsarBus;
	if (bus && typeof bus.emit === 'function') {
		bus.emit('gamepad-active', { active });
	}
}

// ── Rumble ────────────────────────────────────────────────────────────────────

function _subscribeRumble() {
	listen('rumble', (payload) => {
		if (payload?.slot !== _slot) return;
		_doRumble(payload.large ?? 0, payload.small ?? 0);
		_flashRumbleDot();
	}).then((unlisten) => { if (unlisten) _unlisteners.push(unlisten); });
}

/**
 * Deliver rumble to the physical pad (if any) or fall back to the phone's
 * navigator.vibrate().
 * @param {number} large  0-255
 * @param {number} small  0-255
 */
function _doRumble(large, small) {
	// Try each physical pad's haptic actuator first
	const pads = navigator.getGamepads ? navigator.getGamepads() : [];
	let deliveredToPhysical = false;

	for (let i = 0; i < pads.length; i++) {
		const pad = pads[i];
		if (!pad) continue;
		/** @type {any} */
		const actuator = pad.vibrationActuator;
		if (actuator && typeof actuator.playEffect === 'function') {
			actuator.playEffect('dual-rumble', {
				startDelay:      0,
				duration:        250,
				weakMagnitude:   small  / 255,
				strongMagnitude: large  / 255,
			}).catch(() => {});
			deliveredToPhysical = true;
		}
	}

	// Fall back: phone haptic via navigator.vibrate (short pulse)
	if (!deliveredToPhysical && navigator.vibrate) {
		const durationMs = Math.max(20, Math.round((large + small) / 255 * 80));
		navigator.vibrate(durationMs);
	}
}

// ── Physical pad poll ─────────────────────────────────────────────────────────

function _startPhysicalPoll() {
	let lastPoll = 0;

	const poll = (now) => {
		if (!_mounted) return;
		_rafHandle = requestAnimationFrame(poll);

		if (now - lastPoll < POLL_MS) return;
		lastPoll = now;

		_pollPhysicalPads();
	};

	_rafHandle = requestAnimationFrame(poll);
}

function _pollPhysicalPads() {
	if (!navigator.getGamepads) return;
	const pads = navigator.getGamepads();
	let connected = 0;

	for (let i = 0; i < pads.length; i++) {
		const pad = pads[i];
		if (!pad || !pad.connected) {
			// Was it previously connected? If so, disconnect it.
			if (_lastPadState.has(i)) {
				_lastPadState.delete(i);
				_sendDisconnect(1 + i); // physical pads: pad index 1, 2, …
				_updateStatusChip();
			}
			continue;
		}

		connected++;
		const state = _gamepadToState(pad);
		const last  = _lastPadState.get(i);

		// Send on change
		if (!last || !_stateEqual(state, last)) {
			_lastPadState.set(i, state);
			// SESSION slot stays `_slot` (host routing); the pad index is offset past
			// the on-screen pad (0) so physical pads land on players 1, 2, …
			const padIdx = 1 + i;
			_padTarget[padIdx] = gamepadTarget(_padShortName(pad.id));
			invoke('send_gamepad', {
				slot:    _slot,
				padIdx,
				buttons: state.buttons,
				lx:      state.lx,
				ly:      state.ly,
				rx:      state.rx,
				ry:      state.ry,
				lt:      state.lt,
				rt:      state.rt,
				target:  _padTarget[padIdx],
			}).catch(() => {});
		}
	}

	if (connected !== _physicalPadCount) {
		_physicalPadCount = connected;
		_updateStatusChip();
	}
}

/**
 * Convert a W3C Gamepad to our wire state.
 * @param {Gamepad} pad
 * @returns {{ buttons:number, lx:number, ly:number, rx:number, ry:number, lt:number, rt:number }}
 */
function _gamepadToState(pad) {
	const b = pad.buttons;
	const a = pad.axes;

	let buttons = 0;
	if (b[W3C.DPAD_UP]?.pressed)   buttons |= (1 << BTN.DPAD_UP);
	if (b[W3C.DPAD_DOWN]?.pressed)  buttons |= (1 << BTN.DPAD_DOWN);
	if (b[W3C.DPAD_LEFT]?.pressed)  buttons |= (1 << BTN.DPAD_LEFT);
	if (b[W3C.DPAD_RIGHT]?.pressed) buttons |= (1 << BTN.DPAD_RIGHT);
	if (b[W3C.START]?.pressed)  buttons |= (1 << BTN.START);
	if (b[W3C.SELECT]?.pressed) buttons |= (1 << BTN.SELECT);
	if (b[W3C.L3]?.pressed)     buttons |= (1 << BTN.L3);
	if (b[W3C.R3]?.pressed)     buttons |= (1 << BTN.R3);
	if (b[W3C.L1]?.pressed)     buttons |= (1 << BTN.L1);
	if (b[W3C.R1]?.pressed)     buttons |= (1 << BTN.R1);
	if (b[W3C.A]?.pressed)      buttons |= (1 << BTN.A);
	if (b[W3C.B]?.pressed)      buttons |= (1 << BTN.B);
	if (b[W3C.X]?.pressed)      buttons |= (1 << BTN.X);
	if (b[W3C.Y]?.pressed)      buttons |= (1 << BTN.Y);
	// Guide button: map to Select (bit 5) as a hint (no dedicated host bit)
	if (b[W3C.GUIDE]?.pressed)  buttons |= (1 << BTN.SELECT);

	// Sticks: apply dead-zone, convert to i16
	const lxRaw = a[0] ?? 0;
	const lyRaw = a[1] ?? 0;
	const rxRaw = a[2] ?? 0;
	const ryRaw = a[3] ?? 0;

	const lx = _axisToI16(_applyDeadzone(lxRaw));
	const ly = _axisToI16(_applyDeadzone(lyRaw));
	const rx = _axisToI16(_applyDeadzone(rxRaw));
	const ry = _axisToI16(_applyDeadzone(ryRaw));

	// Triggers: W3C gives 0..1 for button.value, or axis 2/3 on some pads
	const ltVal = b[W3C.L2]?.value ?? Math.max(0, (a[4] ?? -1 + 1) / 2);
	const rtVal = b[W3C.R2]?.value ?? Math.max(0, (a[5] ?? -1 + 1) / 2);
	const lt = Math.round(ltVal * 255);
	const rt = Math.round(rtVal * 255);

	return { buttons, lx, ly, rx, ry, lt, rt };
}

/**
 * Apply radial dead-zone to a single axis value (-1..1).
 * @param {number} v
 * @returns {number}
 */
function _applyDeadzone(v) {
	const abs = Math.abs(v);
	if (abs < DEADZONE) return 0;
	return (v < 0 ? -1 : 1) * ((abs - DEADZONE) / (1 - DEADZONE));
}

/**
 * Map a -1..1 float axis value to i16 (-32768..32767).
 * @param {number} v
 * @returns {number}
 */
function _axisToI16(v) {
	return Math.round(Math.max(-1, Math.min(1, v)) * 32767);
}

/**
 * Compare two gamepad states for equality.
 */
function _stateEqual(a, b) {
	return a.buttons === b.buttons &&
	       a.lx === b.lx && a.ly === b.ly &&
	       a.rx === b.rx && a.ry === b.ry &&
	       a.lt === b.lt && a.rt === b.rt;
}

// ── Stick math ────────────────────────────────────────────────────────────────

/**
 * Convert a stick drag delta (dx, dy in CSS px) to axis i16 values.
 * Y-axis: UP = negative (XInput convention).
 * @param {number} dx
 * @param {number} dy
 * @returns {{ x: number, y: number }}
 */
function _stickDeltaToAxes(dx, dy) {
	const dist  = Math.sqrt(dx * dx + dy * dy);
	const maxR  = STICK_RADIUS;
	const scale = dist > maxR ? maxR / dist : 1;
	const nx    = dx * scale / maxR;  // -1..1
	const ny    = dy * scale / maxR;  // -1..1 (positive = down)
	return {
		x: _axisToI16(nx),
		y: _axisToI16(ny),  // keep Y positive = down (XInput: positive = down)
	};
}

/**
 * Convert a trigger drag delta (dy in CSS px) to a 0..255 trigger value.
 * Dragging DOWN from the trigger button increases the value.
 * @param {number} dy
 * @returns {number}
 */
function _triggerDeltaToValue(dy) {
	const v = Math.max(0, Math.min(1, dy / TRIGGER_FULL_PX));
	return Math.round(v * 255);
}

// ── Button bit helpers ────────────────────────────────────────────────────────

function _setBtn(bit, pressed) {
	if (pressed) _buttons |=  (1 << bit);
	else         _buttons &= ~(1 << bit);
}

// ── DOM: gamepad layer ────────────────────────────────────────────────────────

function _buildLayer() {
	_layerEl?.remove();

	const el = document.createElement('div');
	el.id        = 'gamepad-layer';
	el.className = 'gamepad-layer';
	el.setAttribute('aria-label', 'Sanal oyun kolu');
	el.setAttribute('aria-hidden', 'true');  // decorative from a11y perspective
	el.style.display = 'none';

	el.innerHTML = _layerHTML();
	document.body.appendChild(el);
	_layerEl = el;

	_injectStyles();
	_wireTouches();
}

function _showLayer() {
	if (!_layerEl) return;
	_layerEl.style.display = '';
	localStorage.setItem(LS_VISIBLE, 'true');
	_emitActive(true);
}

function _hideLayer() {
	if (!_layerEl) return;
	_layerEl.style.display = 'none';
	localStorage.setItem(LS_VISIBLE, 'false');
	// Release all buttons when hiding
	_buttons = 0; _lx = 0; _ly = 0; _rx = 0; _ry = 0; _lt = 0; _rt = 0;
	_sendOnscreen();
	_emitActive(false);
}

/**
 * Build the full gamepad layer HTML.
 * All interactive controls have data-ctrl attributes for touch wiring.
 */
function _layerHTML() {
	return `
<!-- ── Left side ───────────────────────────── -->
<div class="gp-left">
  <div class="gp-triggers-left">
    <button class="gp-btn gp-trigger" data-ctrl="lt" aria-label="L2" type="button">
      <span class="gp-label">L2</span>
      <div class="gp-trigger-fill" id="gp-lt-fill"></div>
    </button>
    <button class="gp-btn gp-shoulder" data-ctrl="l1" aria-label="L1" type="button">
      <span class="gp-label">L1</span>
    </button>
  </div>
  <div class="gp-dpad" data-ctrl="dpad" aria-label="D-pad">
    <button class="gp-btn gp-dpad-btn gp-dpad-up"    data-ctrl="dpad-up"    aria-label="Yukarı"  type="button">▲</button>
    <button class="gp-btn gp-dpad-btn gp-dpad-left"  data-ctrl="dpad-left"  aria-label="Sol"     type="button">◀</button>
    <div class="gp-dpad-center" aria-hidden="true"></div>
    <button class="gp-btn gp-dpad-btn gp-dpad-right" data-ctrl="dpad-right" aria-label="Sağ"     type="button">▶</button>
    <button class="gp-btn gp-dpad-btn gp-dpad-down"  data-ctrl="dpad-down"  aria-label="Aşağı"   type="button">▼</button>
  </div>
</div>

<!-- ── Left stick ──────────────────────────── -->
<div class="gp-lstick-zone" data-ctrl="lstick" aria-label="Sol analog çubuk">
  <div class="gp-stick-ring" id="gp-lstick-ring">
    <div class="gp-stick-knob" id="gp-lstick-knob"></div>
  </div>
  <button class="gp-stick-press gp-btn" data-ctrl="l3" aria-label="L3" type="button"></button>
</div>

<!-- ── Center buttons ─────────────────────── -->
<div class="gp-center">
  <button class="gp-btn gp-center-btn" data-ctrl="select" aria-label="Select" type="button">
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2"/>
      <line x1="8" y1="12" x2="16" y2="12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
    </svg>
  </button>
  <button class="gp-btn gp-guide-btn" data-ctrl="guide" aria-label="Guide" type="button">
    <svg width="16" height="16" viewBox="0 0 52 52" fill="none" aria-hidden="true">
      <circle cx="26" cy="26" r="20" stroke="currentColor" stroke-width="2" opacity="0.5"/>
      <circle cx="26" cy="26" r="11" stroke="currentColor" stroke-width="2.5" opacity="0.8"/>
      <circle cx="26" cy="26" r="5" fill="currentColor"/>
    </svg>
  </button>
  <button class="gp-btn gp-center-btn" data-ctrl="start" aria-label="Start" type="button">
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2"/>
      <line x1="8" y1="12" x2="16" y2="12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
      <line x1="12" y1="8" x2="12" y2="16" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
    </svg>
  </button>
</div>

<!-- ── Right stick ─────────────────────────── -->
<div class="gp-rstick-zone" data-ctrl="rstick" aria-label="Sağ analog çubuk">
  <div class="gp-stick-ring" id="gp-rstick-ring">
    <div class="gp-stick-knob" id="gp-rstick-knob"></div>
  </div>
  <button class="gp-stick-press gp-btn" data-ctrl="r3" aria-label="R3" type="button"></button>
</div>

<!-- ── Right side ──────────────────────────── -->
<div class="gp-right">
  <div class="gp-triggers-right">
    <button class="gp-btn gp-shoulder" data-ctrl="r1" aria-label="R1" type="button">
      <span class="gp-label">R1</span>
    </button>
    <button class="gp-btn gp-trigger" data-ctrl="rt" aria-label="R2" type="button">
      <span class="gp-label">R2</span>
      <div class="gp-trigger-fill" id="gp-rt-fill"></div>
    </button>
  </div>
  <div class="gp-face">
    <button class="gp-btn gp-face-btn gp-y" data-ctrl="y" aria-label="Y" type="button"><span>Y</span></button>
    <div class="gp-face-middle">
      <button class="gp-btn gp-face-btn gp-x" data-ctrl="x" aria-label="X" type="button"><span>X</span></button>
      <button class="gp-btn gp-face-btn gp-b" data-ctrl="b" aria-label="B" type="button"><span>B</span></button>
    </div>
    <button class="gp-btn gp-face-btn gp-a" data-ctrl="a" aria-label="A" type="button"><span>A</span></button>
  </div>
</div>
	`.trim();
}

// ── Touch wiring ──────────────────────────────────────────────────────────────

function _wireTouches() {
	if (!_layerEl) return;

	// Use pointer events (covers both touch and mouse for test)
	_layerEl.addEventListener('pointerdown',  _onPointerDown,  { passive: false });
	_layerEl.addEventListener('pointermove',  _onPointerMove,  { passive: false });
	_layerEl.addEventListener('pointerup',    _onPointerUp,    { passive: false });
	_layerEl.addEventListener('pointercancel',_onPointerCancel,{ passive: false });

	// Prevent context menu on long-press
	_layerEl.addEventListener('contextmenu', (e) => e.preventDefault());
}

// Track active pointers
const _activePointers = new Map(); // pointerId → { ctrl, el }

function _onPointerDown(e) {
	e.preventDefault();

	const el   = e.target.closest('[data-ctrl]');
	if (!el) return;
	const ctrl = el.dataset.ctrl;
	if (!ctrl) return;

	// Capture the pointer on this element
	try { el.setPointerCapture(e.pointerId); } catch (_) {}
	_activePointers.set(e.pointerId, { ctrl, el, startX: e.clientX, startY: e.clientY });

	_handleControlDown(ctrl, e.pointerId, e.clientX, e.clientY, el);
}

function _onPointerMove(e) {
	e.preventDefault();
	const info = _activePointers.get(e.pointerId);
	if (!info) return;
	_handleControlMove(info.ctrl, e.pointerId, e.clientX, e.clientY, info.startX, info.startY);
}

function _onPointerUp(e) {
	e.preventDefault();
	const info = _activePointers.get(e.pointerId);
	if (!info) return;
	_activePointers.delete(e.pointerId);
	try { info.el.releasePointerCapture(e.pointerId); } catch (_) {}
	_handleControlUp(info.ctrl, e.pointerId);
}

function _onPointerCancel(e) {
	const info = _activePointers.get(e.pointerId);
	if (!info) return;
	_activePointers.delete(e.pointerId);
	_handleControlUp(info.ctrl, e.pointerId);
}

// ── Control event dispatch ─────────────────────────────────────────────────────

function _handleControlDown(ctrl, pointerId, cx, cy, el) {
	switch (ctrl) {
		// ── Digital buttons ────────────────────────────────────────
		case 'a':          _setBtn(BTN.A,         true); break;
		case 'b':          _setBtn(BTN.B,         true); break;
		case 'x':          _setBtn(BTN.X,         true); break;
		case 'y':          _setBtn(BTN.Y,         true); break;
		case 'l1':         _setBtn(BTN.L1,        true); break;
		case 'r1':         _setBtn(BTN.R1,        true); break;
		case 'start':      _setBtn(BTN.START,     true); break;
		case 'select':     _setBtn(BTN.SELECT,    true); break;
		case 'guide':      _setBtn(BTN.SELECT,    true); break; // map guide → select
		case 'dpad-up':    _setBtn(BTN.DPAD_UP,   true); break;
		case 'dpad-down':  _setBtn(BTN.DPAD_DOWN, true); break;
		case 'dpad-left':  _setBtn(BTN.DPAD_LEFT, true); break;
		case 'dpad-right': _setBtn(BTN.DPAD_RIGHT,true); break;
		case 'l3':         _setBtn(BTN.L3,        true); break;
		case 'r3':         _setBtn(BTN.R3,        true); break;

		// ── Left stick ────────────────────────────────────────────
		case 'lstick': {
			const zone = el.closest ? el.closest('.gp-lstick-zone') : null;
			const rect = zone ? zone.getBoundingClientRect() : el.getBoundingClientRect();
			_lstick.active    = true;
			_lstick.pointerId = pointerId;
			_lstick.originX   = rect.left + rect.width  / 2;
			_lstick.originY   = rect.top  + rect.height / 2;
			break;
		}

		// ── Right stick ───────────────────────────────────────────
		case 'rstick': {
			const zone = el.closest ? el.closest('.gp-rstick-zone') : null;
			const rect = zone ? zone.getBoundingClientRect() : el.getBoundingClientRect();
			_rstick.active    = true;
			_rstick.pointerId = pointerId;
			_rstick.originX   = rect.left + rect.width  / 2;
			_rstick.originY   = rect.top  + rect.height / 2;
			break;
		}

		// ── Left trigger (analog vertical drag) ───────────────────
		case 'lt': {
			_ltrigger.active    = true;
			_ltrigger.pointerId = pointerId;
			_ltrigger.originY   = cy;
			_lt = 255; // full press on down while dragging hasn't started
			_updateTriggerFill('lt', 1);
			break;
		}

		// ── Right trigger ─────────────────────────────────────────
		case 'rt': {
			_rtrigger.active    = true;
			_rtrigger.pointerId = pointerId;
			_rtrigger.originY   = cy;
			_rt = 255;
			_updateTriggerFill('rt', 1);
			break;
		}

		default: return;
	}
	_sendOnscreen();
	_haptic();
}

function _handleControlMove(ctrl, pointerId, cx, cy, startX, startY) {
	switch (ctrl) {
		case 'lstick': {
			if (!_lstick.active || _lstick.pointerId !== pointerId) return;
			const dx = cx - _lstick.originX;
			const dy = cy - _lstick.originY;
			const axes = _stickDeltaToAxes(dx, dy);
			_lx = axes.x;
			_ly = axes.y;
			_updateStickKnob('lstick', dx, dy);
			break;
		}
		case 'rstick': {
			if (!_rstick.active || _rstick.pointerId !== pointerId) return;
			const dx = cx - _rstick.originX;
			const dy = cy - _rstick.originY;
			const axes = _stickDeltaToAxes(dx, dy);
			_rx = axes.x;
			_ry = axes.y;
			_updateStickKnob('rstick', dx, dy);
			break;
		}
		case 'lt': {
			if (!_ltrigger.active || _ltrigger.pointerId !== pointerId) return;
			const dy = cy - _ltrigger.originY;
			_lt = _triggerDeltaToValue(dy);
			_updateTriggerFill('lt', _lt / 255);
			break;
		}
		case 'rt': {
			if (!_rtrigger.active || _rtrigger.pointerId !== pointerId) return;
			const dy = cy - _rtrigger.originY;
			_rt = _triggerDeltaToValue(dy);
			_updateTriggerFill('rt', _rt / 255);
			break;
		}
		default: return;
	}
	_sendOnscreen();
}

function _handleControlUp(ctrl, pointerId) {
	switch (ctrl) {
		case 'a':          _setBtn(BTN.A,         false); break;
		case 'b':          _setBtn(BTN.B,         false); break;
		case 'x':          _setBtn(BTN.X,         false); break;
		case 'y':          _setBtn(BTN.Y,         false); break;
		case 'l1':         _setBtn(BTN.L1,        false); break;
		case 'r1':         _setBtn(BTN.R1,        false); break;
		case 'start':      _setBtn(BTN.START,     false); break;
		case 'select':     _setBtn(BTN.SELECT,    false); break;
		case 'guide':      _setBtn(BTN.SELECT,    false); break;
		case 'dpad-up':    _setBtn(BTN.DPAD_UP,   false); break;
		case 'dpad-down':  _setBtn(BTN.DPAD_DOWN, false); break;
		case 'dpad-left':  _setBtn(BTN.DPAD_LEFT, false); break;
		case 'dpad-right': _setBtn(BTN.DPAD_RIGHT,false); break;
		case 'l3':         _setBtn(BTN.L3,        false); break;
		case 'r3':         _setBtn(BTN.R3,        false); break;

		case 'lstick': {
			if (_lstick.pointerId !== pointerId) return;
			_lstick.active    = false;
			_lstick.pointerId = -1;
			_lx = 0; _ly = 0;
			_updateStickKnob('lstick', 0, 0);
			break;
		}
		case 'rstick': {
			if (_rstick.pointerId !== pointerId) return;
			_rstick.active    = false;
			_rstick.pointerId = -1;
			_rx = 0; _ry = 0;
			_updateStickKnob('rstick', 0, 0);
			break;
		}
		case 'lt': {
			if (_ltrigger.pointerId !== pointerId) return;
			_ltrigger.active    = false;
			_ltrigger.pointerId = -1;
			_lt = 0;
			_updateTriggerFill('lt', 0);
			break;
		}
		case 'rt': {
			if (_rtrigger.pointerId !== pointerId) return;
			_rtrigger.active    = false;
			_rtrigger.pointerId = -1;
			_rt = 0;
			_updateTriggerFill('rt', 0);
			break;
		}
		default: return;
	}
	_sendOnscreen();
}

// ── Visual feedback helpers ────────────────────────────────────────────────────

/**
 * Move the stick knob visually within its ring.
 * @param {'lstick'|'rstick'} which
 * @param {number} dx  CSS px delta from center (clamped to STICK_RADIUS)
 * @param {number} dy
 */
function _updateStickKnob(which, dx, dy) {
	const knobId = which === 'lstick' ? 'gp-lstick-knob' : 'gp-rstick-knob';
	const knob   = document.getElementById(knobId);
	if (!knob) return;

	const dist  = Math.sqrt(dx * dx + dy * dy);
	const maxR  = STICK_RADIUS;
	const scale = dist > maxR ? maxR / dist : 1;
	const x     = dx * scale;
	const y     = dy * scale;

	knob.style.transform = `translate(calc(-50% + ${x}px), calc(-50% + ${y}px))`;
}

/**
 * Update the trigger fill bar to show analog depth.
 * @param {'lt'|'rt'} which
 * @param {number} fraction  0..1
 */
function _updateTriggerFill(which, fraction) {
	const fillId = which === 'lt' ? 'gp-lt-fill' : 'gp-rt-fill';
	const fill   = document.getElementById(fillId);
	if (!fill) return;
	fill.style.height = `${Math.round(fraction * 100)}%`;
}

/**
 * Brief haptic tap on button press (on-screen).
 */
function _haptic() {
	if (navigator.vibrate) navigator.vibrate(4);
}

// ── Status chip ───────────────────────────────────────────────────────────────

function _updateStatusChip() {
	let chip = document.getElementById('gp-status-chip');
	if (!chip) {
		chip = document.createElement('div');
		chip.id        = 'gp-status-chip';
		chip.className = 'gp-status-chip';
		chip.setAttribute('aria-live', 'polite');
		const bar = document.getElementById('bar');
		if (bar) bar.prepend(chip);
	}

	if (!_mounted) {
		chip.style.display = 'none';
		return;
	}

	chip.style.display = '';
	const count = _physicalPadCount;
	if (count > 0) {
		chip.innerHTML = `
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<rect x="3" y="7" width="18" height="12" rx="3" stroke="currentColor" stroke-width="2"/>
				<circle cx="8" cy="13" r="1.5" fill="currentColor"/>
				<circle cx="12" cy="11" r="1.5" fill="currentColor"/>
				<circle cx="12" cy="15" r="1.5" fill="currentColor"/>
				<circle cx="16" cy="13" r="1.5" fill="currentColor"/>
				<path d="M9 4h6" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
				<path d="M12 4v3" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
			</svg>
			<span class="mono">${count}</span>
		`;
		chip.title = `${count} fiziksel kontrolcü bağlı`;
	} else {
		chip.innerHTML = `
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true" style="opacity:0.4">
				<rect x="3" y="7" width="18" height="12" rx="3" stroke="currentColor" stroke-width="2"/>
			</svg>
		`;
		chip.title = 'Fiziksel kontrolcü bağlı değil';
	}
}

// ── Overlay card rendering ─────────────────────────────────────────────────────

/**
 * Render the controller overlay card (toggle + physical pad status).
 * @param {HTMLElement} el  — the overlay card container
 */
function _renderCard(el) {
	if (!el) return;

	// Translate helper with inline fallbacks
	const _tr = (key, fb) => { try { const v = t(key); return v !== key ? v : fb; } catch(_) { return fb; } };

	el.innerHTML = `
		<p class="overlay-card-label">${_tr('m.overlay.sec.controllers', 'Kontrolcüler')}</p>

		<!-- Toggle the on-screen gamepad -->
		<div class="overlay-row">
			<span class="label">${_tr('m.gamepad.show', 'Sanal kol')}</span>
			<label class="overlay-toggle" aria-label="${_tr('m.gamepad.show', 'Sanal kol')}">
				<input type="checkbox" id="gp-card-toggle" ${_visible ? 'checked' : ''}>
				<span class="track" aria-hidden="true"></span>
				<span class="thumb" aria-hidden="true"></span>
			</label>
		</div>

		<!-- Physical pad count indicator -->
		<div class="overlay-row" id="gp-card-pads">
			<span class="label">Bağlı kontrolcü</span>
			<span class="overlay-chip" id="gp-card-pad-count">${_physicalPadCount > 0 ? _physicalPadCount + ' adet' : 'Yok'}</span>
		</div>

		<!-- Rumble indicator (cyan flash on rumble event) -->
		<div class="overlay-row gp-card-rumble-row" id="gp-card-rumble-row" aria-live="polite">
			<span class="label">${_tr('m.gamepad.rumble', 'Titreşim')}</span>
			<span class="gp-rumble-dot" id="gp-card-rumble-dot" aria-hidden="true"></span>
		</div>
	`;

	// Wire the toggle
	const toggleEl = el.querySelector('#gp-card-toggle');
	toggleEl?.addEventListener('change', () => {
		_visible = toggleEl.checked;
		if (_visible) _showLayer(); else _hideLayer();
	});
}

/**
 * Flash the rumble dot in the overlay card when a rumble event arrives.
 */
function _flashRumbleDot() {
	const dot = document.getElementById('gp-card-rumble-dot');
	if (!dot) return;
	dot.classList.add('active');
	setTimeout(() => dot.classList.remove('active'), 400);
}

// ── Styles ────────────────────────────────────────────────────────────────────

function _injectStyles() {
	if (document.getElementById('gamepad-layer-styles')) return;
	const s = document.createElement('style');
	s.id = 'gamepad-layer-styles';
	s.textContent = `
/* ════════════════════════════════════════════════════════
   Gamepad layer — DT-onscreen-gamepad
   Cyan game-mode brand; fixed over the transparent surface.
   Multi-touch via pointer events on individual controls.
   All interactive targets ≥ 44px (--touch-min).
   ════════════════════════════════════════════════════════ */

/* ── Layer container ──────────────────────────────────── */
#gamepad-layer {
	position: fixed;
	inset: 0;
	/* Sit BELOW the overlay dock (z:15) and ABOVE the video surface */
	z-index: 10;
	pointer-events: none;  /* pass-through except on controls */
	display: flex;
	align-items: flex-end;
	justify-content: space-between;
	padding: 0 4px calc(var(--safe-bottom, env(safe-area-inset-bottom, 0px)) + var(--nav-h, 64px) + 4px);
	/* Only shown in landscape (portrait would be unplayable) */
	user-select: none;
	-webkit-user-select: none;
	touch-action: none;
}

/* ── Shared button base ──────────────────────────────── */
.gp-btn {
	pointer-events: auto;
	background: oklch(0 0 0 / 0.38);
	border: 1.5px solid oklch(0.62 0.15 215 / 0.55); /* cyan border */
	color: oklch(0.97 0.04 215);
	border-radius: var(--r, 13px);
	display: flex;
	align-items: center;
	justify-content: center;
	cursor: pointer;
	-webkit-tap-highlight-color: transparent;
	transition: background 80ms, transform 60ms;
	touch-action: none;
	font-family: var(--font-sans, sans-serif);
	font-weight: 700;
	position: relative;
	overflow: hidden;
}
.gp-btn:active,
.gp-btn.pressed {
	background: oklch(0.62 0.15 215 / 0.42);
	transform: scale(0.92);
}

/* ── Left / right side columns ──────────────────────── */
.gp-left,
.gp-right {
	display: flex;
	flex-direction: column;
	gap: 8px;
	align-items: center;
}

/* ── Triggers + shoulders row ───────────────────────── */
.gp-triggers-left,
.gp-triggers-right {
	display: flex;
	gap: 6px;
	align-items: flex-end;
}

/* Shoulder buttons (L1 / R1) */
.gp-shoulder {
	width: 56px;
	height: 36px;
	border-radius: var(--r-sm, 9px);
	font-size: 12px;
	font-weight: 700;
}

/* Trigger buttons (L2 / R2) — analog depth fill */
.gp-trigger {
	width: 52px;
	height: 60px;
	border-radius: var(--r, 13px);
	font-size: 12px;
	flex-direction: column;
	gap: 2px;
}
.gp-trigger-fill {
	position: absolute;
	bottom: 0; left: 0; right: 0;
	height: 0;
	background: oklch(0.62 0.15 215 / 0.55);
	border-radius: 0 0 calc(var(--r, 13px) - 1px) calc(var(--r, 13px) - 1px);
	transition: height 40ms linear;
	pointer-events: none;
}
.gp-label {
	font-size: 11px;
	font-weight: 700;
	letter-spacing: 0.04em;
	position: relative; /* above fill */
	z-index: 1;
}

/* ── D-pad ───────────────────────────────────────────── */
.gp-dpad {
	display: grid;
	grid-template-columns: 48px 20px 48px;
	grid-template-rows:    48px 20px 48px;
	gap: 2px;
	pointer-events: auto;
}
.gp-dpad-up    { grid-column: 2; grid-row: 1; }
.gp-dpad-left  { grid-column: 1; grid-row: 2; }
.gp-dpad-center{ grid-column: 2; grid-row: 2; background: oklch(0 0 0 / 0.2); border-radius: var(--r-xs, 6px); }
.gp-dpad-right { grid-column: 3; grid-row: 2; }
.gp-dpad-down  { grid-column: 2; grid-row: 3; }

.gp-dpad-btn {
	width: 100%;
	height: 100%;
	min-width: 44px;
	min-height: 44px;
	font-size: 14px;
	border-radius: var(--r-sm, 9px);
}

/* ── Analog sticks ────────────────────────────────────── */
.gp-lstick-zone,
.gp-rstick-zone {
	width: 120px;
	height: 120px;
	position: relative;
	pointer-events: auto;
	margin: 0 4px;
	flex: none;
	align-self: flex-end;
	margin-bottom: 8px;
}

/* The outermost zone is also the drag capture area */
.gp-lstick-zone[data-ctrl],
.gp-rstick-zone[data-ctrl] {
	border-radius: 50%;
	background: oklch(0 0 0 / 0.18);
	border: 1.5px solid oklch(0.62 0.15 215 / 0.25);
}

.gp-stick-ring {
	position: absolute;
	inset: 10px;
	border-radius: 50%;
	background: oklch(0 0 0 / 0.28);
	border: 2px solid oklch(0.62 0.15 215 / 0.4);
}

.gp-stick-knob {
	position: absolute;
	top: 50%; left: 50%;
	width: 40px;
	height: 40px;
	border-radius: 50%;
	background: oklch(0.62 0.15 215 / 0.75);
	border: 2px solid oklch(0.62 0.15 215);
	box-shadow: 0 0 12px oklch(0.62 0.15 215 / 0.5);
	transform: translate(-50%, -50%);
	transition: transform 20ms linear;
	pointer-events: none;
}

/* L3 / R3 tap area (overlaid, transparent — entire zone is the press area) */
.gp-stick-press {
	position: absolute;
	inset: 0;
	border-radius: 50%;
	background: transparent;
	border: none;
	pointer-events: auto;
	z-index: 1;
}
.gp-stick-press:active {
	background: oklch(0.62 0.15 215 / 0.15);
	transform: none;
}

/* ── Center buttons (Select / Guide / Start) ─────────── */
.gp-center {
	display: flex;
	flex-direction: row;
	gap: 8px;
	align-items: center;
	align-self: center;
	pointer-events: auto;
}

.gp-center-btn {
	width: 44px;
	height: 44px;
	border-radius: 50%;
	font-size: 11px;
	background: oklch(0 0 0 / 0.45);
}

.gp-guide-btn {
	width: 52px;
	height: 52px;
	border-radius: 50%;
	background: oklch(0.62 0.15 215 / 0.22);
	border-color: oklch(0.62 0.15 215 / 0.7);
	box-shadow: 0 0 16px oklch(0.62 0.15 215 / 0.3);
}
.gp-guide-btn:active {
	background: oklch(0.62 0.15 215 / 0.5);
	box-shadow: 0 0 24px oklch(0.62 0.15 215 / 0.6);
}

/* ── Face buttons (ABXY) ─────────────────────────────── */
.gp-face {
	display: flex;
	flex-direction: column;
	align-items: center;
	gap: 4px;
}
.gp-face-middle {
	display: flex;
	gap: 4px;
}
.gp-face-btn {
	width: 52px;
	height: 52px;
	border-radius: 50%;
	font-size: 15px;
	font-weight: 700;
}

/* Face button colors (Moonlight-style) */
.gp-a { background: oklch(0 0 0 / 0.35); border-color: oklch(0.55 0.2 140 / 0.7); color: oklch(0.7 0.18 140); }
.gp-b { background: oklch(0 0 0 / 0.35); border-color: oklch(0.55 0.2 27  / 0.7); color: oklch(0.75 0.18 27 ); }
.gp-x { background: oklch(0 0 0 / 0.35); border-color: oklch(0.55 0.2 250 / 0.7); color: oklch(0.7 0.18 250); }
.gp-y { background: oklch(0 0 0 / 0.35); border-color: oklch(0.55 0.18 80 / 0.7); color: oklch(0.75 0.16 80 ); }

.gp-a:active { background: oklch(0.55 0.2  140 / 0.5); }
.gp-b:active { background: oklch(0.55 0.2  27  / 0.5); }
.gp-x:active { background: oklch(0.55 0.2  250 / 0.5); }
.gp-y:active { background: oklch(0.55 0.18 80  / 0.5); }

/* ── Status chip in the in-session bar ───────────────── */
.gp-status-chip {
	display: flex;
	align-items: center;
	gap: 4px;
	color: oklch(0.75 0.1 215);
	font-size: 12px;
	min-height: var(--touch-min, 44px);
	padding: 0 4px;
}
.gp-status-chip .mono {
	font-family: var(--font-mono, 'JetBrains Mono', monospace);
	font-size: 12px;
	font-weight: 600;
}

/* ── Overlay card elements ───────────────────────────── */
.gp-card-rumble-row {
	margin-top: 4px;
}
.gp-rumble-dot {
	width: 12px;
	height: 12px;
	border-radius: 50%;
	background: oklch(0.62 0.15 215 / 0.25);
	border: 1.5px solid oklch(0.62 0.15 215 / 0.5);
	transition: background 80ms, box-shadow 80ms;
	flex: none;
}
.gp-rumble-dot.active {
	background: oklch(0.62 0.15 215);
	box-shadow: 0 0 8px oklch(0.62 0.15 215 / 0.7);
}

/* ── Landscape-only hint: show layer only in landscape ── */
@media (orientation: portrait) {
	#gamepad-layer {
		/* Still display in portrait but with a hint overlay */
		align-items: center;
		justify-content: center;
	}
	#gamepad-layer > * {
		opacity: 0.3;
		pointer-events: none;
	}
	#gamepad-layer::after {
		content: 'Oyun kolu için yatay modu kullanın';
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		color: oklch(0.92 0 0);
		font-size: 14px;
		font-weight: 600;
		font-family: var(--font-sans, sans-serif);
		text-shadow: 0 1px 4px oklch(0 0 0 / 0.6);
		pointer-events: none;
	}
}
	`;
	document.head.appendChild(s);
}

// ── Boot ──────────────────────────────────────────────────────────────────────

// Wire the bus to mount/unmount when the session starts/ends
function _wireBus() {
	const bus = window.__pulsarBus;
	if (!bus) return;

	bus.on('session-started', ({ slot, mode }) => {
		if (mode !== 'game') return;
		mount(slot ?? 0);
	});

	bus.on('session-ended', () => {
		unmount();
	});
}

// Self-register with the overlay registry
_selfRegister();

// Wire bus (retry pattern same as overlay.js)
if (window.__pulsarBus) {
	_wireBus();
} else {
	const _tryWire = () => {
		if (window.__pulsarBus) {
			_wireBus();
		}
	};
	if (document.readyState === 'loading') {
		document.addEventListener('DOMContentLoaded', _tryWire);
	} else {
		setTimeout(_tryWire, 0);
	}
}
