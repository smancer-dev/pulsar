import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach } from 'vitest';
import { cleanup } from '@testing-library/svelte';
import { setLang } from './src/lib/i18n.svelte';

// jsdom reports `en-US`, so the UI would default to English. The component tests
// assert the Turkish copy, so pin the language to Turkish for the whole suite.
beforeEach(() => setLang('tr'));

afterEach(() => cleanup());
