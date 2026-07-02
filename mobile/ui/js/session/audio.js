/**
 * audio.js — In-session audio controls card (W3-media-native + W4-mic)
 *
 * DT-audio + DT-mic:
 *   Speaker/mute toggle (both remote + game modes).
 *   Mic toggle with Android runtime permission flow + recording indicator
 *   (remote only, auto-mutes on visibilitychange / backgrounding).
 *
 * Registers with the overlay card registry:
 *   section: 'audio'
 *   modes:   ['remote', 'game']   — card shown in both; mic row is remote-only
 *
 * Plugin commands (JS→native via plugin:pulsar-video|*):
 *   setAudioMuted  { muted: bool }
 *   micStart       {}   — arms AudioRecord (VOICE_COMMUNICATION, 48k mono s16le)
 *   micStop        {}   — stops AudioRecord + clears buffer
 *
 * Tauri commands (JS→Rust, W4-rust-client):
 *   mic_start  { slot }  — Rust drain loop: pollMicFrame → DataMsg::Audio
 *   mic_stop   { slot }  — Rust sends DataMsg::AudioEnd
 *
 * Permission flow (Android):
 *   - micStart plugin returns { ok:false, detail:"permission_requested" }
 *     when RECORD_AUDIO is not yet granted.
 *   - The JS side shows an in-card explanation then retries micStart once the
 *     user taps "İzin ver" (a brief bottom-sheet over the card).
 *   - After the system dialog is dismissed (granted or denied) the JS retries
 *     automatically via a one-shot listener on the 'mic-permission-result' bus
 *     event (emitted by the W4-rust-client mic_start command on re-try success).
 *
 * Auto-mute on background (visibilitychange):
 *   When the document goes hidden (home button, task-switcher, lock screen) AND
 *   the mic is active, mic_stop + micStop are called immediately.  The 'session-bg'
 *   bus event is also honoured for explicit background notifications from the host
 *   activity (e.g. MediaProjection FGS minimise).
 *
 * Design (DT-mic):
 *   - Large touch targets ≥ 44px, thumb-friendly layout.
 *   - Speaker and mic rows separated by a subtle divider.
 *   - Recording state: pulsing red dot on mic button + "Kaydediyor" label.
 *   - Awaiting-permission state: amber indicator + "İzin bekleniyor".
 *   - Denied state: strikethrough icon + "Erişim reddedildi" sub-label.
 *   - Brand token var(--brand) → indigo in remote mode, cyan in game mode.
 *   - Safe-area-aware card padding via var(--safe-bottom).
 */

import { invoke, listen } from '../tauri.js';
import { t } from '../i18n.js';

// Apply the Settings "host audio" pref at each session's first frame: if the user
// turned host audio off, start the session muted (the plugin keeps the gain even
// if the AudioTrack is created slightly later). Default = play.
listen('play-firstframe', () => {
	let play = true;
	try { play = JSON.parse(localStorage.getItem('pulsar.prefs.v1') || '{}').playHostAudio !== false; } catch (_) {}
	_muted = !play;
	try { _renderMuteState(); } catch (_) {}
	invoke('plugin:pulsar-video|set_audio_muted', { muted: _muted }).catch(() => {});
});

// ── State ─────────────────────────────────────────────────────────────────────

/** Whether remote audio is muted (AudioTrack volume = 0). */
let _muted = false;

/** Whether mic is actively recording. */
let _micActive = false;

/**
 * Mic permission state:
 *   'unknown'   — not yet requested this session
 *   'requesting' — system dialog is up / waiting
 *   'granted'   — RECORD_AUDIO granted
 *   'denied'    — user denied; show explanation + Settings link
 */
let _micPermState = 'unknown';

/** Active session slot (updated on session-started). */
let _slot = 0;

/** Active session mode: 'remote' | 'game'. */
let _mode = 'remote';

/** True if visibilitychange listener is installed. */
let _visibilityWired = false;

// ── DOM helpers ───────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);

// ── Card ID ───────────────────────────────────────────────────────────────────

const CARD_ID = 'audio-card';

// ── Card mount ────────────────────────────────────────────────────────────────

/**
 * Mount the audio card into the provided container element.
 * Called by overlay.js when the overlay opens the audio section.
 * @param {HTMLElement} container
 */
