/**
 * hud.js — Performance HUD card (W3-hud-js + W5-quality-adv)
 *
 * DT-perf-hud: Compact translucent mono strip showing fps / Mbps / RTT / decode
 * latency, readable at arm's length, safe-area-aware; cyan in game, thin in
 * remote. Plus a first-frame "ilk kare bekleniyor" pulse loader and a mid-session
 * "Yayın durdu" stall overlay over the transparent surface. Collapsible.
 *
 * W5-quality-adv addition:
 *   Listens to the `decoder-error` Tauri event (emitted by the plugin's
 *   decoderState path, §2.8) and shows the "yeniden eşitleniyor" resync overlay.
 *   The overlay auto-dismisses after RESYNC_DISMISS_MS (5 s) — long enough for
 *   the W5-rust-session keyframe nudge + MediaCodec reconfigure to complete.
 *   The overlay is also dismissible on the next `play-firstframe` event.
 *   `showResyncOverlay` / `hideResyncOverlay` remain exported so other modules
 *   can trigger the overlay programmatically (e.g. from quality.js on HDR change).
 *
 * Self-registration: calls registerCard() at import time so overlay.js mounts
 * this card in both remote and game modes (section: 'gauges').
 *
 * Events consumed (Tauri):
 *   play-stats      { slot, fps, mbps, transport }     — ~1s cadence from read loop
 *   play-vstats     { slot, decodeMs }                 — optional decode latency from plugin
 *   play-stall      { slot, stalled: bool }            — video stall state
 *   play-firstframe { slot }                           — first decoded frame arrived
 *   decoder-error   { slot }                           — MediaCodec exception / W5-native
 *
 * Tauri commands invoked:
 *   request_keyframe { slot }  — on decoder-error, nudge the host for a fresh IDR
 *                                (A5) so a decode error recovers in ~1 frame.
 *
 * Design choices:
 *   - The HUD strip (.hud-strip) is always in the DOM when in-session and is
 *     lightweight (single fixed div).
 *   - The overlay card (registered via registerCard) provides the collapsible
 *     expanded view with all metrics.
 *   - Stall overlay: a full-screen translucent frost layer + "Yayın durdu" text
 *     + pulsing ring, matching the connecting screen aesthetic.
 *   - Resync overlay: mirrors the stall overlay with a rotate-arrow icon and
 *     "Yeniden eşitleniyor…" text. Auto-dismiss after RESYNC_DISMISS_MS.
 *   - First-frame loader: covers the native video surface until the first AU
 *     arrives. Fades out on play-firstframe.
 *   - Both remote (indigo) and game (cyan) personalities handled via var(--brand).
 *
 * Depends on: tauri.js (listen), i18n.js (t), overlay.js (registerCard — lazy
 * via window.__pulsarRegisterCard set by overlay.js when it mounts).
 */

import { listen, invoke } from '../tauri.js';
import { t }              from '../i18n.js';

// ---------------------------------------------------------------------------
// Constants (W5-quality-adv)
// ---------------------------------------------------------------------------

/**
 * Auto-dismiss timeout for the resync overlay (ms).
 * Long enough for the W5-rust-session keyframe nudge + MediaCodec reconfigure.
 */
const RESYNC_DISMISS_MS = 5000;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/** @type {{ slot:number, fps:number, mbps:number, transport:string, decodeMs:number|null, stalled:boolean, firstFrame:boolean } | null} */
let _state = null;

/** Whether the expanded card is collapsed by the user. */
let _collapsed = false;

/** Per-slot stats keyed by slot number. */
const _perSlot = new Map();

/** Auto-dismiss timer for the resync overlay (W5-quality-adv). */
let _resyncTimer = null;

/**
 * A5: throttle for the decoder-error keyframe nudge (request_keyframe). A burst of
 * decoder-error events must not spam the host with MediaNacks.
 */
let _lastKeyframeReq = 0;
const KEYFRAME_REQ_MIN_MS = 500;

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------------------
// HUD strip (always-visible compact bar when in-session)
// ---------------------------------------------------------------------------

/**
 * Get-or-create the .hud-strip element in the DOM.
 * Appended once after DOMContentLoaded; subsequent calls return the same node.
 * @returns {HTMLElement}
 */
