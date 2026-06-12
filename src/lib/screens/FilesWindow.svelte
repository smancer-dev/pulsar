<script lang="ts">
	// Standalone per-session file-manager window (opened by Rust as a separate OS
	// window with `window.__FILES__={id,peer}` set). One window per remote-play
	// session — the header names the peer so multiple sessions stay tellable apart;
	// Rust force-closes it when that session ends (play/hold.rs).
	import Icon from '$lib/Icon.svelte';
	import Files from './Session/Files.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = { playId: number; peer: string };
	let { playId, peer }: Props = $props();
</script>

<div class="fwin">
	<header class="fhead">
		<span class="fmark"><Icon name="file" size={15} /></span>
		<div>
			<div class="fpeer">{peer}</div>
			<div class="fsub">{t('files.windowTitle')}</div>
		</div>
	</header>
	<div class="fbody">
		<Files {playId} />
	</div>
</div>

<style>
	.fwin {
		position: fixed;
		inset: 0;
		display: flex;
		flex-direction: column;
		background: oklch(0.17 0.012 265);
		color: oklch(0.92 0.008 265);
		font-family: var(--font-body);
	}
	.fhead {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px;
		border-bottom: 1px solid oklch(0.3 0.014 265 / 0.6);
		flex: none;
	}
	.fmark {
		display: grid;
		place-items: center;
		width: 30px;
		height: 30px;
		border-radius: 8px;
		background: oklch(0.3 0.05 272 / 0.55);
		color: oklch(0.8 0.08 272);
		flex: none;
	}
	.fpeer {
		font-size: 14px;
		font-weight: 700;
		font-family: var(--font-display);
	}
	.fsub {
		font-size: 11px;
		color: oklch(0.66 0.02 265);
		margin-top: 1px;
	}
	.fbody {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		padding: 12px 16px 16px;
	}
</style>
