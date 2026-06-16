<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api, onFsEntries, onFileRecv, onFileBegin, type FsEntry } from '$lib/api';
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

	// Mirror of Rust's `sanitize_filename` (files.rs) so pendingDownloads is keyed by
	// the same name the Rust reassembler will emit in the file-recv event.  Without this
	// any file whose name is altered by sanitize_filename (colons on Windows, leading/
	// trailing whitespace, or control chars on any platform) produces a map key that
	// never matches e.name, so the concurrency slot drains only via the 30 s timeout
	// and the success/fail flash is never shown.
	const onWindows = typeof navigator !== 'undefined' && /Win/i.test(navigator.platform);
	// Reserved DOS device stems (checked case-insensitively, before extension).
	const DOS_RESERVED = new Set([
		'CON','PRN','AUX','NUL',
		'COM1','COM2','COM3','COM4','COM5','COM6','COM7','COM8','COM9',
		'LPT1','LPT2','LPT3','LPT4','LPT5','LPT6','LPT7','LPT8','LPT9',
	]);
	function sanitizeFilename(name: string): string {
		// Step 1: last path component, trim surrounding whitespace.
		const base = (name.split(/[/\\]/).pop() ?? name).trim();
		// Step 2: strip control chars (and ':' on Windows).
		let cleaned = '';
		for (const ch of base) {
			const cp = ch.codePointAt(0) ?? 0;
			if (cp <= 0x1f) continue;
			if (onWindows && ch === ':') continue;
			cleaned += ch;
		}
		// Step 3: structural guard — must be a single normal path component.
		// Treat empty, '.', and '..' as invalid (mirrors Path::components check).
		if (!cleaned || cleaned === '.' || cleaned === '..') return 'dosya';
		// Step 4: Windows-only escape_reserved — trim trailing dots/spaces, prefix
		// reserved device names.
		if (onWindows) {
			const trimmed = cleaned.replace(/[. ]+$/, '') || cleaned;
			const stem = (trimmed.split('.')[0] ?? trimmed).toUpperCase();
			if (DOS_RESERVED.has(stem) || trimmed.length !== cleaned.length) {
				return '_' + trimmed;
			}
			return trimmed;
		}
		return cleaned;
	}

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

	// Two-level pending-download tracking (C21 fix: keyed by transfer id, not name).
	//
	// Phase 1 — before `file-begin` arrives: entries live in `waitingByName`, a
	// FIFO queue per filename.  A download() call registers here immediately (before
	// the FsGet even reaches the host) so nothing is lost if file-begin is fast.
	//
	// Phase 2 — once `file-begin` fires with the host-assigned xferId: the head of
	// the matching waitingByName queue is popped and moved into `pendingById` under
	// its numeric xferId.  `onBegin()` is called here (cancels the short no-response
	// timeout and arms the long safety backstop).
	//
	// Phase 3 — `file-recv` arrives with the same xferId: look up `pendingById`
	// and resolve/reject that exact slot, then delete it.
	//
	// This guarantees that a late file-recv for a timed-out download (whose entry
	// was already spliced out of waitingByName by the timeout) can never match a
	// different in-flight download that happens to share the same basename.
	// `onBegin` receives the host-assigned xferId so the entry's removeEntry helper
	// can clean up pendingById (not waitingByName) if the safety backstop fires.
	type PendingEntry = { resolve: () => void; reject: () => void; onBegin: (xferId: number) => void };
	const waitingByName = new Map<string, Array<PendingEntry>>();
	const pendingById   = new Map<number, PendingEntry>();

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
			// The host started streaming a download (FileBegin received).
			// Move the head of waitingByName[name] → pendingById[xferId] and call
			// onBegin(xferId) to cancel the short no-response timeout and arm the
			// long safety backstop.  FIFO order inside waitingByName matches the
			// order the user clicked 'Indir', so the correct entry is always promoted.
			onFileBegin((e) => {
				if (e.peer !== idStr) return;
				const queue = waitingByName.get(e.name);
				if (queue && queue.length > 0) {
					const entry = queue.shift()!;
					if (queue.length === 0) waitingByName.delete(e.name);
					pendingById.set(e.xferId, entry);
					entry.onBegin(e.xferId);
				}
			}),
			// A download landed (the host streamed our fsGet back): surface the result —
			// the file was saved under "Pulsar Alınanlar" by the Rust side.
			// Keyed by xferId so a late file-recv for a timed-out same-name download
			// cannot drain a different in-flight download's concurrency slot (C21 fix).
			onFileRecv((e) => {
				if (e.peer !== idStr) return;
				flash(
					e.ok ? t('files.downloaded', { name: e.name }) : t('files.downloadFail', { name: e.name })
				);
				// Drain the concurrency slot held for this exact transfer.
				const entry = pendingById.get(e.xferId);
				if (entry) {
					pendingById.delete(e.xferId);
					if (e.ok) entry.resolve(); else entry.reject();
				}
				// If xferId is absent from pendingById this file-recv belongs to a
				// transfer that already timed out (its entry was spliced out by the
				// no-response or safety-backstop timer) — silently ignore it so it
				// cannot affect any other queued slot.
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

	// How long to wait for the FIRST response from the host (FileBegin / file-recv)
	// before giving up and releasing the concurrency slot.  Covers the "host refused /
	// jailed path / network lost before the host could reply" case.  Once FileBegin
	// arrives the `onBegin` callback cancels this and arms DOWNLOAD_SAFETY_TIMEOUT_MS
	// instead, so a legitimately-slow transfer is never evicted.
	const DOWNLOAD_NO_RESPONSE_MS = 30_000;
	// Safety backstop armed after FileBegin: 2× the Rust idle-sweep window (60 s) so
	// the Rust side gets a chance to emit file-recv{ok:false} via its own sweep first,
	// and we only release as a last resort for a truly dead transfer (lost FileEnd with
	// no subsequent download to trigger the sweep).
	const DOWNLOAD_SAFETY_TIMEOUT_MS = 120_000;

	function download(name: string) {
		flash(t('files.downloading', { name }));
		// Key by the sanitized name so it matches what the Rust FileBegin event
		// carries in `e.name` (sanitize_filename in files.rs strips ':' on Windows,
		// trims leading/trailing whitespace, and handles reserved names).
		const key = sanitizeFilename(name);
		enqueueXfer(async () => {
			// 1. Send the FsGet request to the host.
			await api.fsGet(playId, join(remotePath, name)).catch(() => {
				flash(t('files.downloadFail', { name }));
			});
			// 2. Wait for the actual file-recv completion event for this file.
			//    Phase A: a short no-response timer covers "host never replied at all".
			//    Phase B: onFileBegin (fired when FileBegin datagram arrives) moves
			//    this entry from waitingByName into pendingById[xferId], cancels the
			//    short timer, and arms a long safety backstop — the slot stays occupied
			//    for the whole duration of a legitimately-slow transfer.
			//    Phase C: onFileRecv (keyed by xferId) resolves/rejects this entry.
			await new Promise<void>((resolve, reject) => {
				let timer: ReturnType<typeof setTimeout> | undefined;

				// removeEntry removes this entry from whichever map currently holds it.
				// Before file-begin: waitingByName (no xferId yet).
				// After file-begin: pendingById (onBegin sets assignedId below).
				let assignedId: number | undefined;
				function removeEntry() {
					if (assignedId !== undefined) {
						pendingById.delete(assignedId);
					} else {
						const queue = waitingByName.get(key);
						if (queue) {
							const idx = queue.indexOf(entry);
							if (idx !== -1) queue.splice(idx, 1);
							if (queue.length === 0) waitingByName.delete(key);
						}
					}
				}

				const entry: PendingEntry = {
					resolve: () => { clearTimeout(timer); resolve(); },
					reject:  () => { clearTimeout(timer); reject(); },
					// Called by onFileBegin when the host starts streaming: the entry
					// has already been moved from waitingByName to pendingById[xferId]
					// by the onFileBegin handler above.  Record xferId for removeEntry,
					// cancel the short no-response timer, and arm the long safety backstop.
					onBegin: (xferId: number) => {
						assignedId = xferId;
						clearTimeout(timer);
						timer = setTimeout(() => {
							removeEntry();
							// Safety backstop: FileBegin arrived but FileEnd never came
							// (lost packet or mid-stream host crash).  The Rust idle-sweep
							// should have already emitted file-recv{ok:false}, but if that
							// event was also lost show the failure flash here as a last resort.
							flash(t('files.downloadFail', { name }));
							resolve(); // drain the slot so queued downloads can proceed
						}, DOWNLOAD_SAFETY_TIMEOUT_MS);
					},
				};
				if (!waitingByName.has(key)) waitingByName.set(key, []);
				waitingByName.get(key)!.push(entry);
				// Phase A: no-response timeout — release the slot if the host never
				// sends FileBegin (e.g. path refused / network lost before host reply,
				// or the synthetic FileBegin+FileEnd was itself lost on a lossy link).
				// Show a failure flash so the user is not left with a silent
				// "indiriliyor…" note stuck on screen.
				timer = setTimeout(() => {
					removeEntry();
					flash(t('files.downloadFail', { name }));
					resolve(); // drain the slot so queued downloads can proceed
				}, DOWNLOAD_NO_RESPONSE_MS);
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
					{#if e.sentinel}
						<div class="row sentinel" aria-label={e.name}>
							<span class="nm trunc">{e.name}</span>
						</div>
					{:else if e.dir}
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
	.row.sentinel {
		cursor: default;
		opacity: 0.5;
	}
	.nm.trunc {
		font-style: italic;
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
