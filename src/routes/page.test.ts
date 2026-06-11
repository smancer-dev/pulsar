import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import Page from './+page.svelte';

describe('App shell', () => {
	it('renders the frameless chrome, sidebar nav and the home screen', () => {
		render(Page);
		// The OS draws the window title now; the in-app chrome shows just the screen
		// name ("Bağlan"), which also appears on the sidebar/Home buttons.
		expect(screen.getAllByText('Bağlan').length).toBeGreaterThan(0);
		for (const label of ['Bağlan', 'Cihazlar', 'Oyunlar', 'Ayarlar']) {
			// "Bağlan" also appears on the Home connect button, so allow multiple.
			expect(screen.getAllByRole('button', { name: new RegExp(label) }).length).toBeGreaterThan(0);
		}
	});

	it('navigates to the Devices screen', async () => {
		render(Page);
		await fireEvent.click(screen.getByRole('button', { name: /Cihazlar/ }));
		expect(screen.getByText(/Adres defterin/)).toBeInTheDocument();
	});

	it('navigates to Settings', async () => {
		render(Page);
		await fireEvent.click(screen.getByRole('button', { name: /Ayarlar/ }));
		expect(screen.getByText(/ağ ve güvenlik tercihlerini yönet/)).toBeInTheDocument();
	});
});
