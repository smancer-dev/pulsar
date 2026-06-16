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
