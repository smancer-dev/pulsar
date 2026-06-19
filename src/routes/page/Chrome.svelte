<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import { isTauri } from '$lib/api';
	import { t, i18n, cycleLang, LANGS } from '$lib/i18n.svelte';
	import { gamingNav } from '$lib/gamepadNav.svelte';

	type Props = {
		title: string;
		dark: boolean;
		onToggleTheme: () => void;
		/** Whether the app is in gaming mode (drives the toggle's active styling). */
		gaming: boolean;
		/** Flip the app-level personality (remote ↔ gaming). */
		onToggleMode: () => void;
		/** Current split layout ('off' = normal single-tab flow) — lights up the button. */
		splitMode?: 'off' | 'h2' | 'v2' | 'grid4';
		/** Open the split-layout chooser (SplitPicker). */
		onSplit?: () => void;
		/** Window fullscreen state — lights up the fullscreen toggle. */
		fullscreen?: boolean;
		/** Toggle window fullscreen (plain — keeps this bar on the home; immersive is F11). */
		onToggleFullscreen?: () => void;
	};
	let {
		title,
		dark,
		onToggleTheme,
		gaming,
		onToggleMode,
		splitMode = 'off',
		onSplit,
		fullscreen = false,
		onToggleFullscreen
	}: Props = $props();

	// Short code (TR/EN) shown on the language toggle button.
	const langShort = $derived(LANGS.find((l) => l.value === i18n.lang)?.short ?? 'EN');

	// Real app version (from tauri.conf) instead of a hardcoded string. Empty outside Tauri.
	let ver = $state('');
	// The window is FRAMELESS (decorations:false) — this bar IS the title bar, so it draws its
	// own minimize / maximize-restore / close controls. `maximized` swaps the max↔restore glyph.
	let maximized = $state(false);
	let unlistenResize: (() => void) | undefined;
	// Sync onMount returning a sync cleanup; the async setup runs in an IIFE (an async onMount's
	// return value is NOT used as a cleanup by Svelte, which is what tripped the type check).
	onMount(() => {
		if (!isTauri) return;
		(async () => {
			try {
				const { getVersion } = await import('@tauri-apps/api/app');
				ver = await getVersion();
			} catch {
				/* keep empty → shows plain "Pulsar" */
			}
			try {
				const { getCurrentWindow } = await import('@tauri-apps/api/window');
				const w = getCurrentWindow();
				maximized = await w.isMaximized();
				unlistenResize = await w.onResized(async () => {
					maximized = await w.isMaximized();
				});
			} catch {
				/* not in Tauri / API unavailable */
			}
		})();
		return () => unlistenResize?.();
	});

	// Frameless window controls — minimize / toggle-maximize / close via the Tauri window API.
	async function winCtl(action: 'min' | 'max' | 'close') {
		if (!isTauri) return;
		try {
			const { getCurrentWindow } = await import('@tauri-apps/api/window');
			const w = getCurrentWindow();
			if (action === 'min') await w.minimize();
			else if (action === 'max') await w.toggleMaximize();
			else await w.close();
		} catch {
			/* ignore */
		}
	}

	// Register a top-bar button with the gaming nav ONLY while in gaming mode, so the
	// controller can roam up to it (gaming/language/theme). Reactive: re-applies when the
	// mode toggles. In remote mode the nav isn't running, so it stays a plain button.
	function navItem(node: HTMLElement, on: boolean) {
		let off: (() => void) | undefined;
		const apply = (g: boolean) => {
			off?.();
			off = undefined;
			if (g) off = gamingNav.item(node).destroy;
		};
		apply(on);
		return { update: apply, destroy: () => off?.() };
	}
</script>

<!-- App toolbar (the OS now draws the title bar + window controls + drag/resize). -->
<div class="chrome" data-tauri-drag-region>
	<div class="ctitle">{title}</div>
	<div class="cright">
		{#if onSplit}
			<button
				class="game-btn"
				class:on={splitMode !== 'off'}
				use:navItem={gaming}
				title={t('chrome.split')}
				aria-label={t('chrome.splitToggle')}
				aria-pressed={splitMode !== 'off'}
				onclick={onSplit}
			>
				<Icon name="split" size={16} />
			</button>
		{/if}
		{#if onToggleFullscreen}
			<button
				class="game-btn"
				class:on={fullscreen}
				use:navItem={gaming}
				title={t('gaming.fullscreen')}
				aria-label={t('gaming.fullscreen')}
				aria-pressed={fullscreen}
				onclick={onToggleFullscreen}
			>
				<Icon name="expand" size={16} />
			</button>
		{/if}
		<button
			class="game-btn"
			class:on={gaming}
			use:navItem={gaming}
			title={t('chrome.gamingMode')}
			aria-label={t('chrome.gamingToggle')}
			aria-pressed={gaming}
			onclick={onToggleMode}
		>
			<Icon name="gaming" size={16} />
		</button>
		<button
			class="lang-btn"
			use:navItem={gaming}
			title={t('chrome.language')}
			aria-label={t('chrome.languageToggle')}
			onclick={cycleLang}
		>
			<Icon name="globe" size={15} /><span class="lang-code mono">{langShort}</span>
		</button>
		<button
			class="icon-btn"
			use:navItem={gaming}
			title={t('chrome.theme')}
			aria-label={t('chrome.themeToggle')}
			onclick={onToggleTheme}
		>
			<Icon name={dark ? 'sun' : 'monitor'} size={16} />
		</button>
		<span class="mono ver">{ver ? `Pulsar v${ver}` : 'Pulsar'}</span>
		{#if isTauri}
			<div class="winctl">
				<button class="wc" onclick={() => winCtl('min')} title={t('chrome.minimize')} aria-label={t('chrome.minimize')}>
					<Icon name="winmin" size={15} />
				</button>
				<button class="wc" onclick={() => winCtl('max')} title={t('chrome.maximize')} aria-label={t('chrome.maximize')}>
					<Icon name={maximized ? 'winrestore' : 'winmax'} size={14} />
				</button>
				<button class="wc close" onclick={() => winCtl('close')} title={t('chrome.close')} aria-label={t('chrome.close')}>
					<Icon name="x" size={15} />
				</button>
			</div>
		{/if}
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
	/* Frameless window controls: flush to the top-right corner, full bar height, no border —
	   the standard custom-title-bar look. `margin-right` cancels the bar's right padding so they
	   reach the window edge; close gets the red hover. Not a drag region (they're buttons). */
	.winctl {
		display: flex;
		align-items: stretch;
		height: 44px;
		margin-left: 6px;
		margin-right: -14px;
	}
	.wc {
		width: 44px;
		border: none;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		display: grid;
		place-items: center;
		transition:
			background var(--dur) var(--ease),
			color var(--dur) var(--ease);
	}
	.wc:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.wc.close:hover {
		background: #e81123;
		color: #fff;
	}
</style>
