import { describe, it, expect, beforeEach } from 'vitest';
import { api } from './api';

describe('api bridge (browser mock)', () => {
	beforeEach(async () => {
		// reset mock relay config to auto
		const c = await api.getConfig();
		await api.setConfig({ ...c, network_mode: 'auto' });
	});

	it('returns a default config with a relay endpoint', async () => {
		const c = await api.getConfig();
		expect(c.relay).toContain(':');
		expect(['auto', 'p2p-only', 'relay-only']).toContain(c.network_mode);
	});

	it('go_online yields a 9-digit grouped id', async () => {
		const id = await api.goOnline();
		expect(id.replace(/\D/g, '')).toHaveLength(9);
	});

	it('connect reports direct transport in auto mode and relay in relay-only', async () => {
		const auto = await api.connect('719 204 663');
		expect(auto.transport).toBe('direct');

		const c = await api.getConfig();
		await api.setConfig({ ...c, network_mode: 'relay-only' });
		const relayed = await api.connect('719 204 663');
		expect(relayed.transport).toBe('relay');
	});

	it('controllers returns a list (empty without hardware)', async () => {
		const list = await api.controllers();
		expect(Array.isArray(list)).toBe(true);
	});
});
