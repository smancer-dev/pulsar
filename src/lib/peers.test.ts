import { describe, it, expect, beforeEach } from 'vitest';
import {
	allPeers,
	recentPeers,
	savedPeers,
	historyPeers,
	recordConnection,
	addPeer,
	clearHistory,
	_reset
} from './peers.svelte';

describe('peers store', () => {
	beforeEach(() => _reset());

	it('starts empty (no fake recents/devices)', () => {
		expect(allPeers().length).toBe(0);
		expect(recentPeers().length).toBe(0);
	});

	it('records a real connection into recents', () => {
		recordConnection('111 222 333', 'Test PC');
		expect(recentPeers().length).toBe(1);
		expect(recentPeers()[0].id).toBe('111 222 333');
	});

	it('addPeer dedupes by id and is not a recent until connected', () => {
		expect(addPeer('Saved', '444 555 666')).toBe(true);
		expect(addPeer('Dup', '444 555 666')).toBe(false);
		expect(allPeers().length).toBe(1);
		expect(recentPeers().length).toBe(0);
	});

	it('a plain connection goes to history, NOT to saved Devices', () => {
		recordConnection('111 222 333', 'Drive-by');
		expect(historyPeers().length).toBe(1);
		expect(savedPeers().length).toBe(0); // not saved by merely connecting
	});

	it('addPeer saves to Devices; saving a connected peer keeps its history', () => {
		recordConnection('111 222 333', 'PC');
		expect(addPeer('PC', '111 222 333')).toBe(true); // promote to saved
		expect(savedPeers().length).toBe(1);
		expect(historyPeers().length).toBe(1); // still in history too
		expect(addPeer('PC', '111 222 333')).toBe(false); // already saved
	});

	it('clearHistory drops history-only entries but keeps saved devices', () => {
		recordConnection('111 222 333', 'Drive-by'); // history only
		addPeer('Kept', '444 555 666'); // saved, never connected
		clearHistory();
		expect(historyPeers().length).toBe(0);
		expect(savedPeers().length).toBe(1);
		expect(savedPeers()[0].id).toBe('444 555 666');
	});
});
