// Lightweight runtime i18n with LAZY, per-language loading. Each language is a JSON
// file under `./i18n/` (editable without touching code); Vite's `import.meta.glob`
// splits each into its own chunk, so a launch downloads ONLY the active language
// plus English (the fallback) — every other language loads on demand when picked.
//
// The active language AND the loaded dictionaries are reactive `$state`, so every
// `t(...)` used in markup re-renders when the language changes OR when its dictionary
// finishes loading.
//
// First-run language = the system language if we ship it, otherwise English. The
// choice is persisted to localStorage and can be changed at runtime.

import type { Lang, Dict } from './i18n.types';
import { invoke, isTauri } from './api.invoke';

export type { Lang };

/** Languages offered in the switcher (select-list order). */
export const LANGS: { value: Lang; label: string; short: string }[] = [
	{ value: 'tr', label: 'Türkçe', short: 'TR' },
	{ value: 'en', label: 'English', short: 'EN' },
	{ value: 'ru', label: 'Русский', short: 'RU' },
	{ value: 'kk', label: 'Қазақша', short: 'KK' }
];

/** English is always the ultimate fallback for a missing key. */
const FALLBACK: Lang = 'en';

const KEY = 'pulsar.lang.v1';

const VALID = new Set<string>(LANGS.map((l) => l.value));
function isLang(s: unknown): s is Lang {
	return typeof s === 'string' && VALID.has(s);
}

// Per-language lazy loaders. Vite turns each JSON into a separate on-demand chunk,
// so nothing but the language we actually use is ever fetched.
const loaders = import.meta.glob('./i18n/*.json') as Record<
	string,
	() => Promise<{ default: Dict }>
>;

// Dictionaries loaded so far — reactive, so `t()` re-runs when one arrives.
const dicts = $state<Partial<Record<Lang, Dict>>>({});

/** Load a language's dictionary once (idempotent). Silent on failure — `t()` then
 * falls back to English, then the raw key. */
export async function loadLang(l: Lang): Promise<void> {
	if (dicts[l]) return;
	const loader = loaders[`./i18n/${l}.json`];
	if (!loader) return;
	try {
		dicts[l] = (await loader()).default;
	} catch {
		/* leave unloaded — t() falls back */
	}
}

/** System language if we ship it, otherwise English. Honors the OS preference
 * ORDER (`navigator.languages`) — the first shipped language a user prefers wins. */
function detect(): Lang {
	if (typeof navigator === 'undefined') return 'en';
	const langs = [navigator.language, ...(navigator.languages ?? [])];
	for (const l of langs) {
		if (typeof l !== 'string') continue;
		const p = l.toLowerCase();
		if (p.startsWith('tr')) return 'tr';
		if (p.startsWith('ru')) return 'ru';
		if (p.startsWith('kk')) return 'kk';
		if (p.startsWith('en')) return 'en';
	}
	return 'en';
}

function loadChoice(): Lang {
	if (typeof localStorage !== 'undefined') {
		const s = localStorage.getItem(KEY);
		if (isLang(s)) return s; // an explicit, persisted user choice wins
	}
	return detect(); // first run (never changed): follow the PC language
}

// Reactive holder — reading `i18n.lang` inside `t()` makes every `t(...)` call used
// in markup re-run when the language changes.
export const i18n = $state<{ lang: Lang }>({ lang: loadChoice() });

/** Resolves once the active language + the English fallback are loaded. The boot
 * splash awaits this so the first real paint is never a flash of raw keys. */
export const i18nReady: Promise<void> = Promise.all([
	loadLang(i18n.lang),
	i18n.lang === FALLBACK ? Promise.resolve() : loadLang(FALLBACK)
]).then(() => {});

export function setLang(l: Lang) {
	i18n.lang = l;
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, l);
	void loadLang(l); // fetch the newly-selected language on demand
	syncBackend(l);
	broadcast(l); // fan the change out to every other Pulsar window
}

// The Rust side has its OWN language state (Config.language → tray labels, host
// strings, and the native overlay's --lang). It used to be set only from the config
// file, never from this switcher — so the in-session overlay stayed Turkish for a
// user running the UI in English. Push every change (and the startup value, below).
function syncBackend(l: Lang) {
	if (!isTauri) return;
	invoke<void>('set_language', { lang: l }).catch(() => {});
}
syncBackend(i18n.lang);

// Cross-window language sync. Each Pulsar window (main shell, Active Connections,
// file transfer, auth popup) is a SEPARATE webview with its own JS heap, so its own
// `i18n.lang` `$state`. Without this, changing the language in one window left the
// others stale until they were closed + reopened (which re-ran `loadChoice()`). Now
// every window subscribes to a broadcast, so a change anywhere fans out to all.
const LANG_EVENT = 'pulsar://lang';

function broadcast(l: Lang) {
	if (!isTauri) return;
	import('@tauri-apps/api/event')
		.then(({ emit }) => emit(LANG_EVENT, l))
		.catch(() => {});
}

// Apply a language pushed by ANOTHER window without echoing it back — the emitter
// already persisted + synced the backend. The `l === i18n.lang` guard also absorbs
// the emitter's own copy of the event (Tauri delivers a broadcast to its sender too).
function applyIncoming(l: Lang) {
	if (!isLang(l) || l === i18n.lang) return;
	i18n.lang = l;
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, l);
	void loadLang(l);
	syncBackend(l);
}

if (isTauri) {
	import('@tauri-apps/api/event')
		.then(({ listen }) => listen<Lang>(LANG_EVENT, (e) => applyIncoming(e.payload)))
		.catch(() => {});
}

/** Advance to the next language in `LANGS` (wraps). The chrome UI is a select list;
 * this stays for keyboard/back-compat and cycles through all shipped languages. */
export function cycleLang() {
	const i = LANGS.findIndex((l) => l.value === i18n.lang);
	setLang(LANGS[(i + 1) % LANGS.length].value);
}

/** Translate `key`, interpolating `{name}` placeholders from `vars`. Reads the
 * reactive `dicts`/`i18n.lang`, so it re-renders when the language changes or the
 * active dictionary finishes loading. Falls back to English, then the raw key. */
export function t(key: string, vars?: Record<string, string | number>): string {
	const active = dicts[i18n.lang];
	let s = active?.[key] ?? dicts[FALLBACK]?.[key] ?? key;
	if (vars) {
		for (const k in vars) s = s.split(`{${k}}`).join(String(vars[k]));
	}
	return s;
}
