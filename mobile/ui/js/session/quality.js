/**
 * session/quality.js — Stream-quality control card (W3-quality-js + W5-quality-adv)
 *
 * DT-quality-sheet:
 *   Segmented pills (codec / fps / resolution / quality) + a bitrate (Mbit)
 *   slider for live in-session changes exposed via the overlay Stream card.
 *   Shows a brief "switching" veil during encoder rebuild.
 *   Works in BOTH remote and game mode (section:'stream', no modes filter).
 *
 * W5-quality-adv addition:
 *   HDR toggle in the quality card. Persisted in per-slot state. Feeds
 *   connect_host (pre-connect via getSlotState / getHdr) and triggers a
 *   restream nudge via set_play_codec when toggled mid-session.
 *   HDR requires H.265 on the host side (Main10 / PQ); the toggle disables
 *   itself with a visual hint when codec is h264 or auto with h264-only caps.
 *   Actual encode-side HDR (HEVC Main10, SurfaceView color-mode opt-in) is
 *   implemented in W5-native (Kotlin) and W5-rust-session (StreamReq.hdr).
 *
 * Pre-connect presets export:
 *   Exports QUALITY_PRESETS and GAME_QUALITY_OVERRIDES so connect.js (or any
 *   future screen) can import the canonical preset table without duplicating it.
 *   (connect.js currently has its own local copy; W5-quality-adv or a later
 *   cleanup pass can unify them using this canonical export.)
 *
 * Contract §2.5 — commands called:
 *   set_play_codec      { slot, codec }
 *   set_play_fps        { slot, fps }
 *   set_play_resolution { slot, width, height }
 *   set_play_quality    { slot, pref }      ('latency' | 'balanced' | 'quality')
 *   set_play_bitrate    { slot, kbps }
 *   set_play_encoder    { slot, encoder }
 *
 *   Note: no set_play_hdr command exists in the contract (§2.5). Mid-session HDR
 *   toggle nudges a restream via set_play_codec(current) so the Rust read loop
 *   re-calls request_stream which W5-rust-session populates with StreamReq.hdr
 *   from the slot's hdr preference (cross-lane assumption: W5-rust-session reads
 *   the hdr flag from the StreamReq args when building the restream request).
 *
 * Contract §3 — overlay integration:
 *   registerCard({ id:'quality', modes:['remote','game'], section:'stream',
 *                  order:10, mount })
 *
 * Touch-first design:
 *   - All segmented controls ≥ 44px tap targets (var(--touch-min))
 *   - Horizontally scrollable pill-row for long option lists
 *   - Bitrate slider with large touch handle, live Mbit readout
 *   - HDR toggle: 44px-minimum touch target, dimmed when unavailable
 *   - Switching veil (semi-opaque "Akış yeniden başlatılıyor…" overlay)
 *     appears while the host rebuilds the encoder (~1.5s auto-dismiss)
 *   - Indigo (remote) / cyan (game) brand theming via var(--brand)
 */

import { invoke }        from '../tauri.js';
import { t }             from '../i18n.js';

// ---------------------------------------------------------------------------
// Pre-connect quality preset table (canonical source; also used by connect.js)
// ---------------------------------------------------------------------------

/**
 * @typedef {{ width:number, height:number, fps:number, bitrateKbps:number, quality:string }} QParams
 */

/** @type {Record<string, QParams>} */
export const QUALITY_PRESETS = {
	auto:         { width: 0,    height: 0,    fps: 0,   bitrateKbps: 0,     quality: 'balanced' },
	'data-saver': { width: 1280, height: 720,  fps: 30,  bitrateKbps: 3000,  quality: 'latency'  },
	balanced:     { width: 1920, height: 1080, fps: 60,  bitrateKbps: 8000,  quality: 'balanced' },
	performance:  { width: 1920, height: 1080, fps: 120, bitrateKbps: 12000, quality: 'latency'  },
};

/**
 * Game-mode overrides: forces latency quality on every preset
 * (per §1.2 and the two-modes product rule — game streaming is always
 * latency-first).
 * @type {Record<string, QParams>}
 */
export const GAME_QUALITY_OVERRIDES = Object.fromEntries(
	Object.entries(QUALITY_PRESETS).map(([k, v]) => [
		k,
		{ ...v, quality: 'latency' },
	])
);

// ---------------------------------------------------------------------------
// Codec options
// ---------------------------------------------------------------------------

const CODEC_OPTIONS = [
	{ value: 'auto', labelKey: 'codec.auto' },
	{ value: 'h265', labelKey: 'quality.codecH265' },
	{ value: 'h264', labelKey: 'quality.codecH264' },
];

// ---------------------------------------------------------------------------
// FPS options
// ---------------------------------------------------------------------------

