/**
 * Pulsar Mobile — i18n module
 *
 * One flat key/value catalog per language (tr, en, ru, kk), split into
 * ./i18n/<lang>.js. Keys mirror the desktop catalogs (src/lib/i18n.*.ts) plus
 * mobile-only additions prefixed with "m." where a key has no desktop equivalent.
 *
 * API:
 *   import { t, setLang, lang } from './i18n.js';
 *   t('key')             → translated string
 *   t('key', { n: 5 })  → interpolates {n} placeholders
 *   setLang('ru')        → switches language, persists to localStorage
 *   lang                 → current language code ('tr' | 'en' | 'ru' | 'kk')
 *
 * Fallback chain: catalogs[lang][key] → en[key] → key.
 * Language detection: localStorage 'pulsar.lang.v1' → default 'tr' (Turkish-first).
 */

import tr from './i18n/tr.js';
import en from './i18n/en.js';
import ru from './i18n/ru.js';
import kk from './i18n/kk.js';

const LANG_KEY = 'pulsar.lang.v1';

/**
 * Default language when the user has stored no explicit preference.
 * Pulsar is a Turkish-first product and the desktop core hard-defaults to
 * Turkish (`Config.language = Language::Tr`), regardless of system locale — so
 * the mobile app follows the DEVICE language by default (mapped to a shipped
 * catalog), and the user can override it in Settings (persisted to localStorage).
 * Turkish is the fallback for any unsupported locale. The static shell is
 * annotated with data-i18n and translated by applyI18n() at boot + on langchange,
 * so following the system language no longer produces a mixed-language UI.
 */
function detect() {
  // Map the browser/system locale (e.g. "ru-RU", "en-GB") to a shipped catalog;
  // fall back to Turkish (Pulsar's first language) for anything unsupported.
  try {
    const sys = (navigator.language || navigator.userLanguage || '').toLowerCase();
    const code = sys.split('-')[0];
    if (LANGS.includes(code)) return code;
  } catch (_) {}
  return 'tr';
}

/** Languages the app ships translations for. */
export const LANGS = ['tr', 'en', 'ru', 'kk'];

/**
 * Load the persisted language preference, falling back to detection.
 * @returns {typeof LANGS[number]}
 */
function loadLang() {
  try {
    const stored = typeof localStorage !== 'undefined'
      ? localStorage.getItem(LANG_KEY)
      : null;
    if (LANGS.includes(stored)) return stored;
  } catch (_) {
    // localStorage access can throw in some sandboxed contexts
  }
  return detect();
}

/** Current active language. Read-only from outside; changed only via setLang(). */
export let lang = loadLang();

// Reflect the loaded language on <html lang> at boot. index.html hardcodes
// lang="tr", and only setLang() updates it — so on a fresh load with a non-TR
// stored language, the local _t() fallback tables in overlay/sidechannels/split
// (which read document.documentElement.lang) rendered Turkish even in English.
if (typeof document !== 'undefined') document.documentElement.lang = lang;

const catalogs = { tr, en, ru, kk };

/**
 * Switch the active language and persist the choice.
 *
 * W5-settings-lang additions:
 *   1. Persists to localStorage (always).
 *   2. Calls `set_config { language }` via Tauri (best-effort — non-blocking,
 *      non-fatal) so the Rust config layer is in sync (§2.2 contract).
 *   3. Updates `<html lang>` for screen-reader / browser locale signals.
 *   4. Dispatches a 'langchange' CustomEvent on `window` so modules that
 *      rendered strings can re-render if needed.
 *
 * @param {typeof LANGS[number]} l
 */
export function setLang(l) {
  if (!LANGS.includes(l)) return;
  lang = l;

  // 1. localStorage persistence (synchronous, always-first)
  try {
    if (typeof localStorage !== 'undefined') localStorage.setItem(LANG_KEY, l);
  } catch (_) {}

  // 2. Sync to Rust config (best-effort; never blocks the UI)
  if (typeof window !== 'undefined' && window.__TAURI__) {
    // Use the store/config.js setConfig if already loaded to benefit from its
    // cache + normalisation; otherwise fall back to a direct invoke.
    const cfgMod = window.__pulsarConfigMod;
    if (cfgMod && typeof cfgMod.setConfig === 'function') {
      cfgMod.setConfig({ language: l }).catch(() => {});
    } else {
      window.__TAURI__.core.invoke('set_config', { language: l }).catch(() => {});
    }
  }

  // 3. Update <html lang> attribute
  if (typeof document !== 'undefined') {
    document.documentElement.lang = l;
  }

  // 4. Notify all subscribers so they can re-render their strings
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent('langchange', { detail: l }));
  }
}

/**
 * Translate a key with optional variable interpolation.
 *
 * Fallback chain: catalogs[lang][key] → catalogs.en[key] → key
 *
 * Variable syntax: {varName} — e.g. t('devices.minAgo', { n: 5 }) → "5 dk önce"
 *
 * @param {string} key
 * @param {Record<string, string|number>=} vars
 * @returns {string}
 */
export function t(key, vars) {
  let s = (catalogs[lang] && catalogs[lang][key] != null)
    ? catalogs[lang][key]
    : (catalogs.en[key] != null ? catalogs.en[key] : key);

  if (vars) {
    for (const k of Object.keys(vars)) {
      // Use split/join instead of replace to handle all occurrences
      s = s.split(`{${k}}`).join(String(vars[k]));
    }
  }
  return s;
}

/**
 * Translate every annotated node under `root` (default: the whole document).
 * Used for the STATIC shell (index.html) which isn't re-rendered by JS:
 *   data-i18n       → element.textContent
 *   data-i18n-ph    → element.placeholder
 *   data-i18n-aria  → element aria-label
 *   data-i18n-title → element title
 * Call at boot and on every 'langchange'. Idempotent (safe to re-run).
 * @param {ParentNode=} root
 */
export function applyI18n(root = document) {
  if (!root || !root.querySelectorAll) return;
  root.querySelectorAll('[data-i18n]').forEach((el) => {
    el.textContent = t(el.dataset.i18n);
  });
  root.querySelectorAll('[data-i18n-ph]').forEach((el) => {
    el.setAttribute('placeholder', t(el.dataset.i18nPh));
  });
  root.querySelectorAll('[data-i18n-aria]').forEach((el) => {
    el.setAttribute('aria-label', t(el.dataset.i18nAria));
  });
  root.querySelectorAll('[data-i18n-title]').forEach((el) => {
    el.setAttribute('title', t(el.dataset.i18nTitle));
  });
}
