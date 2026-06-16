<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api, onFsEntries, onFileRecv, type FsEntry } from '$lib/api';
	import { listenScope } from '$lib/api.events';

	// Two-pane (AnyDesk-style) file manager inside the session menu: LEFT = this
	// device (local_ls), RIGHT = the host (fsList over the session; replies arrive
	// asynchronously as the `fs-entries` event). Both sides speak HOME-relative
	// paths with `/` separators ("" = home) and are jailed to HOME in Rust, so the
	// breadcrumb math here never has to care about OS path syntax.
	// `onBack` absent = standalone mode (the dedicated per-session files WINDOW):
	// no back button and the panes fill the window instead of the menu's 320px.
	type Props = { playId: number; onBack?: () => void };
	let { playId, onBack }: Props = $props();

	let localPath = $state('');
	let localEntries = $state<FsEntry[]>([]);
	let localLoading = $state(true);
	let remotePath = $state('');
	let remoteEntries = $state<FsEntry[]>([]);
	let remoteLoading = $state(true);
	// Transient status line (download/upload feedback), like the menu's note.
	let note = $state('');
	let noteTimer: ReturnType<typeof setTimeout> | undefined;

	function flash(msg: string) {
		note = msg;
		clearTimeout(noteTimer);
		noteTimer = setTimeout(() => (note = ''), 2600);
	}

	const join = (p: string, n: string) => (p ? `${p}/${n}` : n);
	const segs = (p: string) => (p ? p.split('/') : []);
	const parent = (p: string) => p.split('/').slice(0, -1).join('/');

	async function loadLocal(path: string) {
		localLoading = true;
		localPath = path;
		try {
			localEntries = await api.localLs(path);
		} catch {
			localEntries = [];
		}
		localLoading = false;
	}

	function loadRemote(path: string) {
		remoteLoading = true;
		remotePath = path; // optimistic — the host echoes it back in the reply
		api.fsList(playId, path).catch(() => (remoteLoading = false));
	}

	// Pending download completions keyed by filename — each entry is a queue of
	// {resolve, reject} for in-flight enqueueXfer slots waiting on file-recv.
	// Using an array per name so two simultaneous downloads of the same filename
	// are handled in FIFO order.
	const pendingDownloads = new Map<string, Array<{ resolve: () => void; reject: () => void }>>();

	// Inbound listings + download results for THIS play; initial load of both
	// panes. Scoped to the component so it tears down when the panel closes.
	$effect(() => {
		const idStr = String(playId);
		const scope = listenScope();
		scope.add(
			onFsEntries((e) => {
				if (e.id !== playId) return;
				remotePath = e.path;
				remoteEntries = e.entries;
				remoteLoading = false;
			}),
			// A download landed (the host streamed our fsGet back): surface the result —
			// the file was saved under "Pulsar Alınanlar" by the Rust side.
			onFileRecv((e) => {
				if (e.peer !== idStr) return;
				flash(
					e.ok ? t('files.downloaded', { name: e.name }) : t('files.downloadFail', { name: e.name })
				);
				// Drain the concurrency slot held for this download (keyed by filename).
				const queue = pendingDownloads.get(e.name);
				if (queue && queue.length > 0) {
					const { resolve, reject } = queue.shift()!;
					if (queue.length === 0) pendingDownloads.delete(e.name);
					if (e.ok) resolve(); else reject();
				}
			})
		);
		loadLocal('');
		loadRemote('');
		return scope.dispose;
	});

	function fmtSize(n: number): string {
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
		if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
		return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
	}

	// ── Transfer concurrency guard ────────────────────────────────────────────
	// The Rust reassemblers (handlers.rs / hold.rs) cap concurrent in-flight
	// transfers at MAX_CONCURRENT_XFERS=8 and evict the oldest active entry when
	// the cap is reached, which silently fails the evicted transfer.  To prevent
	// that, we limit the number of transfers we put on the wire at once to
	// MAX_WIRE_XFERS (< 8) and queue the rest; each completed transfer (resolved
	// or rejected) drains one item from the queue.
	const MAX_WIRE_XFERS = 6; // safely below the reassembler cap of 8
	let activeXfers = 0;
	const xferQueue: Array<() => void> = [];

	/** Run `fn` now if under the wire limit, otherwise enqueue it. */
	function enqueueXfer(fn: () => Promise<void>): void {
		if (activeXfers < MAX_WIRE_XFERS) {
			startXfer(fn);
		} else {
			xferQueue.push(() => startXfer(fn));
		}
	}

	function startXfer(fn: () => Promise<void>): void {
		activeXfers++;
		fn().finally(() => {
			activeXfers--;
			const next = xferQueue.shift();
			if (next) next();
		});
	}

	// Timeout (ms) after which a download slot is released even if file-recv never
	// arrives (host refusal / lost FileEnd / jailed path returns nothing).
	const DOWNLOAD_TIMEOUT_MS = 30_000;

	function download(name: string) {
		flash(t('files.downloading', { name }));
		enqueueXfer(async () => {
			// 1. Send the FsGet request to the host.
			await api.fsGet(playId, join(remotePath, name)).catch(() => {
				flash(t('files.downloadFail', { name }));
			});
			// 2. Wait for the actual file-recv completion event for this file.
			//    The slot stays occupied until the host finishes streaming back OR
			//    the timeout fires — whichever comes first.
			await new Promise<void>((resolve, reject) => {
				let timer: ReturnType<typeof setTimeout> | undefined;
				const entry = {
					resolve: () => { clearTimeout(timer); resolve(); },
					reject:  () => { clearTimeout(timer); reject(); },
				};
				if (!pendingDownloads.has(name)) pendingDownloads.set(name, []);
				pendingDownloads.get(name)!.push(entry);
				// Timeout: release the slot even if the host never answers.
				timer = setTimeout(() => {
					const queue = pendingDownloads.get(name);
					if (queue) {
						const idx = queue.indexOf(entry);
						if (idx !== -1) queue.splice(idx, 1);
						if (queue.length === 0) pendingDownloads.delete(name);
					}
					resolve(); // drain the slot; the flash was already shown by onFileRecv or stays as "downloading"
				}, DOWNLOAD_TIMEOUT_MS);
			}).catch(() => {/* reject just means failed download — slot still drains */});
		});
	}

	function upload(name: string) {
		flash(t('session.fileSending', { name }));
		enqueueXfer(async () => {
			try {
				await api.sendFilePath(playId, join(localPath, name));
				flash(t('session.fileSent', { name }));
			} catch {
				flash(t('session.fileError', { name }));
			}
		});
	}
