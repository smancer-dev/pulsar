// theme.js — app-controlled Light / Dark / System theme.
//
// The user picks the theme in Settings (independent of the device setting). We
// persist the choice, set <html data-theme="light|dark"> (the CSS in tokens.css
// keys its dark palette off that), and tell the native side to flip the status /
// navigation bar icon colour so they stay visible on whichever background.
//
//   import { theme, setTheme } from './theme.js';
//   theme()         → current SETTING: 'light' | 'dark' | 'system'
//   setTheme('dark')→ persists + applies + fires window 'themechange'

import { invoke, hasTauri } from './tauri.js';

const THEME_KEY = 'pulsar.theme.v1';

/** Current SETTING ('light' | 'dark' | 'system'). The resolved value (what the
 *  user actually sees) is `resolved()`. */
let _setting = loadTheme();

const _mql = window.matchMedia
	? window.matchMedia('(prefers-color-scheme: dark)')
	: null;

export function theme() {
	return _setting;
}

function loadTheme() {
	try {
		const s = localStorage.getItem(THEME_KEY);
		if (s === 'light' || s === 'dark' || s === 'system') return s;
	} catch (_) {}
	return 'system';
}

/** Resolve a setting to the concrete 'light' | 'dark' actually displayed. */
function resolved(setting = _setting) {
	if (setting === 'system') return _mql && _mql.matches ? 'dark' : 'light';
	return setting;
}

/** Apply the resolved theme to <html> + the native status/nav bars. */
function apply() {
	const r = resolved();
	document.documentElement.setAttribute('data-theme', r);
	// Native bars: light theme → dark icons (lightTheme=true); dark → light icons.
	if (hasTauri) {
		invoke('plugin:pulsar-video|set_status_bar', { lightTheme: r === 'light' }).catch(() => {});
	}
}

/** @param {'light'|'dark'|'system'} setting */
export function setTheme(setting) {
	if (setting !== 'light' && setting !== 'dark' && setting !== 'system') return;
	_setting = setting;
	try { localStorage.setItem(THEME_KEY, setting); } catch (_) {}
	apply();
	window.dispatchEvent(
		new CustomEvent('themechange', { detail: { setting, resolved: resolved() } })
	);
}

// Follow the device only while the setting is 'system'.
if (_mql) {
	const onSys = () => { if (_setting === 'system') apply(); };
	if (_mql.addEventListener) _mql.addEventListener('change', onSys);
	else if (_mql.addListener) _mql.addListener(onSys);
}

// Apply immediately at import (before first paint where possible).
apply();
