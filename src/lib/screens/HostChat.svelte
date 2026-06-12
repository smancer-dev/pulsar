<script lang="ts">
	// Host-side side-channel TOASTS: incoming clipboard pushes and file transfers.
	// The old floating chat FAB + panel that lived here is GONE — host↔client chat
	// moved to the connections window's per-peer message modal (Connections.svelte);
	// an inbound message now opens that window instead of popping a panel here.
	import Icon from '$lib/Icon.svelte';
	import { copyText, onClipboardIn, onFileRecv } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Toast = { id: number; kind: 'clip' | 'file' | 'fileFail'; peer: string; text: string; copied?: boolean };
	let toasts = $state<Toast[]>([]);
	let toastSeq = 0;
	function pushToast(toast: Omit<Toast, 'id'>) {
		const id = toastSeq++;
		toasts = [...toasts, { ...toast, id }];
		setTimeout(() => (toasts = toasts.filter((x) => x.id !== id)), 9000);
	}
	function dismiss(id: number) {
		toasts = toasts.filter((x) => x.id !== id);
	}

	// file-recv also fires on the CLIENT when its own file-manager download lands
	// (peer = play id, e.g. "3") — the Files panel already flashes for those, so only
	// device-id-keyed events (real host-side receives) belong in these toasts.
	const isDeviceId = (p: string) => /^\d{9}$/.test(p.replace(/\s/g, '')) || /[.:]/.test(p);

	$effect(() => {
		// Dead-flag guard (same as Connections/Connecting): a listen() resolving AFTER
		// this teardown must unlisten immediately, not land in a dead array (it would leak).
		let dead = false;
		const offs: Array<() => void> = [];
		const track = (o: () => void) => {
			if (dead) o();
			else offs.push(o);
		};
		onClipboardIn((e) => {
			copyText(e.text).catch(() => {});
			pushToast({ kind: 'clip', peer: e.peer, text: e.text });
		}).then(track);
		onFileRecv((e) => {
			if (!isDeviceId(e.peer)) return;
			pushToast({ kind: e.ok ? 'file' : 'fileFail', peer: e.peer, text: e.name });
		}).then(track);
		return () => {
			dead = true;
			offs.forEach((o) => o());
		};
	});

	async function copyToast(toa: Toast) {
		const ok = await copyText(toa.text);
		if (ok) toasts = toasts.map((x) => (x.id === toa.id ? { ...x, copied: true } : x));
	}
</script>

<!-- toasts -->
{#if toasts.length}
	<div class="toasts">
		{#each toasts as toa (toa.id)}
			<div class="toast" class:fail={toa.kind === 'fileFail'}>
				<div class="ticon">
					<Icon name={toa.kind === 'clip' ? 'clipboard' : 'file'} size={16} />
				</div>
				<div class="tbody">
					{#if toa.kind === 'clip'}
						<div class="ttitle">{t('host.clipboardRecv', { peer: toa.peer })}</div>
						<div class="tprev mono">{toa.text.slice(0, 80)}</div>
					{:else if toa.kind === 'file'}
						<div class="ttitle">{t('host.fileRecv', { peer: toa.peer, name: toa.text })}</div>
						<div class="tprev">{t('host.fileSaved')}</div>
					{:else}
						<div class="ttitle">{t('host.fileFailed', { peer: toa.peer, name: toa.text })}</div>
					{/if}
				</div>
				{#if toa.kind === 'clip'}
					<button class="tcopy" onclick={() => copyToast(toa)}>
						{toa.copied ? t('host.clipboardCopied') : t('host.clipboardCopy')}
					</button>
				{/if}
				<button class="tx" aria-label={t('host.toastClose')} onclick={() => dismiss(toa.id)}>
					<Icon name="x" size={13} />
				</button>
			</div>
		{/each}
	</div>
{/if}


<style>
	.toasts {
		position: fixed;
		bottom: 16px;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		gap: 8px;
		z-index: 60;
		width: min(420px, calc(100vw - 32px));
	}
	.toast {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 12px;
		border-radius: var(--r);
		background: var(--surface);
		border: 1px solid var(--border);
		box-shadow: var(--shadow-lg);
	}
	.toast.fail {
		border-color: color-mix(in oklch, var(--danger) 45%, var(--border));
	}
	.ticon {
		width: 30px;
		height: 30px;
		flex: none;
		display: grid;
		place-items: center;
		border-radius: 8px;
		background: var(--accent-soft);
		color: var(--accent);
	}
	.tbody {
		flex: 1;
		min-width: 0;
	}
	.ttitle {
		font-size: 13px;
		font-weight: 600;
	}
	.tprev {
		font-size: 11.5px;
		color: var(--text-faint);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		margin-top: 2px;
	}
	.tcopy {
		flex: none;
		font-size: 12px;
		font-weight: 600;
		padding: 6px 11px;
		border-radius: var(--r-sm);
		border: 1px solid var(--accent-soft-2);
		background: var(--accent-soft);
		color: var(--accent-press);
		cursor: pointer;
	}
	.tx {
		flex: none;
		border: none;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		display: grid;
		place-items: center;
		padding: 2px;
	}
</style>
