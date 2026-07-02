/**
 * screens/settings.js — Ayarlar (Settings) screen
 *
 * W5-settings-lang — full implementation per §5 brief + DT-language design spec.
 *
 * Features:
 *   - Relay server field (validated host:port / host input)
 *   - Connection method segmented control (auto / p2p-only / relay-only)
 *   - Display name text input
 *   - Video codec segmented control (auto / h265 / h264)
 *   - Default quality segmented control (latency / balanced / quality)
 *   - Theme segmented control (Light / Dark / System) → setTheme()
 *   - Language segmented control (tr / en / ru / kk) → setLang() + set_config + <html lang> + live re-render
 *   - About / GPLv3 row
 *
 * Live re-render: all string nodes use data-i18n attributes and are refreshed
 * on the 'langchange' window event, so switching language re-renders instantly.
 *
 * Design: touch-first, ≥44px tap targets, .seg pattern, .field/.card atoms,
 * .setting rows, Pulsar indigo accent, saves on blur/change with a toast
 * confirmation (t('settings.savedToast')).
 *
 * Commands called: set_config (via store/config.js setConfig)
 */

import { registerScreen }           from '../router.js';
import { t, setLang, lang }         from '../i18n.js';
import { theme, setTheme }          from '../theme.js';
import { getConfig, setConfig }     from '../store/config.js';
import { getPref, setPref, gamepadTarget, setGamepadTarget } from '../store/prefs.js';
import { invoke }                   from '../tauri.js';
import { resolveFrameRate, refreshHz, resolveResolution } from './connect.js';
import { connectedPads } from '../session/gamepad-monitor.js';

// ── Expose config module on window so i18n.js setLang can call setConfig ──────
// (i18n.js checks window.__pulsarConfigMod before falling back to raw invoke)
if (typeof window !== 'undefined') {
  window.__pulsarConfigMod = { setConfig };
}

// ── Save-toast state ──────────────────────────────────────────────────────────

let _toastTimer = null;