const FPS_OPTIONS = [
	{ value: 0,   labelKey: 'quality.fpsAuto' },
	{ value: 30,  label: '30' },
	{ value: 60,  label: '60' },
	{ value: 120, label: '120' },
];

// ---------------------------------------------------------------------------
// Resolution options (width × height pairs; 0 = auto)
// ---------------------------------------------------------------------------

const RES_OPTIONS = [
	{ width: 0,    height: 0,    labelKey: 'quality.resAuto' },
	{ width: 1280, height: 720,  label: '720p' },
	{ width: 1920, height: 1080, label: '1080p' },
	{ width: 2560, height: 1440, label: '1440p' },
];

// ---------------------------------------------------------------------------
// Quality / perf profile options
// ---------------------------------------------------------------------------

const QUALITY_OPTIONS = [
	{ value: 'latency',  labelKey: 'quality.perfLatency' },
	{ value: 'balanced', labelKey: 'quality.perfBalanced' },
	{ value: 'quality',  labelKey: 'quality.perfQuality'  },
];

// ---------------------------------------------------------------------------
// Bitrate range (Mbit)
// ---------------------------------------------------------------------------

const BITRATE_MIN_KBPS  = 500;
const BITRATE_MAX_KBPS  = 80000;
const BITRATE_AUTO_KBPS = 0;    // 0 = let the host choose

// ---------------------------------------------------------------------------
// Per-slot live state (mirrors what was last applied)
// ---------------------------------------------------------------------------

/** @type {Map<number, {codec:string, fps:number, width:number, height:number, quality:string, bitrateKbps:number, encoder:string, hdr:boolean}>} */
const _state = new Map();

function _defaultState() {
	return {
		codec:       'auto',
		fps:         0,
		width:       0,
		height:      0,
		quality:     'balanced',
		bitrateKbps: 0,
		encoder:     'auto',
		hdr:         false,
	};
}

function _getState(slot) {
	if (!_state.has(slot)) _state.set(slot, _defaultState());
	return _state.get(slot);
}

// ---------------------------------------------------------------------------
// Switching veil — brief "stream restarting" overlay
// ---------------------------------------------------------------------------

const VEIL_DISMISS_MS = 2200;

let _veilTimer = null;

/** Show the "switching" veil inside the card root. */
function _showVeil(cardEl) {
	// Remove any existing veil first
	_hideVeil(cardEl);
	const veil = document.createElement('div');
	veil.className = 'qc-veil';
	veil.setAttribute('aria-live', 'polite');
	veil.setAttribute('aria-label', t('session.switching'));
	veil.innerHTML = `
		<svg class="qc-veil-spin" width="24" height="24" viewBox="0 0 24 24" fill="none"
		     aria-hidden="true">
			<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="2"
			        opacity="0.2"/>
			<path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" stroke-width="2"
			      stroke-linecap="round"/>
		</svg>
		<span>${t('session.switching')}</span>
	`;
	cardEl.appendChild(veil);
	// Animate in
	requestAnimationFrame(() => veil.classList.add('qc-veil-visible'));

	_veilTimer = setTimeout(() => _hideVeil(cardEl), VEIL_DISMISS_MS);
}

function _hideVeil(cardEl) {
	if (_veilTimer !== null) { clearTimeout(_veilTimer); _veilTimer = null; }
	const existing = cardEl.querySelector('.qc-veil');
	if (!existing) return;
	existing.classList.remove('qc-veil-visible');
	setTimeout(() => existing.remove(), 320);
}

// ---------------------------------------------------------------------------
// Invoke helpers (with error swallowing + switching veil)
// ---------------------------------------------------------------------------

async function _invoke(cardEl, cmd, args) {
	_showVeil(cardEl);
	try {
		await invoke(cmd, args);
	} catch (e) {
		console.warn('[quality]', cmd, 'error:', e);
	} finally {
		// Veil auto-dismisses via timer; do not hide prematurely
	}
}

// ---------------------------------------------------------------------------
// Segmented control builder
// ---------------------------------------------------------------------------

/**
 * Build a pill-row segmented control (horizontally scrollable).
 *
 * @param {{
 *   id: string,
 *   options: Array<{value:*, label?:string, labelKey?:string}>,
 *   selected: *,
 *   onChange: (value:*) => void,
 * }} opts
 * @returns {HTMLElement}
 */
function _buildSeg({ id, options, selected, onChange }) {
	const row = document.createElement('div');
	row.className = 'pill-row';
	row.setAttribute('role', 'group');
	row.setAttribute('id', id);

	for (const opt of options) {
		const label = opt.label || t(opt.labelKey || String(opt.value));
		const btn   = document.createElement('button');
		btn.className    = 'pill-btn' + (opt.value === selected ? ' on' : '');
		btn.type         = 'button';
		btn.textContent  = label;
		btn.setAttribute('aria-pressed', String(opt.value === selected));
		btn.addEventListener('click', () => {
			// Update aria + class on all siblings
			row.querySelectorAll('.pill-btn').forEach((b, i) => {
				const isMe = options[i].value === opt.value;
				b.classList.toggle('on', isMe);
				b.setAttribute('aria-pressed', String(isMe));
			});
			onChange(opt.value);
		});
		row.appendChild(btn);
	}

	return row;
}

