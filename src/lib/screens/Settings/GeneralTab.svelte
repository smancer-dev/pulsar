<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import type { Config } from '$lib/types';
	import { ui, saveUi } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';
	import { api, isTauri } from '$lib/api';
	import pkg from '../../../../package.json';

	let {
		config = $bindable(),
		saveConfig,
		setAvatar
	}: {
		config: Config | null;
		saveConfig: () => void;
		setAvatar: (mode: string) => void;
	} = $props();

	// The shipped version comes from the Tauri shell (tauri.conf / Cargo.toml);
	// the browser mock falls back to package.json's.
	let version = $state(pkg.version);
	if (isTauri) {
		import('@tauri-apps/api/app')
			.then((m) => m.getVersion())
			.then((v) => (version = v))
			.catch(() => {});
	}

	// Display name defaults: an unset name falls back to the OS user's name on the
	// wire (host.rs/play.rs already do this), so the input mirrors that — the OS
	// name is the PLACEHOLDER, and a value equal to it (or the legacy "Pulsar
	// Cihazı" default) renders as empty instead of looking like a custom choice.
	let userName = $state('');
	api.deviceUserName().then((n) => (userName = n)).catch(() => {});
	const shownName = $derived.by(() => {
		const v = config?.device_name ?? '';
		return v === userName || v === 'Pulsar Cihazı' ? '' : v;
	});
	function onNameChange(e: Event) {
		if (!config) return;
		config.device_name = (e.currentTarget as HTMLInputElement).value.trim();
		saveConfig();
	}
</script>

<div class="srow">
	<div class="st"><b>{t('settings.displayName')}</b><span>{t('settings.displayNameDesc')}</span></div>
	<div class="field" style="width:220px">
		<Icon name="devices" size={15} />
		{#if config}
			<input
				value={shownName}
				placeholder={userName}
				onchange={onNameChange}
				aria-label={t('settings.displayName')}
			/>
		{/if}
	</div>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.avatar')}</b><span>{t('settings.avatarDesc')}</span></div>
	<div class="seg">
		{#each [['user', t('settings.avatarUser')], ['wallpaper', t('settings.avatarWall')], ['anonymous', t('settings.avatarAnon')]] as [v, l] (v)}
			<button class:active={config?.avatar_mode === v} onclick={() => setAvatar(v)}>{l}</button>
		{/each}
	</div>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.tray')}</b><span>{t('settings.trayDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.tray')} class:on={ui.tray} aria-pressed={ui.tray} onclick={() => { ui.tray = !ui.tray; saveUi(); api.setTray(ui.tray).catch(() => {}); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.debug')}</b><span>{t('settings.debugDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.debug')} class:on={ui.debug} aria-pressed={ui.debug} onclick={() => { ui.debug = !ui.debug; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.version')}</b><span>{t('settings.versionDesc')}</span></div>
	<span class="mono ver">Pulsar v{version}</span>
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
	.ver {
		font-size: 13px;
		color: var(--text-muted);
	}
</style>
