<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';
	import { update } from '$lib/update.svelte';
	import { installUpdate } from '$lib/updater';

	// Close is allowed in every phase except the terminal restart (the app is about
	// to relaunch anyway); an in-flight download keeps running in the background and
	// the badge stays lit, so reopening shows live progress.
	const busy = $derived(update.phase === 'downloading' || update.phase === 'installing');

	function fmtMB(bytes: number): string {
		return (bytes / (1024 * 1024)).toFixed(1);
	}
</script>

{#if update.open && update.available}
	<div class="umodal" role="dialog" aria-modal="true" aria-label={t('update.available')}>
		<div class="ucard">
			<div class="uhdr">
				<Icon name="download" size={18} />
				<span>{t('update.available')}</span>
				<button class="uclose" aria-label={t('update.close')} onclick={() => (update.open = false)}>
					<Icon name="x" size={14} />
				</button>
			</div>

			<div class="uver mono">
				v{update.from}
				<Icon name="arrowRight" size={14} />
				<strong>v{update.to}</strong>
			</div>

			{#if update.notes}
				<div class="usect">{t('update.notes')}</div>
				<div class="unotes">{update.notes}</div>
			{/if}

			{#if !update.installable}
				<!-- This install can't self-update (flatpak / package manager / raw binary /
				     AppImage without FUSE): the update still SHOWS, but installing is manual. -->
				<div class="uhint">{t('update.noSelfUpdate')}</div>
			{/if}

			{#if update.phase === 'error'}
				<div class="uerr">{t('update.error')}: {update.error}</div>
			{/if}

			{#if update.phase === 'downloading'}
				<div class="uprog">
					<div class="uprog-label">
						<span>{t('update.downloading')}</span>
						<span class="mono">
							{#if update.total > 0}
								{fmtMB(update.received)} / {fmtMB(update.total)} MB ({update.progressPct}%)
							{:else}
								{fmtMB(update.received)} MB
							{/if}
						</span>
					</div>
					<div class="ubar" class:indet={update.total === 0}>
						<div class="ubar-fill" style:width={update.total > 0 ? `${update.progressPct}%` : '100%'}></div>
					</div>
				</div>
			{:else if update.phase === 'installing'}
				<div class="uprog">
					<div class="uprog-label"><span>{t('update.installing')}</span></div>
					<div class="ubar indet"><div class="ubar-fill" style:width="100%"></div></div>
				</div>
			{:else if update.phase === 'restarting'}
				<div class="uprog">
					<div class="uprog-label"><span>{t('update.restarting')}</span></div>
					<div class="ubar indet"><div class="ubar-fill" style:width="100%"></div></div>
				</div>
			{/if}

			<div class="uact">
				<button class="ubtn ghost" disabled={update.phase === 'restarting'} onclick={() => (update.open = false)}>
					{t('update.close')}
				</button>
				<button
					class="ubtn primary"
					disabled={!update.installable || busy || update.phase === 'restarting'}
					onclick={() => void installUpdate()}
				>
					{update.phase === 'error' ? t('update.retry') : t('update.install')}
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.umodal {
		position: absolute;
		inset: 0;
		z-index: 30;
		display: grid;
		place-items: center;
		background: oklch(0.2 0.01 265 / 0.45);
		backdrop-filter: blur(2px);
	}
	.ucard {
		width: 420px;
		max-width: calc(100vw - 48px);
		max-height: calc(100vh - 96px);
		display: flex;
		flex-direction: column;
		background: var(--surface-1);
		border: 1px solid var(--border);
		border-radius: var(--r-md);
		padding: 18px 20px;
		box-shadow: 0 18px 50px oklch(0.1 0.02 265 / 0.35);
	}
	.uhdr {
		display: flex;
		align-items: center;
		gap: 9px;
		font-weight: 700;
		font-size: 15px;
		color: var(--accent);
	}
	.uhdr span {
		color: var(--text);
	}
	.uclose {
		margin-left: auto;
		display: inline-flex;
		border: 0;
		background: none;
		color: var(--text-faint);
		cursor: pointer;
		padding: 4px;
	}
	.uclose:hover {
		color: var(--text);
	}
	.uver {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-top: 12px;
		font-size: 14px;
		color: var(--text-muted);
	}
	.uver strong {
		color: var(--text);
	}
	.usect {
		margin-top: 14px;
		font-size: 11.5px;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text-faint);
	}
	.unotes {
		margin-top: 6px;
		padding: 10px 12px;
		font-size: 12.5px;
		line-height: 1.55;
		white-space: pre-wrap;
		overflow-y: auto;
		max-height: 200px;
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-radius: var(--r-sm);
		color: var(--text-muted);
	}
	.uhint {
		margin-top: 12px;
		padding: 10px 12px;
		font-size: 12.5px;
		line-height: 1.5;
		background: var(--surface-2);
		border: 1px solid var(--border);
		border-left: 3px solid var(--accent);
		border-radius: var(--r-sm);
		color: var(--text-muted);
	}
	.uerr {
		margin-top: 12px;
		font-size: 12.5px;
		color: oklch(0.55 0.2 25);
		word-break: break-word;
	}
	.uprog {
		margin-top: 14px;
	}
	.uprog-label {
		display: flex;
		justify-content: space-between;
		font-size: 12px;
		color: var(--text-muted);
		margin-bottom: 6px;
	}
	.ubar {
		height: 6px;
		border-radius: 3px;
		background: var(--surface-2);
		border: 1px solid var(--border);
		overflow: hidden;
	}
	.ubar-fill {
		height: 100%;
		background: var(--accent);
		border-radius: 3px;
		transition: width 0.25s ease;
	}
	.ubar.indet .ubar-fill {
		animation: indet-slide 1.2s ease-in-out infinite;
	}
	@keyframes indet-slide {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(100%);
		}
	}
	.uact {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
		margin-top: 18px;
	}
	.ubtn {
		padding: 8px 16px;
		font-size: 13px;
		font-weight: 600;
		border-radius: var(--r-sm);
		cursor: pointer;
		border: 1px solid var(--border);
	}
	.ubtn.ghost {
		background: var(--surface-2);
		color: var(--text-muted);
	}
	.ubtn.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: #fff;
	}
	.ubtn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
