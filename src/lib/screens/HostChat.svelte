<script lang="ts">
	// Host-side side-channel surface: a floating chat panel (reply to connected
	// clients) plus toasts for incoming clipboard pushes and file transfers.
	import Icon from '$lib/Icon.svelte';
	import { api, copyText, onHostChat, onClipboardIn, onFileRecv } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Msg = { me: boolean; text: string };
	// Conversations keyed by peer id. Using a plain object in $state so Svelte
	// tracks replacement; we always reassign to trigger reactivity.
	let convos = $state<Record<string, Msg[]>>({});
	let peers = $derived(Object.keys(convos));
	let active = $state('');
	let open = $state(false);
	let unread = $state(0);
	let reply = $state('');
	let logBox = $state<HTMLDivElement | null>(null);

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

	function append(peer: string, m: Msg) {
		convos = { ...convos, [peer]: [...(convos[peer] ?? []), m] };
		queueMicrotask(() => logBox?.scrollTo({ top: logBox.scrollHeight }));
	}

	function openPanel(peer?: string) {
		if (peer) active = peer;
		else if (!active && peers.length) active = peers[0];
		open = true;
		unread = 0;
	}
	function sendReply() {
		const text = reply.trim();
		if (!text || !active) return;
		api.hostSendChat(active, text).catch(() => {});
		append(active, { me: true, text });
		reply = '';
	}

	$effect(() => {
		let offs: Array<() => void> = [];
		onHostChat((e) => {
			append(e.peer, { me: false, text: e.text });
			if (!active) active = e.peer;
			if (!open || active !== e.peer) unread++;
		}).then((o) => offs.push(o));
		onClipboardIn((e) => {
			copyText(e.text).catch(() => {});
			pushToast({ kind: 'clip', peer: e.peer, text: e.text });
		}).then((o) => offs.push(o));
		onFileRecv((e) => {
			pushToast({ kind: e.ok ? 'file' : 'fileFail', peer: e.peer, text: e.name });
		}).then((o) => offs.push(o));
		return () => offs.forEach((o) => o());
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

<!-- chat FAB + panel (only once a client has messaged) -->
{#if peers.length}
	{#if !open}
		<button class="fab" onclick={() => openPanel()} aria-label={t('host.chatTitle')}>
			<Icon name="chat" size={20} />
			{#if unread > 0}<span class="fab-badge">{unread}</span>{/if}
		</button>
	{:else}
		<div class="panel">
			<div class="phead">
				<Icon name="chat" size={15} />
				<span class="ptitle">{t('host.chatTitle')}</span>
				<button class="pclose" aria-label={t('host.toastClose')} onclick={() => (open = false)}>
					<Icon name="x" size={14} />
				</button>
			</div>
			{#if peers.length > 1}
				<div class="ptabs">
					{#each peers as p (p)}
						<button class="ptab mono" class:on={active === p} onclick={() => (active = p)}>
							{p}
						</button>
					{/each}
				</div>
			{:else}
				<div class="ppeer mono">{active}</div>
			{/if}
			<div class="log" bind:this={logBox}>
				{#if !(convos[active]?.length)}
					<div class="empty">{t('host.chatEmpty')}</div>
				{:else}
					{#each convos[active] as m, i (i)}
						<div class="bubble" class:me={m.me}>
							<span class="who">{m.me ? t('host.you') : active}</span>
							<span class="txt">{m.text}</span>
						</div>
					{/each}
				{/if}
			</div>
			<div class="inrow">
				<input
					bind:value={reply}
					placeholder={t('host.chatPlaceholder')}
					onkeydown={(e) => e.key === 'Enter' && sendReply()}
					aria-label={t('host.chatTitle')}
				/>
				<button class="send" onclick={sendReply} aria-label="send"><Icon name="arrowRight" size={16} /></button>
			</div>
		</div>
	{/if}
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
	.fab {
		position: fixed;
		right: 20px;
		bottom: 20px;
		width: 50px;
		height: 50px;
		border-radius: 50%;
		border: none;
		background: var(--accent);
		color: #fff;
		display: grid;
		place-items: center;
		cursor: pointer;
		box-shadow: var(--shadow-lg);
		z-index: 55;
	}
	.fab:hover {
		background: var(--accent-press);
	}
	.fab-badge {
		position: absolute;
		top: -3px;
		right: -3px;
		min-width: 18px;
		height: 18px;
		padding: 0 4px;
		border-radius: var(--r-pill);
		background: var(--danger);
		color: #fff;
		font-size: 10.5px;
		font-weight: 700;
		display: grid;
		place-items: center;
	}
	.panel {
		position: fixed;
		right: 20px;
		bottom: 20px;
		width: 320px;
		height: 420px;
		max-height: calc(100vh - 80px);
		display: flex;
		flex-direction: column;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r-lg);
		box-shadow: var(--shadow-lg);
		z-index: 55;
		overflow: hidden;
	}
	.phead {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 14px;
		border-bottom: 1px solid var(--border);
		font-weight: 600;
		color: var(--accent-press);
	}
	.ptitle {
		flex: 1;
	}
	.pclose {
		border: none;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		display: grid;
		place-items: center;
	}
	.ptabs {
		display: flex;
		gap: 4px;
		padding: 8px 10px 0;
		overflow-x: auto;
	}
	.ptab {
		font-size: 11px;
		padding: 4px 8px;
		border-radius: var(--r-pill);
		border: 1px solid var(--border);
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		white-space: nowrap;
	}
	.ptab.on {
		background: var(--accent-soft);
		color: var(--accent-press);
		border-color: var(--accent-soft-2);
	}
	.ppeer {
		font-size: 11.5px;
		color: var(--text-faint);
		padding: 8px 14px 0;
	}
	.log {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 12px 14px;
	}
	.empty {
		margin: auto;
		text-align: center;
		font-size: 12.5px;
		color: var(--text-faint);
		line-height: 1.5;
		padding: 0 10px;
	}
	.bubble {
		display: flex;
		flex-direction: column;
		gap: 2px;
		max-width: 84%;
		padding: 7px 10px;
		border-radius: 12px;
		background: var(--surface-3);
		align-self: flex-start;
	}
	.bubble.me {
		align-self: flex-end;
		background: var(--accent);
		color: #fff;
	}
	.bubble .who {
		font-size: 9.5px;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		opacity: 0.7;
	}
	.bubble .txt {
		font-size: 13px;
		line-height: 1.35;
		word-break: break-word;
	}
	.inrow {
		display: flex;
		gap: 6px;
		padding: 10px;
		border-top: 1px solid var(--border);
	}
	.inrow input {
		flex: 1;
		min-width: 0;
		padding: 9px 11px;
		border-radius: var(--r-sm);
		border: 1px solid var(--border-strong);
		background: var(--surface-2);
		color: var(--text);
		font-family: var(--font-sans);
		font-size: 13px;
	}
	.inrow input:focus {
		outline: none;
		border-color: var(--accent);
	}
	.send {
		flex: none;
		width: 38px;
		display: grid;
		place-items: center;
		border: none;
		border-radius: var(--r-sm);
		background: var(--accent);
		color: #fff;
		cursor: pointer;
	}
	.send:hover {
		background: var(--accent-press);
	}
</style>
