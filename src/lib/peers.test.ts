import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
	allPeers,
	recentPeers,
	savedPeers,
	historyPeers,
	recordConnection,
	addPeer,
	clearHistory,
	removeFromHistory,
	normalizeId,
	fmtPeerId,
	setPeerIdentity,
	_reset
} from './peers.svelte';

describe('peers store', () => {
	beforeEach(() => _reset());

	it('starts empty (no fake recents/devices)', () => {
		expect(allPeers().length).toBe(0);
		expect(recentPeers().length).toBe(0);
	});

	it('records a real connection into recents (id stored canonical, displayed grouped)', () => {
		recordConnection('111 222 333', 'Test PC');
		expect(recentPeers().length).toBe(1);
		expect(recentPeers()[0].id).toBe('111222333');
		expect(fmtPeerId(recentPeers()[0].id)).toBe('111 222 333');
	});

	it('a grouped GUI connect and a raw CLI connect are the SAME peer', () => {
		recordConnection('641 724 395', 'Pi');
		recordConnection('641724395', 'Pi');
		expect(historyPeers().length).toBe(1);
	});

	it('normalizeId/fmtPeerId: relay ids despace/group, addresses pass through', () => {
		expect(normalizeId('641 724 395')).toBe('641724395');
		expect(fmtPeerId('641724395')).toBe('641 724 395');
		expect(normalizeId('192.168.1.42:9000')).toBe('192.168.1.42:9000');
		expect(fmtPeerId('192.168.1.42:9000')).toBe('192.168.1.42:9000');
	});

	it('addPeer dedupes by id (any spacing) and is not a recent until connected', () => {
		expect(addPeer('Saved', '444 555 666')).toBe(true);
		expect(addPeer('Dup', '444555666')).toBe(false);
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

	it('history caps at 20 (oldest dropped; saved devices only lose the stamp)', () => {
		// Distinct timestamps — recordConnection stamps Date.now(), and a same-ms burst
		// would make "oldest" ambiguous.
		vi.useFakeTimers();
		try {
			vi.setSystemTime(1_000_000);
			addPeer('Kalıcı', '100 000 001');
			recordConnection('100 000 001', 'Kalıcı'); // oldest history entry, but SAVED
			for (let i = 0; i < 20; i++) {
				vi.advanceTimersByTime(1000);
				recordConnection(String(200000000 + i), `PC ${i}`);
			}
			expect(historyPeers().length).toBe(20);
			// The saved device fell out of history but stays in the address book.
			expect(historyPeers().some((p) => p.id === '100000001')).toBe(false);
			expect(savedPeers().some((p) => p.id === '100000001')).toBe(true);
		} finally {
			vi.useRealTimers();
		}
	});

	it('removeFromHistory: × drops a history-only entry, keeps a saved device', () => {
		recordConnection('111 222 333', 'Drive-by');
		addPeer('Kept', '444 555 666');
		recordConnection('444 555 666', 'Kept');
		removeFromHistory('111 222 333');
		removeFromHistory('444 555 666');
		expect(historyPeers().length).toBe(0);
		expect(allPeers().some((p) => p.id === '111222333')).toBe(false);
		expect(savedPeers().some((p) => p.id === '444555666')).toBe(true);
	});

	it('clearHistory drops history-only entries but keeps saved devices', () => {
		recordConnection('111 222 333', 'Drive-by'); // history only
		addPeer('Kept', '444 555 666'); // saved, never connected
		clearHistory();
		expect(historyPeers().length).toBe(0);
		expect(savedPeers().length).toBe(1);
		expect(savedPeers()[0].id).toBe('444555666');
	});

	it('recordConnection never renames a known peer (generic tab labels stay out)', () => {
		addPeer('Salon PC', '111 222 333');
		recordConnection('111 222 333', 'Uzak Cihaz'); // Home's manual-connect placeholder
		expect(savedPeers()[0].name).toBe('Salon PC');
		// …but it DOES upgrade an id-only placeholder name to a real one.
		recordConnection('444 555 666', '');
		expect(historyPeers().find((p) => p.id === '444555666')?.name).toBe('444 555 666');
		recordConnection('444 555 666', 'Oyun Rig’i');
		expect(historyPeers().find((p) => p.id === '444555666')?.name).toBe('Oyun Rig’i');
	});

	it('setPeerIdentity keeps a saved device’s user-chosen name (avatar still updates)', () => {
		addPeer('Annemin PC’si', '111 222 333');
		setPeerIdentity('111 222 333', { name: 'DESKTOP-X1Y2Z3', avatar: 'data:x' });
		expect(savedPeers()[0].name).toBe('Annemin PC’si');
		expect(savedPeers()[0].avatar).toBe('data:x');
	});

	it('identity-only entries are capped (inbound pushes can’t grow the store forever)', () => {
		for (let i = 0; i < 30; i++) setPeerIdentity(String(300000000 + i), { name: `Client ${i}` });
		const ghosts = allPeers().filter((p) => !p.saved && p.lastConnected == null);
		expect(ghosts.length).toBe(20);
		// Oldest evicted first, newest kept.
		expect(ghosts.some((p) => p.id === '300000000')).toBe(false);
		expect(ghosts.some((p) => p.id === '300000029')).toBe(true);
	});
});
