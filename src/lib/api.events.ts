// Tauri event subscriptions: side-channel messages, session activity, auth
// prompts, and the client-side play telemetry stream. Re-exported by `api.ts`.

import { isTauri } from './api.invoke';
import type { AuthPrompt, DataText, FileRecv, FsEntries, LocalCaps, SessionEvent } from './api.types';

async function listenTo<T>(event: string, cb: (e: T) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<T>(event, (e) => cb(e.payload));
}

/** Effect-scoped subscription collector. `listen()` resolves ASYNCHRONOUSLY, so an
 * effect teardown that races it would see nothing to unsubscribe and the listener
 * would register afterwards — permanently. `add()` parks each pending unlisten
 * (running it immediately when the scope is already disposed); return `dispose`
 * as the `$effect` cleanup. */
export function listenScope() {
	let disposed = false;
	const offs: Array<() => void> = [];
	return {
		add(...subs: Array<Promise<() => void>>) {
			for (const p of subs)
				p.then((off) => {
					if (disposed) off();
					else offs.push(off);
				});
		},
		dispose() {
			disposed = true;
			offs.forEach((off) => off());
			offs.length = 0;
		}
	};
}

/** The startup capability probe finished (every launch): platform + validated
 * encoder/decoder lists. The splash waits for this; Settings gates on it. */
export const onLocalCaps = (cb: (caps: LocalCaps) => void) => listenTo<LocalCaps>('local-caps', cb);
/** The node (re)bound its UDP socket on `go_online` — the ACTUAL port for direct
 * `ip:port` connects (Home shows it next to the local IP). */
export const onNodePort = (cb: (port: number) => void) => listenTo<number>('node-port', cb);
/** Client: a chat line arrived from the host (`peer` = play id as string). */
export const onChatMsg = (cb: (e: DataText) => void) => listenTo<DataText>('chat-msg', cb);
/** Host: a chat line arrived from a connected client. */
export const onHostChat = (cb: (e: DataText) => void) => listenTo<DataText>('host-chat', cb);
/** Host: a client pushed clipboard text. */
export const onClipboardIn = (cb: (e: DataText) => void) => listenTo<DataText>('clipboard', cb);
/** Client: the host pushed clipboard text. */
export const onDataClip = (cb: (e: DataText) => void) => listenTo<DataText>('data-clip', cb);
/** Host: an inbound file transfer finished. Also fires on the CLIENT when a
 * file-manager download lands (`peer` = play id as string then). */
export const onFileRecv = (cb: (e: FileRecv) => void) => listenTo<FileRecv>('file-recv', cb);
/** Client: a host directory listing arrived for the file panel (the reply to
 * `api.fsList`; `id` = play id, `path` = the echoed HOME-relative path). */
export const onFsEntries = (cb: (e: FsEntries) => void) => listenTo<FsEntries>('fs-entries', cb);
/** A peer's identity image arrived over a session (pushed once right after it's
 * up). `peer` is the connection's peer id on the host side, or the play id as a
 * string on the client side; `dataUrl` plugs straight into an `<img src>`. */
export const onPeerAvatar = (cb: (e: { peer: string; dataUrl: string }) => void) =>
	listenTo<{ peer: string; dataUrl: string }>('peer-avatar', cb);
/** A peer's display name arrived over a session (`DataMsg::PeerName`). Same peer
 * keying as `onPeerAvatar` (host side: peer id; client side: play id as string). */
export const onPeerName = (cb: (e: { peer: string; name: string }) => void) =>
	listenTo<[string, string]>('peer-name', (p) => cb({ peer: p[0], name: p[1] }));
/** Client (Windows hook): the user pressed the Ctrl+Alt+Shift leave combo while
 * the OS-level keyboard hook had focus — the UI should drop control. */
export const onKbdLeave = (cb: () => void) => listenTo<null>('kbd-leave', () => cb());
/** Client (Linux native): capture ENGAGED — the user clicked the session video and the
 * evdev grab is now live (counterpart of kbd-leave, which only disengages capture). */
