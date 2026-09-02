<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import { relayAuth } from '$lib/relayAuth.svelte';

	// The relay-authentication prompt. It appears only when a relay actually demands a
	// credential (go_online replied RELAY_AUTH_REQUIRED) — there is no permanent password
	// field anywhere in Settings, because the answer is never stored: the relay issues a
	// durable access key in return and THAT is what is persisted. So this dialog is a
	// once-per-device event, not part of the normal connect flow.
	let { onSubmit }: { onSubmit: (password: string, totp: string) => void } = $props();

	let password = $state('');
	let totp = $state('');

	// Enough typed to be worth sending: every factor the relay asked for is filled in.
	const ready = $derived(
		(!relayAuth.needPassword || password.trim().length > 0) &&
			(!relayAuth.needTotp || totp.trim().length > 0)
	);

	function submit() {
		if (!ready || relayAuth.busy) return;
		relayAuth.busy = true;
		onSubmit(password.trim(), totp.trim());
		// The credential is one-shot: drop it from memory as soon as it's handed over.
		password = '';
		totp = '';
	}

	function onKey(e: KeyboardEvent) {
		if (e.key === 'Enter') submit();
		else if (e.key === 'Escape' && !relayAuth.busy) relayAuth.open = false;
	}
</script>

{#if relayAuth.open}
	<div class="rmodal" role="dialog" aria-modal="true" aria-label={t('relayAuth.title')}>
		<div class="rcard">
			<div class="rhdr">
				<Icon name="shield" size={18} />
				<span>{t('relayAuth.title')}</span>
				<button
					class="rclose"
					aria-label={t('relayAuth.cancel')}
					disabled={relayAuth.busy}
					onclick={() => (relayAuth.open = false)}
				>
					<Icon name="x" size={14} />
				</button>
			</div>

			<div class="rrelay mono">{relayAuth.relay}</div>
			<p class="rdesc">{t('relayAuth.desc')}</p>

			{#if relayAuth.failed}
				<div class="rerr">{t('relayAuth.failed')}</div>
			{/if}

			{#if relayAuth.needPassword}
				<label class="rlabel" for="relay-auth-pw">{t('relayAuth.password')}</label>
				<input
					id="relay-auth-pw"
					class="rinput"
					type="password"
					autocomplete="off"
					bind:value={password}
					onkeydown={onKey}
					disabled={relayAuth.busy}
				/>
			{/if}

			{#if relayAuth.needTotp}
				<label class="rlabel" for="relay-auth-totp">{t('relayAuth.totp')}</label>
				<input
					id="relay-auth-totp"
					class="rinput mono totp"
					type="text"
					inputmode="numeric"
					autocomplete="one-time-code"
					maxlength="8"
					bind:value={totp}
					onkeydown={onKey}
					disabled={relayAuth.busy}
					placeholder="000000"
				/>
				<span class="rhint">{t('relayAuth.totpHint')}</span>
			{/if}

			<div class="rnote">
				<Icon name="shield" size={13} />
				<span>{t('relayAuth.keyNote')}</span>
			</div>

			<div class="ract">
				<button class="rbtn ghost" disabled={relayAuth.busy} onclick={() => (relayAuth.open = false)}>
					{t('relayAuth.cancel')}
				</button>
				<button class="rbtn primary" disabled={!ready || relayAuth.busy} onclick={submit}>
					{relayAuth.busy ? t('relayAuth.connecting') : t('relayAuth.connect')}
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.rmodal {
		position: absolute;
		inset: 0;
		z-index: 40;
		display: grid;
		place-items: center;
		background: oklch(0.2 0.01 265 / 0.45);
		backdrop-filter: blur(2px);
	}
	.rcard {
		width: 400px;
		max-width: calc(100vw - 48px);
		display: flex;
		flex-direction: column;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r-md);
		padding: 18px 20px;
		box-shadow: 0 18px 50px oklch(0.1 0.02 265 / 0.35);
	}
	.rhdr {
		display: flex;
		align-items: center;
		gap: 9px;
		font-weight: 700;
		font-size: 15px;
		color: var(--accent);
	}
	.rhdr span {
		color: var(--text);
	}
	.rclose {
		margin-left: auto;
		display: inline-flex;
		border: 0;
		background: none;
		color: var(--text-faint);
		cursor: pointer;
		padding: 4px;
	}
	.rclose:hover:not(:disabled) {
		color: var(--text);
	}
	.rrelay {
		margin-top: 12px;
		font-size: 13px;
		color: var(--text);
		font-family: var(--font-mono);
	}
	.rdesc {
		margin: 6px 0 0;
		font-size: 12.5px;
		line-height: 1.5;
		color: var(--text-muted);
	}
	.rerr {
		margin-top: 12px;
		padding: 8px 10px;
		font-size: 12.5px;
		border-radius: var(--r-sm);
		background: oklch(0.55 0.2 25 / 0.12);
		border: 1px solid oklch(0.55 0.2 25 / 0.35);
		color: oklch(0.55 0.2 25);
	}
	.rlabel {
		display: block;
		margin: 14px 0 6px;
		font-size: 11.5px;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.rinput {
		width: 100%;
		padding: 9px 11px;
		font: inherit;
		font-size: 13px;
		color: var(--text);
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
	}
	.rinput:focus {
		outline: none;
		border-color: var(--accent);
	}
	.rinput.totp {
		font-family: var(--font-mono);
		letter-spacing: 0.28em;
		font-size: 16px;
		text-align: center;
	}
	.rhint {
		display: block;
		margin-top: 6px;
		font-size: 11.5px;
		color: var(--text-faint);
	}
	.rnote {
		display: flex;
		align-items: flex-start;
		gap: 7px;
		margin-top: 16px;
		padding: 9px 11px;
		font-size: 12px;
		line-height: 1.45;
		color: var(--text-muted);
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-left: 3px solid var(--accent);
		border-radius: var(--r-sm);
	}
	.ract {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
	.rbtn {
		padding: 8px 16px;
		font-size: 13px;
		font-weight: 600;
		border-radius: var(--r-sm);
		cursor: pointer;
		border: 1px solid var(--border);
	}
	.rbtn.ghost {
		background: var(--surface-2);
		color: var(--text-muted);
	}
	.rbtn.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: #fff;
	}
	.rbtn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
