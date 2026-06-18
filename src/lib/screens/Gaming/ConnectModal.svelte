<script lang="ts">
	// Gaming-mode connect pop-up: the ID box + on-screen numpad (type with a controller or
	// the keyboard, Enter/Bağlan to continue). It ONLY collects the host id — the fetched
	// games are then shown on a separate in-stage screen (GamesScreen), not in this pop-up.
	// Fully pad-navigable: the modal root carries `data-navmodal` so GamepadNav confines
	// focus to it, and it pushes its close handler so B / Esc dismiss the pop-up.
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/Icon.svelte';
	import Numpad from './Numpad.svelte';
	import { t } from '$lib/i18n.svelte';
	import { canConnectTarget, fmtTarget } from '$lib/connectTarget';
	import type { GamepadNav } from '$lib/gamepadNav.svelte';

	type Props = {
		nav: GamepadNav;
		/** Continue with the entered host id → the shell opens the games screen. */
		onPick: (id: string) => void;
		onClose: () => void;
	};
	let { nav, onPick, onClose }: Props = $props();
	const navItem = nav.item;

	let target = $state('');
	let inputEl = $state<HTMLInputElement>();
	const canConnect = $derived(canConnectTarget(target));

	function setTarget(v: string) {
		target = fmtTarget(v);
	}
	function go() {
		if (!canConnect) return;
		const id = fmtTarget(target);
		onClose();
		onPick(id);
	}

	onMount(() => {
		nav.pushBack(onClose);
		// Focus the ID box next frame (after items register) so the keyboard can type
		// immediately and controller focus is confined to the modal.
		requestAnimationFrame(() => {
			if (inputEl) nav.focus(inputEl);
			else nav.focusFirst();
		});
	});
	onDestroy(() => {
		nav.popBack(onClose);
		nav.focusFirst(); // return focus to the shell
	});
</script>

<div
	class="backdrop"
	role="presentation"
	onclick={(e) => {
		if (e.target === e.currentTarget) onClose();
	}}
>
	<div class="modal" data-navmodal role="dialog" aria-modal="true" aria-label={t('gaming.title')}>
		<button class="close" use:navItem onclick={onClose} aria-label={t('gaming.close')}>
			<Icon name="x" size={16} />
		</button>

		<div class="mhead">{t('gaming.title')}</div>
		<div class="idfield">
			<Icon name="connect" size={20} />
			<input
				bind:this={inputEl}
				use:navItem
				value={target}
				oninput={(e) => setTarget(e.currentTarget.value)}
				onkeydown={(e) => e.key === 'Enter' && go()}
				placeholder="000 000 000"
				aria-label={t('home.targetAria')}
				inputmode="numeric"
			/>
		</div>
		<div class="hint">{t('home.idOrIp')}</div>

		<Numpad value={target} setValue={setTarget} {navItem} />

		<button class="btn btn-primary go" use:navItem disabled={!canConnect} onclick={go}>
			<Icon name="gaming" size={18} />
			{t('gaming.connect')}
		</button>
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: grid;
		place-items: center;
		background: color-mix(in oklch, var(--bg) 40%, transparent);
		backdrop-filter: blur(8px);
		padding: 24px;
	}
	.modal {
		position: relative;
		width: 100%;
		max-width: 380px;
		max-height: 90vh;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		padding: 26px 24px 22px;
		background: var(--surface);
		border: 1px solid var(--border-strong);
		border-radius: var(--r-xl);
		box-shadow: var(--shadow-lg), var(--shadow-accent);
	}
	.close {
		position: absolute;
		top: 12px;
		right: 12px;
		width: 30px;
		height: 30px;
		display: grid;
		place-items: center;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--surface-2);
		color: var(--text-muted);
		cursor: pointer;
	}
	.close:hover {
		background: var(--surface-3);
		color: var(--text);
	}
	.mhead {
		font-family: var(--font-display);
		font-size: 19px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}
	.idfield {
		display: flex;
		align-items: center;
		gap: 12px;
		width: 100%;
		padding: 13px 16px;
		background: var(--surface-2);
		border: 1px solid var(--border-strong);
		border-radius: var(--r-lg);
		color: var(--accent);
	}
	.idfield input {
		flex: 1;
		min-width: 0;
		border: none;
		background: transparent;
		outline: none;
		color: var(--text);
		font-family: var(--font-mono);
		font-size: 26px;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-align: center;
	}
	.hint {
		font-size: 12px;
		color: var(--text-faint);
		text-align: center;
	}
	.go {
		justify-content: center;
		width: 100%;
		padding: 13px;
		font-size: 16px;
	}
</style>