/** Show a brief "saved" toast in the settings section. */
function showToast(msg) {
  const el = document.getElementById('settings-toast');
  if (!el) return;
  el.textContent = msg;
  el.classList.add('show');
  clearTimeout(_toastTimer);
  _toastTimer = setTimeout(() => el && el.classList.remove('show'), 2200);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/**
 * Wire a segmented control:
 *   - highlights the active option (aria-selected)
 *   - calls onChange(value) on click
 * @param {HTMLElement} container
 * @param {string} currentValue
 * @param {(v:string)=>void} onChange
 */
function wireSegControl(container, currentValue, onChange) {
  if (!container) return;
  const buttons = container.querySelectorAll('button[data-v]');
  buttons.forEach((btn) => {
    const v = btn.dataset.v;
    btn.setAttribute('aria-selected', String(v === currentValue));
    btn.addEventListener('click', () => {
      buttons.forEach((b) => b.setAttribute('aria-selected', 'false'));
      btn.setAttribute('aria-selected', 'true');
      onChange(v);
    });
  });
}

/**
 * Set the active value in an already-rendered segmented control without
 * rewiring listeners (used on config load / language change).
 */
function setSegValue(container, value) {
  if (!container) return;
  container.querySelectorAll('button[data-v]').forEach((btn) => {
    btn.setAttribute('aria-selected', String(btn.dataset.v === value));
  });
}

// ── Section render (called once from mount, re-called on langchange) ──────────

/**
 * Render / re-render all translatable text nodes inside #t-settings.
 * Does NOT destroy and recreate the DOM — just updates text content so form
 * state (input values, seg selections) is preserved.
 */
function renderText() {
  const section = document.getElementById('t-settings');
  if (!section) return;
  section.querySelectorAll('[data-i18n]').forEach((el) => {
    const key = el.dataset.i18n;
    el.textContent = t(key);
  });
  // aria-labels
  section.querySelectorAll('[data-i18n-aria]').forEach((el) => {
    el.setAttribute('aria-label', t(el.dataset.i18nAria));
  });
  // placeholders
  section.querySelectorAll('[data-i18n-ph]').forEach((el) => {
    el.setAttribute('placeholder', t(el.dataset.i18nPh));
  });
}

// ── Build the settings DOM (first mount only) ─────────────────────────────────

function buildSettingsDom(section) {
  section.innerHTML = `
    <!-- Header -->
    <h2 class="title" data-i18n="settings.title">${t('settings.title')}</h2>
    <p class="sub" data-i18n="settings.sub">${t('settings.sub')}</p>

    <!-- Save toast -->
    <div id="settings-toast" class="settings-toast" aria-live="polite"></div>

    <!-- ── Ağ / Network ── -->
    <div class="sect-label" data-i18n="settings.relay" style="margin-bottom:8px;">${t('settings.relay')}</div>
    <div class="card">
      <div class="field">
        <label for="s-relay" data-i18n="settings.relay">${t('settings.relay')}</label>
        <input
          id="s-relay"
          class="input mono"
          type="text"
          inputmode="url"
          autocomplete="off"
          autocorrect="off"
          autocapitalize="none"
          spellcheck="false"
          data-i18n-aria="settings.relayAria"
          aria-label="${t('settings.relayAria')}"
          placeholder="127.0.0.1:21116"
        />
        <span class="settings-field-hint" data-i18n="settings.relayDesc">${t('settings.relayDesc')}</span>
      </div>

      <div class="field">
        <label data-i18n="settings.connMethod">${t('settings.connMethod')}</label>
        <div class="seg" id="s-netmode" role="group" aria-label="${t('settings.connMethod')}">
          <button data-v="auto"      data-i18n="settings.modeAuto"  aria-selected="true">${t('settings.modeAuto')}</button>
          <button data-v="p2p-only"  data-i18n="settings.modeP2p"   aria-selected="false">${t('settings.modeP2p')}</button>
          <button data-v="relay-only" data-i18n="settings.modeRelay" aria-selected="false">${t('settings.modeRelay')}</button>
        </div>
        <span class="settings-field-hint" data-i18n="settings.connMethodDesc">${t('settings.connMethodDesc')}</span>
      </div>

      <div class="field">
        <label for="s-nodeport" data-i18n="m.settings.nodePort">${t('m.settings.nodePort')}</label>
        <input id="s-nodeport" class="input mono" type="number" inputmode="numeric" min="0" max="65535"
               data-i18n-ph="m.settings.localPortOffline" placeholder="${t('m.settings.localPortOffline')}" />
        <span class="settings-field-hint" data-i18n="m.settings.nodePortHint">${t('m.settings.nodePortHint')}</span>
      </div>
    </div>

    <!-- ── Yerel relay / Local relay server ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.localRelay">${t('m.settings.localRelay')}</div>
    <div class="card">
      <div class="setting">
        <div>
          <div class="lbl" data-i18n="m.settings.localRelay">${t('m.settings.localRelay')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.localRelayHint">${t('m.settings.localRelayHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-local-relay" aria-label="${t('m.settings.localRelay')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
      <div class="field" style="margin-top:14px">
        <label for="s-relay-port" data-i18n="m.settings.localRelayPort">${t('m.settings.localRelayPort')}</label>
        <input id="s-relay-port" class="input mono" type="number" inputmode="numeric" min="1" max="65535" placeholder="21116" />
        <span class="settings-field-hint" data-i18n="m.settings.localRelayPortHint">${t('m.settings.localRelayPortHint')}</span>
      </div>
      <div class="field" id="s-relay-addr-field" style="margin-top:14px;display:none">
        <label data-i18n="m.settings.localRelayAddr">${t('m.settings.localRelayAddr')}</label>
        <div id="s-relay-addr" class="relay-addr mono" role="button" tabindex="0"
             aria-label="${t('m.settings.localRelayCopy')}"></div>
        <span class="settings-field-hint" data-i18n="m.settings.localRelayAddrHint">${t('m.settings.localRelayAddrHint')}</span>
      </div>
    </div>

    <!-- ── Cihaz / Device ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="settings.displayName">${t('settings.displayName')}</div>
    <div class="card">
      <div class="field">
        <label for="s-devname" data-i18n="settings.displayName">${t('settings.displayName')}</label>
        <input
          id="s-devname"
          class="input"
          type="text"
          autocomplete="off"
          autocorrect="off"
          data-i18n-ph="settings.displayName"
          placeholder="${t('settings.displayName')}"
        />
        <span class="settings-field-hint" data-i18n="settings.displayNameDesc">${t('settings.displayNameDesc')}</span>
      </div>
      <div class="field" style="margin-top:14px">
        <label data-i18n="settings.avatar">${t('settings.avatar')}</label>
        <div style="display:flex;align-items:center;gap:12px">
          <img id="s-avatar-preview" alt=""
               style="width:48px;height:48px;border-radius:50%;object-fit:cover;background:var(--surface-2);flex:0 0 auto;display:none" />
          <div class="seg" id="s-avatar" role="group" style="flex:1" aria-label="${t('settings.avatar')}">
            <button data-v="wallpaper" data-i18n="settings.avatarWall">${t('settings.avatarWall')}</button>
            <button data-v="photo" data-i18n="settings.avatarPhoto">${t('settings.avatarPhoto')}</button>
            <button data-v="anonymous" data-i18n="settings.avatarAnon">${t('settings.avatarAnon')}</button>
          </div>
        </div>
        <button id="s-avatar-pick" type="button" data-i18n="settings.avatarPick"
                style="margin-top:10px;display:none;padding:10px 14px;border-radius:10px;border:1px solid var(--border);background:var(--surface-2);color:var(--text);font:inherit;width:100%">${t('settings.avatarPick')}</button>
        <input id="s-avatar-file" type="file" accept="image/*" style="display:none" />
        <span class="settings-field-hint" data-i18n="settings.avatarDesc">${t('settings.avatarDesc')}</span>
      </div>
    </div>

    <!-- ── Host access (unattended + permanent password) ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.hostAccess">${t('m.settings.hostAccess')}</div>
    <div class="card">
      <div class="setting">
        <div>
          <div class="lbl" data-i18n="m.settings.unattended">${t('m.settings.unattended')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.unattendedHint">${t('m.settings.unattendedHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-unattended" aria-label="${t('m.settings.unattended')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
      <div class="field" style="margin-top:14px">
        <label for="s-cpw" data-i18n="m.settings.customPw">${t('m.settings.customPw')}</label>
        <input id="s-cpw" class="input mono" type="text" autocomplete="off" autocorrect="off"
               data-i18n-ph="m.settings.customPwPh" placeholder="${t('m.settings.customPwPh')}" />
        <span class="settings-field-hint" id="s-cpw-hint" data-i18n="m.settings.customPwHint">${t('m.settings.customPwHint')}</span>
        <span class="settings-warn" id="s-cpw-warn" data-i18n="m.settings.unattendedWarn"
              style="display:none">${t('m.settings.unattendedWarn')}</span>
      </div>
    </div>

    <!-- ── Session (in-session playback/UI prefs, client-only) ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.sessionSec">${t('m.settings.sessionSec')}</div>
    <div class="card">
      <div class="setting">
        <div>
          <div class="lbl" data-i18n="m.settings.hostAudio">${t('m.settings.hostAudio')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.hostAudioHint">${t('m.settings.hostAudioHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-host-audio" aria-label="${t('m.settings.hostAudio')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
      <div class="setting" style="margin-top:12px">
        <div>
          <div class="lbl" data-i18n="m.settings.perfHud">${t('m.settings.perfHud')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.perfHudHint">${t('m.settings.perfHudHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-perf-hud" aria-label="${t('m.settings.perfHud')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
      <div class="setting" style="margin-top:12px">
        <div>
          <div class="lbl" data-i18n="m.settings.overlayBtn">${t('m.settings.overlayBtn')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.overlayBtnHint">${t('m.settings.overlayBtnHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-overlay-btn" aria-label="${t('m.settings.overlayBtn')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
    </div>

    <!-- ── Controllers (live connected-pad list) ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.controllers">${t('m.settings.controllers')}</div>
    <div class="card">
      <div id="s-controllers-list"></div>
    </div>

    <!-- ── Video ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="settings.codec">${t('settings.codec')}</div>
    <div class="card">
      <div class="field">
        <label data-i18n="settings.codec">${t('settings.codec')}</label>
        <div class="seg" id="s-codec" role="group">
          <button data-v="auto" aria-selected="true"><span data-i18n="codec.auto">${t('codec.auto')}</span></button>
          <button data-v="h265" aria-selected="false">H.265</button>
          <button data-v="h264" aria-selected="false">H.264</button>
        </div>
        <span class="settings-field-hint" data-i18n="settings.codecDesc">${t('settings.codecDesc')}</span>
      </div>

      <div class="field">
        <label data-i18n="settings.quality">${t('settings.quality')}</label>
        <div class="seg" id="s-quality" role="group">
          <button data-v="latency"  aria-selected="false"><span data-i18n="session.qLatency">${t('session.qLatency')}</span></button>
          <button data-v="balanced" aria-selected="true"><span data-i18n="settings.qAuto">${t('settings.qAuto')}</span></button>
          <button data-v="quality"  aria-selected="false"><span data-i18n="session.qQuality">${t('session.qQuality')}</span></button>
        </div>
        <span class="settings-field-hint" data-i18n="settings.qualityDesc">${t('settings.qualityDesc')}</span>
      </div>

      <div class="field">
        <label for="s-framerate" data-i18n="m.settings.frameRate">${t('m.settings.frameRate')}</label>
        <select id="s-framerate" class="input framerate-select">
          <option value="auto" data-i18n="m.settings.frameRateAuto">${t('m.settings.frameRateAuto')}</option>
          <option value="30">30 FPS</option>
          <option value="60">60 FPS</option>
          <option value="120">120 FPS</option>
          <option value="144">144 FPS</option>
          <option value="168">168 FPS</option>
          <option value="244">244 FPS</option>
          <option value="unlimited" data-i18n="m.settings.frameRateUnlimited">${t('m.settings.frameRateUnlimited')}</option>
        </select>
        <span class="settings-field-hint" data-i18n="m.settings.frameRateHint">${t('m.settings.frameRateHint')}</span>
      </div>

      <div class="field">
        <label for="s-resolution" data-i18n="m.settings.resolution">${t('m.settings.resolution')}</label>
        <select id="s-resolution" class="input framerate-select">
          <option value="auto" data-i18n="m.settings.resolutionAuto">${t('m.settings.resolutionAuto')}</option>
          <option value="720">720p</option>
          <option value="1080">1080p</option>
          <option value="1440">1440p</option>
          <option value="2160">2160p (4K)</option>
        </select>
        <span class="settings-field-hint" data-i18n="m.settings.resolutionHint">${t('m.settings.resolutionHint')}</span>
      </div>

      <div class="setting" style="margin-top:4px">
        <div>
          <div class="lbl" data-i18n="m.settings.hdr">${t('m.settings.hdr')}</div>
          <div class="settings-field-hint" data-i18n="m.settings.hdrHint">${t('m.settings.hdrHint')}</div>
        </div>
        <label class="toggle">
          <input type="checkbox" id="s-hdr" aria-label="${t('m.settings.hdr')}" />
          <span class="track"></span>
          <span class="thumb"></span>
        </label>
      </div>
    </div>

    <!-- ── Tema / Theme ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.theme">${t('m.settings.theme')}</div>
    <div class="card">
      <div class="setting">
        <div>
          <div class="lbl" data-i18n="m.settings.theme">${t('m.settings.theme')}</div>
        </div>
        <div class="seg theme-seg" id="s-theme" role="group" aria-label="${t('m.settings.theme')}" style="min-width:210px;">
          <button data-v="light" aria-selected="${theme() === 'light' ? 'true' : 'false'}"
            data-i18n-self="m.settings.themeLight">${t('m.settings.themeLight')}</button>
          <button data-v="dark" aria-selected="${theme() === 'dark' ? 'true' : 'false'}"
            data-i18n-self="m.settings.themeDark">${t('m.settings.themeDark')}</button>
          <button data-v="system" aria-selected="${theme() === 'system' ? 'true' : 'false'}"
            data-i18n-self="m.settings.themeSystem">${t('m.settings.themeSystem')}</button>
        </div>
      </div>
    </div>

    <!-- ── Dil / Language ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="m.settings.lang">${t('m.settings.lang')}</div>
    <div class="card">
      <div class="setting">
        <div>
          <div class="lbl" data-i18n="m.settings.lang">${t('m.settings.lang')}</div>
        </div>
        <!-- Selection input (scales to any number of languages; native names). -->
        <select id="s-lang" class="input lang-select" aria-label="${t('m.settings.lang')}">
          <option value="tr" ${lang === 'tr' ? 'selected' : ''}>Türkçe</option>
          <option value="en" ${lang === 'en' ? 'selected' : ''}>English</option>
          <option value="ru" ${lang === 'ru' ? 'selected' : ''}>Русский</option>
          <option value="kk" ${lang === 'kk' ? 'selected' : ''}>Қазақша</option>
        </select>
      </div>
    </div>

    <!-- ── Hakkında / About ── -->
    <div class="sect-label" style="margin:16px 2px 8px;" data-i18n="settings.version">${t('settings.version')}</div>
    <div class="card">
      <div class="setting" id="s-about-card" role="button" tabindex="0" style="cursor:pointer">
        <div>
          <div class="lbl">Pulsar Mobile <span id="s-version" class="mono" style="font-weight:500;color:var(--text-muted)"></span></div>
          <div class="hint" data-i18n="m.settings.licensesTap">${t('m.settings.licensesTap')}</div>
        </div>
        <svg viewBox="0 0 24 24" fill="none" width="20" height="20" aria-hidden="true">
          <path d="M9 6l6 6-6 6" stroke="var(--text-muted)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </div>
    </div>
  `;
}

// ── Wire interactive elements (after buildSettingsDom) ────────────────────────

/** Save a config patch and show the toast. */
async function save(patch) {
  try {
    await setConfig(patch);
    showToast(t('settings.savedToast'));
  } catch (e) {
    console.warn('[settings] setConfig error', e);
    showToast(t('settings.saveFailed'));
  }
}

/**
 * Refresh the avatar preview <img> for `mode` via the native `self_avatar`
 * resolver (wallpaper-gradient / saved photo → data URL). Hides the image for
 * "anonymous" or when nothing is available. Returns true if an image is shown.
 */
async function updateAvatarPreview(mode) {
  const img = document.getElementById('s-avatar-preview');
  if (!img) return false;
  if (mode === 'anonymous') {
    img.style.display = 'none';
    img.removeAttribute('src');
    return false;
  }
  try {
    const url = await invoke('self_avatar', { mode });
    if (url) {
      img.src = url;
      img.style.display = '';
      return true;
    }
  } catch { /* fall through */ }
  img.style.display = 'none';
  img.removeAttribute('src');
  return false;
}

/**
 * Read an image File, center-crop to a square, scale to `edge`px, and return a
 * JPEG data URL. Used by the avatar photo picker (no permission — the file input
 * grants access to the chosen image only).
 */
function cropImageToDataUrl(file, edge) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      try {
        const side = Math.min(img.width, img.height) || edge;
        const sx = (img.width - side) / 2;
        const sy = (img.height - side) / 2;
        const cv = document.createElement('canvas');
        cv.width = edge; cv.height = edge;
        const ctx = cv.getContext('2d');
        ctx.drawImage(img, sx, sy, side, side, 0, 0, edge, edge);
        URL.revokeObjectURL(url);
        resolve(cv.toDataURL('image/jpeg', 0.8));
      } catch (e) { URL.revokeObjectURL(url); reject(e); }
    };
    img.onerror = () => { URL.revokeObjectURL(url); reject(new Error('image load failed')); };
    img.src = url;
  });
}

