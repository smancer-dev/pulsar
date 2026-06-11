// Lightweight runtime i18n. Strings live in `tr`/`en` catalogs (in the sibling
// `i18n.catalogs.ts` data module); `t(key, vars)` resolves against the active
// language (falling back to English, then the raw key). The active language is a
// reactive `$state`, so any `t(...)` call used in markup re-renders automatically
// when it changes.
//
// First-run language = the system language if we have it, otherwise English.
// The choice is persisted to localStorage and can be toggled at runtime.

import { catalogs } from './i18n.catalogs';
import type { Lang } from './i18n.catalogs';
import { invoke, isTauri } from './api.invoke';

export type { Lang };

/** Languages offered in the switcher (in toggle order). */
export const LANGS: { value: Lang; label: string; short: string }[] = [
	{ value: 'tr', label: 'Türkçe', short: 'TR' },
	{ value: 'en', label: 'English', short: 'EN' }
];

const KEY = 'pulsar.lang.v1';

/** System language if we ship it, otherwise English. */
function detect(): Lang {
	if (typeof navigator === 'undefined') return 'en';
	const langs = [navigator.language, ...(navigator.languages ?? [])];
	for (const l of langs) {
		if (typeof l === 'string' && l.toLowerCase().startsWith('tr')) return 'tr';
	}
	return 'en';
}

function load(): Lang {
	if (typeof localStorage !== 'undefined') {
		const s = localStorage.getItem(KEY);
		if (s === 'tr' || s === 'en') return s;
	}
	return detect();
}

// Reactive holder — reading `i18n.lang` inside `t()` makes every `t(...)` call
// used in markup re-run when the language changes.
export const i18n = $state<{ lang: Lang }>({ lang: load() });

export function setLang(l: Lang) {
	i18n.lang = l;
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, l);
	syncBackend(l);
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

/** Toggle to the next language in `LANGS` (currently just tr ⇄ en). */
export function cycleLang() {
	const i = LANGS.findIndex((l) => l.value === i18n.lang);
	setLang(LANGS[(i + 1) % LANGS.length].value);
}

/** Translate `key`, interpolating `{name}` placeholders from `vars`. */
export function t(key: string, vars?: Record<string, string | number>): string {
	let s = catalogs[i18n.lang][key] ?? catalogs.en[key] ?? key;
	if (vars) {
		for (const k in vars) s = s.split(`{${k}}`).join(String(vars[k]));
	}
	return s;
}
