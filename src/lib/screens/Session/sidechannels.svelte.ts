// Side channels for a remote-desktop session — clipboard, file transfer, two-way chat, and
// microphone — lifted out of Session.svelte. Owns the chat/clipboard/file/mic state shown in
// the menu and routes inbound chat/clipboard events for THIS play back into it. Behaviour is
// identical to the original inline script.
//
// Instantiated at component init so the inbound-events $effect scopes to + tears down with the
// component. `playId`/`menuOpen` are read through getters (the inbound unread-count logic and
// the API calls track them as they did inline).

import { api, copyText, readClipboard, onChatMsg, onDataClip } from '$lib/api';
import { t } from '$lib/i18n.svelte';

type Panel = 'menu' | 'chat';
type ChatMsg = { me: boolean; text: string };

type Inputs = {
	playId: () => number;
	menuOpen: () => boolean;
};

export class SessionSideChannels {
	#in: Inputs;

	// Which body the menu shows (the chat panel is a side channel, so it lives here).
	panel = $state<Panel>('menu');
	note = $state(''); // transient status line under the menu grid
	#noteTimer: ReturnType<typeof setTimeout> | undefined;
	micOn = $state(false);
	messages = $state<ChatMsg[]>([]);
	chatInput = $state('');
	unread = $state(0);
	chatBox = $state<HTMLDivElement | null>(null);
	fileInput: HTMLInputElement | undefined = $state();

	constructor(inputs: Inputs) {
		this.#in = inputs;

		// Inbound side-channel data for THIS play (events carry the play id as `peer`).
		$effect(() => {
			const idStr = String(inputs.playId());
			let offChat: (() => void) | undefined;
			let offClip: (() => void) | undefined;
			onChatMsg((e) => {
				if (e.peer !== idStr) return;
				this.messages = [...this.messages, { me: false, text: e.text }];
				if (this.panel !== 'chat' || !inputs.menuOpen()) this.unread++;
				queueMicrotask(() => this.chatBox?.scrollTo({ top: this.chatBox.scrollHeight }));
			}).then((off) => (offChat = off));
			onDataClip((e) => {
				if (e.peer !== idStr) return;
				copyText(e.text).catch(() => {});
				this.flash(t('session.clipboardRecv'));
			}).then((off) => (offClip = off));
			return () => {
				offChat?.();
				offClip?.();
			};
		});
	}

	flash(msg: string) {
		this.note = msg;
		clearTimeout(this.#noteTimer);
		this.#noteTimer = setTimeout(() => (this.note = ''), 2600);
	}

	// Clipboard → remote.
	sendClipboard = async () => {
		const playId = this.#in.playId();
		if (playId < 0) return;
		let text = '';
		try {
			text = await readClipboard();
		} catch {
			this.flash(t('session.clipboardError'));
			return;
		}
		if (!text) {
			this.flash(t('session.clipboardEmpty'));
			return;
		}
		api.sendClipboard(playId, text).catch(() => {});
		this.flash(t('session.clipboardSent'));
	};

	// File → remote.
	pickFile = () => {
		this.fileInput?.click();
	};
	onFilePicked = async (e: Event) => {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = '';
		const playId = this.#in.playId();
		if (!file || playId < 0) return;
		if (file.size > 50 * 1024 * 1024) {
			this.flash(t('session.fileTooBig'));
			return;
		}
		this.flash(t('session.fileSending', { name: file.name }));
		try {
			const buf = new Uint8Array(await file.arrayBuffer());
			await api.sendFile(playId, file.name, Array.from(buf));
			this.flash(t('session.fileSent', { name: file.name }));
		} catch {
			this.flash(t('session.fileError', { name: file.name }));
		}
	};

	// Microphone → remote.
	toggleMic = () => {
		const playId = this.#in.playId();
		if (playId < 0) return;
		this.micOn = !this.micOn;
		if (this.micOn) api.micStart(playId).catch(() => (this.micOn = false));
		else api.micStop(playId).catch(() => {});
	};

	// Chat (two-way).
	openChat = () => {
		this.panel = 'chat';
		this.unread = 0;
	};
	sendChatLine = () => {
		const text = this.chatInput.trim();
		const playId = this.#in.playId();
		if (!text || playId < 0) return;
		api.sendChat(playId, text).catch(() => {});
		this.messages = [...this.messages, { me: true, text }];
		this.chatInput = '';
		queueMicrotask(() => this.chatBox?.scrollTo({ top: this.chatBox.scrollHeight }));
	};
}