function getOrCreateStrip() {
	let el = document.getElementById('hud-strip');
	if (el) return el;

	el = document.createElement('div');
	el.id         = 'hud-strip';
	el.className  = 'hud-strip';
	el.setAttribute('role', 'status');
	el.setAttribute('aria-live', 'polite');
	el.setAttribute('aria-label', 'Performans göstergeleri');
	// Touch: tapping the strip toggles the expanded card
	el.style.pointerEvents = 'auto';
	el.addEventListener('click', () => {
		_collapsed = !_collapsed;
		_renderCard();
	});
	// Accessible label
	el.title = 'Performans göstergeleri — genişletmek için dokun';
	// Honour the Settings "Performance HUD" pref (client-only).
	if (_hudPref('hudVisible') === false) el.style.display = 'none';
	document.body.appendChild(el);
	return el;
}

/** Best-effort read of a client pref without a hard dependency on the store. */
function _hudPref(k) {
	try {
		return JSON.parse(localStorage.getItem('pulsar.prefs.v1') || '{}')[k];
	} catch (_) {
		return undefined;
	}
}

/**
 * Render the compact strip with current stats.
 */
function renderStrip() {
	const strip = getOrCreateStrip();
	const s = _state;

	if (!s) {
		strip.innerHTML = '';
		return;
	}

	/** Format transport label as short mono text */
	const transportLabel = s.transport === 'direct' ? 'P2P' : s.transport === 'relay' ? 'RLY' : '';

	/** Build metric fragments. Empty slots are omitted. */
	const parts = [];

	// FPS
	if (s.fps != null && s.fps >= 0) {
		parts.push(
			`<span class="hud-val" aria-label="${s.fps.toFixed(0)} FPS">${s.fps.toFixed(0)}</span>` +
			`<span class="hud-lbl">fps</span>`
		);
	}

	// Latency (RTT, from play-rtt) — network round-trip, the key responsiveness number
	if (s.rtt != null && s.rtt >= 0) {
		parts.push(
			`<span class="hud-val" aria-label="${s.rtt.toFixed(0)} milisaniye gecikme">${s.rtt.toFixed(0)}</span>` +
			`<span class="hud-lbl">ms</span>`
		);
	}

	// Mbps
	if (s.mbps != null && s.mbps >= 0) {
		const mbpsStr = s.mbps >= 10 ? s.mbps.toFixed(1) : s.mbps.toFixed(2);
		parts.push(
			`<span class="hud-val" aria-label="${mbpsStr} megabit per saniye">${mbpsStr}</span>` +
			`<span class="hud-lbl">Mbps</span>`
		);
	}

	// Decode latency (from play-vstats)
	if (s.decodeMs != null) {
		parts.push(
			`<span class="hud-val" aria-label="${s.decodeMs} milisaniye çözme">${s.decodeMs}</span>` +
			`<span class="hud-lbl">ms</span>`
		);
	}

	// Transport badge
	if (transportLabel) {
		parts.push(`<span class="hud-badge" aria-label="${s.transport === 'direct' ? 'Doğrudan P2P bağlantı' : 'Relay bağlantısı'}">${transportLabel}</span>`);
	}

	// Build inner HTML with separators between parts
	let inner = '';
	for (let i = 0; i < parts.length; i++) {
		if (i > 0) inner += '<span class="hud-sep" aria-hidden="true"></span>';
		inner += `<span class="hud-pair">${parts[i]}</span>`;
	}

	strip.innerHTML = inner || '<span class="hud-lbl hud-waiting">…</span>';
}

// ---------------------------------------------------------------------------
// First-frame loader overlay
// ---------------------------------------------------------------------------

function showFirstFrameLoader(slot) {
	if ($('hud-loader')) return; // already showing
	// Don't (re)show once frames are already flowing — prevents a stuck/flickering
	// loader when session-started races behind the first play-stats/play-firstframe.
	const s = _perSlot.get(slot) || _state;
	if (s && (s.firstFrame || (s.fps || 0) > 0)) return;

	const el = document.createElement('div');
	el.id        = 'hud-loader';
	el.className = 'hud-loader';
	el.setAttribute('role', 'status');
	el.setAttribute('aria-label', t('session.connecting'));
	el.innerHTML = `
		<div class="hud-loader-rings" aria-hidden="true">
			<div class="hud-ring hud-ring-1"></div>
			<div class="hud-ring hud-ring-2"></div>
			<div class="hud-ring hud-ring-3"></div>
		</div>
		<p class="hud-loader-text">${t('session.connecting')}</p>
		<p class="hud-loader-sub">${t('session.waiting')}</p>
	`;
	document.body.appendChild(el);
}