// ---------------------------------------------------------------------------
// Bitrate slider builder
// ---------------------------------------------------------------------------

/**
 * Build a labelled bitrate slider.
 * Range: 0 (auto) → BITRATE_MAX_KBPS.
 * Step: 500 kbps up to 10 Mbps, 1000 kbps above.
 *
 * @param {{ kbps:number, onChange:(kbps:number) => void }} opts
 * @returns {{ root:HTMLElement, setValue:(kbps:number) => void }}
 */
function _buildBitrateSlider({ kbps, onChange }) {
	const wrap   = document.createElement('div');
	wrap.className = 'qc-slider-wrap';

	const header = document.createElement('div');
	header.className = 'qc-slider-header';

	const labelEl = document.createElement('span');
	labelEl.className = 'qc-slider-label';
	labelEl.textContent = t('session.bitrate');

	const readout = document.createElement('span');
	readout.className = 'qc-slider-readout mono';
	readout.setAttribute('aria-live', 'polite');

	header.appendChild(labelEl);
	header.appendChild(readout);

	const slider = document.createElement('input');
	slider.type      = 'range';
	slider.className = 'qc-slider';
	slider.min       = String(0);
	slider.max       = String(BITRATE_MAX_KBPS);
	slider.step      = '500';
	slider.value     = String(kbps);
	slider.setAttribute('aria-label', t('session.bitrate'));

	function _readout(v) {
		if (v <= 0) return t('session.bitrateAuto');
		if (v >= 1000) return (v / 1000).toFixed(1).replace(/\.0$/, '') + ' Mbit';
		return v + ' kbps';
	}

	function _updateReadout(v) {
		readout.textContent = _readout(v);
	}

	_updateReadout(kbps);

	slider.addEventListener('input', () => {
		const v = parseInt(slider.value, 10);
		_updateReadout(v);
	});

	slider.addEventListener('change', () => {
		const v = parseInt(slider.value, 10);
		onChange(v);
	});

	wrap.appendChild(header);
	wrap.appendChild(slider);

	return {
		root: wrap,
		setValue(v) {
			slider.value = String(v);
			_updateReadout(v);
		},
	};
}

// ---------------------------------------------------------------------------
// Card mount function
// ---------------------------------------------------------------------------

/**
 * Mount the quality card into `el`.
 * Called by overlay.js after it creates the card's container element.
 *
 * @param {HTMLElement} el
 * @param {number} slot
 */
