<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import type { Config, NetworkMode } from '$lib/types';
	import { t } from '$lib/i18n.svelte';
	import { saveTick, configTick } from '$lib/settings.svelte';
	import { navContainer } from '$lib/gamepadNav.svelte';
	import DisplayTab from './Settings/DisplayTab.svelte';
	import NetworkTab from './Settings/NetworkTab.svelte';
	import SecurityTab from './Settings/SecurityTab.svelte';
	import GeneralTab from './Settings/GeneralTab.svelte';

	// `onReconnect` re-registers after relay/network changes. `padNav` = make every control
	// here controller-navigable (set when Settings is shown inside the gaming-mode shell).
	let { onReconnect, padNav = false }: { onReconnect?: () => void; padNav?: boolean } = $props();

	let tab = $state<'display' | 'network' | 'security' | 'general'>('display');
	let config = $state<Config | null>(null);
	let saveErr = $state('');
	let detected = $state<string[]>([]);

	// Centered "saved" toast, shared by both save paths. Rapid saves restart the
	// fade timer instead of stacking toasts (debounce), so a burst of changes
	// shows one steady toast that fades 3s after the LAST save.
	let toast = $state(false);
	let toastTimer: ReturnType<typeof setTimeout> | undefined;
	function notifySaved() {
		toast = true;
		clearTimeout(toastTimer);
		toastTimer = setTimeout(() => (toast = false), 3000);
	}
	// The tabs call saveUi() directly (it bumps saveTick) — react here so UI-only
	// saves get the same toast. Skip the initial mount value.
	let lastTick = saveTick.n;
	$effect(() => {
		if (saveTick.n !== lastTick) {
			lastTick = saveTick.n;
			notifySaved();
		}
	});

	const tabs = [
		{ id: 'display', icon: 'monitor' },
		{ id: 'network', icon: 'wifi' },
		{ id: 'security', icon: 'shield' },
		{ id: 'general', icon: 'settings' }
	] as const;

	// Only these fields affect the relay registration — anything else (audio,
	// unattended, avatar…) saves without tearing the node down and re-registering.
	const reconnectKey = (c: Config) => `${c.relay}|${c.network_mode}|${c.node_port}|${c.device_name}`;
	let lastReconnectKey = '';

	onMount(async () => {
		config = await api.getConfig();
		lastReconnectKey = reconnectKey(config);
		detected = await api.availableEncoders().catch(() => []);
	});

	// Core config (relay / network mode) — persisted via the Rust core, then
	// re-register so the change takes effect.
	async function saveConfig() {
		if (!config) return;
		// A cleared number input binds null (and >65535 overflows the core's u16) —
		// clamp before the snapshot crosses to Rust, or set_config rejects EVERY
		// subsequent save (they all send the whole snapshot).
		config.node_port = Math.min(65535, Math.max(0, Math.round(Number(config.node_port)) || 0));
		saveErr = '';
		try {
			await api.setConfig($state.snapshot(config));
		} catch (e) {
			saveErr = e instanceof Error ? e.message : String(e);
			return;
		}
		notifySaved();
		// Bump the shared tick for CORE saves too: the shell (+page) re-fetches its
		// own config copy off this, so screens reading shell config (the Home
		// unattended-access warning / blanked one-time password) reflect a Settings
		// change immediately instead of only after the next go-online.
		saveTick.n += 1;
		lastTick = saveTick.n; // our own bump — don't double-toast via the effect
		configTick.n += 1; // shell re-fetches its config copy off this (core saves only)
		const key = reconnectKey(config);
		if (key !== lastReconnectKey) {
			lastReconnectKey = key;
			onReconnect?.();
		}
	}
	function setMode(m: NetworkMode) {
		if (config) {
			config.network_mode = m;
			saveConfig();
		}
	}
	// Audio toggles live on the core Config (the host applies them per session).
	function toggleTransmit() {
		if (config) {
			config.transmit_audio = !config.transmit_audio;
			saveConfig();
		}
	}
	function toggleMute() {
		if (config) {
			config.mute_host_audio = !config.mute_host_audio;
			saveConfig();
		}
	}
	// How this device presents itself to peers (shown in their connect prompt).
	function setAvatar(mode: string) {
		if (config) {
			config.avatar_mode = mode;
			saveConfig();
		}
	}
	function toggleNative() {
		if (config) {
			config.native_player = !config.native_player;
			saveConfig();
		}
	}
	// Unattended access lives on the core Config (the host skips the one-time-password
	// auth when it's on).
	function toggleUnattended() {
		if (config) {
			config.unattended_access = !config.unattended_access;
			saveConfig();
		}
	}
