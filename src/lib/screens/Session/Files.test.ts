import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';

// Mock only the file-manager surface of the api: local listings resolve
// immediately, remote listings are captured so each test can push the host's
// `fs-entries` reply by hand (the real flow is fire-and-forget + event).
let fsEntriesCb:
	| ((e: { id: number; path: string; entries: { name: string; dir: boolean; size: number }[] }) => void)
	| undefined;

vi.mock('$lib/api', async (importOriginal) => {
	const orig = await importOriginal<typeof import('$lib/api')>();
	return {
		...orig,
		api: {
			...orig.api,
			localLs: vi.fn(async (path: string) =>
				path === ''
					? [
							{ name: 'Belgeler', dir: true, size: 0 },
							{ name: 'yerel.txt', dir: false, size: 2048 }
						]
					: []
			),
			fsList: vi.fn(async () => {}),
			fsGet: vi.fn(async () => {}),
			sendFilePath: vi.fn(async () => {})
		},
		onFsEntries: vi.fn(async (cb: typeof fsEntriesCb) => {
			fsEntriesCb = cb;
			return () => {};
		}),
		onFileRecv: vi.fn(async () => () => {})
	};
});

import Files from './Files.svelte';
import { api } from '$lib/api';

const props = (playId: number) => ({ props: { playId, onBack: () => {} } });

describe('Files panel (session file manager)', () => {
	beforeEach(() => {
		fsEntriesCb = undefined;
		vi.clearAllMocks();
	});

	it('renders the local pane from local_ls and requests the host home listing', async () => {
		render(Files, props(7));
		expect(await screen.findByText('Belgeler')).toBeInTheDocument();
		expect(screen.getByText('yerel.txt')).toBeInTheDocument();
		// size column for the local file
		expect(screen.getByText('2.0 KB')).toBeInTheDocument();
		// the remote pane asked the host for "" (= its HOME) on open
		expect(api.fsList).toHaveBeenCalledWith(7, '');
	});

	it('renders host entries when the fs-entries reply lands and downloads via fsGet', async () => {
		render(Files, props(7));
		await screen.findByText('Belgeler');
		fsEntriesCb?.({
			id: 7,
			path: '',
			entries: [
				{ name: 'Sunum', dir: true, size: 0 },
				{ name: 'rapor.pdf', dir: false, size: 123456 }
			]
		});
		expect(await screen.findByText('rapor.pdf')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'İndir: rapor.pdf' }));
		expect(api.fsGet).toHaveBeenCalledWith(7, 'rapor.pdf');
	});

	it('enters a remote directory by clicking its row (fsList with the joined path)', async () => {
		render(Files, props(3));
		await screen.findByText('Belgeler');
		fsEntriesCb?.({ id: 3, path: '', entries: [{ name: 'Sunum', dir: true, size: 0 }] });
		await fireEvent.click(await screen.findByText('Sunum'));
		expect(api.fsList).toHaveBeenLastCalledWith(3, 'Sunum');
	});

	it('ignores fs-entries replies addressed to another play id', async () => {
		render(Files, props(7));
		await screen.findByText('Belgeler');
		fsEntriesCb?.({ id: 99, path: '', entries: [{ name: 'baskasi.txt', dir: false, size: 1 }] });
		// stays in the loading state — no foreign entries shown
		expect(screen.queryByText('baskasi.txt')).not.toBeInTheDocument();
	});

	it('sends a local file with gönder (send_file_path with the HOME-relative path)', async () => {
		render(Files, props(7));
		await screen.findByText('yerel.txt');
		await fireEvent.click(screen.getByRole('button', { name: 'Gönder: yerel.txt' }));
		expect(api.sendFilePath).toHaveBeenCalledWith(7, 'yerel.txt');
	});
});
