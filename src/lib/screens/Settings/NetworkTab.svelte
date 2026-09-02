<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import type { Config, NetworkMode } from '$lib/types';
	import { api, onNodePort } from '$lib/api';
	import { t } from '$lib/i18n.svelte';

	import { relayAuth } from '$lib/relayAuth.svelte';

	let {
		config = $bindable(),
		saveConfig,
		setMode,
		onReconnect
	}: {
		config: Config | null;
		saveConfig: () => void;
		setMode: (m: NetworkMode) => void;
		onReconnect?: () => void;
	} = $props();

	// v4 relay auth: submit the one-shot 2FA code (held in the shared relayAuth store) by
	// re-registering. The password is saved into config and re-registers via saveConfig's
	// reconnect key; 2FA is never stored.
	function submitTotp() {
		if (relayAuth.totp.trim()) onReconnect?.();
	}

	// When no port is pinned (node_port == 0) the box shows the ACTUAL random port
	// in use as its placeholder. Snapshot at mount; the node-port event keeps it
	// live across go_online rebinds. Falls back to "Random" while unknown/0.
	let livePort = $state(0);
	onMount(() => {
		api.nodePort().then((p) => (livePort = p)).catch(() => {});
		let off: (() => void) | undefined;
		let dead = false;
		onNodePort((p) => (livePort = p)).then((o) => {
			if (dead) o();
			else off = o;
		});
		return () => {
			dead = true;
			off?.();
		};
	});
</script>

<div class="srow">
	<div class="st">
		<b>{t('settings.connMethod')}</b>
		<span>{t('settings.connMethodDesc')}</span>
	</div>
	<div class="seg">
		{#each [['auto', t('settings.modeAuto')], ['p2p-only', t('settings.modeP2p')], ['relay-only', t('settings.modeRelay')]] as [v, l] (v)}
			<button
				class:active={config?.network_mode === v}
				onclick={() => setMode(v as NetworkMode)}>{l}</button
			>
		{/each}
	</div>
</div>
<div class="srow">
	<div class="st">
		<b>{t('settings.relay')}</b>
		<span>{t('settings.relayDesc')}</span>
	</div>
	<div class="field relayfield">
		<Icon name="plug" size={15} />
		{#if config}
			<input
				bind:value={config.relay}
				onchange={saveConfig}
				aria-label={t('settings.relayAria')}
				style="font-family:var(--font-mono);font-size:12.5px"
			/>
		{/if}
	</div>
</div>

<!-- Relay authentication (v4). Only needed for a relay whose operator set a password
     and/or 2FA; leaving these empty is correct for the public / open relay. -->
<div class="srow">
	<div class="st">
		<b>
			{t('settings.relayAuth')}
			{#if relayAuth.e2eRequired}
				<span class="e2ebadge" title={t('settings.relayE2eDesc')}>
					<Icon name="shield" size={12} />
					{t('settings.relayE2e')}
				</span>
			{/if}
		</b>
		<span>{t('settings.relayAuthDesc')}</span>
		{#if relayAuth.failed}
			<span class="authmsg err">{t('relayAuth.failed')}</span>
		{:else if relayAuth.needPassword || relayAuth.needTotp}
			<span class="authmsg warn">{t('relayAuth.required')}</span>
		{/if}
	</div>
	<div class="authcol">
		<div class="field authfield" class:want={relayAuth.needPassword}>
			<Icon name="shield" size={15} />
			{#if config}
				<input
					type="password"
					bind:value={config.relay_password}
					onchange={saveConfig}
					placeholder={t('settings.relayPassword')}
					aria-label={t('settings.relayPassword')}
					autocomplete="off"
					style="font-family:var(--font-mono);font-size:12.5px"
				/>
			{/if}
		</div>
		<div class="field authfield" class:want={relayAuth.needTotp}>
			<Icon name="shield" size={15} />
			<input
				type="text"
				inputmode="numeric"
				maxlength="8"
				bind:value={relayAuth.totp}
				onkeydown={(e) => e.key === 'Enter' && submitTotp()}
				placeholder={t('settings.relayTotp')}
				aria-label={t('settings.relayTotp')}
				autocomplete="off"
				style="font-family:var(--font-mono);font-size:12.5px;letter-spacing:2px"
			/>
			<button class="verify" onclick={submitTotp} disabled={!relayAuth.totp.trim()}>
				{t('settings.relayTotpVerify')}
			</button>
		</div>
	</div>
</div>
<div class="srow">
	<div class="st">
		<b>{t('settings.nodePort')}</b>
		<span>{t('settings.nodePortDesc')}</span>
	</div>
	<div class="field" style="width:150px">
		<Icon name="plug" size={15} />
		{#if config}
			<!-- Unset (0) renders as EMPTY with a "random" placeholder — a literal 0 in
			     the box read like a (nonsense) port. Clearing the field returns to the
			     random-port default; the live port shows on Home's ip:port line. -->
			<input
				type="number"
				min="1"
				max="65535"
				value={config.node_port > 0 ? config.node_port : ''}
				onchange={(e) => {
					if (!config) return;
					const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
					config.node_port = Number.isFinite(v) && v > 0 && v <= 65535 ? v : 0;
					saveConfig();
				}}
				aria-label={t('settings.nodePort')}
				placeholder={livePort > 0 ? String(livePort) : t('settings.portRandom')}
				style="font-family:var(--font-mono);font-size:12.5px;width:90px"
			/>
		{/if}
	</div>
</div>

<style>
	.srow {
		display: flex;
		align-items: center;
		gap: 20px;
		padding: 16px 0;
		border-bottom: 1px solid var(--border);
	}
	.st {
		flex: 1;
	}
	.st b {
		font-size: 14px;
		font-weight: 600;
		display: block;
	}
	.st span {
		font-size: 12.5px;
		color: var(--text-faint);
		margin-top: 3px;
		line-height: 1.45;
		display: block;
		max-width: 46ch;
	}
	.relayfield {
		width: 250px;
	}
	.authcol {
		display: flex;
		flex-direction: column;
		gap: 8px;
		width: 250px;
	}
	.authfield {
		width: 100%;
	}
	/* Highlight the input the relay is actually asking for (from a RELAY_AUTH_REQUIRED reply). */
	.authfield.want {
		border-color: var(--accent);
		box-shadow: 0 0 0 2px color-mix(in oklab, var(--accent) 25%, transparent);
	}
	.verify {
		margin-left: 6px;
		padding: 3px 10px;
		font-size: 12px;
		font-weight: 600;
		border: 1px solid var(--border);
		border-radius: var(--r-sm, 8px);
		background: var(--surface-2, transparent);
		color: var(--text);
		cursor: pointer;
		white-space: nowrap;
	}
	.verify:hover:not(:disabled) {
		border-color: var(--accent);
		color: var(--accent);
	}
	.verify:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}
	.e2ebadge {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		margin-left: 8px;
		padding: 1px 7px;
		font-size: 10.5px;
		font-weight: 700;
		letter-spacing: 0.03em;
		text-transform: uppercase;
		color: var(--accent);
		border: 1px solid color-mix(in oklab, var(--accent) 45%, transparent);
		border-radius: 999px;
		vertical-align: middle;
	}
	.authmsg {
		display: block;
		margin-top: 5px;
		font-size: 12px;
		font-weight: 600;
	}
	.authmsg.warn {
		color: var(--accent);
	}
	.authmsg.err {
		color: oklch(0.62 0.2 25);
	}
</style>
