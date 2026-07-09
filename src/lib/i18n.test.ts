import { describe, it, expect, beforeEach, beforeAll } from 'vitest';
import { t, i18n, setLang, cycleLang, LANGS, loadLang, i18nReady } from './i18n.svelte';

describe('i18n', () => {
	// Dictionaries load lazily (per-language JSON chunks). Preload every language the
	// assertions touch so the synchronous `t()` calls below see them.
	beforeAll(async () => {
		await i18nReady;
		await Promise.all(LANGS.map((l) => loadLang(l.value)));
	});
	beforeEach(() => setLang('tr'));

	it('resolves keys in the active language', () => {
		setLang('tr');
		expect(t('nav.settings')).toBe('Ayarlar');
		setLang('en');
		expect(t('nav.settings')).toBe('Settings');
	});

	it('interpolates {placeholders}', () => {
		setLang('en');
		expect(t('activity.connected', { peer: '123 456 789' })).toBe('123 456 789 connected');
		expect(t('devices.minAgo', { n: 5 })).toBe('5 min ago');
	});

	it('returns the raw key when the string is unknown', () => {
		expect(t('does.not.exist')).toBe('does.not.exist');
	});

	it('offers Turkish, English, Russian and Kazakh', () => {
		expect(LANGS.map((l) => l.value).sort()).toEqual(['en', 'kk', 'ru', 'tr']);
	});

	it('persists the choice and cycles through all languages via cycleLang', () => {
		setLang('tr');
		expect(i18n.lang).toBe('tr');
		expect(localStorage.getItem('pulsar.lang.v1')).toBe('tr');
		cycleLang();
		expect(i18n.lang).toBe('en');
		cycleLang();
		expect(i18n.lang).toBe('ru');
		cycleLang();
		expect(i18n.lang).toBe('kk');
		cycleLang();
		expect(i18n.lang).toBe('tr');
	});
});
