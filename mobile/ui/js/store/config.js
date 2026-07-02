/**
 * store/config.js — single source of truth for app configuration.
 *
 * Wraps the Rust `get_config` / `set_config` Tauri commands (W1-rust / §2.2).
 * Maintains a localStorage cache so reads are synchronous after the first load,
 * and falls back to sensible defaults when Tauri is unavailable (browser dev).
 *
 * Exported surface (§3):
 *   getConfig()      → Promise<ConfigObj>   — force-refresh from Rust
 *   setConfig(patch) → Promise<ConfigObj>   — merge + persist, returns new state
 *   relay()          → String               — cached relay endpoint
 *   netmode()        → String               — "auto" | "p2p-only" | "relay-only"
 *   deviceName()     → String               — friendly device name
 *   codec()          → String               — "auto" | "h265" | "h264"
 *   quality()        → String               — "latency" | "balanced" | "quality"
 *   language()       → String               — "tr" | "en"
 *
 * Config object shape (mirrors Rust MobileConfig — §2.2):
 *   { relay, networkMode, deviceName, language, unattendedAccess,
 *     codecPref, qualityPref }
 *
 * NetworkMode vocab: "auto" / "p2p-only" / "relay-only"  (kebab-case, matches Rust)
 * Language vocab:    "tr" / "en"                          (lowercase, matches Rust)
 * QualityPref vocab: "latency" / "balanced" / "quality"  (lowercase, matches Rust)
 * CodecPref vocab:   "auto" / "h265" / "h264"
 */

// ---------------------------------------------------------------------------
// localStorage cache key (version-namespaced to allow future schema bumps)
// ---------------------------------------------------------------------------
const LS_KEY = 'pulsar.config.v1';

// ---------------------------------------------------------------------------
// Defaults (must match Rust defaults: DEFAULT_RELAY, NetworkMode::Auto, etc.)
// ---------------------------------------------------------------------------
const DEFAULTS = {
  relay: '127.0.0.1:21116',
  networkMode: 'auto',
  deviceName: 'Pulsar Cihazı',
  language: 'tr',
  unattendedAccess: false,
  connectPassword: '',
  codecPref: 'auto',
  qualityPref: 'balanced',
  nodePort: 0,
  avatarMode: 'wallpaper',
};

// ---------------------------------------------------------------------------
// Internal state — the single in-memory cache
// ---------------------------------------------------------------------------
let _cache = null;   // ConfigObj | null — null means "not yet loaded"
let _loading = null; // Promise<ConfigObj> | null — in-flight getConfig() guard

// ---------------------------------------------------------------------------
// Tauri invoke — lazy import so this module parses without the webview
// ---------------------------------------------------------------------------
async function _invoke(cmd, args) {
  // Access via the same guard pattern used by the rest of the app (tauri.js
  // is the canonical guard; we fall back inline here so config.js can be
  // imported independently if needed during tests / browser dev).
  if (typeof window !== 'undefined' && window.__TAURI__) {
    return window.__TAURI__.core.invoke(cmd, args);
  }
  // Tauri unavailable (browser dev) — return null so callers use the cache.
  return null;
}

// ---------------------------------------------------------------------------
// localStorage helpers
// ---------------------------------------------------------------------------
function _lsRead() {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return null;
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function _lsWrite(cfg) {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify(cfg));
  } catch {
    // Storage full or private-mode — not fatal
  }
}

