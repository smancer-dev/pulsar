<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	type Props = {
		pwInput: string;
		pwError: string;
		pwChecking: boolean;
		onSubmit: () => void;
		onCancel: () => void;
	};
	let {
		pwInput = $bindable(),
		pwError,
		pwChecking,
		onSubmit,
		onCancel
	}: Props = $props();
</script>

<div class="pwmodal">
	<div class="pwcard">
		<div class="pwhdr"><Icon name="shield" size={18} /><span>{t('pw.title')}</span></div>
		<!-- eslint-disable-next-line svelte/no-at-html-tags -->
		<p class="pwlead">{@html t('pw.lead')}</p>
		{#if pwError}<div class="pwerr">{pwError}</div>{/if}
		<!-- svelte-ignore a11y_autofocus -->
		<input
			class="pwfield mono"
			type="text"
			bind:value={pwInput}
			disabled={pwChecking}
			onkeydown={(e) => e.key === 'Enter' && onSubmit()}
			placeholder={t('pw.placeholder')}
			aria-label={t('pw.aria')}
			autofocus
		/>
		<div class="pwact">
			<button class="pwbtn ghost" onclick={onCancel}>{t('pw.cancel')}</button>
			<button class="pwbtn primary" disabled={pwChecking} onclick={onSubmit}>
				{pwChecking ? t('pw.checking') : t('pw.submit')}
			</button>
		</div>
	</div>
</div>

<style>
	.pwmodal {
		position: absolute;
		inset: 0;
		z-index: 20;
		display: grid;
		place-items: center;
		background: oklch(0.2 0.01 265 / 0.45);
		backdrop-filter: blur(3px);
	}
	.pwcard {
		width: 340px;
		max-width: calc(100% - 40px);
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--r);
		box-shadow: var(--shadow-lg);
		padding: 20px;
	}
	.pwhdr {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: 16px;
		font-weight: 700;
		color: var(--accent-press);
	}
	.pwlead {
		margin: 9px 0 14px;
		font-size: 13px;
		color: var(--text-muted);
		line-height: 1.5;
	}
	.pwerr {
		margin-bottom: 10px;
		font-size: 12.5px;
		color: var(--danger);
		font-weight: 600;
	}
	.pwfield {
		width: 100%;
		box-sizing: border-box;
		padding: 11px 12px;
		font-size: 17px;
		letter-spacing: 0.04em;
		border: 1px solid var(--border-strong);
		border-radius: var(--r-sm);
		background: var(--surface-2);
		color: var(--text);
	}
	.pwfield:focus {
		outline: none;
		border-color: var(--accent);
	}
	.pwact {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 10px;
		margin-top: 16px;
	}
	.pwbtn {
		padding: 10px 0;
		border-radius: var(--r-sm);
		font-weight: 600;
		font-size: 14px;
		cursor: pointer;
		border: 1px solid var(--border);
	}
	.pwbtn.ghost {
		background: var(--surface-2);
		color: var(--text);
	}
	.pwbtn.ghost:hover {
		background: var(--surface-3);
	}
	.pwbtn.primary {
		background: var(--accent);
		color: #fff;
		border-color: transparent;
	}
	.pwbtn.primary:hover {
		background: var(--accent-press);
	}
</style>
