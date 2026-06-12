// Light/dark theme, shared across ALL windows. The main window and the secondary
// popup windows (Allow/Deny approval, Connections) each load the same index.html
// in a separate webview, so an in-memory toggle wouldn't reach the others. The
// choice is persisted to localStorage (per the i18n pattern) and a `storage` event
// listener keeps every open window in sync when one of them toggles the theme.

const KEY = 'pulsar.theme.v1';

function load(): boolean {
	if (typeof localStorage !== 'undefined') {
		const s = localStorage.getItem(KEY);
		if (s === 'dark') return true;
		if (s === 'light') return false;
	}
	return false;
}

// Reactive holder — reading `theme.dark` re-runs anything that depends on it.
export const theme = $state<{ dark: boolean }>({ dark: load() });

/** Apply the current theme to <html> (a no-op in SSR where there's no document). */
function apply() {
	if (typeof document !== 'undefined') {
		document.documentElement.setAttribute('data-theme', theme.dark ? 'dark' : 'light');
	}
}

export function setDark(dark: boolean) {
	theme.dark = dark;
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, dark ? 'dark' : 'light');
	apply();
}

export function toggleTheme() {
	setDark(!theme.dark);
}

// localStorage writes from a SIBLING window fire `storage` here (never in the window
// that wrote it), so toggling the theme in the main window updates an already-open
// popup live, and vice-versa.
if (typeof window !== 'undefined') {
	window.addEventListener('storage', (e) => {
		if (e.key !== KEY) return;
		theme.dark = e.newValue === 'dark';
		apply();
	});
}