// ---------------------------------------------------------------------------
// Internal: merge a raw object from Rust (or localStorage) with defaults
// ---------------------------------------------------------------------------
function _normalise(raw) {
  if (!raw || typeof raw !== 'object') return { ...DEFAULTS };
  return {
    relay:            typeof raw.relay === 'string'            ? raw.relay            : DEFAULTS.relay,
    networkMode:      typeof raw.networkMode === 'string'      ? raw.networkMode      : DEFAULTS.networkMode,
    deviceName:       typeof raw.deviceName === 'string'       ? raw.deviceName       : DEFAULTS.deviceName,
    language:         typeof raw.language === 'string'         ? raw.language         : DEFAULTS.language,
    unattendedAccess: typeof raw.unattendedAccess === 'boolean' ? raw.unattendedAccess : DEFAULTS.unattendedAccess,
    connectPassword:  typeof raw.connectPassword === 'string'  ? raw.connectPassword  : DEFAULTS.connectPassword,
    codecPref:        typeof raw.codecPref === 'string'        ? raw.codecPref        : DEFAULTS.codecPref,
    qualityPref:      typeof raw.qualityPref === 'string'      ? raw.qualityPref      : DEFAULTS.qualityPref,
    nodePort:         Number.isFinite(raw.nodePort)            ? (raw.nodePort | 0)   : DEFAULTS.nodePort,
    avatarMode:       typeof raw.avatarMode === 'string'       ? raw.avatarMode       : DEFAULTS.avatarMode,
  };
}

// ---------------------------------------------------------------------------
// Internal: load from Rust, fall back to localStorage, fall back to defaults
// ---------------------------------------------------------------------------
async function _load() {
  // 1. Try Tauri command
  let raw = await _invoke('get_config', {});
  if (raw && typeof raw === 'object') {
    const cfg = _normalise(raw);
    _cache = cfg;
    _lsWrite(cfg);
    return cfg;
  }

  // 2. Fall back to localStorage cache (Tauri unavailable or returned null)
  const ls = _lsRead();
  if (ls) {
    const cfg = _normalise(ls);
    _cache = cfg;
    return cfg;
  }

  // 3. Absolute fallback: defaults
  _cache = { ...DEFAULTS };
  return _cache;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * getConfig() — returns a fresh ConfigObj from the Rust backend, updating the
 * in-memory cache and localStorage.  Safe to call multiple times concurrently
 * (the in-flight request is deduplicated).
 *
 * @returns {Promise<ConfigObj>}
 */
export async function getConfig() {
  if (_loading) return _loading;
  _loading = _load().finally(() => { _loading = null; });
  return _loading;
}

/**
 * setConfig(patch) — merge a partial config object, persist via `set_config`,
 * update the cache and localStorage, and return the merged result.
 *
 * The patch may contain any subset of ConfigObj keys; unknown keys are ignored
 * so callers can safely spread full objects.
 *
 * @param {Partial<ConfigObj>} patch
 * @returns {Promise<ConfigObj>}
 */
export async function setConfig(patch) {
  // Ensure we have a base config before merging
  if (!_cache) {
    await getConfig();
  }

  // Build the merged candidate locally
  const merged = _normalise({ ..._cache, ...patch });

  // Try to persist via Rust; use the Rust-confirmed result if available
  const result = await _invoke('set_config', merged);
  const confirmed = (result && typeof result === 'object') ? _normalise(result) : merged;

  _cache = confirmed;
  _lsWrite(confirmed);
  return confirmed;
}

// ---------------------------------------------------------------------------
// Synchronous accessors — return cached value or default if not yet loaded.
// Call getConfig() (awaited) on app boot to warm the cache before using these.
// ---------------------------------------------------------------------------

/** @returns {string} relay host:port — e.g. "127.0.0.1:21116" */
export function relay() {
  return _cache ? _cache.relay : DEFAULTS.relay;
}

/** @returns {"auto"|"p2p-only"|"relay-only"} */
export function netmode() {
  return _cache ? _cache.networkMode : DEFAULTS.networkMode;
}

/** @returns {string} friendly device name */
export function deviceName() {
  return _cache ? _cache.deviceName : DEFAULTS.deviceName;
}

/**
 * @returns {"auto"|"h265"|"h264"}
 */
export function codec() {
  return _cache ? _cache.codecPref : DEFAULTS.codecPref;
}

/**
 * @returns {"latency"|"balanced"|"quality"}
 */
export function quality() {
  return _cache ? _cache.qualityPref : DEFAULTS.qualityPref;
}

/**
 * @returns {"tr"|"en"}
 */
export function language() {
  return _cache ? _cache.language : DEFAULTS.language;
}
