<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { api, copyText } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Props = {
		selfId: string;
		selfPw?: string;
		online?: boolean;
		connecting?: boolean;
		hostSessions: { peer: string; since: number }[];
		activity: string[];
		debug?: boolean;
		localIp: string;
		onRefreshPw?: () => void;
		onDisconnect?: (peer: string) => void;
	};
	let {
		selfId,
		selfPw = '',
		online = false,
		connecting = false,
		hostSessions,
		activity,
		debug = false,
		localIp,
		onRefreshPw = () => {},
		onDisconnect = () => {}
	}: Props = $props();

	let copied = $state(false);
	async function copyId() {
		const ok = await copyText(selfId.replace(/\s/g, ''));
		if (ok) {
			copied = true;
			setTimeout(() => (copied = false), 1400);
		}
	}
	let pwCopied = $state(false);
	async function copyPw() {
		if (!online || !selfPw) return;
		if (await copyText(selfPw)) {
			pwCopied = true;
			setTimeout(() => (pwCopied = false), 1400);
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
	<div class="lab">{t('home.deviceId')}</div>
	<div class="row">
		<span class="bigid mono">{selfId}</span>
		<button class="icon-btn push" onclick={copyId} title={t('home.copy')} aria-label={t('home.copyId')}>
			<Icon name={copied ? 'check' : 'copy'} size={17} />
		</button>
	</div>
	{#if localIp}
		<div
			style="display:flex;align-items:center;gap:6px;margin-top:8px;font-size:12.5px;color:var(--text-muted)"
			title={t('home.localIp')}
		>
			<Icon name="globe" size={13} /><span class="mono">{localIp}</span>
		</div>
	{/if}
	<div class="sep"></div>
	<div class="lab">{t('home.otp')}</div>
	<div class="row">
		<span class="pw mono">{online ? selfPw || '—' : '—'}</span>
		<button
			class="icon-btn push"
			title={t('home.copy')}
			aria-label={t('home.copyPw')}
			onclick={copyPw}
			disabled={!online || !selfPw}
		>
			<Icon name={pwCopied ? 'check' : 'copy'} size={16} />
		</button>
		<button
			class="icon-btn"
			title={t('home.refresh')}
			aria-label={t('home.refreshPw')}
			onclick={onRefreshPw}
			disabled={!online}
		>
			<Icon name="refresh" size={16} />
		</button>
	</div>
	<div class="sep"></div>
	<div class="connhdr">{t('home.connectedHdr')}</div>
	{#if hostSessions.length === 0}
		<div class="connempty">{t('home.noConnected')}</div>
	{:else}
		{#each hostSessions as s (s.peer)}
			<div class="connrow">
				<span class="cdot"></span><span class="mono">{s.peer}</span>
				<button class="kick" onclick={() => onDisconnect(s.peer)} title={t('home.kick')}>
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
