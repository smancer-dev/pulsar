<script lang="ts">
	import PulsarMark from '$lib/PulsarMark.svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type View = 'home' | 'devices' | 'gaming' | 'settings';
	type Props = {
		nav: { id: View; icon: string }[];
		view: View;
		online: boolean;
		connecting: boolean;
		connError: string;
		/** Active inbound (host) connections — shown as a badge on "Bağlantılar". */
		hostCount?: number;
		onView: (v: View) => void;
		onGoOnline: () => void;
	};
	let { nav, view, online, connecting, connError, hostCount = 0, onView, onGoOnline }: Props = $props();

	// The identity chip: the OS account image (or wallpaper, per avatar_mode) and
	// the OS user's display name replace the static "Sen" / "Bu cihaz" labels.
	// Loaded once — both only change outside the app. Failures (browser mock,
	// anonymous mode, no image) keep the textual fallbacks.
	let avatarUrl = $state('');
	let userName = $state('');
	api.selfAvatar()
		.then((u) => (avatarUrl = u ?? ''))
		.catch(() => {});
	api.deviceUserName()
		.then((n) => (userName = n ?? ''))
		.catch(() => {});
</script>

<aside class="sidebar">
	<div class="brand">
		<PulsarMark size={26} />
		<span class="nm">Pulsar</span>
	</div>
	<nav class="nav">
		{#each nav as n (n.id)}
			<button
				class="navlink"
				class:on={view === n.id}
				onclick={() => onView(n.id)}
			>
				<Icon name={n.icon} size={19} />
				{t('nav.' + n.id)}
			</button>
		{/each}
		{#if online}
			<!-- Reveal/focus the dedicated connections window (it auto-opens on a new
			     connection — forward for remote, hidden for game — this re-surfaces it). -->
			<button class="navlink" onclick={() => api.showConnections().catch(() => {})}>
				<Icon name="devices" size={19} />
				{t('host.connectionsBtn')}
				{#if hostCount > 0}
					<span class="navbadge">{hostCount}</span>
				{/if}
			</button>
		{/if}
	</nav>
	<div class="sidefoot">
		<div class="me">
			<div class="meavatar">
				{#if avatarUrl}<img src={avatarUrl} alt={t('sidebar.me')} />{:else}{t('sidebar.me')}{/if}
			</div>
			<div>
				<div class="mename">{userName || t('sidebar.thisDevice')}</div>
				<div class="mestatus" class:off={!online}>
					<span class="dot"></span>
					{#if connecting}{t('status.connecting')}{:else if online}{t('status.online')}{:else}{t('status.offline')}{/if}
				</div>
				{#if !online && !connecting}
					<button class="reconnect" onclick={onGoOnline} title={connError}>{t('status.goOnline')}</button>
					{#if connError}<div class="connerr" title={connError}>{t('status.netError')}</div>{/if}
				{/if}
			</div>
		</div>
	</div>
</aside>

<style>
	.sidebar {
		width: 224px;
		flex: none;
		background: var(--surface-2);
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		padding: 14px 12px;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 8px 16px;
	}
	.brand .nm {
		font-family: var(--font-display);
		font-weight: 600;
		font-size: 18px;
		letter-spacing: -0.03em;
	}
	.nav {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.navlink {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 10px 11px;
		border: none;
		border-radius: var(--r-sm);
		cursor: pointer;
		text-align: left;
		font-family: var(--font-sans);
		font-size: 14.5px;
		font-weight: 500;
		color: var(--text-muted);
		background: transparent;
		transition: all var(--dur) var(--ease);
	}
	.navlink:hover {
		background: var(--surface-3);
	}
	.navlink.on {
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
	}
	/* live-connection count pill on "Bağlantılar" (only rendered when ≥1) */
	.navbadge {
		margin-left: auto;
		min-width: 18px;
		height: 18px;
		padding: 0 5px;
		border-radius: var(--r-pill, 999px);
		background: var(--accent);
		color: var(--text-on-accent);
		font-size: 11px;
		font-weight: 700;
		display: grid;
		place-items: center;
	}
	.sidefoot {
		margin-top: auto;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.me {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 4px 6px;
	}
	.meavatar {
		width: 32px;
		height: 32px;
		border-radius: 8px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		font-weight: 700;
		font-size: 11px;
		font-family: var(--font-display);
		/* the account-image variant must stay inside the rounded chip */
		overflow: hidden;
		flex: none;
	}
	.meavatar img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.mename {
		font-size: 13px;
		font-weight: 600;
	}
	.mestatus {
		font-size: 11.5px;
		color: var(--ok);
		display: flex;
		align-items: center;
		gap: 5px;
	}
	.mestatus .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--ok);
	}
	.mestatus.off {
		color: var(--text-faint);
	}
	.mestatus.off .dot {
		background: var(--border-strong);
	}
	.reconnect {
		margin-top: 6px;
		font-size: 11.5px;
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
		border: 1px solid var(--accent-soft-2);
		border-radius: var(--r-sm);
		padding: 4px 9px;
		cursor: pointer;
	}
	.reconnect:hover {
		background: var(--accent-soft-2);
	}
	.connerr {
		margin-top: 5px;
		font-size: 10.5px;
		color: var(--danger);
		max-width: 180px;
		line-height: 1.35;
		word-break: break-word;
	}
</style>
