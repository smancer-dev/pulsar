<script lang="ts">
	import Modal from '$lib/Modal.svelte';
	import { addGame, updateGame, type Game, type GameType } from '$lib/games.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = { open?: (g?: Game) => void };
	let { open = $bindable() }: Props = $props();

	const typeLabel = (ty: GameType) => t('type.' + ty);
	const TYPES: GameType[] = ['program', 'command'];

	let showForm = $state(false);
	let editingId = $state<string | null>(null);
	let form = $state<Omit<Game, 'id'>>(blank());
	let coverInput = $state<HTMLInputElement>();

	// Cover image: either a URL typed into the field, or a local file read as a data URL.
	function onCoverFile(e: Event) {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		const reader = new FileReader();
		reader.onload = () => (form.image = String(reader.result));
		reader.readAsDataURL(f);
	}

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
			<div class="cover-row">
				<div class="field cover-url"><input bind:value={form.image} placeholder={t('games.fCoverPlaceholder')} aria-label={t('games.fCover')} /></div>
				<button type="button" class="btn btn-ghost upload" onclick={() => coverInput?.click()}>{t('games.fCoverUpload')}</button>
				<input bind:this={coverInput} type="file" accept="image/*" class="hidden-file" onchange={onCoverFile} />
			</div>
			{#if form.image}
				<div class="cover-prev"><img src={form.image} alt="" /><button type="button" class="clear" onclick={() => (form.image = '')} aria-label={t('games.fCoverClear')}>×</button></div>
			{/if}

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
	.cover-row {
		display: flex;
		gap: 8px;
		align-items: stretch;
	}
	.cover-url {
		flex: 1;
		min-width: 0;
	}
	.upload {
		flex: none;
		white-space: nowrap;
	}
	.hidden-file {
		display: none;
	}
	.cover-prev {
		position: relative;
		margin-top: 8px;
		width: 160px;
		height: 90px;
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		overflow: hidden;
		background: var(--surface-2);
	}
	.cover-prev img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.cover-prev .clear {
		position: absolute;
		top: 4px;
		right: 4px;
		width: 20px;
		height: 20px;
		border: none;
		border-radius: 50%;
		background: oklch(0.2 0.02 265 / 0.6);
		color: #fff;
		font-size: 14px;
		line-height: 1;
		cursor: pointer;
		display: grid;
		place-items: center;
	}
</style>