</script>

{#snippet crumbs(path: string, nav: (p: string) => void, label: string)}
	<nav class="crumbs" aria-label={label}>
		<button class="crumb" onclick={() => nav('')}>{t('files.home')}</button>
		{#each segs(path) as seg, i (i)}
			<span class="sep">/</span>
			<button
				class="crumb"
				onclick={() =>
					nav(
						segs(path)
							.slice(0, i + 1)
							.join('/')
					)}>{seg}</button
			>
		{/each}
	</nav>
{/snippet}

{#snippet listing(
	path: string,
	entries: FsEntry[],
	loading: boolean,
	nav: (p: string) => void,
	action: (name: string) => void,
	actionIcon: string,
	actionLabel: string
)}
	<div class="list">
		{#if loading}
			<div class="state">{t('files.loading')}</div>
		{:else}
			{#if path}
				<button class="row" onclick={() => nav(parent(path))}>
					<Icon name="folder" size={14} />
					<span class="nm up">{t('files.up')}</span>
				</button>
			{/if}
			{#if entries.length === 0}
				<div class="state">{t('files.empty')}</div>
			{:else}
				{#each entries as e (e.name)}
					{#if e.dir}
						<button class="row" onclick={() => nav(join(path, e.name))}>
							<Icon name="folder" size={14} />
							<span class="nm">{e.name}</span>
						</button>
					{:else}
						<div class="row file">
							<Icon name="file" size={14} />
							<span class="nm">{e.name}</span>
							<span class="sz mono">{fmtSize(e.size)}</span>
							<button
								class="act"
								title={actionLabel}
								aria-label={`${actionLabel}: ${e.name}`}
								onclick={() => action(e.name)}
							>
								<Icon name={actionIcon} size={13} />
							</button>
						</div>
					{/if}
				{/each}
			{/if}
		{/if}
	</div>
{/snippet}

<div class="files" class:standalone={!onBack}>
	{#if onBack}
		<button class="files-back" onclick={onBack}>
			<Icon name="arrowRight" size={14} class="flip" />{t('session.back')}
		</button>
	{/if}
	<div class="panes">
		<section class="pane" aria-label={t('files.local')}>
			<div class="pane-head"><Icon name="monitor" size={13} />{t('files.local')}</div>
			{@render crumbs(localPath, loadLocal, t('files.local'))}
			{@render listing(
				localPath,
				localEntries,
				localLoading,
				loadLocal,
				upload,
				'upload',
				t('files.upload')
			)}
		</section>
		<section class="pane" aria-label={t('files.remote')}>
			<div class="pane-head"><Icon name="wifi" size={13} />{t('files.remote')}</div>
			{@render crumbs(remotePath, loadRemote, t('files.remote'))}
			{@render listing(
				remotePath,
				remoteEntries,
				remoteLoading,
				loadRemote,
				download,
				'download',
				t('files.download')
			)}
		</section>
	</div>
	{#if note}<div class="files-note">{note}</div>{/if}
</div>

<style>
	.files {
		display: flex;
		flex-direction: column;
		height: 320px;
	}
	.files.standalone {
		height: 100%;
		min-height: 0;
		flex: 1;
	}
	.files-back {
		align-self: flex-start;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		border: none;
		background: transparent;
		color: oklch(0.7 0.02 265);
		font-size: 12px;
		cursor: pointer;
		padding: 0 0 8px;
	}
	.files-back :global(.flip) {
		transform: rotate(180deg);
	}
	.panes {
		flex: 1;
		min-height: 0;
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 10px;
	}
	.pane {
		min-width: 0;
		display: flex;
		flex-direction: column;
		border: 1px solid oklch(0.3 0.014 265 / 0.6);
		border-radius: var(--r-sm);
		background: oklch(0.2 0.012 265 / 0.6);
		overflow: hidden;
	}
	.pane-head {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 7px 9px;
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: oklch(0.66 0.02 265);
		border-bottom: 1px solid oklch(0.3 0.014 265 / 0.6);
	}
	.crumbs {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 2px;
		padding: 5px 8px;
		border-bottom: 1px solid oklch(0.3 0.014 265 / 0.5);
		font-size: 11px;
	}
	.crumb {
		border: none;
		background: transparent;
		padding: 1px 3px;
		border-radius: 4px;
		color: oklch(0.8 0.02 265);
		font-size: 11px;
		cursor: pointer;
		max-width: 110px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.crumb:hover {
		background: oklch(0.3 0.016 272 / 0.8);
		color: #fff;
	}
	.sep {
		color: oklch(0.55 0.02 265);
	}
	.list {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 4px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.state {
		margin: auto;
		font-size: 11.5px;
		color: oklch(0.6 0.02 265);
		text-align: center;
		padding: 8px;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 7px;
		width: 100%;
		padding: 6px 7px;
		border: none;
		border-radius: 6px;
		background: transparent;
		color: oklch(0.92 0.008 265);
		font-size: 12px;
		text-align: left;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.row:hover {
		background: oklch(0.3 0.016 272 / 0.7);
	}
	.row.file {
		cursor: default;
	}
	.nm {
		flex: 1;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.nm.up {
		color: oklch(0.72 0.02 265);
	}
	.sz {
		flex: none;
		font-size: 10px;
		color: oklch(0.62 0.02 265);
	}
	.act {
		flex: none;
		display: grid;
		place-items: center;
		width: 24px;
		height: 24px;
		border: 1px solid oklch(0.36 0.016 265);
		border-radius: 6px;
		background: oklch(0.26 0.014 265 / 0.9);
		color: oklch(0.92 0.008 265);
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.act:hover {
		background: var(--accent);
		border-color: var(--accent);
		color: #fff;
	}
	.files-note {
		margin-top: 8px;
		font-size: 11.5px;
		color: oklch(0.82 0.02 265);
		text-align: center;
		line-height: 1.4;
	}
</style>