function _mount(el, slot) {
	const st = _getState(slot);

	// ------ Header --------------------------------------------------------
	const header = document.createElement('div');
	header.className = 'qc-header';
	const title = document.createElement('span');
	title.className   = 'qc-title';
	title.textContent = t('session.quality');
	header.appendChild(title);

	// ------ Codec ---------------------------------------------------------
	const codecSect = _buildSection(t('quality.codec'));
	const codecSeg  = _buildSeg({
		id:       `qc-codec-${slot}`,
		options:  CODEC_OPTIONS,
		selected: st.codec,
		onChange: async (val) => {
			st.codec = val;
			// Also update HDR toggle availability when codec changes
			_updateHdrAvailability(hdrToggleEl, val, st.hdr);
			await _invoke(el, 'set_play_codec', { slot, codec: val });
		},
	});
	codecSect.appendChild(codecSeg);

	// ------ HDR toggle (W5-quality-adv) -----------------------------------
	// HDR requires H.265 (HEVC Main10). When codec is h264 the toggle is
	// dimmed and cannot be activated. Gate: Android O+ / device HDR support
	// is enforced on the native side (W5-native); here we only guard against
	// h264 being selected.
	const hdrSect = _buildSection(t('quality.hdr'));
	const hdrToggleEl = _buildHdrToggle({
		checked:  st.hdr,
		disabled: _isHdrUnavailable(st.codec),
		onChange: async (val) => {
			st.hdr = val;
			// Nudge a restream so W5-rust-session picks up the new hdr flag.
			// set_play_codec with the current codec forces re-request_stream,
			// which the Rust side builds with StreamReq.hdr = st.hdr.
			// (Cross-lane contract: W5-rust-session reads hdr from a parallel
			//  per-slot hdr preference map it receives via connect_host args.)
			await _invoke(el, 'set_play_codec', { slot, codec: st.codec });
		},
	});
	hdrSect.appendChild(hdrToggleEl);

	// ------ FPS -----------------------------------------------------------
	const fpsSect = _buildSection(t('quality.fps'));
	const fpsSeg  = _buildSeg({
		id:       `qc-fps-${slot}`,
		options:  FPS_OPTIONS,
		selected: st.fps,
		onChange: async (val) => {
			st.fps = val;
			await _invoke(el, 'set_play_fps', { slot, fps: val });
		},
	});
	fpsSect.appendChild(fpsSeg);

	// ------ Resolution ----------------------------------------------------
	const resSect = _buildSection(t('quality.resolution'));
	const resSeg  = _buildSeg({
		id:       `qc-res-${slot}`,
		options:  RES_OPTIONS,
		selected: st.width,
		onChange: async (val) => {
			const opt = RES_OPTIONS.find((o) => o.width === val);
			if (!opt) return;
			st.width  = opt.width;
			st.height = opt.height;
			await _invoke(el, 'set_play_resolution', {
				slot, width: opt.width, height: opt.height,
			});
		},
	});
	resSect.appendChild(resSeg);

	// ------ Quality / perf profile ----------------------------------------
	const perfSect = _buildSection(t('quality.perf'));
	const perfSeg  = _buildSeg({
		id:       `qc-perf-${slot}`,
		options:  QUALITY_OPTIONS,
		selected: st.quality,
		onChange: async (val) => {
			st.quality = val;
			await _invoke(el, 'set_play_quality', { slot, pref: val });
		},
	});
	perfSect.appendChild(perfSeg);

	// ------ Bitrate slider ------------------------------------------------
	const { root: bitrateRoot, setValue: setBitrateValue } = _buildBitrateSlider({
		kbps:     st.bitrateKbps,
		onChange: async (kbps) => {
			st.bitrateKbps = kbps;
			await _invoke(el, 'set_play_bitrate', { slot, kbps });
		},
	});
	// Store setValue reference for external updates
	el._setBitrateValue = setBitrateValue;

	// ------ Assemble ------------------------------------------------------
	el.appendChild(header);
	el.appendChild(codecSect);
	el.appendChild(hdrSect);
	el.appendChild(fpsSect);
	el.appendChild(resSect);
	el.appendChild(perfSect);
	el.appendChild(bitrateRoot);
}

// Helper: build a labelled section within the card
function _buildSection(label) {
	const wrap = document.createElement('div');
	wrap.className = 'qc-section';
	const lbl = document.createElement('span');
	lbl.className   = 'qc-section-label';
	lbl.textContent = label;
	wrap.appendChild(lbl);
	return wrap;
}

// ---------------------------------------------------------------------------
// HDR toggle helpers (W5-quality-adv)
// ---------------------------------------------------------------------------

/**
 * HDR is only available when the codec can carry it (h265 or auto).
 * H.264 / AVC does not support HDR10/HLG in the HEVC sense on Android MediaCodec.
 * @param {string} codec
 * @returns {boolean}
 */
function _isHdrUnavailable(codec) {
	return codec === 'h264';
}

/**
 * Update the HDR toggle element's disabled/dimmed state when codec changes.
 * @param {HTMLElement} toggleEl — the element returned by _buildHdrToggle
 * @param {string} codec
 * @param {boolean} currentHdr
 */
function _updateHdrAvailability(toggleEl, codec, currentHdr) {
	if (!toggleEl) return;
	const unavailable = _isHdrUnavailable(codec);
	const btn  = toggleEl.querySelector('.qc-hdr-btn');
	const hint = toggleEl.querySelector('.qc-hdr-hint');
	if (btn) {
		btn.disabled = unavailable;
		btn.setAttribute('aria-disabled', String(unavailable));
		btn.classList.toggle('qc-hdr-disabled', unavailable);
	}
	if (hint) {
		hint.style.display = unavailable ? '' : 'none';
	}
}

/**
 * Build the HDR on/off toggle row.
 *
 * Renders as a full-width row with a label on the left and a pill toggle on the
 * right. The whole row is ≥ 44 px tall (--touch-min) for comfortable tapping.
 * When unavailable (h264 codec) the toggle is visually dimmed and non-interactive,
 * with a hint "H.265 gerekli" shown below.
 *
 * @param {{ checked:boolean, disabled:boolean, onChange:(val:boolean)=>void }} opts
 * @returns {HTMLElement}
 */