function mount(container) {
  container.innerHTML = `
<div class="audio-card" id="${CARD_ID}" role="group" aria-label="${t('audio.cardLabel')}">

  <!-- ── Speaker / mute row ── -->
  <div class="audio-row audio-row-speaker">
    <button
      class="audio-icon-btn${_muted ? ' audio-muted' : ''}"
      id="audio-mute-btn"
      type="button"
      aria-pressed="${_muted}"
      aria-label="${_muted ? t('audio.unmute') : t('audio.mute')}"
    >
      <span class="audio-icon-wrap" aria-hidden="true">
        ${_muted ? _iconMuted() : _iconSpeaker()}
      </span>
    </button>
    <div class="audio-row-info">
      <span class="audio-row-label" id="audio-mute-label">
        ${_muted ? t('audio.muted') : t('audio.unmuted')}
      </span>
      <span class="audio-row-sub">${t('audio.speakerSub')}</span>
    </div>
    <span class="audio-mute-chip" id="audio-mute-chip"
          aria-hidden="true"
          style="display:${_muted ? 'inline-flex' : 'none'}">
      🔇 ${t('audio.mutedChip')}
    </span>
  </div>

  <!-- ── Divider (remote only) ── -->
  <div class="audio-divider" id="audio-divider"
       style="display:${_mode === 'remote' ? 'block' : 'none'}"></div>

  <!-- ── Mic row (remote only) ── -->
  <div class="audio-row audio-row-mic" id="audio-mic-row"
       style="display:${_mode === 'remote' ? 'flex' : 'none'}">
    <button
      class="audio-icon-btn${_micActive ? ' mic-active' : ''}${_micPermState === 'denied' ? ' mic-denied' : ''}"
      id="audio-mic-btn"
      type="button"
      aria-pressed="${_micActive}"
      aria-label="${_micBtnLabel()}"
    >
      <span class="audio-icon-wrap" aria-hidden="true">
        ${_micIcon()}
      </span>
      <!-- Recording indicator dot — visible while _micActive -->
      <span class="audio-rec-dot" id="audio-rec-dot"
            style="display:${_micActive ? 'block' : 'none'}"
            aria-hidden="true"></span>
      <!-- Permission-pending indicator -->
      <span class="audio-perm-dot" id="audio-perm-dot"
            style="display:${_micPermState === 'requesting' ? 'block' : 'none'}"
            aria-hidden="true"></span>
    </button>
    <div class="audio-row-info">
      <span class="audio-row-label" id="audio-mic-label">
        ${_micStateLabel()}
      </span>
      <span class="audio-row-sub" id="audio-mic-sub">
        ${_micSubLabel()}
      </span>
    </div>
    <!-- "İzin ver" inline action — shown only when state is denied -->
    <button class="audio-perm-btn" id="audio-perm-btn" type="button"
            style="display:${_micPermState === 'denied' ? 'inline-flex' : 'none'}"
            aria-label="${t('audio.micPermSettings')}">
      ${t('audio.micPermSettings')}
    </button>
  </div>

  <!-- ── Permission explanation sheet (inline, slides down when needed) ── -->
  <div class="audio-perm-sheet" id="audio-perm-sheet"
       style="display:${_micPermState === 'requesting' ? 'flex' : 'none'}"
       role="alert" aria-live="polite">
    <span class="audio-perm-icon" aria-hidden="true">${_iconMicOn()}</span>
    <div class="audio-perm-text">
      <strong>${t('audio.micPermTitle')}</strong>
      <span>${t('audio.micPermBody')}</span>
    </div>
  </div>

</div>
`;

  $('audio-mute-btn')?.addEventListener('click', _toggleMute);
  $('audio-mic-btn')?.addEventListener('click', _handleMicTap);
  $('audio-perm-btn')?.addEventListener('click', _openMicSettings);

  _wireVisibility();
}

// ── Mute toggle ───────────────────────────────────────────────────────────────

async function _toggleMute() {
  const next = !_muted;
  _muted = next;
  _renderMuteState();

  try {
    await invoke('plugin:pulsar-video|set_audio_muted', { muted: next });
  } catch (e) {
    console.warn('[audio] setAudioMuted error:', e);
  }
}

