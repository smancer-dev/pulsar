<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import { startH264Canvas } from '$lib/h264';
	import { api, copyText, readClipboard, onChatMsg, onDataClip } from '$lib/api';
	import { evdevCode } from '$lib/keymap';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		playId: number;
		target: Target;
		mode: 'remote' | 'game';
		conn: 'direct' | 'relay';
		wsPort?: number;
		local?: boolean;
		fullscreen?: boolean;
		onToggleFullscreen: () => void;
		onEnd?: () => void;
	};
	let {
		playId,
		target,
		mode,
		conn,
		wsPort = 0,
		local = false,
		fullscreen = false,
		onToggleFullscreen,
		onEnd = () => {}
	}: Props = $props();

	const connLabel = $derived(conn === 'relay' ? 'Relay' : 'P2P');

	let canvas: HTMLCanvasElement;
	let hasVideo = $state(false);
	let videoErr = $state('');
	let fps = $state(0);
	let frames = 0;
	// Once video has started, "stalled" means frames stopped arriving — e.g. the
	// host revoked/closed the screen share — so we surface an error to the user.
	let stalled = $state(false);
	let staleSecs = 0;

	$effect(() => {
		if (!wsPort || !canvas) return;
		hasVideo = false;
		videoErr = '';
		stalled = false;
		staleSecs = 0;
		const sink = startH264Canvas(
			canvas,
			(m) => (videoErr = m),
			() => {
				hasVideo = true;
				frames++;
			}
		);
		let ws: WebSocket | null = null;
		try {
			ws = new WebSocket(`ws://127.0.0.1:${wsPort}`);
			ws.binaryType = 'arraybuffer';
			ws.onmessage = (ev) => sink.push(new Uint8Array(ev.data as ArrayBuffer));
			ws.onerror = () => (videoErr = t('session.videoErr'));
		} catch (e) {
			videoErr = String(e);
		}
		return () => {
			ws?.close();
			sink.close();
		};
	});

	$effect(() => {
		const timer = setInterval(() => {
			const count = frames;
			frames = 0;
			fps = count;
			// Detect a dead stream: no frames for ~3s after video had started.
			if (hasVideo) {
				if (count === 0) {
					staleSecs++;
					if (staleSecs >= 3) stalled = true;
				} else {
					staleSecs = 0;
					stalled = false;
				}
			}
		}, 1000);
		return () => clearInterval(timer);
	});

	// Control via absolute positioning (VNC-style) — NOT pointer lock, which froze
	// the webview (and fed back on the same machine). Click the screen to start
	// controlling; the remote cursor follows yours over the canvas. Esc stops.
	let controlling = $state(false);
	// Can't usefully control a host on the SAME machine (the injected cursor fights
	// yours) — disabled, with a hint to use a second device.
	const controllable = $derived(wsPort > 0 && !local);

	let moveDirty = false;
	let nx = 0;
	let ny = 0;

	function norm(e: PointerEvent) {
		const r = canvas.getBoundingClientRect();
		nx = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
		ny = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height));
	}
	function releaseButtons() {
		if (playId < 0) return;
		api.inputButton(playId, 0, false).catch(() => {});
		api.inputButton(playId, 1, false).catch(() => {});
		api.inputButton(playId, 2, false).catch(() => {});
	}
	function startControl() {
		if (controllable && !controlling) {
			controlling = true;
			canvas.focus();
		}
	}
	function stopControl() {
		if (controlling) {
			controlling = false;
			releaseButtons();
		}
	}
	const hostButton = (b: number) => (b === 2 ? 1 : b === 1 ? 2 : 0);

	function onMove(e: PointerEvent) {
		if (!controlling) return;
		norm(e);
		moveDirty = true;
	}
	function onDown(e: PointerEvent) {
		if (!controlling) {
			startControl();
			return; // the focusing click isn't forwarded
		}
		e.preventDefault();
		norm(e);
		moveDirty = true;
		api.inputButton(playId, hostButton(e.button), true).catch(() => {});
	}
	function onUp(e: PointerEvent) {
		if (!controlling) return;
		e.preventDefault();
		api.inputButton(playId, hostButton(e.button), false).catch(() => {});
	}
	let lastScroll = 0;
	function onWheel(e: WheelEvent) {
		if (!controlling) return;
		e.preventDefault();
		const now = performance.now();
		if (now - lastScroll < 30) return;
		lastScroll = now;
		api.inputScroll(playId, e.deltaX, e.deltaY).catch(() => {});
	}
	function onKey(e: KeyboardEvent, down: boolean) {
		if (!controlling) return;
		if (e.code === 'Escape') {
			stopControl();
			return;
		}
		const code = evdevCode(e.code);
		if (!code) return;
		e.preventDefault();
		api.inputKey(playId, code, down).catch(() => {});
	}

	// While controlling: a rAF pump sends the latest pointer position at ≈refresh
	// rate (never per raw event), plus window-level keyboard capture.
	$effect(() => {
		if (!controlling || typeof window === 'undefined') return;
		let raf = requestAnimationFrame(function pump() {
			if (moveDirty) {
				moveDirty = false;
				api.inputPointer(playId, nx, ny).catch(() => {});
			}
			raf = requestAnimationFrame(pump);
		});
		const kd = (e: KeyboardEvent) => onKey(e, true);
		const ku = (e: KeyboardEvent) => onKey(e, false);
		window.addEventListener('keydown', kd, true);
		window.addEventListener('keyup', ku, true);
		return () => {
			cancelAnimationFrame(raf);
			window.removeEventListener('keydown', kd, true);
			window.removeEventListener('keyup', ku, true);
		};
	});

	const netClass = $derived(!hasVideo ? 'bad' : fps >= 24 ? 'ok' : fps >= 12 ? 'mid' : 'bad');

	// Parsec-style floating control menu — a static handle (always in the same
	// spot) that expands to all session actions. Opening it drops out of control
	// mode so the pointer is free to use the menu.
	let menuOpen = $state(false);
	function toggleMenu() {
		menuOpen = !menuOpen;
		if (menuOpen) stopControl();
	}
	function closeMenu() {
		menuOpen = false;
		panel = 'menu';
	}
	function endSession() {
		closeMenu();
		onEnd();
	}
	function doFullscreen() {
		closeMenu();
		onToggleFullscreen();
	}

	// --- side channels: clipboard, file transfer, chat, microphone ---
	type Panel = 'menu' | 'chat';
	let panel = $state<Panel>('menu');
	let note = $state(''); // transient status line under the menu grid
	let noteTimer: ReturnType<typeof setTimeout> | undefined;
	function flash(msg: string) {
		note = msg;
		clearTimeout(noteTimer);
		noteTimer = setTimeout(() => (note = ''), 2600);
	}

	// Clipboard → remote.
	async function sendClipboard() {
		if (playId < 0) return;
		let text = '';
		try {
			text = await readClipboard();
		} catch {
			flash(t('session.clipboardError'));
			return;
		}
		if (!text) {
			flash(t('session.clipboardEmpty'));
			return;
		}
		api.sendClipboard(playId, text).catch(() => {});
		flash(t('session.clipboardSent'));
	}

	// File → remote.
	let fileInput: HTMLInputElement;
	function pickFile() {
		fileInput?.click();
	}
	async function onFilePicked(e: Event) {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = '';
		if (!file || playId < 0) return;
		if (file.size > 50 * 1024 * 1024) {
			flash(t('session.fileTooBig'));
			return;
		}
		flash(t('session.fileSending', { name: file.name }));
		try {
			const buf = new Uint8Array(await file.arrayBuffer());
			await api.sendFile(playId, file.name, Array.from(buf));
			flash(t('session.fileSent', { name: file.name }));
		} catch {
			flash(t('session.fileSent', { name: file.name }));
		}
	}

	// Microphone → remote.
	let micOn = $state(false);
	function toggleMic() {
		if (playId < 0) return;
		micOn = !micOn;
		if (micOn) api.micStart(playId).catch(() => (micOn = false));
		else api.micStop(playId).catch(() => {});
	}

	// Chat (two-way).
	type ChatMsg = { me: boolean; text: string };
	let messages = $state<ChatMsg[]>([]);
	let chatInput = $state('');
	let unread = $state(0);
	let chatBox = $state<HTMLDivElement | null>(null);
	function openChat() {
		panel = 'chat';
		unread = 0;
	}
	function sendChatLine() {
		const text = chatInput.trim();
		if (!text || playId < 0) return;
		api.sendChat(playId, text).catch(() => {});
		messages = [...messages, { me: true, text }];
		chatInput = '';
		queueMicrotask(() => chatBox?.scrollTo({ top: chatBox.scrollHeight }));
	}

	// Inbound side-channel data for THIS play (events carry the play id as `peer`).
	$effect(() => {
		const idStr = String(playId);
		let offChat: (() => void) | undefined;
		let offClip: (() => void) | undefined;
		onChatMsg((e) => {
			if (e.peer !== idStr) return;
			messages = [...messages, { me: false, text: e.text }];
			if (panel !== 'chat' || !menuOpen) unread++;
			queueMicrotask(() => chatBox?.scrollTo({ top: chatBox.scrollHeight }));
		}).then((off) => (offChat = off));
		onDataClip((e) => {
			if (e.peer !== idStr) return;
			copyText(e.text).catch(() => {});
			flash(t('session.clipboardRecv'));
		}).then((off) => (offClip = off));
		return () => {
			offChat?.();
			offClip?.();
		};
	});

	// Close the menu on Escape (when it isn't being used to leave control mode).
	$effect(() => {
		if (!menuOpen || typeof window === 'undefined') return;
		const onEsc = (e: KeyboardEvent) => {
			if (e.key === 'Escape') {
				e.stopPropagation();
				closeMenu();
			}
		};
		window.addEventListener('keydown', onEsc, true);
		return () => window.removeEventListener('keydown', onEsc, true);
	});