</script>

<div class="head"><h1>{t('settings.title')}</h1><p class="sub">{t('settings.sub')}</p></div>

<div class="layout" use:navContainer={padNav}>
	<div class="rail">
		{#each tabs as tb (tb.id)}
			<button class="tab" class:on={tab === tb.id} onclick={() => (tab = tb.id)}>
				<Icon name={tb.icon} size={17} />{t('settings.tab.' + tb.id)}
			</button>
		{/each}
	</div>

	<div class="card">
		{#if tab === 'display'}
			<DisplayTab
				bind:config
				{detected}
				{saveConfig}
				{toggleNative}
				{toggleTransmit}
				{toggleMute}
			/>
		{:else if tab === 'network'}
			<NetworkTab bind:config {saveConfig} {setMode} />
		{:else if tab === 'security'}
			<SecurityTab bind:config {toggleUnattended} {saveConfig} />
		{:else}
			<GeneralTab bind:config {saveConfig} {setAvatar} />
		{/if}
		{#if saveErr}
			<div class="saveerr" role="alert">{t('settings.saveFailed')} · {saveErr}</div>
		{/if}
	</div>
</div>

{#if toast}
	<div class="toast" role="status" aria-live="polite">
		<span class="tick"><Icon name="check" size={13} /></span>{t('settings.savedToast')}
	</div>
{/if}

<style>
	.head {
		margin-bottom: 28px;
	}
	h1 {
		font-size: 27px;
		letter-spacing: -0.03em;
	}
	.sub {
		color: var(--text-muted);
		font-size: 14.5px;
		margin: 7px 0 0;
	}
	.layout {
		display: grid;
		grid-template-columns: 186px 1fr;
		gap: 24px;
		align-items: start;
	}
	.rail {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.tab {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 12px;
		border: none;
		border-radius: var(--r-sm);
		cursor: pointer;
		text-align: left;
		font-family: var(--font-sans);
		font-size: 14px;
		font-weight: 500;
		color: var(--text-muted);
		background: transparent;
	}
	.tab.on {
		font-weight: 600;
		color: var(--accent-press);
		background: var(--accent-soft);
	}
	.saveerr {
		font-size: 12.5px;
		color: var(--danger);
		padding-top: 12px;
		word-break: break-word;
	}
	/* Bottom-center semi-transparent confirmation pill (green check + text);
	 * never intercepts clicks. The 3s lifetime is owned by the JS timer — this
	 * only plays the visual fade. */
	.toast {
		position: fixed;
		left: 0;
		right: 0;
		bottom: 36px;
		margin: 0 auto;
		width: max-content;
		z-index: 50;
		pointer-events: none;
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 11px 20px;
		border-radius: 999px;
		background: color-mix(in oklch, var(--text) 80%, transparent);
		color: var(--surface);
		font-size: 14px;
		font-weight: 600;
		box-shadow: var(--shadow-lg);
		animation: toast-fade 3s ease forwards;
	}
	.tick {
		width: 19px;
		height: 19px;
		border-radius: 50%;
		background: var(--ok);
		color: #fff;
		display: grid;
		place-items: center;
		flex: none;
	}
	@keyframes toast-fade {
		0% {
			opacity: 0;
			transform: translateY(6px) scale(0.98);
		}
		8%,
		70% {
			opacity: 1;
			transform: translateY(0) scale(1);
		}
		100% {
			opacity: 0;
			transform: translateY(0) scale(1);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.toast {
			animation: none;
		}
	}
</style>