// ── Mic toggle & permission flow ─────────────────────────────────────────────

async function _handleMicTap() {
  if (_micPermState === 'denied') {
    _openMicSettings();
    return;
  }

  if (_micActive) {
    await _stopMic();
  } else {
    await _startMic();
  }
}

async function _startMic() {
  // 1. Arm the plugin's AudioRecord (triggers permission dialog if needed)
  let pluginRes;
  try {
    pluginRes = await invoke('plugin:pulsar-video|mic_start', {});
  } catch (e) {
    console.warn('[audio] micStart plugin error:', e);
    return;
  }

  if (!pluginRes?.ok) {
    const detail = pluginRes?.detail ?? '';
    if (detail === 'permission_requested') {
      // System dialog is up — show explanation + wait
      _micPermState = 'requesting';
      _renderMicState();
      // The _permissionResultListener will fire when the Rust command re-calls
      // micStart after the user taps Allow/Deny in the system dialog.
      _installPermissionResultListener();
    } else if (detail === 'permission_denied') {
      _micPermState = 'denied';
      _renderMicState();
    }
    return;
  }

  // Plugin returned ok:true — permission was already granted
  _micPermState = 'granted';

  // 2. Tell the Rust side to start draining PCM and sending DataMsg::Audio
  try {
    await invoke('mic_start', { slot: _slot });
  } catch (e) {
    // mic_start Tauri command lives in W4-rust-client — log only if not yet wired
    console.info('[audio] mic_start Tauri command (W4-rust-client):', e);
  }

  _micActive = true;
  _renderMicState();
}

async function _stopMic() {
  if (!_micActive && _micPermState !== 'requesting') return;

  _micActive = false;
  _micPermState = _micPermState === 'requesting' ? 'unknown' : _micPermState;

  // 1. Stop the native capture first (no more PCM into the buffer)
  try {
    await invoke('plugin:pulsar-video|mic_stop', {});
  } catch (e) {
    console.warn('[audio] micStop plugin error:', e);
  }

  // 2. Tell Rust to flush remaining buffer → DataMsg::AudioEnd
  try {
    await invoke('mic_stop', { slot: _slot });
  } catch (e) {
    console.info('[audio] mic_stop Tauri command (W4-rust-client):', e);
  }

  _renderMicState();
}

/**
 * Install a one-shot bus listener for 'mic-permission-result' that fires when
 * the W4-rust-client mic_start command re-tries after the system dialog closes.
 */
function _installPermissionResultListener() {
  const bus = window.__pulsarBus;
  if (!bus) return;

  const onResult = ({ granted }) => {
    if (granted) {
      _micPermState = 'granted';
      _micActive = true;
    } else {
      _micPermState = 'denied';
      _micActive = false;
    }
    _renderMicState();
    bus.off('mic-permission-result', onResult);
  };

  bus.on('mic-permission-result', onResult);
}

/**
 * Open Android's app permission settings page so the user can grant RECORD_AUDIO
 * if they previously denied it (Android doesn't show the dialog a third time).
 */
function _openMicSettings() {
  // The Tauri core `open` plugin / shell plugin would launch Intent.ACTION_APPLICATION_DETAILS_SETTINGS.
  // Here we emit a bus event that W4-rust-client (or a future settings command) can act on.
  const bus = window.__pulsarBus;
  if (bus) bus.emit('open-mic-settings', {});
  // Fallback: surface a hint in the sub-label
  const sub = $('audio-mic-sub');
  if (sub) sub.textContent = t('audio.micPermHint');
}

// ── Auto-mute on background ───────────────────────────────────────────────────

function _wireVisibility() {
  if (_visibilityWired) return;
  _visibilityWired = true;

  document.addEventListener('visibilitychange', () => {
    if (document.hidden && _micActive) {
      _stopMic();
    }
  });
}

// ── Render helpers ────────────────────────────────────────────────────────────

