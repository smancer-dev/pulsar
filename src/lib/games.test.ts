import { describe, it, expect, beforeEach } from 'vitest';
import {
	gameStore,
	addGame,
	removeGame,
	addScanned,
	addFolder,
	removeFolder,
	isBuiltin,
	DESKTOP_ID,
	ensureSteamDefault,
	_reset
} from './games.svelte';

// Count only user games (the built-in Desktop entry is always present).
const userGames = () => gameStore.games.filter((g) => !isBuiltin(g.id));

const sample = {
	title: 'Test',
	type: 'program' as const,
	path: '/x',
	args: '',
	command: '',
	image: '',
	cmdStart: '',
	cmdStop: ''
};

describe('games store', () => {
	beforeEach(() => _reset());

	it('has only the built-in Desktop entry by default', () => {
		expect(userGames().length).toBe(0);
		expect(gameStore.games.some((g) => g.id === DESKTOP_ID)).toBe(true);
		expect(gameStore.host.fps).toBe(60);
	});

	it('the Desktop built-in cannot be removed', () => {
		removeGame(DESKTOP_ID);
		expect(gameStore.games.some((g) => g.id === DESKTOP_ID)).toBe(true);
	});

	it('adds and removes a game', () => {
		const g = addGame(sample);
		expect(userGames().length).toBe(1);
		removeGame(g.id);
		expect(userGames().length).toBe(0);
	});

	it('dedupes scanned apps by path', () => {
		expect(addScanned('A', '/a')).toBe(true);
		expect(addScanned('A again', '/a')).toBe(false);
		expect(userGames().length).toBe(1);
	});

	it('seeds a deletable Steam default only when installed', () => {
		ensureSteamDefault(''); // not installed → nothing added
		expect(gameStore.games.some((g) => g.id === 'steam')).toBe(false);
		ensureSteamDefault('/usr/bin/steam');
		expect(gameStore.games.some((g) => g.id === 'steam')).toBe(true);
		// Steam is deletable (not a built-in).
		expect(isBuiltin('steam')).toBe(false);
	});

	it('manages scan folders without duplicates', () => {
		addFolder('/games');
		addFolder('/games');
		expect(gameStore.scan.folders.length).toBe(1);
		removeFolder('/games');
		expect(gameStore.scan.folders.length).toBe(0);
	});
});