</script>

<div class="screen">
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<canvas
		bind:this={canvas}
		class="video"
		class:on={hasVideo}
		class:control={controlling}
		tabindex="0"
		onpointerdown={onDown}
		onpointermove={onMove}
		onpointerup={onUp}
		onwheel={onWheel}
		onpointerleave={() => (moveDirty = false)}
		onblur={stopControl}
		oncontextmenu={(e) => e.preventDefault()}
	></canvas>

	{#if !hasVideo}
		<div class="ghost">
			<Icon name={mode === 'game' ? 'gaming' : 'monitor'} size={46} />
			<div class="gname">{target.name}</div>
			<div class="gid mono">{target.id}</div>
			{#if videoErr}
				<div class="note err">{videoErr}</div>
			{:else}
				<div class="note">{t('session.waiting')}</div>
			{/if}
		</div>
	{:else if stalled}
		<div class="stall">
			<Icon name="shield" size={34} />
			<div class="stallmsg">{t('session.streamStopped')}</div>
		</div>
	{:else if !controllable}
		<div class="focushint">{t('session.controlOffSame')}</div>
	{:else if !controlling}
		<button class="focushint" onpointerdown={startControl}>{t('session.clickToControl')}</button>
	{:else}
		<div class="focushint locked">{t('session.controllingPre')}<kbd>Esc</kbd>{t('session.controllingSuf')}</div>
	{/if}

	<!-- Floating control handle (always top-center) + expandable menu -->
	{#if menuOpen}
		<button class="scrim" aria-label={t('session.menu')} onclick={closeMenu}></button>
	{/if}
	<div class="dock" class:open={menuOpen}>
		<button
			class="handle"
			class:active={menuOpen}
			onclick={toggleMenu}
			title={t('session.menu')}
			aria-label={t('session.menu')}
			aria-expanded={menuOpen}
		>
			<Icon name="grip" size={15} />
			<span class="net-dot {netClass}"></span>
			<span class="hfps mono">{fps} fps</span>
		</button>

		{#if menuOpen}
			<div class="menu" role="menu">
				<div class="m-head">
					<div class="m-name">{target.name}</div>
					<div class="m-sub mono">
						<span class="net-dot {netClass}"></span>{target.id} · {connLabel} · {fps} fps
					</div>
				</div>

				{#if panel === 'chat'}
					<div class="chat">
						<button class="chat-back" onclick={() => (panel = 'menu')}>
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
								onkeydown={(e) => e.key === 'Enter' && sendChatLine()}
								aria-label={t('session.chat')}
							/>
							<button class="chat-send" onclick={sendChatLine} aria-label={t('session.send')}>
								<Icon name="arrowRight" size={16} />
							</button>
						</div>
					</div>
				{:else}
					<div class="m-grid">
						<button class="m-item" role="menuitem" onclick={doFullscreen}>
							<Icon name="expand" size={18} />
							<span>{fullscreen ? t('session.exitFullscreen') : t('session.fullscreen')}</span>
						</button>
						<button class="m-item" role="menuitem" onclick={sendClipboard}>
							<Icon name="clipboard" size={18} />
							<span>{t('session.clipboard')}</span>
						</button>
						<button class="m-item" role="menuitem" onclick={pickFile}>
							<Icon name="file" size={18} />
							<span>{t('session.files')}</span>
						</button>
						<button class="m-item" class:active={micOn} role="menuitem" onclick={toggleMic}>
							<Icon name="mic" size={18} />
							<span>{micOn ? t('session.micOn') : t('session.mic')}</span>
						</button>
						<button class="m-item wide" role="menuitem" onclick={openChat}>
							<Icon name="chat" size={18} />
							<span>{t('session.chat')}</span>
							{#if unread > 0}<span class="badge">{unread}</span>{/if}
						</button>
					</div>
					{#if note}<div class="m-note">{note}</div>{/if}
				{/if}

				<button class="m-end" role="menuitem" onclick={endSession}>
					<Icon name="power" size={16} />{t('session.end')}
				</button>
			</div>
		{/if}
	</div>

	<!-- hidden picker for "send file" -->
	<input class="filepick" type="file" bind:this={fileInput} onchange={onFilePicked} />
</div>

<style>
	.screen {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		background:
			radial-gradient(700px 380px at 50% 30%, oklch(0.3 0.06 272 / 0.3), transparent 70%),
			#0c0d12;
		overflow: hidden;
	}
	.video {
		max-width: 100%;
		max-height: 100%;
		display: none;
		background: #000;
	}
	.video.on {
		display: block;
	}
	.video:focus {
		outline: none;
	}
	.video.control {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
	}
	.ghost {
		position: absolute;
		text-align: center;
		color: oklch(0.6 0.02 265);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
	}
	.gname {
		font-family: var(--font-display);
		font-size: 20px;
		color: oklch(0.82 0.02 265);
		margin-top: 6px;
	}
	.gid {
		font-size: 12px;
	}
	.note {
		max-width: 360px;
		margin-top: 14px;
		font-size: 12px;
		line-height: 1.5;
		color: oklch(0.62 0.02 265);
	}
	.note.err {
		color: var(--danger);
	}
	.stall {
		position: absolute;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 12px;
		max-width: 340px;
		padding: 22px 26px;
		text-align: center;
		color: #ffd9d4;
		background: oklch(0.18 0.03 25 / 0.86);
		border: 1px solid color-mix(in oklch, var(--danger) 55%, transparent);
		border-radius: var(--r-lg);
		box-shadow: var(--shadow-lg);
		backdrop-filter: blur(6px);
		z-index: 3;
	}
	.stallmsg {
		font-size: 13.5px;
		line-height: 1.5;
		font-weight: 500;
	}
	.focushint {
		position: absolute;
		bottom: 18px;
		left: 50%;
		transform: translateX(-50%);
		font-size: 12.5px;
		color: oklch(0.95 0.008 265);
		background: oklch(0.2 0.012 265 / 0.92);
		border: 1px solid oklch(0.4 0.016 265);
		padding: 8px 14px;
		border-radius: var(--r-pill);
		cursor: pointer;
		z-index: 2;
	}
	.focushint.locked {
		cursor: default;
	}
	.focushint kbd {
		font-family: var(--font-mono);
		background: oklch(0.3 0.015 265);
		padding: 1px 6px;
		border-radius: 4px;
	}
	/* click-away scrim behind the open menu */
	.scrim {
		position: absolute;
		inset: 0;
		border: none;
		background: transparent;
		padding: 0;
		margin: 0;
		cursor: default;
		z-index: 6;
	}
	/* floating dock — always anchored top-center, like Parsec's pull-down handle */
	.dock {
		position: absolute;
		top: 0;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		align-items: center;
		z-index: 7;
	}
	.handle {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		padding: 5px 12px;
		border: 1px solid oklch(0.42 0.016 265 / 0.6);
		border-top: none;
		border-radius: 0 0 var(--r-pill) var(--r-pill);
		background: oklch(0.18 0.012 265 / 0.78);
		color: oklch(0.96 0.008 265);
		cursor: pointer;
		opacity: 0.55;
		transition:
			opacity var(--dur) var(--ease),
			background var(--dur) var(--ease);
		backdrop-filter: blur(6px);
	}
	.handle:hover,
	.handle.active {
		opacity: 1;
		background: oklch(0.24 0.014 265 / 0.92);
	}
	.hfps {
		font-size: 11px;
	}
	.net-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex: none;
		display: inline-block;
	}
	.net-dot.ok {
		background: var(--ok);
	}
	.net-dot.mid {
		background: #f4bf4f;
	}
	.net-dot.bad {
		background: var(--danger);
	}
	.menu {
		margin-top: 6px;
		width: 268px;
		padding: 12px;
		border-radius: var(--r-lg);
		background: oklch(0.17 0.012 265 / 0.96);
		border: 1px solid oklch(0.36 0.016 265 / 0.7);
		box-shadow: var(--shadow-lg);
		color: oklch(0.96 0.008 265);
		backdrop-filter: blur(10px);
	}
	.m-head {
		padding: 2px 4px 12px;
		border-bottom: 1px solid oklch(0.32 0.016 265 / 0.6);
		margin-bottom: 10px;
	}
	.m-name {
		font-family: var(--font-display);
		font-size: 15px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.m-sub {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 11px;
		color: oklch(0.72 0.02 265);
		margin-top: 4px;
	}
	.m-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 6px;
	}
	.m-item {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		padding: 12px 6px;
		border: 1px solid oklch(0.3 0.014 265 / 0.6);
		border-radius: var(--r-sm);
		background: oklch(0.22 0.013 265 / 0.6);
		color: oklch(0.94 0.008 265);
		font-size: 12px;
		font-weight: 500;
		cursor: pointer;
		position: relative;
		transition: background var(--dur) var(--ease);
	}
	.m-item:hover:not(:disabled) {
		background: oklch(0.3 0.016 272 / 0.8);
	}
	.m-item:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.m-item.wide {
		grid-column: 1 / -1;
		flex-direction: row;
		justify-content: center;
		padding: 11px 6px;
	}
	.m-item.active {
		background: color-mix(in oklch, var(--accent) 30%, transparent);
		border-color: var(--accent);
		color: #fff;
	}
	.badge {
		min-width: 17px;
		height: 17px;
		padding: 0 4px;
		border-radius: var(--r-pill);
		background: var(--accent);
		color: #fff;
		font-size: 10.5px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.m-note {
		margin-top: 9px;
		font-size: 11.5px;
		color: oklch(0.82 0.02 265);
		text-align: center;
		line-height: 1.4;
	}
	.filepick {
		display: none;
	}
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
	.m-end {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		width: 100%;
		margin-top: 10px;
		padding: 11px 0;
		border: 1px solid color-mix(in oklch, var(--danger) 50%, transparent);
		border-radius: var(--r-sm);
		background: color-mix(in oklch, var(--danger) 22%, transparent);
		color: #ffd9d4;
		font-size: 13.5px;
		font-weight: 600;
		cursor: pointer;
		transition: background var(--dur) var(--ease);
	}
	.m-end:hover {
		background: color-mix(in oklch, var(--danger) 40%, transparent);
		color: #fff;
	}
</style>