function _renderMuteState() {
  const btn  = $('audio-mute-btn');
  const lbl  = $('audio-mute-label');
  const chip = $('audio-mute-chip');
  const wrap = btn?.querySelector('.audio-icon-wrap');

  if (btn) {
    btn.setAttribute('aria-pressed', String(_muted));
    btn.setAttribute('aria-label', _muted ? t('audio.unmute') : t('audio.mute'));
    btn.classList.toggle('audio-muted', _muted);
  }
  if (wrap) wrap.innerHTML = _muted ? _iconMuted() : _iconSpeaker();
  if (lbl)  lbl.textContent  = _muted ? t('audio.muted') : t('audio.unmuted');
  if (chip) chip.style.display = _muted ? 'inline-flex' : 'none';
}

function _renderMicState() {
  const btn   = $('audio-mic-btn');
  const lbl   = $('audio-mic-label');
  const sub   = $('audio-mic-sub');
  const dot   = $('audio-rec-dot');
  const pdot  = $('audio-perm-dot');
  const wrap  = btn?.querySelector('.audio-icon-wrap');
  const sheet = $('audio-perm-sheet');
  const pbtn  = $('audio-perm-btn');

  if (btn) {
    btn.setAttribute('aria-pressed', String(_micActive));
    btn.setAttribute('aria-label', _micBtnLabel());
    btn.classList.toggle('mic-active', _micActive);
    btn.classList.toggle('mic-denied', _micPermState === 'denied');
    btn.classList.toggle('mic-requesting', _micPermState === 'requesting');
  }
  if (wrap)  wrap.innerHTML     = _micIcon();
  if (lbl)   lbl.textContent    = _micStateLabel();
  if (sub)   sub.textContent    = _micSubLabel();
  if (dot)   dot.style.display  = _micActive ? 'block' : 'none';
  if (pdot)  pdot.style.display = _micPermState === 'requesting' ? 'block' : 'none';
  if (sheet) sheet.style.display = _micPermState === 'requesting' ? 'flex' : 'none';
  if (pbtn)  pbtn.style.display = _micPermState === 'denied' ? 'inline-flex' : 'none';
}

// ── Label helpers ─────────────────────────────────────────────────────────────

function _micBtnLabel() {
  if (_micPermState === 'denied')    return t('audio.micPermDenied');
  if (_micPermState === 'requesting') return t('audio.micPermRequesting');
  return _micActive ? t('audio.micOff') : t('audio.micOn');
}

function _micStateLabel() {
  if (_micPermState === 'denied')    return t('audio.micPermDenied');
  if (_micPermState === 'requesting') return t('audio.micPermRequesting');
  if (_micActive)                     return t('audio.micActive');
  return t('audio.micOff');
}

function _micSubLabel() {
  if (_micPermState === 'denied')    return t('audio.micPermHint');
  if (_micPermState === 'requesting') return t('audio.micPermWaiting');
  return t('audio.micSub');
}

// ── SVG icons ─────────────────────────────────────────────────────────────────

function _iconSpeaker() {
  return `<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
    <path d="M15.54 8.46a5 5 0 010 7.07"/>
    <path d="M19.07 4.93a10 10 0 010 14.14"/>
  </svg>`;
}

function _iconMuted() {
  return `<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
    <line x1="22" y1="9" x2="16" y2="15"/>
    <line x1="16" y1="9" x2="22" y2="15"/>
  </svg>`;
}

function _iconMicOn() {
  return `<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <path d="M12 2a3 3 0 013 3v7a3 3 0 01-6 0V5a3 3 0 013-3z"/>
    <path d="M19 10v2a7 7 0 01-14 0v-2"/>
    <line x1="12" y1="19" x2="12" y2="23"/>
    <line x1="8" y1="23" x2="16" y2="23"/>
  </svg>`;
}

function _iconMicOff() {
  return `<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <line x1="1" y1="1" x2="23" y2="23"/>
    <path d="M9 9v3a3 3 0 005.12 2.12M15 9.34V5a3 3 0 00-5.94-.6"/>
    <path d="M17 16.95A7 7 0 015 12v-2m14 0v2a7 7 0 01-.11 1.23"/>
    <line x1="12" y1="19" x2="12" y2="23"/>
    <line x1="8" y1="23" x2="16" y2="23"/>
  </svg>`;
}

function _iconMicDenied() {
  return `<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <circle cx="12" cy="12" r="10"/>
    <path d="M9 9v3a3 3 0 005.12 2.12M15 9.34V5a3 3 0 00-5.94-.6"/>
    <line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/>
  </svg>`;
}