/**
 * Reflect the unattended-access state on the password field: when unattended is on
 * the host skips auth entirely, so the permanent password is moot — disable + dim
 * it and surface a warning instead of its normal hint.
 */
function applyUnattendedUi(on) {
  const cpw  = document.getElementById('s-cpw');
  const hint = document.getElementById('s-cpw-hint');
  const warn = document.getElementById('s-cpw-warn');
  if (cpw)  { cpw.disabled = !!on; cpw.style.opacity = on ? '0.45' : ''; }
  if (hint) hint.style.display = on ? 'none' : '';
  if (warn) warn.style.display = on ? '' : 'none';
}

/**
 * Reflect a RelayStatus ({running, ip, port}) on the local-relay UI: sync the
 * toggle and show/hide the address peers connect to.
 */
function applyRelayStatus(st) {
  const toggle = document.getElementById('s-local-relay');
  const field  = document.getElementById('s-relay-addr-field');
  const addr   = document.getElementById('s-relay-addr');
  const running = !!(st && st.running);
  if (toggle) toggle.checked = running;
  if (field)  field.style.display = running ? '' : 'none';
  if (addr) {
    if (running) {
      const ip = (st && st.ip) ? st.ip : '0.0.0.0';
      const port = (st && st.port) ? st.port : 21116;
      addr.textContent = `${ip}:${port}`;
    } else {
      addr.textContent = '';
    }
  }
}

