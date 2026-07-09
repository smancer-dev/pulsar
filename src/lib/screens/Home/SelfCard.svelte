<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { api, copyText } from '$lib/api';
	import { fmtPeerId } from '$lib/peers.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = {
		selfId: string;
		selfPw?: string;
		online?: boolean;
		connecting?: boolean;
		/** Host has unattended access ON → no one-time password is issued; anyone with
		 * the ID can connect without approval. */
		unattended?: boolean;
		hostSessions: { sid: number; peer: string; since: number }[];
		/** Peer map-key → the client's own device id (pushed via DataMsg::PeerId): show
		 * the client's ID even on a direct/same-LAN connect (peer key is an ip:port then). */
		hostClientIds?: Record<string, string>;
		/** Peer map-key → the client's pushed display name (shown in parens after the id/ip). */
		hostNames?: Record<string, string>;
		activity: string[];
		debug?: boolean;
		localIp: string;
		/** The node's actual bound UDP port (0 = offline) — shown as "ip:port". */
		nodePort?: number;
		onRefreshPw?: () => void;
		/** Kick a connected session by its session id. */
		onDisconnect?: (sid: number) => void;
	};
	let {
		selfId,
		selfPw = '',
		online = false,
		connecting = false,
		unattended = false,
		hostSessions,
		hostClientIds = {},
		hostNames = {},
		activity,
		debug = false,
		localIp,
		nodePort = 0,
		onRefreshPw = () => {},
		onDisconnect = () => {}
	}: Props = $props();

	// When the relay is unreachable the node still binds + serves locally (LAN peers can
	// reach us by IP:port), but the 9-digit relay ID only resolves THROUGH the relay — so
	// it's useless offline. Promote the direct address to the PRIMARY identifier then.
	const relayless = $derived(!online && !!localIp && nodePort > 0);
	const primaryLabel = $derived(relayless ? t('home.localIp') : t('home.deviceId'));
	const primaryValue = $derived(relayless ? `${localIp}:${nodePort}` : selfId);

	// Show the client's pushed device ID when known, else the raw peer key (ip:port on a
	// direct/same-LAN connect, the grouped relay id otherwise) — and append the pushed
	// display name in parens when known: "303 036 449 (orangepi)".
	function peerLabel(peer: string): string {
		const id = fmtPeerId(hostClientIds[peer] ?? peer);
		const name = hostNames[peer];
		return name ? `${id} (${name})` : id;
	}

	let copied = $state(false);
	async function copyId() {
		const ok = await copyText(primaryValue.replace(/\s/g, ''));
		if (ok) {
			copied = true;
			setTimeout(() => (copied = false), 1400);
		}
	}
	let pwCopied = $state(false);
	async function copyPw() {
		if (unattended || !online || !selfPw) return;
		if (await copyText(selfPw)) {
			pwCopied = true;
			setTimeout(() => (pwCopied = false), 1400);
		}
	}
	let addrCopied = $state(false);
	async function copyAddr() {
		if (await copyText(`${localIp}${nodePort > 0 ? `:${nodePort}` : ''}`)) {
			addrCopied = true;
			setTimeout(() => (addrCopied = false), 1400);
		}
	}
</script>

