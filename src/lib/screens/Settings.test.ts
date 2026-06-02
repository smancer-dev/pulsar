import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import Settings from './Settings.svelte';
import { api } from '$lib/api';

describe('Settings screen', () => {
	it('exposes the relay server as an editable field on the Ağ tab', async () => {
		render(Settings);
		await fireEvent.click(screen.getByRole('button', { name: /Ağ/ }));
		const relay = await screen.findByLabelText('Relay sunucusu adresi');
		// loaded from the (mock) core config
		expect((relay as HTMLInputElement).value).toContain(':');
	});

	it('changing the network mode persists it through the api', async () => {
		render(Settings);
		await fireEvent.click(screen.getByRole('button', { name: /Ağ/ }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Yalnız relay' }));
		// the mock api stored the new mode
		const cfg = await api.getConfig();
		expect(cfg.network_mode).toBe('relay-only');
		// restore for other tests
		await api.setConfig({ ...cfg, network_mode: 'auto' });
	});
});