/** Query the live relay status and reflect it (relay survives tab navigation). */
async function refreshRelayStatus() {
  try {
    const st = await invoke('local_relay_status');
    applyRelayStatus(st);
  } catch (_) {
    applyRelayStatus({ running: false });
  }
}

/**
 * Bottom-sheet confirm before enabling unattended access (a security-sensitive
 * toggle). Resolves true only if the user explicitly confirms.
 * @returns {Promise<boolean>}
 */
function confirmUnattended() {
  return new Promise((resolve) => {
    const backdrop = document.createElement('div');
    backdrop.className = 'sheet-backdrop';
    const sheet = document.createElement('div');
    sheet.className = 'sheet';
    sheet.setAttribute('role', 'dialog');
    sheet.setAttribute('aria-modal', 'true');
    sheet.innerHTML = `
      <div class="sheet-handle"></div>
      <div style="font-size:17px;font-weight:700;margin:4px 2px 8px;">${t('m.settings.unattendedConfirmTitle')}</div>
      <div style="font-size:14px;color:var(--text-muted);line-height:1.45;margin:0 2px 18px;">${t('m.settings.unattendedConfirmBody')}</div>
      <div style="display:flex;gap:10px;">
        <button class="btn btn-ghost" style="flex:1" id="cf-cancel">${t('m.common.cancel')}</button>
        <button class="btn" style="flex:1;background:var(--danger);color:oklch(0.99 0 0)" id="cf-ok">${t('m.settings.unattendedConfirmYes')}</button>
      </div>`;
    document.body.appendChild(backdrop);
    document.body.appendChild(sheet);
    requestAnimationFrame(() => { backdrop.classList.add('open'); sheet.classList.add('open'); });
    let done = false;
    const close = (val) => {
      if (done) return; done = true;
      backdrop.classList.remove('open'); sheet.classList.remove('open');
      setTimeout(() => { backdrop.remove(); sheet.remove(); }, 280);
      resolve(val);
    };
    sheet.querySelector('#cf-cancel').addEventListener('click', () => close(false));
    sheet.querySelector('#cf-ok').addEventListener('click', () => close(true));
    backdrop.addEventListener('click', () => close(false));
  });
}

/**
 * Open-source licenses bottom sheet. Loads `licenses.json` (every Rust dependency
 * + its SPDX license, generated from `cargo metadata`) and renders them grouped by
 * license, under the project's own GPLv3 line.
 */