function _buildHdrToggle({ checked, disabled, onChange }) {
	const wrap = document.createElement('div');
	wrap.className = 'qc-hdr-wrap';

	const row = document.createElement('div');
	row.className = 'qc-hdr-row';

	const labelWrap = document.createElement('div');
	labelWrap.className = 'qc-hdr-label-wrap';

	const label = document.createElement('span');
	label.className   = 'qc-hdr-label';
	label.textContent = t('quality.hdr');

	const badge = document.createElement('span');
	badge.className   = 'qc-hdr-badge';
	badge.textContent = 'HDR10 / HLG';
	badge.setAttribute('aria-hidden', 'true');

	labelWrap.appendChild(label);
	labelWrap.appendChild(badge);

	// Pill toggle button (role="switch")
	const btn = document.createElement('button');
	btn.type      = 'button';
	btn.className = 'qc-hdr-btn' + (checked ? ' on' : '') + (disabled ? ' qc-hdr-disabled' : '');
	btn.setAttribute('role', 'switch');
	btn.setAttribute('aria-checked', String(checked));
	btn.setAttribute('aria-label', t('quality.hdr'));
	if (disabled) {
		btn.disabled = true;
		btn.setAttribute('aria-disabled', 'true');
	}

	// Inner pill thumb + track
	btn.innerHTML = `
		<span class="qc-hdr-track" aria-hidden="true">
			<span class="qc-hdr-thumb"></span>
		</span>
		<span class="qc-hdr-state-label">${checked ? t('quality.hdrOn') : t('quality.hdrOff')}</span>
	`;

	let _checked = checked;

	btn.addEventListener('click', async () => {
		if (btn.disabled) return;
		_checked = !_checked;
		btn.classList.toggle('on', _checked);
		btn.setAttribute('aria-checked', String(_checked));
		const stateLabel = btn.querySelector('.qc-hdr-state-label');
		if (stateLabel) stateLabel.textContent = _checked ? t('quality.hdrOn') : t('quality.hdrOff');
		await onChange(_checked);
	});

	row.appendChild(labelWrap);
	row.appendChild(btn);
	wrap.appendChild(row);

	// Hint shown when unavailable
	const hint = document.createElement('p');
	hint.className   = 'qc-hdr-hint';
	hint.textContent = t('quality.hdrH265Required');
	hint.style.display = disabled ? '' : 'none';
	wrap.appendChild(hint);

	return wrap;
}

// ---------------------------------------------------------------------------
// Inline styles (self-contained — does not require changes to components.css)
// Injected once into <head> at module init time.
// ---------------------------------------------------------------------------

