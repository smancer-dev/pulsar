<script lang="ts">
	// Dedicated connections-management window (opened by Rust as a separate OS window
	// with `window.__CONNECTIONS__` set). Lists every active inbound connection and
	// lets the host disconnect each — the single place to manage all connections.
	// Opens forward for a Remote connection, hidden for a Game one (decided host-side).
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api, onHostChat, onPeerAvatar, onPeerName, onSessionEvent } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Row = { peer: string; since: number; viewOnly: boolean };
	let rows = $state<Row[]>([]);
	// Peer id → pushed identity (image + display name). Seeded from the snapshot
	// (peer_meta host-side) so a window opened AFTER the push still shows them;
	// kept across a row's disconnect/reconnect so a brief drop doesn't blank them.
	let avatars = $state<Record<string, string>>({});
	let names = $state<Record<string, string>>({});

	onMount(() => {
		// Initial snapshot (connections that existed before this window opened).
		api
			.listConnections()
			.then((list) => {
				rows = list.map((c) => ({ peer: c.peer, since: c.since_ms, viewOnly: !!c.view_only }));
				for (const c of list) {
					if (c.avatar) avatars = { ...avatars, [c.peer]: c.avatar };
					if (c.name) names = { ...names, [c.peer]: c.name };
				}
			})
			.catch(() => {});
		// Live updates: the same `session` events the host emits broadcast to every window.
		const unlistens: (() => void)[] = [];
		onSessionEvent((e) => {
			if (e.kind === 'connected') {
				if (!rows.some((r) => r.peer === e.peer))
					rows = [...rows, { peer: e.peer, since: Date.now(), viewOnly: false }];
			} else if (e.kind === 'disconnected') {
				rows = rows.filter((r) => r.peer !== e.peer);
			}
		}).then((u) => {
			unlistens.push(u);
		});
		onPeerAvatar((e) => {
			avatars = { ...avatars, [e.peer]: e.dataUrl };
		}).then((u) => {
			unlistens.push(u);
		});
		onPeerName((e) => {
			names = { ...names, [e.peer]: e.name };
		}).then((u) => {
			unlistens.push(u);
		});
		// Seed the modal's history from the host-side backlog: events broadcast only
		// to live windows, so lines from before this window opened live there.
		api
			.chatLog()
			.then((log) => {
				const seeded: Record<string, Msg[]> = {};
				for (const [peer, text, me] of log) {
					(seeded[peer] ??= []).push({ me, text });
				}
				history = seeded;
			})
			.catch(() => {});
		// Inbound chat from clients → the per-peer history + an unread badge until
		// that peer's modal is opened.
		onHostChat((e) => {
			pushMsg(e.peer, { me: false, text: e.text });
			if (msgFor !== e.peer) unread = { ...unread, [e.peer]: (unread[e.peer] ?? 0) + 1 };
		}).then((u) => {
			unlistens.push(u);
		});
		return () => unlistens.forEach((u) => u());
	});

	// Live "for N" elapsed label, refreshed each second.
	let now = $state(Date.now());
	$effect(() => {
		const tmr = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(tmr);
	});
	function elapsed(since: number): string {
		const s = Math.max(0, Math.floor((now - since) / 1000));
		if (s < 60) return t('host.elapsedSec', { n: s });
		const m = Math.floor(s / 60);
		return m < 60
			? t('host.elapsedMin', { n: m })
			: t('host.elapsedHour', { h: Math.floor(m / 60), m: m % 60 });
	}

	// "Mesaj gönder" MODAL: per-peer chat with this window's session HISTORY (inbound
	// via host-chat above + everything we sent). Sending reuses the host→client chat
	// channel (DataMsg::Chat via host_send_chat) — it pops up on the client as a toast.
	type Msg = { me: boolean; text: string };
	let history = $state<Record<string, Msg[]>>({});
	let msgFor = $state<string | null>(null);
	let msgText = $state('');
	// Per-peer unread counter (badge on the row's chat button) — cleared on open.
	let unread = $state<Record<string, number>>({});
	function pushMsg(peer: string, m: Msg) {
		history = { ...history, [peer]: [...(history[peer] ?? []), m] };
	}
	function openMsg(peer: string) {
		msgFor = peer;
		msgText = '';
		sendErr = null;
		unread = { ...unread, [peer]: 0 };
	}
	function closeMsg() {
		msgFor = null;
		sendErr = null;
	}
	// A failed send (peer gone, channel closed) must be VISIBLE: only a confirmed
	// send lands in the history; on failure the composer gets the text back and the
	// modal shows the backend's reason (e.g. "cihaz bağlı değil").
	let sendErr = $state<string | null>(null);
	function sendMsg(peer: string) {
		const text = msgText.trim();
		if (!text) return;
		sendErr = null;
		msgText = '';
		api
			.hostSendChat(peer, text)
			.then(() => {
				pushMsg(peer, { me: true, text });
			})
			.catch((e) => {
				if (msgFor === peer) {
					sendErr = typeof e === 'string' && e ? e : t('host.msgFailed');
					if (!msgText) msgText = text;
				}
			});
	}

	// "Sadece izleme": revoke/restore this client's control — the host drops its
	// input events while set; the stream keeps running (AnyDesk permission model).
	function toggleViewOnly(r: Row) {
		const next = !r.viewOnly;
		api.setViewOnly(r.peer, next).catch(() => {});
		rows = rows.map((x) => (x.peer === r.peer ? { ...x, viewOnly: next } : x));
	}