let _licensesCache = null;
async function openLicenses() {
  const backdrop = document.createElement('div');
  backdrop.className = 'sheet-backdrop';
  const sheet = document.createElement('div');
  sheet.className = 'sheet licenses-sheet';
  sheet.setAttribute('role', 'dialog');
  sheet.setAttribute('aria-modal', 'true');
  sheet.innerHTML = `
    <div class="sheet-handle"></div>
    <div style="font-size:17px;font-weight:700;margin:2px 2px 6px;">${t('m.settings.licensesTitle')}</div>
    <div style="font-size:13px;color:var(--text-muted);line-height:1.45;margin:0 2px 12px;">${t('m.settings.licensesIntro')}</div>
    <div class="licenses-list" id="licenses-list">…</div>`;
  document.body.appendChild(backdrop);
  document.body.appendChild(sheet);
  requestAnimationFrame(() => { backdrop.classList.add('open'); sheet.classList.add('open'); });
  const close = () => {
    backdrop.classList.remove('open'); sheet.classList.remove('open');
    setTimeout(() => { backdrop.remove(); sheet.remove(); }, 280);
  };
  backdrop.addEventListener('click', close);

  const esc = (s) => String(s).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
  const list = () => document.getElementById('licenses-list');
  try {
    if (!_licensesCache) _licensesCache = await (await fetch('licenses.json')).json();
    const groups = {};
    for (const d of _licensesCache) (groups[d.license] ??= []).push(d);
    const order = Object.keys(groups).sort((a, b) => groups[b].length - groups[a].length);
    let html = `<div class="lic-self">Pulsar — GPLv3</div>`;
    html += `<div class="lic-count">${_licensesCache.length} ${t('m.settings.licensesCount')}</div>`;
    for (const lic of order) {
      html += `<div class="lic-group"><div class="lic-lic">${esc(lic)} <span class="lic-n">${groups[lic].length}</span></div>`;
      html += groups[lic].map((d) => `<div class="lic-dep">${esc(d.name)} <span class="lic-v">${esc(d.version)}</span></div>`).join('');
      html += `</div>`;
    }
    if (list()) list().innerHTML = html;
  } catch (_) {
    if (list()) list().innerHTML = `<div class="lic-self">Pulsar — GPLv3</div>`;
  }
}

