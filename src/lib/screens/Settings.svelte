<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { api } from '$lib/api';
	import type { Config, NetworkMode } from '$lib/types';
	import { t } from '$lib/i18n.svelte';
	import DisplayTab from './Settings/DisplayTab.svelte';
	import NetworkTab from './Settings/NetworkTab.svelte';
	import SecurityTab from './Settings/SecurityTab.svelte';
	import GeneralTab from './Settings/GeneralTab.svelte';

	// Called after the relay/network settings change so the app re-registers.
	let { onReconnect }: { onReconnect?: () => void } = $props();

	let tab = $state<'display' | 'network' | 'security' | 'general'>('display');
	let config = $state<Config | null>(null);
	let saved = $state(false);
	let detected = $state<string[]>([]);

	const tabs = [
		{ id: 'display', icon: 'monitor' },
		{ id: 'network', icon: 'wifi' },
		{ id: 'security', icon: 'shield' },
		{ id: 'general', icon: 'settings' }
	] as const;

	onMount(async () => {
		config = await api.getConfig();
		detected = await api.availableEncoders().catch(() => []);
	});

	// Core config (relay / network mode) — persisted via the Rust core, then
	// re-register so the change takes effect.
	async function saveConfig() {
		if (!config) return;
		await api.setConfig($state.snapshot(config));
		saved = true;
		setTimeout(() => (saved = false), 1200);
		onReconnect?.();
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

<div class="layout">
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
			<NetworkTab bind:config {saved} {saveConfig} {setMode} />
		{:else if tab === 'security'}
			<SecurityTab bind:config {toggleUnattended} />
		{:else}
			<GeneralTab bind:config {saveConfig} {setAvatar} />
		{/if}
	</div>
</div>

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
</style>
