<script lang="ts">
	import { ui, saveUi } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';
	import type { Config } from '$lib/types';

	// Unattended access is backed by the core Config (the host honors it). The twofa/record
	// toggles below are UI-only scaffolding (no core backing yet).
	let { config = $bindable(), toggleUnattended }: { config?: Config | null; toggleUnattended?: () => void } = $props();
</script>

<div class="srow">
	<div class="st"><b>{t('settings.unattended')}</b><span>{t('settings.unattendedDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.unattended')} class:on={config?.unattended_access} aria-pressed={config?.unattended_access ?? false} onclick={() => toggleUnattended?.()}><span class="knob"></span></button>
</div>
<div class="srow">
	<div class="st"><b>{t('settings.twofa')}</b><span>{t('settings.twofaDesc')}</span></div>
	<button class="toggle" aria-label={t('settings.twofa')} class:on={ui.twofa} aria-pressed={ui.twofa} onclick={() => { ui.twofa = !ui.twofa; saveUi(); }}><span class="knob"></span></button>
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
</style>
