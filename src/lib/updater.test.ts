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

	it('does not relaunch when isBusy returns true after download completes', async () => {
		const mockDownloadAndInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ downloadAndInstall: mockDownloadAndInstall });

		// isBusy is false before download but true after (session started during download)
		let callCount = 0;
		const isBusy = () => {
			// First call: after check() — not busy yet
			// Second call: after downloadAndInstall() — now busy
			return ++callCount >= 2;
		};

		await silentUpdateCheck({ isBusy });

		expect(mockDownloadAndInstall).toHaveBeenCalledOnce();
		expect(mockRelaunch).not.toHaveBeenCalled();
	});

	it('does relaunch when isBusy is false at both guard points', async () => {
		const mockDownloadAndInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ downloadAndInstall: mockDownloadAndInstall });

		await silentUpdateCheck({ isBusy: () => false });

		expect(mockDownloadAndInstall).toHaveBeenCalledOnce();
		expect(mockRelaunch).toHaveBeenCalledOnce();
	});

	it('skips download entirely when isBusy is true after check()', async () => {
		const mockDownloadAndInstall = vi.fn().mockResolvedValue(undefined);
		mockCheck.mockResolvedValue({ downloadAndInstall: mockDownloadAndInstall });

		await silentUpdateCheck({ isBusy: () => true });

		expect(mockDownloadAndInstall).not.toHaveBeenCalled();
		expect(mockRelaunch).not.toHaveBeenCalled();
	});
});
