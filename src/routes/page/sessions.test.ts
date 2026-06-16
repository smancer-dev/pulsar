// @vitest-environment node
// (SessionManager is pure logic — no DOM needed; avoids jsdom ESM issues with
// Svelte runes and $state outside a component context.)
import { describe, it, expect, vi, beforeEach } from 'vitest';

// Stub the event registrations (onPlayReady / onPlayEnded) and the api surface
// before importing SessionManager so the constructor doesn't blow up in Node.
vi.mock('$lib/api', async () => {
	const { vi: _vi } = await import('vitest');
	const mockStartRemotePlay = _vi.fn().mockResolvedValue({
		id: 1,
		transport: 'direct',
		ws_port: 0,
		audio_ws_port: 0,
		local: false,
		native: false,
		embedded: false,
		host_codecs: [],
		host_encoders: [],
		host_displays: []
	});
	return {
		isTauri: false,
		api: {
			startRemotePlay: mockStartRemotePlay,
			stopStream: _vi.fn().mockResolvedValue(undefined),
			runCommand: _vi.fn().mockResolvedValue(undefined)
		},
		onPlayReady: _vi.fn(),
		onPlayEnded: _vi.fn(),
		setFullscreen: _vi.fn().mockResolvedValue(undefined),
		recordConnection: _vi.fn()
	};
});

// Stub recordConnection imported inside sessions.svelte.ts via $lib/peers.svelte
vi.mock('$lib/peers.svelte', () => ({ recordConnection: vi.fn() }));

// Stub i18n (t) to avoid loading JSON files in Node
vi.mock('$lib/i18n.svelte', () => ({ t: (k: string) => k }));

import { api } from '$lib/api';
import { SessionManager } from './sessions.svelte';
import { ui } from '$lib/settings.svelte';

function makeManager(mode: 'remote' | 'game' = 'remote') {
	return new SessionManager({ getMode: () => mode, onAuthDone: () => {} });
}

const target = { name: 'PC', id: '111 222 333' };

describe('SessionManager.startConnect — forwardControllers / gameMode decoupling', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		// Ensure the default setting values are in place
		ui.forwardControllers = true;
		ui.touchpadAsMouse = true;
	});

	it('remote mode: 6th arg = ui.forwardControllers (true), 7th arg = false', async () => {
		const mgr = makeManager('remote');
		await mgr.startConnect(target, 'remote');
		expect(api.startRemotePlay).toHaveBeenCalledOnce();
		const args: unknown[] = (api.startRemotePlay as ReturnType<typeof vi.fn>).mock.calls[0];
		// 6th arg (index 5) = gamepad = ui.forwardControllers
		expect(args[5]).toBe(true);
		// 7th arg (index 6) = gameMode
		expect(args[6]).toBe(false);
	});

	it('game mode: 6th arg = ui.forwardControllers (true), 7th arg = true', async () => {
		const mgr = makeManager('game');
		await mgr.startConnect(target, 'game');
		expect(api.startRemotePlay).toHaveBeenCalledOnce();
		const args: unknown[] = (api.startRemotePlay as ReturnType<typeof vi.fn>).mock.calls[0];
		expect(args[5]).toBe(true);
		expect(args[6]).toBe(true);
	});

	it('remote mode with forwardControllers=false: 6th arg = false, 7th arg = false', async () => {
		ui.forwardControllers = false;
		const mgr = makeManager('remote');
		await mgr.startConnect(target, 'remote');
		const args: unknown[] = (api.startRemotePlay as ReturnType<typeof vi.fn>).mock.calls[0];
		expect(args[5]).toBe(false);
		expect(args[6]).toBe(false);
	});

	it('game mode with forwardControllers=false: gamepad off but gameMode still true', async () => {
		ui.forwardControllers = false;
		const mgr = makeManager('game');
		await mgr.startConnect(target, 'game');
		const args: unknown[] = (api.startRemotePlay as ReturnType<typeof vi.fn>).mock.calls[0];
		// gamepad (arg 5) reflects the setting, NOT the mode
		expect(args[5]).toBe(false);
		// gameMode (arg 6) is still true — mode and gamepad are decoupled
		expect(args[6]).toBe(true);
	});
});