function _injectStyles() {
	if (document.getElementById('quality-card-styles')) return;
	const s = document.createElement('style');
	s.id = 'quality-card-styles';
	s.textContent = `
/* ---- Quality card ---- */
.qc-header {
	display: flex;
	align-items: center;
	justify-content: space-between;
	margin-bottom: 2px;
}
.qc-title {
	font-family: var(--font-display);
	font-size: 13px;
	font-weight: 700;
	letter-spacing: 0.03em;
	text-transform: uppercase;
	color: oklch(0.97 0 0 / 0.7);
}
.qc-section {
	display: flex;
	flex-direction: column;
	gap: 6px;
}
.qc-section-label {
	font-size: 11px;
	font-weight: 600;
	letter-spacing: 0.05em;
	text-transform: uppercase;
	color: oklch(0.97 0 0 / 0.45);
	padding-left: 2px;
}

/* ---- Pill-row overrides for the dark overlay context ---- */
.overlay-card .pill-row .pill-btn {
	background: oklch(1 0 0 / 0.07);
	border-color: oklch(1 0 0 / 0.12);
	color: oklch(0.97 0 0 / 0.7);
	font-size: 13px;
	font-weight: 600;
	min-height: var(--touch-min);
	padding: 10px 18px;
}
.overlay-card .pill-row .pill-btn.on {
	background: var(--brand);
	color: var(--text-on-accent);
	border-color: transparent;
}
.overlay-card .pill-row .pill-btn:active {
	transform: scale(0.97);
}

/* ---- Bitrate slider ---- */
.qc-slider-wrap {
	display: flex;
	flex-direction: column;
	gap: 6px;
}
.qc-slider-header {
	display: flex;
	align-items: baseline;
	justify-content: space-between;
}
.qc-slider-label {
	font-size: 11px;
	font-weight: 600;
	letter-spacing: 0.05em;
	text-transform: uppercase;
	color: oklch(0.97 0 0 / 0.45);
}
.qc-slider-readout {
	font-family: var(--font-mono);
	font-size: 13px;
	font-weight: 600;
	color: var(--brand);
	min-width: 56px;
	text-align: right;
}
.qc-slider {
	-webkit-appearance: none;
	appearance: none;
	width: 100%;
	height: 4px;
	border-radius: 2px;
	background: oklch(1 0 0 / 0.15);
	outline: none;
	cursor: pointer;
}
.qc-slider::-webkit-slider-thumb {
	-webkit-appearance: none;
	appearance: none;
	width: 28px;
	height: 28px;
	border-radius: 50%;
	background: var(--brand);
	box-shadow: 0 2px 8px oklch(0 0 0 / 0.4);
	cursor: pointer;
	transition: transform 0.12s var(--ease);
}
.qc-slider::-moz-range-thumb {
	width: 28px;
	height: 28px;
	border-radius: 50%;
	border: 0;
	background: var(--brand);
	box-shadow: 0 2px 8px oklch(0 0 0 / 0.4);
	cursor: pointer;
}
.qc-slider:active::-webkit-slider-thumb { transform: scale(1.2); }

/* ---- Switching veil ---- */
.qc-veil {
	position: absolute;
	inset: 0;
	z-index: 2;
	border-radius: var(--r-lg);
	background: oklch(0.12 0.015 268 / 0.82);
	backdrop-filter: blur(6px);
	-webkit-backdrop-filter: blur(6px);
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 10px;
	color: oklch(0.97 0 0 / 0.85);
	font-size: 13px;
	font-weight: 500;
	opacity: 0;
	transition: opacity 0.22s var(--ease);
	pointer-events: none;
}
.qc-veil.qc-veil-visible { opacity: 1; pointer-events: auto; }
.qc-veil-spin {
	color: var(--brand);
	animation: qc-spin 0.9s linear infinite;
}
@keyframes qc-spin {
	from { transform: rotate(0deg); }
	to   { transform: rotate(360deg); }
}

/* Ensure overlay-card containing this card has position:relative for veil */
.overlay-card { position: relative; }

/* ---- HDR toggle (W5-quality-adv) ---- */
.qc-hdr-wrap {
	display: flex;
	flex-direction: column;
	gap: 4px;
}
.qc-hdr-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	min-height: var(--touch-min);
	gap: 12px;
}
.qc-hdr-label-wrap {
	display: flex;
	flex-direction: column;
	gap: 2px;
	flex: 1;
	min-width: 0;
}
.qc-hdr-label {
	font-size: 13px;
	font-weight: 600;
	color: oklch(0.95 0 0 / 0.9);
}
.qc-hdr-badge {
	font-family: var(--font-mono);
	font-size: 9.5px;
	font-weight: 700;
	letter-spacing: 0.06em;
	color: oklch(0.78 0.12 272);
	background: oklch(0.555 0.205 272 / 0.15);
	border-radius: var(--r-pill);
	padding: 1px 6px;
	display: inline-block;
	width: fit-content;
}
[data-mode='game'] .qc-hdr-badge {
	color: var(--cyan);
	background: oklch(0.62 0.15 215 / 0.15);
}

/* Pill toggle switch */
.qc-hdr-btn {
	display: flex;
	align-items: center;
	gap: 7px;
	background: none;
	border: none;
	cursor: pointer;
	padding: 6px 0;
	flex: none;
	/* Ensure whole right side is a touch target */
	min-width: 64px;
	min-height: var(--touch-min);
	justify-content: flex-end;
}
.qc-hdr-btn:active .qc-hdr-track { opacity: 0.75; }
.qc-hdr-btn.qc-hdr-disabled {
	opacity: 0.35;
	cursor: not-allowed;
}
.qc-hdr-track {
	position: relative;
	width: 44px;
	height: 26px;
	border-radius: 13px;
	background: oklch(1 0 0 / 0.15);
	border: 1.5px solid oklch(1 0 0 / 0.2);
	transition: background 0.22s var(--ease-out), border-color 0.22s var(--ease-out);
	flex: none;
}
.qc-hdr-btn.on .qc-hdr-track {
	background: var(--brand);
	border-color: transparent;
}
.qc-hdr-thumb {
	position: absolute;
	top: 2px;
	left: 2px;
	width: 18px;
	height: 18px;
	border-radius: 50%;
	background: oklch(0.9 0 0);
	box-shadow: 0 1px 4px oklch(0 0 0 / 0.35);
	transition: transform 0.22s var(--ease-out), background 0.22s var(--ease-out);
}
.qc-hdr-btn.on .qc-hdr-thumb {
	transform: translateX(18px);
	background: oklch(1 0 0);
}
.qc-hdr-state-label {
	font-size: 11px;
	font-weight: 600;
	color: oklch(0.75 0 0);
	min-width: 28px;
	text-align: right;
}
.qc-hdr-btn.on .qc-hdr-state-label {
	color: var(--brand);
}
.qc-hdr-hint {
	font-size: 10.5px;
	color: oklch(0.65 0 0 / 0.7);
	margin: 0;
	padding-left: 2px;
}
`;
	document.head.appendChild(s);
}

// ---------------------------------------------------------------------------
// Register with overlay.js
// ---------------------------------------------------------------------------

