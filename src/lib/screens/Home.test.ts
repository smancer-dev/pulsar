import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import Home from './Home.svelte';
import { recordConnection, _reset } from '$lib/peers.svelte';

const noop = () => {};

describe('Home screen', () => {
	beforeEach(() => _reset());

	it('shows this device id and an empty recents state by default', () => {
		render(Home, { props: { selfId: '482 913 056', mode: 'remote', hostSessions: [], activity: [], onMode: noop, onConnect: noop } });
		expect(screen.getByText('482 913 056')).toBeInTheDocument();
		expect(screen.getByText(/Henüz bağlantı yok/)).toBeInTheDocument();
	});

	it('disables connect until a 6+ digit id is entered, then connects', async () => {
		const onConnect = vi.fn();
		render(Home, { props: { selfId: '482 913 056', mode: 'remote', hostSessions: [], activity: [], onMode: noop, onConnect } });
		const btn = screen.getByRole('button', { name: 'Bağlan' });
		expect(btn).toBeDisabled();

		const input = screen.getByLabelText('Hedef cihaz kimliği');
		await fireEvent.input(input, { target: { value: '719204663' } });
		expect(btn).not.toBeDisabled();
		await fireEvent.click(btn);
		expect(onConnect).toHaveBeenCalledOnce();
		expect(onConnect.mock.calls[0][0].id).toBe('719 204 663');
	});

	it('lists a real recorded connection (id grouped in threes) and re-connects to it', async () => {
		recordConnection('640117992', 'Oyun Rig’i'); // CLI-style raw id
		const onConnect = vi.fn();
		render(Home, { props: { selfId: '482 913 056', mode: 'game', hostSessions: [], activity: [], onMode: noop, onConnect } });
		// Displayed grouped even though it was recorded raw.
		expect(screen.getByText('640 117 992')).toBeTruthy();
		await fireEvent.click(screen.getByText('Oyun Rig’i'));
		// The stored id is canonical (despaced).
		expect(onConnect).toHaveBeenCalledWith({ name: 'Oyun Rig’i', id: '640117992' }, 'game');
	});
});
