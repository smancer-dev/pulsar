// @vitest-environment node
// (No DOM needed — avoids jsdom's html-encoding-sniffer ESM load in this env.)
import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockRelaunch = vi.fn();
const mockCheck = vi.fn();

vi.mock('@tauri-apps/plugin-updater', () => ({
	check: (...args: unknown[]) => mockCheck(...args)
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: (...args: unknown[]) => mockRelaunch(...args) }));

import { silentUpdateCheck } from './updater';

// api.selfUpdatePossible is used inside run(); mock it to return true so the
// download path is exercised.
vi.mock('./api.commands', () => ({
	api: { selfUpdatePossible: vi.fn().mockResolvedValue(true) }
}));

beforeEach(() => {
	mockRelaunch.mockReset().mockResolvedValue(undefined);
	mockCheck.mockReset();
});

describe('silentUpdateCheck', () => {
	it('never throws when the updater backend is unavailable', async () => {
		mockCheck.mockRejectedValue(new Error('no tauri'));
		await expect(silentUpdateCheck()).resolves.toBeUndefined();
	});

	it('does not install when isBusy returns true after download completes', async () => {
		const mockDownload = vi.fn().mockResolvedValue(undefined);
		const mockInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ download: mockDownload, install: mockInstall });

		// isBusy is false before download but true after (session started during download).
		// This specifically covers the Windows case where install() is the destructive step
		// that exits the process — the post-download guard must fire before install(), not
		// after downloadAndInstall() (which was unreachable on Windows).
		let callCount = 0;
		const isBusy = () => {
			// First call: after check() — not busy yet
			// Second call: after download() completes — now busy (session started during DL)
			return ++callCount >= 2;
		};

		await silentUpdateCheck({ isBusy });

		expect(mockDownload).toHaveBeenCalledOnce();
		expect(mockInstall).not.toHaveBeenCalled();
		expect(mockRelaunch).not.toHaveBeenCalled();
	});

	it('does relaunch when isBusy is false at both guard points', async () => {
		const mockDownload = vi.fn().mockResolvedValue(undefined);
		const mockInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ download: mockDownload, install: mockInstall });

		await silentUpdateCheck({ isBusy: () => false });

		expect(mockDownload).toHaveBeenCalledOnce();
		expect(mockInstall).toHaveBeenCalledOnce();
		expect(mockRelaunch).toHaveBeenCalledOnce();
	});

	it('skips download entirely when isBusy is true after check()', async () => {
		const mockDownload = vi.fn().mockResolvedValue(undefined);
		const mockInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ download: mockDownload, install: mockInstall });

		await silentUpdateCheck({ isBusy: () => true });

		expect(mockDownload).not.toHaveBeenCalled();
		expect(mockInstall).not.toHaveBeenCalled();
		expect(mockRelaunch).not.toHaveBeenCalled();
	});
});

// ── Consent-based flow (checkForUpdateUi / installUpdate) ─────────────────────

import { checkForUpdateUi, installUpdate } from './updater';
import { update as updateState } from './update.svelte';
import { ui } from './settings.svelte';

function freshUpdate(over: Record<string, unknown> = {}) {
	return {
		version: '2.0.0',
		currentVersion: '1.0.0',
		body: 'notes',
		download: vi.fn().mockResolvedValue(undefined),
		install: vi.fn().mockResolvedValue(undefined),
		...over
	};
}

beforeEach(() => {
	updateState.available = false;
	updateState.open = false;
	updateState.phase = 'idle';
	updateState.handle = null;
	updateState.installable = true;
	updateState.error = '';
	ui.autoUpdate = false;
});

describe('checkForUpdateUi', () => {
	it('surfaces the update in the store WITHOUT installing when autoUpdate is off', async () => {
		const u = freshUpdate();
		mockCheck.mockResolvedValue(u);

		await checkForUpdateUi();

		expect(updateState.available).toBe(true);
		expect(updateState.from).toBe('1.0.0');
		expect(updateState.to).toBe('2.0.0');
		expect(updateState.notes).toBe('notes');
		expect(u.download).not.toHaveBeenCalled();
		expect(u.install).not.toHaveBeenCalled();
		expect(mockRelaunch).not.toHaveBeenCalled();
	});

	it('auto-installs when the autoUpdate setting is on and nothing is busy', async () => {
		const u = freshUpdate();
		mockCheck.mockResolvedValue(u);
		ui.autoUpdate = true;

		await checkForUpdateUi(() => false);

		expect(u.download).toHaveBeenCalledOnce();
		expect(u.install).toHaveBeenCalledOnce();
		expect(mockRelaunch).toHaveBeenCalledOnce();
	});

	it('does NOT auto-install when a session is busy, but still flags availability', async () => {
		const u = freshUpdate();
		mockCheck.mockResolvedValue(u);
		ui.autoUpdate = true;

		await checkForUpdateUi(() => true);

		expect(updateState.available).toBe(true);
		expect(u.download).not.toHaveBeenCalled();
	});

	it('never throws when check() fails', async () => {
		mockCheck.mockRejectedValue(new Error('offline'));
		await expect(checkForUpdateUi()).resolves.toBeUndefined();
		expect(updateState.available).toBe(false);
	});
});

describe('installUpdate', () => {
	it('is a no-op when the install is not possible on this platform', async () => {
		const u = freshUpdate();
		updateState.handle = u as never;
		updateState.available = true;
		updateState.installable = false;

		await installUpdate();

		expect(u.download).not.toHaveBeenCalled();
		expect(updateState.phase).toBe('idle');
	});

	it('walks download → install → restarting and relaunches', async () => {
		const u = freshUpdate();
		updateState.handle = u as never;
		updateState.available = true;

		await installUpdate();

		expect(u.download).toHaveBeenCalledOnce();
		expect(u.install).toHaveBeenCalledOnce();
		expect(mockRelaunch).toHaveBeenCalledOnce();
		expect(updateState.phase).toBe('restarting');
	});

	it('defers the destructive install when a session went live during the download', async () => {
		const u = freshUpdate();
		updateState.handle = u as never;
		updateState.available = true;

		await installUpdate(() => true);

		expect(u.download).toHaveBeenCalledOnce();
		expect(u.install).not.toHaveBeenCalled();
		expect(updateState.phase).toBe('idle');
	});

	it('lands in the error phase when the download fails', async () => {
		const u = freshUpdate({ download: vi.fn().mockRejectedValue(new Error('net')) });
		updateState.handle = u as never;
		updateState.available = true;

		await installUpdate();

		expect(updateState.phase).toBe('error');
		expect(updateState.error).toContain('net');
	});
});