function hideFirstFrameLoader() {
	const el = $('hud-loader');
	if (!el) return;
	el.classList.add('hud-loader-out');
	// Remove after fade
	el.addEventListener('transitionend', () => el.remove(), { once: true });
	setTimeout(() => el.remove(), 500);
}

// ---------------------------------------------------------------------------
// Stall overlay
// ---------------------------------------------------------------------------

function showStallOverlay() {
	if ($('hud-stall')) return;

	const el = document.createElement('div');
	el.id        = 'hud-stall';
	el.className = 'hud-stall';
	el.setAttribute('role', 'alert');
	el.setAttribute('aria-live', 'assertive');
	el.innerHTML = `
		<div class="hud-stall-ring" aria-hidden="true"></div>
		<svg class="hud-stall-icon" width="40" height="40" viewBox="0 0 24 24" fill="none" aria-hidden="true">
			<circle cx="12" cy="12" r="9.5" stroke="currentColor" stroke-width="1.5" opacity="0.3"/>
			<path d="M12 7v5l3 3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
		</svg>
		<p class="hud-stall-title">${t('m.session.stalled')}</p>
	`;
	document.body.appendChild(el);

	// Animate in
	requestAnimationFrame(() => el.classList.add('hud-stall-in'));
}

function hideStallOverlay() {
	const el = $('hud-stall');
	if (!el) return;
	el.classList.remove('hud-stall-in');
	el.addEventListener('transitionend', () => el.remove(), { once: true });
	setTimeout(() => el.remove(), 450);
}

// ---------------------------------------------------------------------------
// Expanded overlay card (registered with overlay.js)
// ---------------------------------------------------------------------------

/**
 * Build and return the HTML for the expanded metrics card.
 * @returns {string}
 */