<div class="card">
	<div class="row sb">
		<span class="eyebrow mono">{t('home.allowThis')}</span>
		<span class="badge" class:online class:pending={connecting && !online} class:off={!online && !connecting}>
			<span class="dot"></span>
			{#if connecting}{t('status.connecting')}{:else if online}{t('home.ready')}{:else}{t('status.offline')}{/if}
		</span>
	</div>
	<div class="lab">{primaryLabel}</div>
	<div class="row">
		<span class="bigid mono">{primaryValue}</span>
		<button class="icon-btn push" onclick={copyId} title={t('home.copy')} aria-label={t('home.copyId')}>
			<Icon name={copied ? 'check' : 'copy'} size={17} />
		</button>
	</div>
	{#if online && localIp}
		<!-- Secondary direct-connect address (relay-online only; when offline the address
		     IS the primary identifier above, so this row would just duplicate it). The
		     whole row copies it (same check-flash as the id). -->
		<button
			class="addr"
			onclick={copyAddr}
			title={t('home.localIp') + ' · ' + t('home.copy')}
			aria-label={t('home.copyAddr')}
		>
			<Icon name="globe" size={13} /><span class="mono"
				>{localIp}{nodePort > 0 ? `:${nodePort}` : ''}</span
			>
			<Icon name={addrCopied ? 'check' : 'copy'} size={12} />
		</button>
	{/if}
	<div class="sep"></div>
	<div class="lab">{t('home.otp')}</div>
	<div class="row">
		<span class="pw mono">{unattended ? '—' : online ? selfPw || '—' : '—'}</span>
		<button
			class="icon-btn push"
			title={t('home.copy')}
			aria-label={t('home.copyPw')}
			onclick={copyPw}
			disabled={unattended || !online || !selfPw}
		>
			<Icon name={pwCopied ? 'check' : 'copy'} size={16} />
		</button>
		<button
			class="icon-btn"
			title={t('home.refresh')}
			aria-label={t('home.refreshPw')}
			onclick={onRefreshPw}
			disabled={unattended || !online}
		>
			<Icon name="refresh" size={16} />
		</button>
	</div>
	{#if unattended}
		<!-- Unattended access bypasses the one-time-password gate entirely: warn loudly so
		     the operator knows anyone who can reach this ID connects without approval. -->
		<div class="warn" role="alert">
			<Icon name="shield" size={15} />
			<div>
				<div class="warn-title">{t('home.unattendedOn')}</div>
				<div class="warn-body">{t('home.unattendedWarn')}</div>
			</div>
		</div>
	{/if}
	<div class="sep"></div>
	<div class="connhdr">{t('home.connectedHdr')}</div>
	{#if hostSessions.length === 0}
		<div class="connempty">{t('home.noConnected')}</div>
	{:else}
		{#each hostSessions as s (s.sid)}
			<div class="connrow">
				<span class="cdot"></span><span class="mono">{peerLabel(s.peer)}</span>
				<button class="kick" onclick={() => onDisconnect(s.sid)} title={t('home.kick')}>
					<Icon name="x" size={12} />{t('home.kickLabel')}
				</button>
			</div>
		{/each}
	{/if}
	{#if debug && activity.length > 0}
		<div class="actlog">
			{#each activity as line, i (i)}<div class="actline">{line}</div>{/each}
		</div>
	{/if}
</div>

<style>
	.row {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.row.sb {
		justify-content: space-between;
		margin-bottom: 18px;
	}
	.push {
		margin-left: auto;
	}
	.icon-btn:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}
	.addr {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-top: 8px;
		padding: 0;
		border: none;
		background: transparent;
		font: inherit;
		font-size: 12.5px;
		color: var(--text-muted);
		cursor: pointer;
	}
	.addr:hover {
		color: var(--accent-press);
	}
	.eyebrow {
		font-size: 11px;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.lab {
		font-size: 12.5px;
		color: var(--text-muted);
		font-weight: 600;
		margin-bottom: 7px;
	}
	.bigid {
		font-size: 27px;
		font-weight: 500;
		letter-spacing: 0.04em;
		white-space: nowrap;
	}
	.pw {
		font-size: 22px;
		font-weight: 500;
		letter-spacing: 0.12em;
	}
	.warn {
		display: flex;
		align-items: flex-start;
		gap: 9px;
		margin-top: 12px;
		padding: 10px 12px;
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--warn) 14%, var(--surface));
		border: 1px solid color-mix(in oklch, var(--warn) 45%, var(--border));
		color: color-mix(in oklch, var(--warn) 65%, var(--text));
	}
	.warn :global(svg) {
		flex: none;
		margin-top: 1px;
	}
	.warn-title {
		font-size: 12.5px;
		font-weight: 700;
	}
	.warn-body {
		font-size: 12px;
		line-height: 1.45;
		margin-top: 2px;
		color: var(--text-muted);
	}
	.sep {
		height: 1px;
		background: var(--border);
		margin: 20px 0;
	}
	.connhdr {
		font-size: 11.5px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-faint);
		margin-bottom: 8px;
	}
	.connempty {
		font-size: 12.5px;
		color: var(--text-faint);
	}
	.connrow {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 0;
		font-size: 13px;
	}
	.cdot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ok);
		flex: none;
	}
	.kick {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-size: 11px;
		font-weight: 600;
		padding: 3px 8px;
		border-radius: var(--r-sm);
		border: 1px solid color-mix(in oklch, var(--danger) 35%, var(--border));
		background: color-mix(in oklch, var(--danger) 10%, transparent);
		color: var(--danger);
		cursor: pointer;
	}
	.kick:hover {
		background: color-mix(in oklch, var(--danger) 20%, transparent);
	}
	.actlog {
		margin-top: 10px;
		border-top: 1px solid var(--border);
		padding-top: 8px;
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.actline {
		font-size: 11.5px;
		color: var(--text-faint);
	}
</style>
