<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t, i18n, cycleLang, LANGS } from '$lib/i18n.svelte';

	type Props = {
		title: string;
		dark: boolean;
		onToggleTheme: () => void;
		/** Whether the app is in gaming mode (drives the toggle's active styling). */
		gaming: boolean;
		/** Flip the app-level personality (remote ↔ gaming). */
		onToggleMode: () => void;
	};
	let { title, dark, onToggleTheme, gaming, onToggleMode }: Props = $props();

	// Short code (TR/EN) shown on the language toggle button.
	const langShort = $derived(LANGS.find((l) => l.value === i18n.lang)?.short ?? 'EN');
</script>

<!-- App toolbar (the OS now draws the title bar + window controls + drag/resize). -->
<div class="chrome" data-tauri-drag-region>
	<div class="ctitle">{title}</div>
	<div class="cright">
		<button
			class="game-btn"
			class:on={gaming}
			title={t('chrome.gamingMode')}
			aria-label={t('chrome.gamingToggle')}
			aria-pressed={gaming}
			onclick={onToggleMode}
		>
			<Icon name="gaming" size={16} />
		</button>
		<button
			class="lang-btn"
			title={t('chrome.language')}
			aria-label={t('chrome.languageToggle')}
			onclick={cycleLang}
		>
			<Icon name="globe" size={15} /><span class="lang-code mono">{langShort}</span>
		</button>
		<button
			class="icon-btn"
			title={t('chrome.theme')}
			aria-label={t('chrome.themeToggle')}
			onclick={onToggleTheme}
		>
			<Icon name={dark ? 'sun' : 'monitor'} size={16} />
		</button>
		<span class="mono ver">Pulsar v1.0</span>
	</div>
</div>

<style>
	.chrome {
		height: 44px;
		flex: none;
		display: flex;
		align-items: center;
		padding: 0 14px;
		border-bottom: 1px solid var(--border);
		background: var(--surface);
		user-select: none;
		position: relative;
	}
	.ctitle {
		position: absolute;
		left: 0;
		right: 0;
		text-align: center;
		font-size: 13px;
		font-weight: 600;
		color: var(--text-muted);
		pointer-events: none;
	}
	.cright {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 10px;
		z-index: 1;
	}
	.cright .ver {
		font-size: 11.5px;
		color: var(--text-faint);
	}
	.game-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 30px;
		height: 28px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		transition:
			background var(--dur) var(--ease),
			color var(--dur) var(--ease),
			border-color var(--dur) var(--ease);
	}
	.game-btn:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	/* Active = gaming mode on: cyan accent (the accent family is already cyan via
	   data-gaming, so this reads as the lit-up gaming control). */
	.game-btn.on {
		background: var(--accent-soft);
		border-color: var(--accent);
		color: var(--accent-press);
	}
	.lang-btn {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		height: 28px;
		padding: 0 9px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
		transition:
			background var(--dur) var(--ease),
			color var(--dur) var(--ease);
	}
	.lang-btn:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.lang-code {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.04em;
	}
</style>
