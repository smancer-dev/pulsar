import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach } from 'vitest';
import { cleanup } from '@testing-library/svelte';
import { setLang, loadLang } from './src/lib/i18n.svelte';

// jsdom reports `en-US`, so the UI would default to English. The component tests
// assert the Turkish copy, so pin the language to Turkish for the whole suite. The
// catalogs now load lazily (async per-language JSON chunks), so AWAIT the active +
// fallback dictionaries here — otherwise a component renders before its dictionary
// resolves and `t()` returns raw keys (which is what broke the copy assertions).
beforeEach(async () => {
	setLang('tr');
	await loadLang('tr');
	await loadLang('en');
});

afterEach(() => cleanup());