/** Render the live connected-controller list in the Settings Controllers section. */
function renderControllers() {
  const el = document.getElementById('s-controllers-list');
  if (!el) return;
  const pads = connectedPads();
  if (!pads.length) { el.innerHTML = `<div class="pad-empty">${t('m.gamepad.none')}</div>`; return; }
  el.innerHTML = pads.map((p) => {
    const bat = p.battery >= 0 ? `<span class="pad-bat">🔋 %${p.battery}</span>` : '';
    const name = String(p.name).replace(/[<>&]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;' }[c]));
    const tgt = gamepadTarget(p.name);
    const opt = (v, l) => `<option value="${v}"${tgt === v ? ' selected' : ''}>${l}</option>`;
    // The host emulates the controller as this — Auto, Xbox 360, or DualShock 4.
    return `<div class="pad-item">
      <div class="pad-row"><span class="pad-dot"></span><span class="pad-name">${name}</span>${bat}</div>
      <select class="input framerate-select pad-target" data-pad="${encodeURIComponent(p.name)}" aria-label="${t('m.gamepad.emuAuto')}">
        ${opt('auto', `${t('m.gamepad.emuAuto')} (Xbox 360)`)}${opt('xbox', 'Xbox 360')}${opt('ds4', 'DualShock 4')}
      </select>
    </div>`;
  }).join('');
  el.querySelectorAll('.pad-target').forEach((sel) => {
    sel.addEventListener('change', () => setGamepadTarget(decodeURIComponent(sel.dataset.pad), sel.value));
  });
}
// Keep the Settings list live as controllers connect/disconnect.
if (typeof window !== 'undefined') window.addEventListener('pulsar-pads-changed', renderControllers);

function wireFields(section) {
  // ── Relay input: save on blur ──
  const relayEl = document.getElementById('s-relay');
  if (relayEl) {
    relayEl.addEventListener('blur', () => {
      const v = relayEl.value.trim();
      if (v) save({ relay: v });
    });
  }

  // ── Network mode seg ──
  const netmodeSeg = document.getElementById('s-netmode');
  wireSegControl(netmodeSeg, 'auto', (v) => save({ networkMode: v }));

  // ── Device name: save on blur ──
  const devnameEl = document.getElementById('s-devname');
  if (devnameEl) {
    devnameEl.addEventListener('blur', () => {
      const v = devnameEl.value.trim();
      if (v) save({ deviceName: v });
    });
  }

  // ── Avatar (identity image): wallpaper-gradient / photo / anonymous ──
  const avatarSeg  = document.getElementById('s-avatar');
  const avatarPick = document.getElementById('s-avatar-pick');
  const avatarFile = document.getElementById('s-avatar-file');
  wireSegControl(avatarSeg, 'wallpaper', (v) => {
    if (avatarPick) avatarPick.style.display = (v === 'photo') ? '' : 'none';
    save({ avatarMode: v });
    updateAvatarPreview(v).then((hasImg) => {
      // Switching to "photo" with no saved image yet → open the picker.
      if (v === 'photo' && !hasImg && avatarFile) avatarFile.click();
    });
  });
  if (avatarPick && avatarFile) {
    avatarPick.addEventListener('click', () => avatarFile.click());
    avatarFile.addEventListener('change', async () => {
      const f = avatarFile.files && avatarFile.files[0];
      avatarFile.value = '';
      if (!f) return;
      try {
        const dataUrl = await cropImageToDataUrl(f, 96);
        const b64 = (dataUrl.split(',')[1]) || '';
        await invoke('set_avatar_image', { data: b64 });
        await save({ avatarMode: 'photo' });
        const img = document.getElementById('s-avatar-preview');
        if (img) { img.src = dataUrl; img.style.display = ''; }
      } catch (e) {
        console.warn('[settings] avatar pick failed', e);
        showToast(t('settings.saveFailed'));
      }
    });
  }

  // ── Host access: unattended toggle + permanent password ──
  // Both feed the host's auth (config-driven): the one-time password keeps working
  // alongside; unattended skips the approval prompt. See host.rs recv_auth.
  const unattendedEl = document.getElementById('s-unattended');
  if (unattendedEl) {
    unattendedEl.addEventListener('change', () => {
      if (unattendedEl.checked) {
        // Enabling is security-sensitive (anyone can connect unattended) — confirm first.
        confirmUnattended().then((ok) => {
          if (ok) { save({ unattendedAccess: true }); applyUnattendedUi(true); }
          else { unattendedEl.checked = false; applyUnattendedUi(false); }
        });
      } else {
        save({ unattendedAccess: false });
        applyUnattendedUi(false);
      }
    });
  }
  const cpwEl = document.getElementById('s-cpw');
  if (cpwEl) {
    cpwEl.addEventListener('blur', () => save({ connectPassword: cpwEl.value.trim() }));
  }

  // ── Node port: save on blur (clamped to a valid u16) ──
  const nodePortEl = document.getElementById('s-nodeport');
  if (nodePortEl) {
    nodePortEl.addEventListener('blur', () => {
      let n = parseInt(nodePortEl.value, 10);
      if (!Number.isFinite(n) || n < 0) n = 0;
      if (n > 65535) n = 65535;
      nodePortEl.value = n ? String(n) : '';
      save({ nodePort: n });
    });
  }

  // ── Local relay: toggle start/stop + show the address peers connect to ──
  const relayToggle = document.getElementById('s-local-relay');
  if (relayToggle) {
    relayToggle.addEventListener('change', async () => {
      if (relayToggle.checked) {
        const portEl = document.getElementById('s-relay-port');
        let port = parseInt(portEl && portEl.value, 10);
        if (!Number.isFinite(port) || port < 1 || port > 65535) port = 0; // 0 → rust default 21116
        try {
          const st = await invoke('start_local_relay', { port });
          applyRelayStatus(st);
          showToast(t('m.settings.localRelayStarted'));
        } catch (e) {
          console.warn('[settings] start_local_relay error', e);
          relayToggle.checked = false;
          applyRelayStatus({ running: false });
          showToast(t('m.settings.localRelayFailed'));
        }
      } else {
        try { await invoke('stop_local_relay'); } catch (_) {}
        applyRelayStatus({ running: false });
        showToast(t('m.settings.localRelayStopped'));
      }
    });
  }
  // Port input: persist the chosen port; if the relay is live, restart it on the new port.
  const relayPortEl = document.getElementById('s-relay-port');
  if (relayPortEl) {
    relayPortEl.addEventListener('blur', async () => {
      let n = parseInt(relayPortEl.value, 10);
      if (!Number.isFinite(n) || n < 1 || n > 65535) { relayPortEl.value = ''; setPref('localRelayPort', 0); return; }
      relayPortEl.value = String(n);
      setPref('localRelayPort', n);
      // Live re-bind: if running, stop + start on the new port so peers' address matches.
      if (relayToggle && relayToggle.checked) {
        try {
          await invoke('stop_local_relay');
          const st = await invoke('start_local_relay', { port: n });
          applyRelayStatus(st);
          showToast(t('m.settings.localRelayStarted'));
        } catch (e) {
          relayToggle.checked = false;
          applyRelayStatus({ running: false });
          showToast(t('m.settings.localRelayFailed'));
        }
      }
    });
  }
  // Tap the address to copy it (so it's easy to type into other devices).
  const relayAddrEl = document.getElementById('s-relay-addr');
  if (relayAddrEl) {
    const copy = async () => {
      const txt = relayAddrEl.textContent.trim();
      if (!txt) return;
      try {
        if (navigator.clipboard) await navigator.clipboard.writeText(txt);
      } catch (_) {}
      if (navigator.vibrate) { try { navigator.vibrate(20); } catch (_) {} }
      showToast(t('m.settings.localRelayCopied'));
    };
    relayAddrEl.addEventListener('click', copy);
    relayAddrEl.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); copy(); } });
  }
  // Reflect the live relay status (it may already be running from a prior visit).
  refreshRelayStatus();

  // ── Session prefs (client-only localStorage, applied where used) ──
  const hostAudioEl = document.getElementById('s-host-audio');
  if (hostAudioEl) {
    hostAudioEl.addEventListener('change', () => setPref('playHostAudio', hostAudioEl.checked));
  }
  const perfHudEl = document.getElementById('s-perf-hud');
  if (perfHudEl) {
    perfHudEl.addEventListener('change', () => {
      setPref('hudVisible', perfHudEl.checked);
      // Live-apply if a HUD strip is currently mounted.
      const strip = document.getElementById('hud-strip');
      if (strip) strip.style.display = perfHudEl.checked ? '' : 'none';
    });
  }
  const overlayBtnEl = document.getElementById('s-overlay-btn');
  if (overlayBtnEl) {
    overlayBtnEl.addEventListener('change', () => {
      setPref('overlayButton', overlayBtnEl.checked);
      // Live-apply if the FAB is currently mounted (in-session).
      const fab = document.getElementById('overlay-fab');
      if (fab) fab.style.display = overlayBtnEl.checked ? '' : 'none';
    });
  }

  // ── Frame rate select: persist + apply live to the active session ──
  const frameRateEl = document.getElementById('s-framerate');
  if (frameRateEl) {
    frameRateEl.addEventListener('change', () => {
      let v = frameRateEl.value;
      if (v !== 'auto' && v !== 'unlimited') v = parseInt(v, 10) || 'auto';
      setPref('frameRate', v);
      // Restream the live session at the new fps (set_play_fps; 0 = unlimited).
      if (document.body.classList.contains('in-session')) {
        invoke('set_play_fps', { slot: 0, fps: resolveFrameRate() }).catch(() => {});
      }
    });
  }

  // ── Resolution select: persist + apply live (set_play_resolution) ──
  const resEl = document.getElementById('s-resolution');
  if (resEl) {
    resEl.addEventListener('change', () => {
      setPref('resolution', resEl.value);
      if (document.body.classList.contains('in-session')) {
        const r = resolveResolution();
        invoke('set_play_resolution', { slot: 0, width: r.width, height: r.height }).catch(() => {});
      }
    });
  }

  // ── HDR toggle: persist (applies on the next connect — no live HDR restream) ──
  const hdrEl = document.getElementById('s-hdr');
  if (hdrEl) {
    hdrEl.addEventListener('change', () => setPref('hdr', hdrEl.checked));
  }

  // ── About card: show the real version + open the open-source licenses sheet ──
  const verEl = document.getElementById('s-version');
  if (verEl) {
    invoke('app_build_info')
      .then((b) => {
        const v = `v${b.version}`;
        verEl.textContent = b.local ? `${v} · ${t('m.settings.localBuild')}` : v;
      })
      .catch(() => { verEl.textContent = ''; });
  }
  const aboutCard = document.getElementById('s-about-card');
  if (aboutCard) {
    const open = () => { if (navigator.vibrate) { try { navigator.vibrate(25); } catch (_) {} } openLicenses(); };
    aboutCard.addEventListener('click', open);
    aboutCard.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(); } });
  }

  // ── Controllers: render the live connected list ──
  renderControllers();

  // ── Codec seg ──
  const codecSeg = document.getElementById('s-codec');
  wireSegControl(codecSeg, 'auto', (v) => save({ codecPref: v }));

  // ── Quality seg ──
  const qualitySeg = document.getElementById('s-quality');
  wireSegControl(qualitySeg, 'balanced', (v) => save({ qualityPref: v }));

  // ── Theme seg (Light / Dark / System) ──
  const themeSeg = document.getElementById('s-theme');
  if (themeSeg) {
    themeSeg.querySelectorAll('button[data-v]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const chosen = btn.dataset.v;
        if (chosen === theme()) return;
        setTheme(chosen); // persists, applies <html data-theme> + native bar style
        themeSeg.querySelectorAll('button[data-v]').forEach((b) =>
          b.setAttribute('aria-selected', b.dataset.v === chosen ? 'true' : 'false'));
      });
    });
  }

  // ── Language select (Türkçe / English / Русский / Қазақша / …) ──
  // setLang() updates the `lang` export, persists to localStorage, syncs Rust
  // config, sets <html lang>, and fires 'langchange' → onLangChange() re-renders.
  const langSel = document.getElementById('s-lang');
  if (langSel) {
    langSel.addEventListener('change', () => {
      if (langSel.value !== lang) setLang(langSel.value);
    });
  }

  // ── Sync inputs from config on first wire ──
  syncFieldsFromConfig();
}

