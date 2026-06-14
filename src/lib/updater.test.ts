// @vitest-environment node
// (No DOM needed — avoids jsdom's html-encoding-sniffer ESM load in this env.)
import { describe, it, expect, vi } from 'vitest';

vi.mock('@tauri-apps/plugin-updater', () => ({
	check: vi.fn().mockRejectedValue(new Error('no tauri'))
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn() }));

import { silentUpdateCheck } from './updater';

describe('silentUpdateCheck', () => {
	it('never throws when the updater backend is unavailable', async () => {
		await expect(silentUpdateCheck()).resolves.toBeUndefined();
	});
});