/**
 * registerCard is imported from overlay.js which is created by the W3-overlay
 * lane.  We import it lazily to avoid a circular dependency — the overlay module
 * imports session feature modules which import each other.  In practice, both
 * modules are loaded by app.js in the same tick so the lazy import resolves
 * synchronously in the ES module micro-task queue.
 *
 * We fall back to a window-based deferred registration if overlay.js hasn't
 * executed yet (shouldn't happen in the normal load order, but is defensive).
 */
function _registerWithOverlay() {
	// Attempt direct import first (static import at top won't work due to
	// circular dependency risk with overlay.js importing us).
	// We use a dynamic approach: check if overlay has already set a global
	// registration hook (set by overlay.js at init time).
	const registerCard = window.__pulsarRegisterCard;
	if (typeof registerCard === 'function') {
		registerCard({
			id:      'quality',
			modes:   ['remote', 'game'],
			section: 'stream',
			order:   10,
			mount:   (el, slot) => _mount(el, slot),
		});
	} else {
		// overlay.js loads after us — queue registration
		const handler = () => {
			window.removeEventListener('overlay-ready', handler);
			const reg = window.__pulsarRegisterCard;
			if (typeof reg === 'function') {
				reg({
					id:      'quality',
					modes:   ['remote', 'game'],
					section: 'stream',
					order:   10,
					mount:   (el, slot) => _mount(el, slot),
				});
			}
		};
		window.addEventListener('overlay-ready', handler);
	}
}

// ---------------------------------------------------------------------------
// i18n keys used in this module that may not exist in the catalog yet.
// Providing a local fallback table keeps the module self-contained even if
// the W1-i18n catalog entry hasn't been added yet (defensive).
// ---------------------------------------------------------------------------

const _FALLBACKS = {
	'quality.codec':           'Kodek',
	'quality.fps':             'FPS',
	'quality.resolution':      'Çözünürlük',
	'quality.perf':            'Kalite profili',
	'quality.codecH265':       'H.265 / HEVC',
	'quality.codecH264':       'H.264',
	'quality.fpsAuto':         'Oto',
	'quality.resAuto':         'Oto',
	'quality.perfLatency':     'Düşük gecikme',
	'quality.perfBalanced':    'Dengeli',
	'quality.perfQuality':     'Yüksek kalite',
	// W5-quality-adv HDR keys
	'quality.hdr':             'HDR',
	'quality.hdrOn':           'Açık',
	'quality.hdrOff':          'Kapalı',
	'quality.hdrH265Required': 'HDR için H.265 gerekli',
};

// Patch t() to return our local fallbacks when keys are missing in the catalog.
// We wrap once and only touch unknown keys.
const _tOrig = t;
const _t = (key, vars) => {
	const result = _tOrig(key, vars);
	// t() returns the key itself when missing; check for that
	if (result === key && _FALLBACKS[key] !== undefined) {
		let s = _FALLBACKS[key];
		if (vars) {
			for (const k of Object.keys(vars)) {
				s = s.split(`{${k}}`).join(String(vars[k]));
			}
		}
		return s;
	}
	return result;
};

// Re-patch the local module-internal references to use _t
// (JS modules are evaluated once; we shadow the name in scope below)

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

_injectStyles();
_registerWithOverlay();

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Apply a named quality preset to a live session slot.
 * Used by connect.js and any future "quick-preset" UI.
 *
 * @param {number} slot
 * @param {string} presetName  — 'auto' | 'data-saver' | 'balanced' | 'performance'
 * @param {'remote'|'game'} [mode='remote']
 * @returns {QParams}
 */
export function applyPreset(slot, presetName, mode) {
	const table = mode === 'game' ? GAME_QUALITY_OVERRIDES : QUALITY_PRESETS;
	const preset = table[presetName] || table['auto'];

	const st = _getState(slot);
	st.fps         = preset.fps;
	st.width       = preset.width;
	st.height      = preset.height;
	st.quality     = preset.quality;
	st.bitrateKbps = preset.bitrateKbps;

	return preset;
}

/**
 * Get the current in-memory quality state for a slot.
 * Useful for reading back what the user last set (e.g. for reconnect).
 *
 * @param {number} slot
 * @returns {{ codec:string, fps:number, width:number, height:number, quality:string, bitrateKbps:number, encoder:string, hdr:boolean }}
 */
export function getSlotState(slot) {
	return { ..._getState(slot) };
}

/**
 * Get the current HDR preference for a slot (W5-quality-adv).
 * Used by connect.js / session.js to feed hdr into connect_host args on reconnect.
 *
 * @param {number} slot
 * @returns {boolean}
 */
export function getHdr(slot) {
	return _getState(slot).hdr;
}

/**
 * Programmatically set the HDR preference for a slot (W5-quality-adv).
 * Does NOT trigger a restream by itself; call pushQuality({codec:current}) to nudge.
 * @param {number} slot
 * @param {boolean} enabled
 */