/** Populate input/seg values from the current config cache. */
async function syncFieldsFromConfig() {
  let cfg;
  try {
    cfg = await getConfig();
  } catch (_) {
    // non-fatal; values stay at HTML defaults
    return;
  }
  const relayEl    = document.getElementById('s-relay');
  const devnameEl  = document.getElementById('s-devname');
  const netmodeSeg = document.getElementById('s-netmode');
  const codecSeg   = document.getElementById('s-codec');
  const qualitySeg = document.getElementById('s-quality');
  const langSeg    = document.getElementById('s-lang');

  const unattendedEl = document.getElementById('s-unattended');
  const cpwEl        = document.getElementById('s-cpw');
  const nodePortEl   = document.getElementById('s-nodeport');
  const hostAudioEl  = document.getElementById('s-host-audio');
  const perfHudEl    = document.getElementById('s-perf-hud');

  if (relayEl    && cfg.relay)       relayEl.value = cfg.relay;
  if (devnameEl  && cfg.deviceName)  devnameEl.value = cfg.deviceName;
  if (unattendedEl)                { unattendedEl.checked = !!cfg.unattendedAccess; applyUnattendedUi(!!cfg.unattendedAccess); }
  if (cpwEl)                         cpwEl.value = cfg.connectPassword || '';
  if (nodePortEl) {
    nodePortEl.value = cfg.nodePort ? String(cfg.nodePort) : '';
    // Placeholder shows the live bound port (the random one currently in use), like
    // the desktop. Empty input ⇒ that random port is what's used; typing pins one.
    // The bound port if a node exists; otherwise the device is offline (no node
    // bound yet) — say so rather than "random" (an empty input still binds a random
    // free port at the next connect; this placeholder just reflects the live state).
    invoke('node_port')
      .then((p) => { nodePortEl.placeholder = (p && p > 0) ? String(p) : t('m.settings.localPortOffline'); })
      .catch(() => { nodePortEl.placeholder = t('m.settings.localPortOffline'); });
  }
  // Local relay port (client-only pref; the relay bind port, default 21116).
  const relayPortEl = document.getElementById('s-relay-port');
  if (relayPortEl) {
    const rp = getPref('localRelayPort');
    relayPortEl.value = (rp && rp > 0) ? String(rp) : '';
  }
  // Reflect the live local-relay status (running/address).
  refreshRelayStatus();
  // Client-only prefs (not on the rust config) come from the prefs store.
  if (hostAudioEl)                   hostAudioEl.checked = getPref('playHostAudio') !== false;
  if (perfHudEl)                     perfHudEl.checked = getPref('hudVisible') !== false;
  const overlayBtnEl = document.getElementById('s-overlay-btn');
  if (overlayBtnEl)                  overlayBtnEl.checked = getPref('overlayButton') !== false;
  const frameRateEl = document.getElementById('s-framerate');
  if (frameRateEl) {
    const fr = getPref('frameRate');
    frameRateEl.value = (fr === 'auto' || fr === 'unlimited') ? fr : String(fr);
    // Surface the resolved refresh on the Auto option, e.g. "Auto (120 Hz)".
    const autoOpt = frameRateEl.querySelector('option[value="auto"]');
    const hz = refreshHz();
    if (autoOpt) autoOpt.textContent = hz
      ? `${t('m.settings.frameRateAuto')} (${hz} FPS)`
      : t('m.settings.frameRateAuto');
  }
  const resEl = document.getElementById('s-resolution');
  if (resEl) resEl.value = String(getPref('resolution') || 'auto');
  const hdrEl = document.getElementById('s-hdr');
  if (hdrEl) hdrEl.checked = getPref('hdr') === true;
  if (netmodeSeg && cfg.networkMode) setSegValue(netmodeSeg, cfg.networkMode);
  if (codecSeg   && cfg.codecPref)   setSegValue(codecSeg, cfg.codecPref);
  if (qualitySeg && cfg.qualityPref) setSegValue(qualitySeg, cfg.qualityPref);
  const avatarSeg = document.getElementById('s-avatar');
  const avatarMode = cfg.avatarMode || 'wallpaper';
  if (avatarSeg) setSegValue(avatarSeg, avatarMode);
  const avatarPickBtn = document.getElementById('s-avatar-pick');
  if (avatarPickBtn) avatarPickBtn.style.display = (avatarMode === 'photo') ? '' : 'none';
  updateAvatarPreview(avatarMode);
  // Language select reflects the LIVE i18n language (the UI source of truth), not
  // cfg.language — set_config persistence lags a setLang(), so reading config
  // here would reset it to the stale value right after a switch.
  if (langSeg)                       langSeg.value = lang;
}

