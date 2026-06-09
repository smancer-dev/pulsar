<script lang="ts">
	import Modal from '$lib/Modal.svelte';
	import { addGame, updateGame, type Game, type GameType } from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = { open?: (g?: Game) => void };
	let { open = $bindable() }: Props = $props();

	const typeLabel = (ty: GameType) => t('type.' + ty);
	const TYPES: GameType[] = ['program', 'command', 'image'];

	let showForm = $state(false);
	let editingId = $state<string | null>(null);
	let form = $state<Omit<Game, 'id'>>(blank());

	function blank(): Omit<Game, 'id'> {
		return { title: '', type: 'program', path: '', args: '', command: '', image: '', cmdStart: '', cmdStop: '' };
	}

	// Exposed to the parent (bound function prop) so the "+" button and per-game
	// edit buttons can open this modal in add/edit mode.
	open = (g?: Game) => {
		if (g) {
			editingId = g.id;
			form = { title: g.title, type: g.type, path: g.path, args: g.args, command: g.command, image: g.image, cmdStart: g.cmdStart, cmdStop: g.cmdStop };
		} else {
			editingId = null;
			form = blank();
		}
		showForm = true;
	};

	function submitForm() {
		if (!form.title.trim()) return;
		if (editingId) updateGame(editingId, form);
		else addGame(form);
		showForm = false;
	}
</script>

{#if showForm}
	<Modal title={editingId ? t('games.editTitle') : t('games.add')} onClose={() => (showForm = false)}>
		<div class="f">
			<span class="fl">{t('games.fTitle')}</span>
			<div class="field"><input bind:value={form.title} placeholder={t('games.fTitlePlaceholder')} aria-label={t('games.fTitle')} /></div>

			<span class="fl">{t('games.fType')}</span>
			<div class="seg">
				{#each TYPES as v (v)}
					<button class:active={form.type === v} onclick={() => (form.type = v)}>{typeLabel(v)}</button>
				{/each}
			</div>

			{#if form.type === 'program'}
				<span class="fl">{t('games.fExePath')}</span>
				<div class="field"><input bind:value={form.path} placeholder={t('games.fExePlaceholder')} aria-label={t('games.fPathAria')} style="font-family:var(--font-mono)" /></div>
				<span class="fl">{t('games.fArgs')}</span>
				<div class="field"><input bind:value={form.args} placeholder="--fullscreen" aria-label={t('games.fArgsAria')} /></div>
			{:else if form.type === 'command'}
				<span class="fl">{t('games.fCommand')}</span>
				<div class="field"><input bind:value={form.command} placeholder="steam steam://rungameid/…" aria-label={t('games.fCommand')} style="font-family:var(--font-mono)" /></div>
			{/if}

			<span class="fl">{t('games.fCover')}</span>
			<div class="field"><input bind:value={form.image} placeholder={t('games.fCoverPlaceholder')} aria-label={t('games.fCover')} /></div>

			<span class="fl">{t('games.fCmdStart')}</span>
			<div class="field"><input bind:value={form.cmdStart} placeholder={t('games.fCmdStartPlaceholder')} aria-label={t('games.fCmdStartAria')} style="font-family:var(--font-mono)" /></div>
			<span class="fl">{t('games.fCmdStop')}</span>
			<div class="field"><input bind:value={form.cmdStop} placeholder={t('games.fCmdStopPlaceholder')} aria-label={t('games.fCmdStopAria')} style="font-family:var(--font-mono)" /></div>

			<div class="factions">
				<button class="btn btn-ghost" onclick={() => (showForm = false)}>{t('games.cancel')}</button>
				<button class="btn btn-primary" disabled={!form.title.trim()} onclick={submitForm}>{t('games.fSave')}</button>
			</div>
		</div>
	</Modal>
{/if}

<style>
	.f {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.fl {
		display: block;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		margin-top: 8px;
	}
	.factions {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
</style>