</script>

<div class="conns">
	<header class="chdr">
		<span class="live"></span>
		<b>{t('host.activeTitle')}</b>
		<span class="count">{rows.length}</span>
	</header>

	{#if rows.length === 0}
		<div class="empty">{t('host.noConnections')}</div>
	{:else}
		<div class="list">
			{#each rows as r (r.peer)}
				<div class="pitem">
					<div class="prow">
						<span class="pavatar">
							{#if avatars[r.peer]}<img src={avatars[r.peer]} alt="" />{:else}{r.peer.slice(0, 2)}{/if}
						</span>
						<div class="pmeta">
							<span class="pname">{names[r.peer] ?? t('home.remoteDevice')}</span>
							<span class="ptime mono">{r.peer} · {elapsed(r.since)}</span>
						</div>
						<!-- Icon-only actions; the labels live in tooltips (they didn't fit). -->
						<div class="acts">
							<button
								class="act"
								class:on={r.viewOnly}
								onclick={() => toggleViewOnly(r)}
								title={r.viewOnly ? t('host.viewOnlyOn') : t('host.viewOnly')}
								aria-label={t('host.viewOnly')}
							>
								<Icon name="shield" size={14} />
							</button>
							<button
								class="act"
								onclick={() => openMsg(r.peer)}
								title={t('host.sendMsg')}
								aria-label={t('host.sendMsg')}
							>
								<Icon name="chat" size={14} />
								{#if (unread[r.peer] ?? 0) > 0}<span class="abadge">{unread[r.peer]}</span>{/if}
							</button>
							<button
								class="act kick"
								onclick={() => api.disconnectPeer(r.peer).catch(() => {})}
								title={t('host.disconnect')}
								aria-label={t('host.disconnect')}
							>
								<Icon name="power" size={14} />
							</button>
						</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}

	<!-- Message popup: this window's chat HISTORY with the peer + a composer.
	     Sent lines go over the host→client chat channel; inbound lines arrive via
	     host-chat. -->
	{#if msgFor}
		<div class="mmask" role="presentation" onclick={closeMsg}>
			<div class="mbox" role="dialog" aria-label={t('host.sendMsg')} onclick={(e) => e.stopPropagation()}>
				<header class="mhdr">
					<span class="pavatar small">
						{#if avatars[msgFor]}<img src={avatars[msgFor]} alt="" />{:else}{msgFor.slice(0, 2)}{/if}
					</span>
					<b>{names[msgFor] ?? t('home.remoteDevice')}</b>
					<span class="mid mono">{msgFor}</span>
					<button class="mclose" onclick={closeMsg} title={t('host.toastClose')}>
						<Icon name="x" size={14} />
					</button>
				</header>
				<div class="mlog">
					{#if (history[msgFor] ?? []).length === 0}
						<div class="mempty">{t('host.chatEmpty')}</div>
					{:else}
						{#each history[msgFor] ?? [] as m, i (i)}
							<div class="mline" class:me={m.me}>{m.text}</div>
						{/each}
					{/if}
				</div>
				{#if sendErr}
					<div class="merr" role="alert">{sendErr}</div>
				{/if}
				<form class="msgrow" onsubmit={(e) => { e.preventDefault(); if (msgFor) sendMsg(msgFor); }}>
					<input
						class="msgin"
						bind:value={msgText}
						placeholder={t('host.msgPlaceholder')}
						aria-label={t('host.msgPlaceholder')}
						onkeydown={(e) => e.key === 'Escape' && closeMsg()}
						{@attach (el) => el.focus()}
					/>
					<button class="msgsend" type="submit" disabled={!msgText.trim()}>{t('host.msgSend')}</button>
				</form>
			</div>
		</div>
	{/if}
</div>

<style>
	.conns {
		height: 100vh;
		display: flex;
		flex-direction: column;
		background: var(--bg, var(--surface));
		color: var(--text);
		padding: 14px;
		gap: 12px;
		box-sizing: border-box;
	}
	.chdr {
		display: flex;
		align-items: center;
		gap: 9px;
	}
	.chdr b {
		font-size: 14px;
		font-weight: 600;
	}
	.live {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--ok);
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--ok) 22%, transparent);
		flex: none;
	}
	.count {
		margin-left: auto;
		background: var(--accent-soft);
		color: var(--accent-press);
		border-radius: 999px;
		font-size: 12px;
		font-weight: 700;
		min-width: 20px;
		height: 20px;
		display: grid;
		place-items: center;
		padding: 0 6px;
	}
	.empty {
		flex: 1;
		display: grid;
		place-items: center;
		color: var(--text-faint);
		font-size: 13px;
	}
	.list {
		display: flex;
		flex-direction: column;
		gap: 8px;
		overflow-y: auto;
	}
	/* the card: a session row + (optionally) its inline message composer */
	.pitem {
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r-md);
		padding: 9px 11px;
	}
	.prow {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.pavatar {
		width: 32px;
		height: 32px;
		border-radius: 50%;
		background: var(--accent-soft);
		color: var(--accent-press);
		display: grid;
		place-items: center;
		font-size: 12px;
		font-weight: 700;
		text-transform: uppercase;
		flex: none;
		/* the pushed identity image must stay inside the round chip */
		overflow: hidden;
	}
	.pavatar img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.pmeta {
		display: flex;
		flex-direction: column;
		min-width: 0;
		flex: 1;
	}
	.pname {
		font-size: 13px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ptime {
		font-size: 11px;
		color: var(--text-faint);
	}
	/* per-row actions, right-aligned (AnyDesk-style) */
	.acts {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 6px;
		flex: none;
	}
	.act {
		display: flex;
		align-items: center;
		gap: 4px;
		background: none;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		color: var(--text-muted);
		font: inherit;
		font-size: 11px;
		padding: 5px 8px;
		cursor: pointer;
		flex: none;
	}
	.act:hover {
		background: var(--accent-soft);
		color: var(--accent-press);
	}
	.act.on {
		background: var(--accent-soft);
		color: var(--accent-press);
		border-color: var(--accent-soft-2);
	}
	.act.kick {
		color: var(--danger, oklch(0.55 0.2 25));
	}
	.act.kick:hover {
		background: color-mix(in oklch, var(--danger, red) 10%, transparent);
		color: var(--danger, oklch(0.55 0.2 25));
	}
	/* inline message composer under the row */
	.msgrow {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}
	.msgin {
		flex: 1;
		min-width: 0;
		font: inherit;
		font-size: 12.5px;
		color: var(--text);
		background: var(--surface);
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		padding: 6px 9px;
	}
	.msgsend {
		flex: none;
		font: inherit;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-on-accent);
		background: var(--accent);
		border: none;
		border-radius: var(--r-sm);
		padding: 6px 11px;
		cursor: pointer;
	}
	.msgsend:hover:not(:disabled) {
		background: var(--accent-hover);
	}
	.act {
		position: relative;
	}
	.abadge {
		position: absolute;
		top: -5px;
		right: -5px;
		min-width: 15px;
		height: 15px;
		padding: 0 4px;
		border-radius: 999px;
		background: var(--accent);
		color: var(--text-on-accent, #fff);
		font-size: 10px;
		font-weight: 700;
		display: grid;
		place-items: center;
	}

	/* Message popup */
	.mmask {
		position: fixed;
		inset: 0;
		background: oklch(0.2 0.02 265 / 0.45);
		display: grid;
		place-items: center;
		z-index: 20;
	}
	.mbox {
		width: min(92vw, 380px);
		max-height: 80vh;
		display: flex;
		flex-direction: column;
		gap: 10px;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 12px;
		padding: 12px;
		box-shadow: var(--shadow-2, 0 18px 40px oklch(0.2 0.03 265 / 0.25));
	}
	.mhdr {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.mhdr .mid {
		color: var(--text-faint);
		font-size: 11px;
		margin-left: auto;
	}
	.mclose {
		border: none;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		display: grid;
		place-items: center;
		padding: 4px;
		border-radius: 6px;
	}
	.mclose:hover {
		background: var(--accent-soft);
	}
	.pavatar.small {
		width: 26px;
		height: 26px;
		font-size: 10px;
	}
	.mlog {
		flex: 1;
		min-height: 140px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 4px 2px;
	}
	.mempty {
		color: var(--text-faint);
		font-size: 12.5px;
		margin: auto;
	}
	.mline {
		max-width: 85%;
		padding: 6px 10px;
		border-radius: 10px;
		background: var(--accent-soft);
		font-size: 13px;
		align-self: flex-start;
		word-break: break-word;
	}
	.mline.me {
		align-self: flex-end;
		background: var(--accent);
		color: var(--text-on-accent, #fff);
	}
	.msgsend:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.merr {
		font-size: 12px;
		color: var(--danger, oklch(0.55 0.2 25));
	}
</style>
