<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { t } from '$lib/i18n.svelte';

	// Two-way chat panel inside the session menu. The parent owns the message list + send
	// logic (it also needs chatBox to auto-scroll on inbound messages), so chatInput + chatBox
	// are $bindable and sending/back are callbacks.
	type ChatMsg = { me: boolean; text: string };
	type Props = {
		messages: ChatMsg[];
		chatInput: string;
		chatBox: HTMLDivElement | null;
		onSend: () => void;
		onBack: () => void;
	};
	let {
		messages,
		chatInput = $bindable(),
		chatBox = $bindable(),
		onSend,
		onBack
	}: Props = $props();
</script>

<div class="chat">
	<button class="chat-back" onclick={onBack}>
		<Icon name="arrowRight" size={14} class="flip" />{t('session.back')}
	</button>
	<div class="chat-log" bind:this={chatBox}>
		{#if messages.length === 0}
			<div class="chat-empty">{t('session.chatEmpty')}</div>
		{:else}
			{#each messages as m, i (i)}
				<div class="bubble" class:me={m.me}>
					<span class="who">{m.me ? t('session.chatYou') : t('session.chatPeer')}</span>
					<span class="txt">{m.text}</span>
				</div>
			{/each}
		{/if}
	</div>
	<div class="chat-input">
		<input
			bind:value={chatInput}
			placeholder={t('session.chatPlaceholder')}
			onkeydown={(e) => e.key === 'Enter' && onSend()}
			aria-label={t('session.chat')}
		/>
		<button class="chat-send" onclick={onSend} aria-label={t('session.send')}>
			<Icon name="arrowRight" size={16} />
		</button>
	</div>
</div>

<style>
	/* chat panel */
	.chat {
		display: flex;
		flex-direction: column;
		height: 280px;
	}
	.chat-back {
		align-self: flex-start;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		border: none;
		background: transparent;
		color: oklch(0.7 0.02 265);
		font-size: 12px;
		cursor: pointer;
		padding: 0 0 8px;
	}
	.chat-back :global(.flip) {
		transform: rotate(180deg);
	}
	.chat-log {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding-right: 2px;
	}
	.chat-empty {
		margin: auto;
		font-size: 12px;
		color: oklch(0.6 0.02 265);
		text-align: center;
		line-height: 1.5;
		padding: 0 12px;
	}
	.bubble {
		display: flex;
		flex-direction: column;
		gap: 2px;
		max-width: 82%;
		padding: 7px 10px;
		border-radius: 12px;
		background: oklch(0.26 0.014 265 / 0.9);
		align-self: flex-start;
	}
	.bubble.me {
		align-self: flex-end;
		background: color-mix(in oklch, var(--accent) 60%, oklch(0.3 0.02 272));
		color: #fff;
	}
	.bubble .who {
		font-size: 9.5px;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		opacity: 0.7;
	}
	.bubble .txt {
		font-size: 13px;
		line-height: 1.35;
		word-break: break-word;
	}
	.chat-input {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}
	.chat-input input {
		flex: 1;
		min-width: 0;
		padding: 9px 11px;
		border-radius: var(--r-sm);
		border: 1px solid oklch(0.36 0.016 265);
		background: oklch(0.22 0.013 265);
		color: #fff;
		font-family: var(--font-sans);
		font-size: 13px;
	}
	.chat-input input:focus {
		outline: none;
		border-color: var(--accent);
	}
	.chat-send {
		flex: none;
		width: 38px;
		display: grid;
		place-items: center;
		border: none;
		border-radius: var(--r-sm);
		background: var(--accent);
		color: #fff;
		cursor: pointer;
	}
	.chat-send:hover {
		background: var(--accent-press);
	}
</style>