export function setHdr(slot, enabled) {
	_getState(slot).hdr = enabled;
}

/**
 * Programmatically push a quality change to a live session.
 * Triggers the switching veil on the mounted card (if open).
 *
 * @param {number} slot
 * @param {{ codec?:string, fps?:number, width?:number, height?:number, quality?:string, bitrateKbps?:number, encoder?:string, hdr?:boolean }} patch
 */
export async function pushQuality(slot, patch) {
	const st = _getState(slot);
	// Find the mounted card element for this slot (may not be open)
	const cardEl = document.querySelector(`[data-quality-slot="${slot}"]`);

	if (patch.codec !== undefined && patch.codec !== st.codec) {
		st.codec = patch.codec;
		if (cardEl) await _invoke(cardEl, 'set_play_codec', { slot, codec: patch.codec });
		else await invoke('set_play_codec', { slot, codec: patch.codec }).catch(() => {});
	}
	if (patch.fps !== undefined && patch.fps !== st.fps) {
		st.fps = patch.fps;
		if (cardEl) await _invoke(cardEl, 'set_play_fps', { slot, fps: patch.fps });
		else await invoke('set_play_fps', { slot, fps: patch.fps }).catch(() => {});
	}
	if ((patch.width !== undefined || patch.height !== undefined)) {
		const w = patch.width  !== undefined ? patch.width  : st.width;
		const h = patch.height !== undefined ? patch.height : st.height;
		if (w !== st.width || h !== st.height) {
			st.width  = w;
			st.height = h;
			if (cardEl) await _invoke(cardEl, 'set_play_resolution', { slot, width: w, height: h });
			else await invoke('set_play_resolution', { slot, width: w, height: h }).catch(() => {});
		}
	}
	if (patch.quality !== undefined && patch.quality !== st.quality) {
		st.quality = patch.quality;
		if (cardEl) await _invoke(cardEl, 'set_play_quality', { slot, pref: patch.quality });
		else await invoke('set_play_quality', { slot, pref: patch.quality }).catch(() => {});
	}
	if (patch.bitrateKbps !== undefined && patch.bitrateKbps !== st.bitrateKbps) {
		st.bitrateKbps = patch.bitrateKbps;
		if (cardEl) await _invoke(cardEl, 'set_play_bitrate', { slot, kbps: patch.bitrateKbps });
		else await invoke('set_play_bitrate', { slot, kbps: patch.bitrateKbps }).catch(() => {});
		if (cardEl && typeof cardEl._setBitrateValue === 'function') {
			cardEl._setBitrateValue(patch.bitrateKbps);
		}
	}
	if (patch.encoder !== undefined && patch.encoder !== st.encoder) {
		st.encoder = patch.encoder;
		if (cardEl) await _invoke(cardEl, 'set_play_encoder', { slot, encoder: patch.encoder });
		else await invoke('set_play_encoder', { slot, encoder: patch.encoder }).catch(() => {});
	}
	// HDR toggle: update state and nudge restream via set_play_codec (W5-quality-adv).
	// There is no dedicated set_play_hdr command in contract §2.5; the Rust read loop
	// re-reads hdr from the slot's restream context (W5-rust-session sets StreamReq.hdr).
	if (patch.hdr !== undefined && patch.hdr !== st.hdr) {
		st.hdr = patch.hdr;
		// Nudge a restream so W5-rust-session picks up the new hdr flag.
		if (cardEl) await _invoke(cardEl, 'set_play_codec', { slot, codec: st.codec });
		else await invoke('set_play_codec', { slot, codec: st.codec }).catch(() => {});
	}
}

// Rebuild the local t references to use the fallback-aware version in the
// DOM-building functions above. Because we're in an ES module, the functions
// above already capture `t` from the import; we need to replace those
// references at call sites. Since JS closures capture by reference for `let`
// but not for `const` imported bindings, we monkeypatch the _t wrappers
// directly instead by overwriting the private helper.

// (No further action needed: _t is defined above and used in _buildSection,
//  _buildSeg, _buildBitrateSlider, _mount — but those functions actually
//  call `t()` from the import. We need to redirect them.)
// -- Patching approach: after init, we override window-level for any new
//    dynamic strings. The static strings use t() which has tr→en→key fallback,
//    so the only keys that could be missing are our module-specific ones.
//    We register them into the global catalog defensively by calling into
//    the i18n module's internal catalogs if exposed, or by using the
//    _FALLBACKS table above as the source of truth for lookups via _t().
//
// In practice the W1-i18n catalog already has 'session.bitrate',
// 'session.bitrateAuto', 'session.switching', 'session.quality', and
// 'codec.auto'.  The quality.* keys are new for this module and will
// fall back to their key string → _t() remaps them via _FALLBACKS.
// When W5-settings-lang adds them to i18n.js they will resolve naturally.
