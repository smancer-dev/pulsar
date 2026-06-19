// @vitest-environment node
// (SessionManager is pure logic — no DOM needed; avoids jsdom ESM issues with
// Svelte runes and $state outside a component context.)
import { describe, it, expect, vi, beforeEach } from 'vitest';

// Stub the event registrations (onPlayReady / onPlayEnded) and the api surface
// before importing SessionManager so the constructor doesn't blow up in Node.
vi.mock('$lib/api', async () => {
	const { vi: _vi } = await import('vitest');
	// 3-monitor host (mirrors the real PC test host) so the same-host free-display search
	// has monitors to assign. Each call returns a distinct play id so concurrent same-host
	// sessions don't share one.
	let nextId = 1;
	const mockStartRemotePlay = _vi.fn().mockImplementation(() =>
		Promise.resolve({
			id: nextId++,
			transport: 'direct',
			ws_port: 0,
			audio_ws_port: 0,
			local: false,
			native: false,
			embedded: false,
			host_codecs: [],
			host_encoders: [],
			host_displays: [
				{ idx: 0, name: 'DISPLAY1', width: 2560, height: 1440, primary: true },
				{ idx: 1, name: 'DISPLAY2', width: 1920, height: 1080, primary: false },
				{ idx: 2, name: 'DISPLAY3', width: 1920, height: 1080, primary: false }
			]
		})
	);
	return {
		isTauri: false,
		api: {
			startRemotePlay: mockStartRemotePlay,
			stopStream: _vi.fn().mockResolvedValue(undefined),
			runCommand: _vi.fn().mockResolvedValue(undefined),
			setPaneCount: _vi.fn().mockResolvedValue(undefined),
			setPlayResolution: _vi.fn().mockResolvedValue(undefined),
			setActiveSession: _vi.fn().mockResolvedValue(undefined)
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

describe('SessionManager — same-host split: per-pane host-display selection (Phase 2a)', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		ui.forwardControllers = true;
		ui.touchpadAsMouse = true;
	});

	// The displayIdx is the 10th positional arg (index 9) of startRemotePlay.
	const displayArg = (call: number) =>
		(api.startRemotePlay as ReturnType<typeof vi.fn>).mock.calls[call][9];

	it('a lone pane keeps single-session behavior: displayIdx undefined (host default 0)', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2');
		await mgr.connectIntoPane(0, target, 'remote');
		expect(api.startRemotePlay).toHaveBeenCalledOnce();
		expect(displayArg(0)).toBeUndefined();
		expect(mgr.sessions).toHaveLength(1);
	});

	it('second same-host pane auto-assigns the first FREE monitor (idx 1)', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2');
		await mgr.connectIntoPane(0, target, 'remote');
		await mgr.connectIntoPane(1, target, 'remote');
		// Two real sessions to the same host (not collapsed).
		expect(mgr.sessions).toHaveLength(2);
		// First pane → host default (undefined → 0); second → first free advertised idx (1).
		expect(displayArg(0)).toBeUndefined();
		expect(displayArg(1)).toBe(1);
	});

	it('two "Masaüstü" (gameId="") game panes to one host do NOT collapse into one session', async () => {
		const mgr = makeManager('game');
		mgr.enterSplit('h2', 'game');
		await mgr.connectIntoPane(0, target, 'game', '');
		await mgr.connectIntoPane(1, target, 'game', '');
		expect(mgr.sessions).toHaveLength(2);
		expect(mgr.sessions.every((s) => s.mode === 'game')).toBe(true);
		expect(displayArg(1)).toBe(1);
	});

	it('a third same-host pane skips both used monitors → idx 2', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('grid4');
		await mgr.connectIntoPane(0, target, 'remote');
		await mgr.connectIntoPane(1, target, 'remote');
		await mgr.connectIntoPane(2, target, 'remote');
		expect(mgr.sessions).toHaveLength(3);
		expect(displayArg(2)).toBe(2);
	});

	it('an explicit picker choice (displayIdx) is passed through and bypasses de-dup', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2');
		await mgr.connectIntoPane(0, target, 'remote');
		// User picks monitor 2 explicitly for the second pane.
		await mgr.connectIntoPane(1, target, 'remote', '', 2);
		expect(mgr.sessions).toHaveLength(2);
		expect(displayArg(1)).toBe(2);
	});

	it('different hosts are independent: each first pane uses the host default', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2');
		await mgr.connectIntoPane(0, target, 'remote');
		await mgr.connectIntoPane(1, { name: 'PC2', id: '444 555 666' }, 'remote');
		expect(mgr.sessions).toHaveLength(2);
		expect(displayArg(0)).toBeUndefined();
		expect(displayArg(1)).toBeUndefined();
	});

	it('connectIntoPane FORCES the split mode — a stray per-pane mode arg is ignored', async () => {
		const mgr = makeManager('remote');
		// Split entered as GAME — every pane must be a game session even if a caller passes
		// 'remote' (no mixed gaming+remote split).
		mgr.enterSplit('h2', 'game');
		await mgr.connectIntoPane(0, target, 'remote');
		await mgr.connectIntoPane(1, target, 'remote');
		expect(mgr.splitSessionMode).toBe('game');
		expect(mgr.sessions.every((s) => s.mode === 'game')).toBe(true);
	});

	it('setLayout preserves the established split mode across a reshape (2→4)', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2', 'game');
		mgr.setLayout('grid4'); // omit mode — must keep 'game', not fall back to ui.appMode
		expect(mgr.splitSessionMode).toBe('game');
	});

	it('helpers report a host\'s known displays + claimed indices', async () => {
		const mgr = makeManager('remote');
		mgr.enterSplit('h2');
		await mgr.connectIntoPane(0, target, 'remote');
		await mgr.connectIntoPane(1, target, 'remote');
		expect(mgr.hostDisplaysFor(target.id)).toHaveLength(3);
		// pane 0 claimed 0 (default), pane 1 claimed 1.
		expect([...mgr.usedDisplaysFor(target.id)].sort()).toEqual([0, 1]);
		// Next free is 2.
		expect(mgr.freeDisplayFor(target.id)).toBe(2);
	});
});
