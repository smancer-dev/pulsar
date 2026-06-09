// Tauri event subscriptions: side-channel messages, session activity, auth
// prompts, and the client-side play telemetry stream. Re-exported by `api.ts`.

import { isTauri } from './api.invoke';
import type { AuthPrompt, DataText, FileRecv, SessionEvent } from './api.types';

async function listenTo<T>(event: string, cb: (e: T) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<T>(event, (e) => cb(e.payload));
}

/** Client: a chat line arrived from the host (`peer` = play id as string). */
export const onChatMsg = (cb: (e: DataText) => void) => listenTo<DataText>('chat-msg', cb);
/** Host: a chat line arrived from a connected client. */
export const onHostChat = (cb: (e: DataText) => void) => listenTo<DataText>('host-chat', cb);
/** Host: a client pushed clipboard text. */
export const onClipboardIn = (cb: (e: DataText) => void) => listenTo<DataText>('clipboard', cb);
/** Client: the host pushed clipboard text. */
export const onDataClip = (cb: (e: DataText) => void) => listenTo<DataText>('data-clip', cb);
/** Host: an inbound file transfer finished. */
export const onFileRecv = (cb: (e: FileRecv) => void) => listenTo<FileRecv>('file-recv', cb);
/** Client (Windows hook): the user pressed the Ctrl+Alt+Shift leave combo while
 * the OS-level keyboard hook had focus — the UI should drop control. */
export const onKbdLeave = (cb: () => void) => listenTo<null>('kbd-leave', () => cb());
/** Client (game mode): the user pressed the Ctrl+Shift+M overlay combo — toggle the
 * in-session gaming overlay. Payload-less; the session stays alive (unlike kbd-leave). */
export const onOverlayToggle = (cb: () => void) => listenTo<null>('overlay-toggle', () => cb());
/** Client (Linux native): the user pressed the Ctrl+Shift+F12 combo — toggle the Tauri
 * window's fullscreen state. Payload-less; the session stays alive (unlike kbd-leave). */
export const onFullscreenToggle = (cb: () => void) => listenTo<null>('fullscreen-toggle', () => cb());
/** Client (Linux native overlay): the user changed a setting in the native egui overlay
 * (`pulsar-render`). Forwarded here so the existing setters apply it to the host. */
export const onOverlayCmd = (cb: (field: string, val: string) => void) =>
	listenTo<[number, string, string]>('overlay-cmd', (p) => cb(p[1], p[2]));
/** Client (Linux native overlay): the user pressed "End" in the native overlay. */
export const onOverlayEnd = (cb: () => void) => listenTo<number>('overlay-end', () => cb());
/** Client (Linux native overlay): the user dismissed the native overlay (scrim click). */
export const onOverlayClose = (cb: () => void) => listenTo<number>('overlay-close', () => cb());
/** The Pulsar window lost focus — close the overlay so the focus-gated combo can't strand it. */
export const onWindowBlur = (cb: () => void) => listenTo<null>('window-blur', () => cb());
/** Client: a play session ended (host closed it, a network error, or we left). The UI must
 * release any input grab and drop the tab — otherwise the native path freezes on mpv's last
 * frame with the keyboard/mouse still captured (you'd be stuck). Payload is the play id. */
export const onPlayEnded = (cb: (id: number) => void) => listenTo<number>('play-ended', (e) => cb(e));

/** Host: a controlling client asked to reverse direction — connect back to `id`. */
export const onReverseRequest = (cb: (e: { id: string }) => void) =>
	listenTo<{ id: string }>('reverse-request', cb);
/** Client: a real connection milestone (transport established) for the Connecting
 * screen, keyed by the target string. */
export const onConnPhase = (cb: (e: { target: string; transport: string }) => void) =>
	listenTo<{ target: string; transport: string }>('conn-phase', cb);
/** Client: round-trip time (ms) for a play session, from keepalive ping/pong. */
export const onPlayRtt = (cb: (e: { id: number; rtt: number }) => void) =>
	listenTo<{ id: number; rtt: number }>('play-rtt', cb);
/** Client: the host's encode summary (e.g. "NVENC · 1080p · 60fps") for a session. */
export const onPlayStats = (cb: (e: { id: number; label: string }) => void) =>
	listenTo<{ id: number; label: string }>('host-stats', cb);
/** Client: real video stats from mpv (single-surface + native --wid renderer): fps,
 * dropped frames, Mbps, and decode/output latency in ms (`decodeMs`). */
export const onPlayVStats = (
	cb: (e: { id: number; fps: number; drops: number; mbps: number; decodeMs: number }) => void
) =>
	listenTo<{ id: number; fps: number; drops: number; mbps: number; decodeMs: number }>(
		'play-vstats',
		cb
	);

/** Subscribe to host-side session activity (connect/launch/stream/disconnect). */
export async function onSessionEvent(cb: (e: SessionEvent) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<SessionEvent>('session', (e) => cb(e.payload));
}

/** Subscribe to host password requests (client side). */
export async function onAuthPrompt(cb: (e: AuthPrompt) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<AuthPrompt>('auth-prompt', (e) => cb(e.payload));
}
