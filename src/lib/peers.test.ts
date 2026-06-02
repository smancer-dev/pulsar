import { describe, it, expect, beforeEach } from 'vitest';
import { allPeers, recentPeers, recordConnection, addPeer, _reset } from './peers.svelte';

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
});
