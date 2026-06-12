<script lang="ts">
	import { ui, saveUi } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';
	import type { Config } from '$lib/types';

	// Unattended access + the custom connect password are backed by the core Config
	// (the host honors both). The twofa/record toggles below are UI-only scaffolding
	// (no core backing yet) — and 2FA is mutually exclusive with unattended access
	// (an unattended host skips auth entirely, so a second factor cannot apply).
	let {
		config = $bindable(),
		toggleUnattended,
		saveConfig
	}: {
		config?: Config | null;
		toggleUnattended?: () => void;
		saveConfig?: () => void;
	} = $props();
</script>

<div class="srow">
	<div class="st"><b>{t('settings.unattended')}</b><span>{t('settings.unattendedDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.unattended')} class:on={config?.unattended_access} aria-pressed={config?.unattended_access ?? false} onclick={() => toggleUnattended?.()}><span class="knob"></span></button>
</div>
<div class="srow" class:dim={config?.unattended_access}>
	<div class="st">
		<b>{t('settings.twofa')}</b><span>{t('settings.twofaDesc')}</span>
		{#if config?.unattended_access}
			<span class="blocked">{t('settings.twofaBlocked')}</span>
		{/if}
	</div>
	<button
		class="toggle"
		aria-label={t('settings.twofa')}
		class:on={ui.twofa && !config?.unattended_access}
		aria-pressed={ui.twofa && !config?.unattended_access}
		disabled={config?.unattended_access}
		onclick={() => { ui.twofa = !ui.twofa; saveUi(); }}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.connectPw')}</b><span>{t('settings.connectPwDesc')}</span></div>
	<div class="field" style="width:220px">
		{#if config}
			<input
				type="password"
				value={config.connect_password ?? ''}
				placeholder={t('settings.connectPwPlaceholder')}
				onchange={(e) => { if (config) { config.connect_password = (e.currentTarget as HTMLInputElement).value.trim(); saveConfig?.(); } }}
				aria-label={t('settings.connectPw')}
			/>
		{/if}
	</div>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.record')}</b><span>{t('settings.recordDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.record')} class:on={ui.record} aria-pressed={ui.record} onclick={() => { ui.record = !ui.record; saveUi(); }}><span class="knob"></span></button>
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
	/* 2FA row while unattended access blocks it: dimmed + an explanatory note. */
	.srow.dim .st b,
	.srow.dim .st span:not(.blocked) {
		opacity: 0.55;
	}
	.blocked {
		color: var(--warn, #b8860b) !important;
		font-size: 12px !important;
	}
	.toggle:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}
	.field {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--surface);
	}
	.field input {
		border: none;
		outline: none;
		background: transparent;
		color: var(--text);
		font-size: 13px;
		width: 100%;
	}
</style>