export const onKbdEngaged = (cb: () => void) => listenTo<null>('kbd-engaged', () => cb());
/** Client (Linux native): capture RELEASED without ending the session (3×RightCtrl) —
 * the grab is off, the session stays alive, the next video click re-engages. */
export const onKbdReleased = (cb: () => void) => listenTo<null>('kbd-released', () => cb());
/** Client (game mode): the user pressed the Ctrl+Shift+M overlay combo — toggle the
 * in-session gaming overlay. Payload-less; the session stays alive (unlike kbd-leave). */
export const onOverlayToggle = (cb: () => void) => listenTo<null>('overlay-toggle', () => cb());
/** Client (Linux native): the user pressed the Ctrl+Shift+F12 combo — toggle the Tauri
 * window's fullscreen state. Payload-less; the session stays alive (unlike kbd-leave). */
export const onFullscreenToggle = (cb: () => void) => listenTo<null>('fullscreen-toggle', () => cb());
/** Client (Linux native overlay): the user changed a setting in the native egui overlay
 * (`pulsar-render`). Forwarded here so the existing setters apply it to the host.
 * Carries the play id — every mounted Session subscribes, so handlers must gate on it. */
export const onOverlayCmd = (cb: (id: number, field: string, val: string) => void) =>
	listenTo<[number, string, string]>('overlay-cmd', (p) => cb(p[0], p[1], p[2]));
/** Client (Linux native overlay): the user pressed "End" in the native overlay.
 * Payload is the play id (gate on it — see `onOverlayCmd`). */
export const onOverlayEnd = (cb: (id: number) => void) => listenTo<number>('overlay-end', (id) => cb(id));
/** Client (Linux native overlay): the user dismissed the native overlay (scrim click).
 * Payload is the play id (gate on it — see `onOverlayCmd`). */
export const onOverlayClose = (cb: (id: number) => void) => listenTo<number>('overlay-close', (id) => cb(id));
/** Client (Linux native overlay): the user sent a chat line from the overlay's
 * NATIVE composer. Payload tuple (play id, text). */
export const onOverlayChat = (cb: (e: { id: number; text: string }) => void) =>
	listenTo<[number, string]>('overlay-chat', (p) => cb({ id: p[0], text: p[1] }));
/** Client (Linux native overlay): native Files view remote-pane ops — `op` is
 * 'fsls' (list dir) / 'fsget' (download) / 'fssend' (upload a LOCAL absolute path). */
export const onOverlayFs = (cb: (e: { id: number; op: string; path: string }) => void) =>
	listenTo<[number, string, string]>('overlay-fs', (p) => cb({ id: p[0], op: p[1], path: p[2] }));
/** Client (native overlay): the Files box was clicked — open the per-session
 * file-manager WINDOW (the session supplies the peer label). Payload = play id. */
export const onOverlayFiles = (cb: (id: number) => void) =>
	listenTo<number>('overlay-files', (id) => cb(id));
/** The Pulsar window lost focus — close the overlay so the focus-gated combo can't strand it. */
export const onWindowBlur = (cb: () => void) => listenTo<null>('window-blur', () => cb());
/** Client: the stream is REALLY up (first decoded frames/bitrate) — the Connecting
 * screen holds until this fires for the play id. */
export const onPlayReady = (cb: (id: number) => void) => listenTo<number>('play-ready', (e) => cb(e));
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
/** Client: the native renderer's ACTUAL decoder ("play-decoder", read-only display) —
 * payload tuple (play id, ffmpeg decoder name, 'hw' | 'sw' | 'na'). */
export const onPlayDecoder = (cb: (e: { id: number; name: string; hw: string }) => void) =>
	listenTo<[number, string, string]>('play-decoder', (p) => cb({ id: p[0], name: p[1], hw: p[2] }));
/** Client: the STREAM's pixel size (first frame / live res switch) — the session window
 * adopts the host's aspect ratio from this. Payload tuple (play id, width, height). */
export const onPlayDims = (cb: (e: { id: number; w: number; h: number }) => void) =>
	listenTo<[number, number, number]>('play-dims', (p) => cb({ id: p[0], w: p[1], h: p[2] }));
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