function buildCardHTML() {
	const s = _state;

	if (!s) {
		return `<p class="hud-card-empty">${t('m.loading')}</p>`;
	}

	const rows = [];

	// FPS
	if (s.fps != null && s.fps >= 0) {
		rows.push({ label: 'FPS', value: s.fps.toFixed(1) });
	}

	// Latency (RTT)
	if (s.rtt != null && s.rtt >= 0) {
		rows.push({ label: 'Gecikme', value: `${s.rtt} ms` });
	}

	// Mbps
	if (s.mbps != null && s.mbps >= 0) {
		const mbpsStr = s.mbps >= 10 ? s.mbps.toFixed(1) : s.mbps.toFixed(2);
		rows.push({ label: 'Mbps', value: mbpsStr });
	}

	// Decode latency
	if (s.decodeMs != null) {
		rows.push({ label: t('session.statDecodeMs'), value: `${s.decodeMs} ms` });
	}

	// Transport
	if (s.transport) {
		const tLabel = s.transport === 'direct'
			? t('m.session.transport.direct')
			: t('m.session.transport.relay');
		rows.push({ label: t('session.statNet'), value: tLabel });
	}

	const rowsHTML = rows.map(({ label, value }) => `
		<div class="hud-row">
			<span class="hud-row-label">${label}</span>
			<span class="hud-row-value">${value}</span>
		</div>
	`).join('');

	const collapseLabel = _collapsed ? 'Genişlet' : 'Daralt';
	const collapseIcon  = _collapsed
		? `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><polyline points="6 9 12 15 18 9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`
		: `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><polyline points="18 15 12 9 6 15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

	return `
		<div class="hud-card-header">
			<span class="hud-card-title">
				<svg class="hud-card-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
					<polyline points="22 12 18 12 15 21 9 3 6 12 2 12" stroke="currentColor"
					          stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
				</svg>
				Performans
			</span>
			<button class="hud-collapse-btn icon-btn" id="hud-collapse-btn"
			        aria-label="${collapseLabel}" type="button">
				${collapseIcon}
			</button>
		</div>
		${_collapsed ? '<div class="hud-card-collapsed-hint">Dokunarak genişlet</div>' : `<div class="hud-card-rows">${rowsHTML}</div>`}
	`;
}

/**
 * Re-render the overlay card if it is currently mounted.
 */
function _renderCard() {
	const container = $('hud-overlay-card-inner');
	if (!container) return;
	container.innerHTML = buildCardHTML();
	// Wire collapse button
	$('hud-collapse-btn')?.addEventListener('click', (e) => {
		e.stopPropagation();
		_collapsed = !_collapsed;
		_renderCard();
	});
}

// ---------------------------------------------------------------------------
// registerCard integration
// ---------------------------------------------------------------------------

/**
 * Mount function called by overlay.js when it renders this card.
 * @param {HTMLElement} el — the container element provided by overlay.js
 */
function mount(el) {
	el.id        = 'hud-overlay-card-inner';
	el.className = (el.className || '') + ' hud-card-body';
	_renderCard();
}

/**
 * Called by overlay.js each time the overlay is opened.
 * Re-render to ensure the latest stats are shown.
 */
function onShow() {
	_renderCard();
}

/**
 * Register this card with the overlay registry.
 * overlay.js exports registerCard() which is also exposed on window by the
 * overlay boot sequence. We guard with a lazy accessor so the import order
 * doesn't matter (hud.js may be imported before overlay.js runs registerCard).
 */
function tryRegister() {
	/** @type {Function|undefined} */
	const register =
		(typeof window !== 'undefined' && window.__pulsarRegisterCard)
			? window.__pulsarRegisterCard
			: null;

	if (register) {
		register({
			id:      'perf-hud',
			modes:   ['remote', 'game'],  // Both personalities show the HUD
			section: 'gauges',
			order:   0,           // First card in gauges section
			mount,
			onShow,
			label:   'Performans',
		});
	} else {
		// overlay.js hasn't registered __pulsarRegisterCard yet — retry once DOM loaded
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', () => {
				setTimeout(tryRegister, 0);
			});
		} else {
			// Already loaded — overlay.js will call our mount() later via the registry
			// when it boots. Nothing to do; overlay.js iterates all registered specs.
			// Since overlay.js may not exist yet in W3 rollout, no-op is safe.
		}
	}
}

// ---------------------------------------------------------------------------
// Stat update helpers
// ---------------------------------------------------------------------------

/**
 * Merge a partial stats update into the per-slot state, then update UI.
 * @param {number} slot
 * @param {Partial<{fps:number, mbps:number, transport:string, decodeMs:number|null}>} partial
 */
function _mergeStats(slot, partial) {
	let entry = _perSlot.get(slot);
	if (!entry) {
		entry = { slot, fps: 0, mbps: 0, transport: '', decodeMs: null, rtt: null, stalled: false, firstFrame: false };
		_perSlot.set(slot, entry);
	}
	Object.assign(entry, partial);

	// If this is the "active" slot (we pick slot 0 for now; W5 wires setActivePane),
	// update _state and re-render.
	const activeSlot = _getActiveSlot();
	if (slot === activeSlot || _state === null) {
		_state = entry;
		renderStrip();
		_renderCard();
	}
}

/**
 * Get the currently active slot index. Falls back to 0.
 * W5-session-js wires window.__pulsarActiveSlot.
 * @returns {number}
 */
function _getActiveSlot() {
	if (typeof window !== 'undefined' && typeof window.__pulsarActiveSlot === 'number') {
		return window.__pulsarActiveSlot;
	}
	return 0;
}

// ---------------------------------------------------------------------------
// Tauri event listeners
// ---------------------------------------------------------------------------

async function _initListeners() {
	// play-stats: ~1s cadence fps / mbps / transport from client.rs read loop
	await listen('play-stats', ({ slot, fps, mbps, transport }) => {
		_mergeStats(slot, { fps, mbps, transport });
		// Frames are flowing → make sure the first-frame loader is gone (covers the
		// case where play-firstframe raced ahead of this listener and was missed).
		if (fps > 0) hideFirstFrameLoader();
	});

	// play-vstats: optional decode latency from the plugin (decoderState event)
	// Payload: { slot, decodeMs }
	await listen('play-vstats', ({ slot, decodeMs }) => {
		_mergeStats(slot, { decodeMs: typeof decodeMs === 'number' ? decodeMs : null });
	});

	// play-rtt: network round-trip latency (ms) from the keepalive Pong (client.rs)
	await listen('play-rtt', ({ slot, rtt }) => {
		_mergeStats(slot, { rtt: typeof rtt === 'number' ? Math.round(rtt) : null });
	});

	// play-stall: video stall state toggle
	await listen('play-stall', ({ slot, stalled }) => {
		_mergeStats(slot, { stalled });

		const activeSlot = _getActiveSlot();
		if (slot !== activeSlot && _state !== null) return;

		if (stalled) {
			showStallOverlay();
		} else {
			hideStallOverlay();
		}
	});

	// play-firstframe: first decoded AU; hide the first-frame loader
	await listen('play-firstframe', ({ slot }) => {
		_mergeStats(slot, { firstFrame: true, stalled: false });
		hideFirstFrameLoader();
		hideStallOverlay();  // stall is also over if we get a frame
		hideResyncOverlay(); // W5-quality-adv: recovery succeeded, dismiss resync
	});

	// decoder-error: MediaCodec threw / prolonged garbage detected (W5-native emits this).
	// Show the resync overlay. The overlay auto-dismisses after RESYNC_DISMISS_MS
	// or is hidden by the next play-firstframe event (recovery confirmation).
	// Only show for the active slot to avoid spurious overlays from background panes.
	await listen('decoder-error', ({ slot }) => {
		const activeSlot = _getActiveSlot();
		if (slot !== activeSlot) return;
		// A5: proactively ask the host for a fresh keyframe so the decoder re-syncs
		// within ~1 frame instead of waiting for the next periodic IDR (which turns
		// a decode error into a multi-second freeze). Throttled so a burst of
		// decoder-error events doesn't spam request_keyframe / MediaNacks.
		const now = Date.now();
		if (now - _lastKeyframeReq >= KEYFRAME_REQ_MIN_MS) {
			_lastKeyframeReq = now;
			invoke('request_keyframe', { slot }).catch(() => {});
		}
		showResyncOverlay('Yeniden eşitleniyor…');
	});

	// JS bus events (session lifecycle) — via window.__pulsarBus
	_wireBus();
}

/**
 * Subscribe to JS-bus events for session start/end lifecycle.
 * The bus is set on window by app.js before DOMContentLoaded.
 */
function _wireBus() {
	const getBus = () => (typeof window !== 'undefined' ? window.__pulsarBus : null);

	const wire = () => {
		const bus = getBus();
		if (!bus) return;

		// When a session starts, show the first-frame loader
		bus.on('session-started', ({ slot }) => {
			_perSlot.set(slot, {
				slot,
				fps:        0,
				mbps:       0,
				transport:  '',
				decodeMs:   null,
				stalled:    false,
				firstFrame: false,
			});
			// If this is slot 0 (primary), show loader and reset state
			if (slot === 0) {
				_state = _perSlot.get(slot);
				showFirstFrameLoader(slot);
				renderStrip();
			}
		});

		// When a session ends, clean up that slot's stats
		bus.on('session-ended', ({ slot }) => {
			_perSlot.delete(slot);
			// If no sessions remain, clear state and overlays
			if (_perSlot.size === 0) {
				_state = null;
				renderStrip();
				hideFirstFrameLoader();
				hideStallOverlay();
			} else {
				// Switch to the first remaining slot
				const first = _perSlot.values().next().value;
				if (first) {
					_state = first;
					renderStrip();
				}
			}
		});
	};

	if (getBus()) {
		wire();
	} else {
		// Bus not yet available — defer
		const onLoad = () => { wire(); };
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', onLoad, { once: true });
		} else {
			setTimeout(onLoad, 0);
		}
	}
}

// ---------------------------------------------------------------------------
// Inline styles (self-contained; does not touch components.css)
// ---------------------------------------------------------------------------

function _injectStyles() {
	if (document.getElementById('hud-module-styles')) return;
	const style = document.createElement('style');
	style.id = 'hud-module-styles';
	style.textContent = `
/* =========================================================
   HUD strip — compact always-on metrics bar at the top
   (over the transparent video surface, pointer-events: auto
    so it can be tapped to toggle the expanded card)
   ========================================================= */

.hud-strip {
	/* Base styles live in components.css (.hud-strip);
	   here we add interaction affordances. */
	cursor: pointer;
	transition:
		background 0.3s var(--ease-out),
		box-shadow 0.3s var(--ease-out),
		opacity 0.3s var(--ease-out);
}
.hud-strip:active {
	background: oklch(0.18 0.02 268 / 0.88);
}
/* B10: in game mode drop the GPU-costly backdrop blur over the live video — a flat
   (slightly more opaque) translucent bg keeps the strip readable without paying a
   per-frame blur pass that competes with the decoder. */
[data-mode='game'] .hud-strip {
	backdrop-filter: none;
	-webkit-backdrop-filter: none;
	background: oklch(0.12 0.015 268 / 0.82);
}

.hud-pair {
	display: inline-flex;
	align-items: baseline;
	gap: 3px;
}
.hud-lbl {
	font-size: 10px;
	color: oklch(0.75 0 0);
	font-family: var(--font-mono);
}
.hud-val {
	font-weight: 700;
	color: oklch(0.97 0 0);
	font-family: var(--font-mono);
	font-size: 11.5px;
}
/* Cyan in game mode */
[data-mode='game'] .hud-strip .hud-val {
	color: var(--cyan);
}
[data-mode='game'] .hud-strip .hud-badge {
	background: oklch(0.62 0.15 215 / 0.25);
	color: var(--cyan);
}
.hud-sep {
	display: inline-block;
	width: 1px;
	height: 11px;
	background: oklch(1 0 0 / 0.2);
	margin: 0 1px;
	vertical-align: middle;
}
.hud-badge {
	font-size: 9.5px;
	font-weight: 700;
	font-family: var(--font-mono);
	letter-spacing: 0.04em;
	background: oklch(0.555 0.205 272 / 0.25);
	color: oklch(0.78 0.12 272);
	border-radius: var(--r-pill);
	padding: 1px 6px;
}
.hud-waiting {
	opacity: 0.5;
	font-size: 10px;
}

/* =========================================================
   First-frame loader overlay
   ========================================================= */

.hud-loader {
	position: fixed;
	inset: 0;
	z-index: 13;
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 16px;
	background: oklch(0.08 0.012 268 / 0.82);
	backdrop-filter: blur(8px);
	-webkit-backdrop-filter: blur(8px);
	/* Fade in */
	opacity: 1;
	transition: opacity 0.4s var(--ease-out);
}
.hud-loader.hud-loader-out {
	opacity: 0;
	pointer-events: none;
}

.hud-loader-rings {
	position: relative;
	width: 72px;
	height: 72px;
}
.hud-ring {
	position: absolute;
	inset: 0;
	border-radius: 50%;
	border: 2px solid var(--brand);
	opacity: 0;
	animation: hud-pulse 2.4s var(--ease-out) infinite;
}
.hud-ring-1 { animation-delay: 0s;    width: 72px;  height: 72px; }
.hud-ring-2 { animation-delay: 0.5s;  width: 72px;  height: 72px; }
.hud-ring-3 { animation-delay: 1.0s;  width: 72px;  height: 72px; }

@keyframes hud-pulse {
	0%   { transform: scale(0.6); opacity: 0.7; }
	70%  { transform: scale(1.3); opacity: 0; }
	100% { transform: scale(0.6); opacity: 0; }
}

.hud-loader-text {
	font-family: var(--font-sans);
	font-size: 15px;
	font-weight: 600;
	color: oklch(0.95 0 0);
	margin: 0;
	text-align: center;
}
.hud-loader-sub {
	font-family: var(--font-mono);
	font-size: 11.5px;
	color: oklch(0.7 0 0);
	margin: -8px 0 0;
	text-align: center;
	letter-spacing: 0.02em;
}

/* =========================================================
   Stall overlay
   ========================================================= */

.hud-stall {
	position: fixed;
	inset: 0;
	z-index: 14;
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 14px;
	background: oklch(0.1 0.01 268 / 0.75);
	backdrop-filter: blur(6px);
	-webkit-backdrop-filter: blur(6px);
	opacity: 0;
	transition: opacity 0.35s var(--ease-out);
	color: oklch(0.95 0 0);
}
.hud-stall.hud-stall-in {
	opacity: 1;
}

.hud-stall-ring {
	width: 80px;
	height: 80px;
	border-radius: 50%;
	border: 2px solid oklch(0.72 0.15 75 / 0.5);
	box-shadow: 0 0 0 0 oklch(0.72 0.15 75 / 0.35);
	animation: hud-stall-pulse 2s var(--ease-out) infinite;
}
@keyframes hud-stall-pulse {
	0%   { box-shadow: 0 0 0 0 oklch(0.72 0.15 75 / 0.4); }
	70%  { box-shadow: 0 0 0 16px oklch(0.72 0.15 75 / 0); }
	100% { box-shadow: 0 0 0 0 oklch(0.72 0.15 75 / 0); }
}

.hud-stall-icon {
	margin-top: -60px; /* overlap the ring */
	color: oklch(0.72 0.15 75);
}

.hud-stall-title {
	font-family: var(--font-sans);
	font-size: 16px;
	font-weight: 700;
	color: oklch(0.95 0 0);
	margin: 0;
	text-align: center;
}
.hud-stall-sub {
	font-family: var(--font-mono);
	font-size: 11.5px;
	color: oklch(0.7 0 0);
	margin: -6px 0 0;
	text-align: center;
}

/* =========================================================
   Expanded overlay card (mounted by overlay.js)
   ========================================================= */

.hud-card-body {
	display: flex;
	flex-direction: column;
	gap: 10px;
}

.hud-card-header {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 8px;
	margin-bottom: 2px;
}
.hud-card-title {
	display: flex;
	align-items: center;
	gap: 7px;
	font-size: 12px;
	font-weight: 700;
	letter-spacing: 0.05em;
	text-transform: uppercase;
	color: oklch(0.75 0 0);
}
.hud-card-icon {
	color: var(--brand);
	flex: none;
}
[data-mode='game'] .hud-card-icon { color: var(--cyan); }

.hud-collapse-btn {
	width: 30px;
	height: 30px;
	min-height: unset; /* override .icon-btn base */
	background: oklch(1 0 0 / 0.08);
	border-radius: var(--r-sm);
	color: oklch(0.75 0 0);
}
.hud-collapse-btn:active {
	background: oklch(1 0 0 / 0.16);
}

.hud-card-rows {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.hud-row {
	display: flex;
	align-items: baseline;
	justify-content: space-between;
	gap: 10px;
	padding: 5px 2px;
	border-bottom: 1px solid oklch(1 0 0 / 0.06);
}
.hud-row:last-child {
	border-bottom: none;
}
.hud-row-label {
	font-size: 12px;
	color: oklch(0.72 0 0);
	font-family: var(--font-sans);
	white-space: nowrap;
}
.hud-row-value {
	font-family: var(--font-mono);
	font-size: 13px;
	font-weight: 700;
	color: oklch(0.97 0 0);
	text-align: right;
}
/* Cyan values in game mode */
[data-mode='game'] .hud-row-value { color: var(--cyan); }

.hud-card-collapsed-hint {
	font-size: 11.5px;
	color: oklch(0.6 0 0);
	text-align: center;
	padding: 4px 0 2px;
}

.hud-card-empty {
	font-size: 12.5px;
	color: oklch(0.65 0 0);
	text-align: center;
	padding: 8px 0;
	margin: 0;
}

/* =========================================================
   Resync overlay (W5-quality-adv)
   Reuses .hud-stall layout; adds a spinning arrow icon and
   brand-colored ring to visually distinguish from stall.
   ========================================================= */

/* The resync ring pulses in brand color (indigo/cyan) instead of amber */
.hud-resync .hud-resync-ring {
	border-color: var(--brand);
	box-shadow: 0 0 0 0 oklch(0.555 0.205 272 / 0.35);
}
[data-mode='game'] .hud-resync .hud-resync-ring {
	border-color: var(--cyan);
	box-shadow: 0 0 0 0 oklch(0.62 0.15 215 / 0.35);
}
@keyframes hud-resync-ring-pulse {
	0%   { box-shadow: 0 0 0 0 oklch(0.555 0.205 272 / 0.4); }
	70%  { box-shadow: 0 0 0 16px oklch(0.555 0.205 272 / 0); }
	100% { box-shadow: 0 0 0 0 oklch(0.555 0.205 272 / 0); }
}
.hud-resync-ring {
	animation: hud-resync-ring-pulse 2s var(--ease-out) infinite;
}
[data-mode='game'] .hud-resync-ring {
	animation: none; /* suppress re-keyframing; pulse is enough via default */
}

/* The resync arrow icon spins */
.hud-resync-icon {
	color: var(--brand);
	animation: hud-resync-spin 1.4s linear infinite;
	margin-top: -60px; /* same overlay positioning as stall icon */
}
[data-mode='game'] .hud-resync-icon {
	color: var(--cyan);
}
@keyframes hud-resync-spin {
	from { transform: rotate(0deg); }
	to   { transform: rotate(360deg); }
}
`;
	document.head.appendChild(style);
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

function boot() {
	_injectStyles();
	// Ensure the strip is in the DOM (body may not be ready in some test envs)
	if (document.body) {
		getOrCreateStrip();
	}
	// Register with overlay.js card registry
	tryRegister();
	// Subscribe to Tauri events
	_initListeners().catch((e) => {
		console.warn('[hud] Tauri listener init error', e);
	});
}

if (document.readyState === 'loading') {
	document.addEventListener('DOMContentLoaded', boot);
} else {
	boot();
}

// ---------------------------------------------------------------------------
// Public API (W5-quality-adv — resync overlay)
// ---------------------------------------------------------------------------

/**
 * Show the "yeniden eşitleniyor" resync overlay.
 * Triggered by the `decoder-error` event or programmatically (e.g. HDR change).
 * Auto-dismisses after RESYNC_DISMISS_MS unless hideResyncOverlay() is called
 * sooner (e.g. on play-firstframe, which means recovery succeeded).
 *
 * @param {string} [msg] - Optional custom message (defaults to Turkish).
 */
export function showResyncOverlay(msg) {
	// Clear any existing auto-dismiss timer
	if (_resyncTimer !== null) {
		clearTimeout(_resyncTimer);
		_resyncTimer = null;
	}

	// If already showing, just refresh the message text and restart the timer
	const existing = $('hud-resync');
	if (existing) {
		const title = existing.querySelector('.hud-stall-title');
		if (title && msg) title.textContent = msg;
		_resyncTimer = setTimeout(() => hideResyncOverlay(), RESYNC_DISMISS_MS);
		return;
	}

	const el = document.createElement('div');
	el.id        = 'hud-resync';
	el.className = 'hud-stall hud-resync'; // reuse stall styles + own class
	el.setAttribute('role', 'status');
	el.setAttribute('aria-live', 'polite');
	el.setAttribute('aria-label', msg || 'Yeniden eşitleniyor');
	el.innerHTML = `
		<div class="hud-stall-ring hud-resync-ring" aria-hidden="true"></div>
		<svg class="hud-stall-icon hud-resync-icon" width="36" height="36"
		     viewBox="0 0 24 24" fill="none" aria-hidden="true">
			<path d="M4 12a8 8 0 0114.93-4" stroke="currentColor" stroke-width="2"
			      stroke-linecap="round"/>
			<polyline points="19 4 18.93 8 15 8" stroke="currentColor" stroke-width="2"
			          stroke-linecap="round" stroke-linejoin="round"/>
		</svg>
		<p class="hud-stall-title">${msg || t('m.session.resync')}</p>
	`;
	document.body.appendChild(el);
	requestAnimationFrame(() => el.classList.add('hud-stall-in'));

	// Auto-dismiss after RESYNC_DISMISS_MS
	_resyncTimer = setTimeout(() => hideResyncOverlay(), RESYNC_DISMISS_MS);
}

/**
 * Hide the resync overlay and cancel any pending auto-dismiss timer.
 */
export function hideResyncOverlay() {
	if (_resyncTimer !== null) {
		clearTimeout(_resyncTimer);
		_resyncTimer = null;
	}
	const el = $('hud-resync');
	if (!el) return;
	el.classList.remove('hud-stall-in');
	el.addEventListener('transitionend', () => el.remove(), { once: true });
	setTimeout(() => el.remove(), 450);
}

/**
 * Get the current state snapshot for the given slot (or the active slot).
 * Consumed by overlay.js / quality.js for inline display.
 * @param {number} [slot]
 * @returns {{ fps:number, mbps:number, transport:string, decodeMs:number|null } | null}
 */
export function getStats(slot) {
	const s = typeof slot === 'number' ? _perSlot.get(slot) : _state;
	if (!s) return null;
	return { fps: s.fps, mbps: s.mbps, transport: s.transport, decodeMs: s.decodeMs };
}
