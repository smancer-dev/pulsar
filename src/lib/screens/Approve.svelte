<script lang="ts">
	import PulsarMark from '$lib/PulsarMark.svelte';
	import Icon from '$lib/Icon.svelte';
	import { api, windowControl } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	type Props = { id: number; peer: string; pw: string };
	let { id, peer, pw }: Props = $props();

	let busy = $state(false);

	// Auto-deny countdown: an unanswered popup (unattended host, user away) must not
	// pile up forever — after 30 s it denies itself, exactly like clicking Reddet.
	let secsLeft = $state(30);
	$effect(() => {
		const tmr = setInterval(() => {
			if (busy) return; // a decision is already in flight
			secsLeft -= 1;
			if (secsLeft <= 0) decide(false);
		}, 1000);
		return () => clearInterval(tmr);
	});

	// Only space-group a 9-digit relay id ("482913056" → "482 913 056"). A direct
	// (relay-less) connect's peer is an address (e.g. "192.168.1.5:9000") — show it
	// as-is instead of mangling its digit runs.
	const grouped = $derived(
		/^\d{9}$/.test(peer.replace(/\s/g, ''))
			? peer.replace(/\s/g, '').replace(/(\d{3})(?=\d)/g, '$1 ').trim()
			: peer
	);
	const pwLabel = $derived(
		pw === 'ok'
			? { text: t('approve.pwOk'), cls: 'ok', icon: 'check' }
			: pw === 'bad'
				? { text: t('approve.pwBad'), cls: 'bad', icon: 'shield' }
				: { text: t('approve.pwNone'), cls: 'none', icon: 'shield' }
	);

	async function decide(allow: boolean, viewOnly = false) {
		busy = true;
		try {
			await api.respondRequest(id, allow, viewOnly);
		} catch {
			/* ignore */
		}
		windowControl('close');
	}
</script>

<div class="approve" data-tauri-drag-region>
	<div class="brand"><PulsarMark size={22} /><span>Pulsar</span></div>
	<div class="title">{t('approve.title')}</div>
	<p class="lead">{t('approve.lead')}</p>

	<div class="who">
		<div class="avatar"><Icon name="monitor" size={20} /></div>
		<div>
			<div class="lab">{t('approve.deviceId')}</div>
			<div class="id mono">{grouped}</div>
		</div>
	</div>

	<div class="pw {pwLabel.cls}">
		<Icon name={pwLabel.icon} size={15} />
		<span>{pwLabel.text}</span>
	</div>

	<div class="actions">
		<button class="btn deny" disabled={busy} onclick={() => decide(false)}>{t('approve.deny')} ({secsLeft})</button>
		<button class="btn allow" disabled={busy} onclick={() => decide(true)}>{t('approve.allow')}</button>
	</div>
	<!-- Secondary, deliberately de-emphasized: grants the session but as view-only (no
	     control). Faint + borderless so it never competes with the primary Allow. -->
	<button class="viewonly" disabled={busy} onclick={() => decide(true, true)}>
		{t('approve.allowViewOnly')}
	</button>
</div>

<style>
	.approve {
		height: 100vh;
		display: flex;
		flex-direction: column;
		padding: 18px 20px;
		background: var(--surface);
		color: var(--text);
		box-sizing: border-box;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-display);
		font-weight: 600;
		font-size: 15px;
	}
	.title {
		margin-top: 12px;
		font-size: 18px;
		font-weight: 700;
		letter-spacing: -0.02em;
	}
	.lead {
		margin: 4px 0 14px;
		font-size: 13px;
		color: var(--text-muted);
	}
	.who {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 11px 12px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
	}
	.avatar {
		width: 38px;
		height: 38px;
		border-radius: 9px;
		background: var(--accent-soft);
		color: var(--accent);
		display: grid;
		place-items: center;
		flex: none;
	}
	.lab {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-faint);
	}
	.id {
		font-size: 17px;
		font-weight: 500;
		letter-spacing: 0.04em;
	}
	.pw {
		display: flex;
		align-items: center;
		gap: 7px;
		margin-top: 12px;
		font-size: 12.5px;
		font-weight: 600;
	}
	.pw.ok {
		color: var(--ok);
	}
	.pw.bad {
		color: var(--danger);
	}
	.pw.none {
		color: var(--text-faint);
	}
	.actions {
		margin-top: auto;
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 10px;
		padding-top: 16px;
	}
	.btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 11px 0;
		border-radius: var(--r-sm);
		font-weight: 600;
		font-size: 14px;
		cursor: pointer;
		border: 1px solid var(--border);
	}
	.btn:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.deny {
		background: var(--surface-2);
		color: var(--text);
	}
	.deny:hover:not(:disabled) {
		background: var(--surface-3);
	}
	.allow {
		background: var(--accent);
		color: #fff;
		border-color: transparent;
	}
	.allow:hover:not(:disabled) {
		background: var(--accent-press);
	}
	/* Tertiary "view-only" action: faint, borderless, centered — discoverable but
	   visually subordinate so the operator's eye lands on Deny/Allow first. */
	.viewonly {
		margin-top: 10px;
		align-self: center;
		background: transparent;
		border: 0;
		color: var(--text-faint);
		font-size: 12px;
		font-weight: 500;
		cursor: pointer;
		padding: 6px 10px;
		border-radius: var(--r-sm);
	}
	.viewonly:hover:not(:disabled) {
		color: var(--text-muted);
		text-decoration: underline;
	}
	.viewonly:disabled {
		opacity: 0.5;
		cursor: default;
	}
</style>