function _micIcon() {
  if (_micPermState === 'denied')    return _iconMicDenied();
  if (_micActive)                    return _iconMicOn();
  return _iconMicOff();
}

// ── Inline styles ─────────────────────────────────────────────────────────────

function _injectStyles() {
  if (document.getElementById('audio-card-styles')) return;
  const style = document.createElement('style');
  style.id = 'audio-card-styles';
  style.textContent = `
/* ================================================================
   audio.js card styles  —  W3-media-native + W4-mic
   Touch-first, ≥44px targets, brand-token theming.
   ================================================================ */

.audio-card {
  display: flex;
  flex-direction: column;
  gap: 0;
  padding: 4px 0 calc(4px + var(--safe-bottom, 0px));
  width: 100%;
}

/* ── Row layout ── */
.audio-row {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 16px;
  min-height: 64px;
}

/* ── Icon button base ── */
.audio-icon-btn {
  position: relative;
  flex: none;
  width: 52px;
  height: 52px;
  border-radius: 50%;
  border: 1.5px solid var(--border);
  background: var(--surface-2);
  color: var(--text);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition:
    background var(--dur, 180ms) var(--ease, ease),
    border-color var(--dur, 180ms) var(--ease, ease),
    color var(--dur, 180ms) var(--ease, ease),
    transform 80ms ease;
  touch-action: manipulation;
  -webkit-tap-highlight-color: transparent;
  /* Ensure the tap target is ≥44px even if the element is smaller */
  min-width: 44px;
  min-height: 44px;
}
.audio-icon-btn:active { transform: scale(0.92); }
.audio-icon-btn:focus-visible {
  outline: 2px solid var(--brand, var(--accent));
  outline-offset: 3px;
}

/* Muted state */
.audio-icon-btn.audio-muted {
  background: oklch(from var(--brand, var(--accent)) l c h / 0.12);
  border-color: oklch(from var(--brand, var(--accent)) l c h / 0.35);
  color: var(--brand, var(--accent));
}

/* Mic active — red tint */
.audio-icon-btn.mic-active {
  background: oklch(from var(--danger, #ef4444) l c h / 0.1);
  border-color: oklch(from var(--danger, #ef4444) l c h / 0.35);
  color: var(--danger, #ef4444);
}

/* Mic permission requesting — amber tint */
.audio-icon-btn.mic-requesting {
  background: oklch(0.85 0.16 80 / 0.12);
  border-color: oklch(0.75 0.16 80 / 0.4);
  color: oklch(0.65 0.16 80);
  animation: audio-perm-pulse 2s ease-in-out infinite;
}
@keyframes audio-perm-pulse {
  0%, 100% { opacity: 1; }
  50%       { opacity: 0.65; }
}

/* Mic permission denied — grey-out */
.audio-icon-btn.mic-denied {
  opacity: 0.5;
  cursor: not-allowed;
}

.audio-icon-wrap {
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}

/* ── Recording indicator dot — pulsing red ── */
.audio-rec-dot {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: var(--danger, #ef4444);
  box-shadow: 0 0 0 0 oklch(from var(--danger, #ef4444) l c h / 0.6);
  animation: audio-rec-pulse 1.4s var(--ease, ease) infinite;
}
@keyframes audio-rec-pulse {
  0%   { box-shadow: 0 0 0 0   oklch(from var(--danger, #ef4444) l c h / 0.5); }
  70%  { box-shadow: 0 0 0 7px oklch(from var(--danger, #ef4444) l c h / 0);   }
  100% { box-shadow: 0 0 0 0   oklch(from var(--danger, #ef4444) l c h / 0);   }
}

/* ── Permission pending dot — amber pulse ── */
.audio-perm-dot {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: oklch(0.75 0.16 80);
  animation: audio-rec-pulse 1.8s ease infinite;
}

/* ── Row info ── */
.audio-row-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.audio-row-label {
  font-family: var(--font-sans, 'Hanken Grotesk', sans-serif);
  font-size: 15px;
  font-weight: 600;
  color: var(--text);
  line-height: 1.3;
}

.audio-row-sub {
  font-size: 12.5px;
  color: var(--text-faint);
  line-height: 1.4;
}

/* ── Muted chip badge ── */
.audio-mute-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 9px;
  border-radius: var(--r-pill, 999px);
  background: oklch(from var(--brand, var(--accent)) l c h / 0.12);
  color: var(--brand, var(--accent));
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.02em;
  white-space: nowrap;
  flex: none;
}

/* ── Divider ── */
.audio-divider {
  height: 1px;
  background: var(--border);
  margin: 0 16px;
}

/* ── "İzin ver" / Settings button ── */
.audio-perm-btn {
  flex: none;
  padding: 6px 12px;
  border-radius: var(--r-pill, 999px);
  border: 1.5px solid var(--brand, var(--accent));
  background: transparent;
  color: var(--brand, var(--accent));
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  white-space: nowrap;
  min-height: 36px;
  touch-action: manipulation;
  transition: background var(--dur, 180ms) var(--ease, ease);
}
.audio-perm-btn:active {
  background: oklch(from var(--brand, var(--accent)) l c h / 0.15);
}

/* ── Permission explanation sheet (inline) ── */
.audio-perm-sheet {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  margin: 0 16px 12px;
  padding: 12px 14px;
  border-radius: var(--r-card, 12px);
  background: oklch(0.85 0.16 80 / 0.1);
  border: 1px solid oklch(0.75 0.16 80 / 0.25);
  animation: audio-sheet-in 240ms var(--ease, ease) both;
}
@keyframes audio-sheet-in {
  from { opacity: 0; transform: translateY(-6px); }
  to   { opacity: 1; transform: translateY(0); }
}

.audio-perm-icon {
  flex: none;
  color: oklch(0.65 0.16 80);
  display: flex;
  align-items: center;
  padding-top: 1px;
}

.audio-perm-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
  font-size: 13px;
  color: var(--text);
  line-height: 1.45;
}

.audio-perm-text strong {
  font-weight: 700;
  font-size: 13.5px;
}
`;
  document.head.appendChild(style);
}