// ── langchange handler: live re-render ───────────────────────────────────────

/**
 * Called on every 'langchange' event.  Re-renders all data-i18n text nodes and
 * syncs the lang seg control to the new language value.
 */
function onLangChange() {
  renderText();

  // Also re-render data-i18n-self buttons (the lang seg labels themselves)
  const section = document.getElementById('t-settings');
  if (section) {
    section.querySelectorAll('[data-i18n-self]').forEach((el) => {
      el.textContent = t(el.dataset.i18nSelf);
    });
  }

  // Sync the language select to the new active language
  const langSel = document.getElementById('s-lang');
  if (langSel) langSel.value = lang;

  // Re-render aria-labels that use translated text
  const netmodeSeg = document.getElementById('s-netmode');
  if (netmodeSeg) netmodeSeg.setAttribute('aria-label', t('settings.connMethod'));
  const langSegEl = document.getElementById('s-lang');
  if (langSegEl) langSegEl.setAttribute('aria-label', t('m.settings.lang'));
}

// ── registerScreen ────────────────────────────────────────────────────────────

registerScreen({
  id: 'settings',
  navIcon: `<svg viewBox="0 0 24 24" fill="none">
    <circle cx="12" cy="12" r="3.2" stroke="currentColor" stroke-width="2"/>
    <path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"
      stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
  </svg>`,
  navLabel: 'Ayarlar',
  navLabelKey: 'nav.settings',

  mount() {
    const section = document.getElementById('t-settings');
    if (!section) return;

    // Build the DOM into the existing section element
    buildSettingsDom(section);

    // Wire inputs/segs
    wireFields(section);

    // Listen for language changes (fired by setLang) → live re-render
    window.addEventListener('langchange', onLangChange);
  },

  onShow() {
    // Refresh field values each time the tab is opened (config may have changed
    // via a host/connect flow that called set_config indirectly).
    syncFieldsFromConfig().catch(() => {});
  },
});

// ── Component-scoped CSS injected once ───────────────────────────────────────
// We inject a small <style> block for elements that are only used by the
// settings screen. This avoids touching components.css (owned by W3-overlay).

(function injectSettingsStyles() {
  if (document.getElementById('settings-screen-style')) return;
  const style = document.createElement('style');
  style.id = 'settings-screen-style';
  style.textContent = `
    /* Settings save toast */
    #settings-toast {
      position: fixed;
      bottom: calc(var(--nav-h) + var(--safe-bottom) + 14px);
      left: 50%;
      transform: translateX(-50%) translateY(12px);
      background: var(--text);
      color: var(--bg);
      font-size: 13px;
      font-weight: 600;
      padding: 10px 20px;
      border-radius: var(--r-pill);
      box-shadow: var(--shadow-lg);
      opacity: 0;
      pointer-events: none;
      transition: opacity 0.22s var(--ease), transform 0.22s var(--ease);
      white-space: nowrap;
      z-index: 50;
    }
    #settings-toast.show {
      opacity: 1;
      transform: translateX(-50%) translateY(0);
    }

    /* Field hint text */
    .settings-field-hint {
      font-size: 12px;
      color: var(--text-faint);
      line-height: 1.5;
      margin-top: -2px;
    }
    .settings-warn {
      display: block;
      font-size: 12px;
      color: var(--danger);
      line-height: 1.5;
      margin-top: 2px;
      font-weight: 600;
    }

    /* Language selection input (native dropdown; scales to any language count) */
    .lang-select {
      width: auto;
      min-width: 140px;
      flex: none;
      font-weight: 600;
      padding-right: 34px;
      background-image:
        url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='%238a8a99' stroke-width='2.4' stroke-linecap='round' stroke-linejoin='round'><path d='M6 9l6 6 6-6'/></svg>");
      background-repeat: no-repeat;
      background-position: right 12px center;
      -webkit-appearance: none;
      appearance: none;
    }
    .framerate-select {
      width: 100%;
      font-weight: 600;
      padding-right: 34px;
      background-image:
        url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='%238a8a99' stroke-width='2.4' stroke-linecap='round' stroke-linejoin='round'><path d='M6 9l6 6 6-6'/></svg>");
      background-repeat: no-repeat;
      background-position: right 12px center;
      -webkit-appearance: none;
      appearance: none;
    }
    /* Local-relay address chip (tap to copy) */
    .relay-addr {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      align-self: flex-start;
      font-size: 15px;
      font-weight: 600;
      color: var(--brand);
      background: var(--surface-2, oklch(0.96 0.005 270));
      border: 1px solid var(--border, oklch(0.9 0.01 270));
      border-radius: var(--r-md, 12px);
      padding: 8px 14px;
      cursor: pointer;
      user-select: all;
      -webkit-tap-highlight-color: transparent;
    }
    .relay-addr:active { background: var(--surface-3, oklch(0.93 0.01 270)); }
    .relay-addr::after {
      content: "";
      width: 15px; height: 15px;
      background: currentColor;
      -webkit-mask: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='9' y='9' width='11' height='11' rx='2'/><path d='M5 15V5a2 2 0 0 1 2-2h10'/></svg>") center/contain no-repeat;
      mask: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='9' y='9' width='11' height='11' rx='2'/><path d='M5 15V5a2 2 0 0 1 2-2h10'/></svg>") center/contain no-repeat;
      opacity: 0.7;
    }
    .licenses-sheet { max-height: 82vh; display: flex; flex-direction: column; }
    .licenses-list { overflow-y: auto; flex: 1; min-height: 0; -webkit-overflow-scrolling: touch; padding-bottom: 8px; }
    .lic-self { font-weight: 700; color: var(--brand); margin-bottom: 4px; }
    .lic-count { font-size: 12px; color: var(--text-faint); margin-bottom: 14px; }
    .lic-group { margin-bottom: 14px; }
    .lic-lic { font-size: 13px; font-weight: 600; color: var(--text); margin-bottom: 4px; }
    .lic-n { color: var(--text-faint); font-weight: 400; font-size: 11px; }
    .lic-dep { font-size: 12px; color: var(--text-muted); font-family: var(--font-mono); line-height: 1.6; }
    .lic-v { color: var(--text-faint); }
  `;
  document.head.appendChild(style);
}());
