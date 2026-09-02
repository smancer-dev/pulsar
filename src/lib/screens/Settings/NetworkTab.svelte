<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import type { Config, NetworkMode } from '$lib/types';
	import { api, onNodePort } from '$lib/api';
	import { t } from '$lib/i18n.svelte';


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


	// The port actually in use, shown in the box when none is pinned.
	//
	// It only ever moves FORWARD to a real port: `go_online` publishes a 0 while it tears
	// the old node down (so Home doesn't offer a copyable ip:port that no longer answers),
	// and honouring that 0 here made the field flash "Random" and snap back on every
	// connect. Ignoring it keeps the current port on screen through the rebind — and since
	// the node re-binds the same port (AppState::sticky_port), the value simply never
	// changes. "Random" is left for the cold start, before any port is known.
	let livePort = $state(0);
	onMount(() => {
		api
			.nodePort()
			.then((p) => {
				if (p > 0) livePort = p;
			})
			.catch(() => {});
		let off: (() => void) | undefined;
		let dead = false;
		onNodePort((p) => {
			if (p > 0) livePort = p;
		}).then((o) => {
			if (dead) o();
			else off = o;
		});
		return () => {
			dead = true;
			off?.();
		};
	});

	// --- In-app relay ------------------------------------------------------------
	// Turning this on runs a relay inside Pulsar and registers against it. There is no
	// address to enter — a local relay is local by definition — so the relay field below
	// is disabled while it's on, and `go_online` points at loopback.
	let relayLocal = $state({ running: false, port: 0, lanAddr: '' });
	let relayLocalErr = $state('');
	onMount(() => {
		api.localRelayStatus()
			.then((s) => (relayLocal = s))
			.catch(() => {});
	});
	async function toggleLocalRelay() {
		if (!config) return;
		const next = !config.use_local_relay;
		relayLocalErr = '';
		try {
			relayLocal = await api.setLocalRelay(next);
			config.use_local_relay = next;
			// Registering has to move to (or away from) the local relay.
			onReconnect?.();
		} catch (e) {
			relayLocalErr = String(e);
		}
	}
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
<!-- Run the relay in this app. On = no address to type; the app registers against
     itself and other devices on the LAN can point at the shown address. -->
<div class="srow">
	<div class="st">
		<b>{t('settings.localRelay')}</b>
		<span>
			{t('settings.localRelayDesc')}
			{#if config?.use_local_relay && relayLocal.running && relayLocal.lanAddr}
				<span class="lanaddr mono">{relayLocal.lanAddr}</span>
			{/if}
		</span>
		{#if relayLocalErr}
			<span class="lrerr">{relayLocalErr}</span>
		{/if}
	</div>
	<button
		class="toggle"
		aria-label={t('settings.localRelay')}
		class:on={config?.use_local_relay}
		aria-pressed={config?.use_local_relay ?? false}
		onclick={toggleLocalRelay}><span class="knob"></span></button
	>
</div>
<div class="srow" class:dimmed={config?.use_local_relay}>
	<div class="st">
		<b>{t('settings.relay')}</b>
		<span>
			{config?.use_local_relay ? t('settings.relayLocalNote') : t('settings.relayDesc')}
		</span>
	</div>
	<div class="field relayfield">
		<Icon name="plug" size={15} />
		{#if config}
			<!-- No port needed: a bare host uses the default relay port. One is only typed
			     when the operator actually runs on a different one. -->
			<input
				bind:value={config.relay}
				onchange={saveConfig}
				disabled={config.use_local_relay}
				placeholder="relay.pulsardesk.com"
				aria-label={t('settings.relayAria')}
				style="font-family:var(--font-mono);font-size:12.5px"
			/>
		{/if}
	</div>
</div>

<!-- Node UDP port -->
<div class="srow">
	<div class="st">
		<b>{t('settings.nodePort')}</b>
		<span>{t('settings.nodePortDesc')}</span>
	</div>
	<div class="field" style="width:150px">
		<Icon name="plug" size={15} />
		{#if config}
			<!-- Unset (0) renders EMPTY, with the port actually in use as the placeholder —
			     a literal 0 in the box read like a (nonsense) port. Clearing the field
			     returns to the random-port default. The placeholder never falls back to
			     "Random" once a port is known (see `livePort`), so pressing go-online
			     doesn't make the box flicker. -->
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
	.dimmed {
		opacity: 0.45;
	}
	.lanaddr {
		font-family: var(--font-mono);
		color: var(--text);
	}
	.lrerr {
		display: block;
		margin-top: 4px;
		font-size: 12px;
		color: oklch(0.55 0.2 25);
	}
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
	/* Highlight the input the relay is actually asking for (from a RELAY_AUTH_REQUIRED reply). */
	/* The generated SVG has no intrinsic size cap — pin it to a scannable square. */
</style>