// ── Bus wiring ────────────────────────────────────────────────────────────────

function _wireBus() {
  const tryBus = () => {
    const bus = window.__pulsarBus;
    if (!bus) return false;

    // New session started — update slot + mode, reset all state
    bus.on('session-started', ({ slot, mode }) => {
      _slot           = slot ?? 0;
      _mode           = mode ?? 'remote';
      _muted          = false;
      _micActive      = false;
      _micPermState   = 'unknown';
      // Show/hide mic row based on new mode
      const micRow  = $('audio-mic-row');
      const divider = $('audio-divider');
      if (micRow)  micRow.style.display  = _mode === 'remote' ? 'flex' : 'none';
      if (divider) divider.style.display = _mode === 'remote' ? 'block' : 'none';
    });

    // Session ended — tear down mic if still running
    bus.on('session-ended', () => {
      if (_micActive) {
        _stopMic();
      }
      _muted        = false;
      _micActive    = false;
      _micPermState = 'unknown';
    });

    // Background / foreground — auto-mute mic on backgrounding (DT-mic)
    bus.on('session-bg', ({ hidden }) => {
      if (hidden && _micActive) {
        _stopMic();
      }
    });

    // Permission result from W4-rust-client mic_start re-try
    bus.on('mic-permission-result', ({ granted }) => {
      if (granted) {
        _micPermState = 'granted';
        // Auto-start recording since the user just tapped Allow
        _startMic();
      } else {
        _micPermState = 'denied';
        _micActive    = false;
        _renderMicState();
      }
    });

    return true;
  };

  if (!tryBus()) {
    window.addEventListener('load', () => tryBus(), { once: true });
  }
}

// ── Register card ─────────────────────────────────────────────────────────────

function _register() {
  const tryReg = () => {
    const overlay = window.__pulsarOverlay;
    if (!overlay || typeof overlay.registerCard !== 'function') return false;

    overlay.registerCard({
      id:      'audio',
      modes:   ['remote', 'game'],
      section: 'audio',
      order:   10,
      label:   () => t('audio.cardLabel'),
      mount,
    });
    return true;
  };

  if (!tryReg()) {
    window.addEventListener('load', () => tryReg(), { once: true });
  }
}

// ── Boot ──────────────────────────────────────────────────────────────────────

_injectStyles();
_wireBus();
_wireVisibility();
_register();
