import { describe, it, expect, beforeEach } from 'vitest';
import {
	gameStore,
	addGame,
	removeGame,
	addScanned,
	addFolder,
	removeFolder,
	_reset
} from './games.svelte';

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

	it('starts empty with default host settings', () => {
		expect(gameStore.games.length).toBe(0);
		expect(gameStore.host.fps).toBe(60);
	});

	it('adds and removes a game', () => {
		const g = addGame(sample);
		expect(gameStore.games.length).toBe(1);
		removeGame(g.id);
		expect(gameStore.games.length).toBe(0);
	});

	it('dedupes scanned apps by path', () => {
		expect(addScanned('A', '/a')).toBe(true);
		expect(addScanned('A again', '/a')).toBe(false);
		expect(gameStore.games.length).toBe(1);
	});

	it('manages scan folders without duplicates', () => {
		addFolder('/games');
		addFolder('/games');
		expect(gameStore.scan.folders.length).toBe(1);
		removeFolder('/games');
		expect(gameStore.scan.folders.length).toBe(0);
	});
});
