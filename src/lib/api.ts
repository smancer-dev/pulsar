// Bridge between the SvelteKit UI and the Rust core (pulsar-core) exposed via
// Tauri commands. When running outside Tauri (e.g. `vite dev` in a browser, or
// component tests) it falls back to a deterministic mock so the UI is fully
// usable without the native shell.

import type { Config, ConnInfo, ControllerInfo } from './types';

export interface ScannedApp {
	name: string;
	path: string;
}

export interface GameInfo {
	id: string;
	title: string;
	kind: string;
}

/** A Pulsar device auto-discovered on the local network (multicast beacon). */
export interface LanDevice {
	/** Grouped relay id (e.g. `482 913 056`), or empty if the peer is relay-less. */
	id: string;
	/** Whether `id` can be used to connect via the normal flow. */
	has_id: boolean;
	name: string;
	/** `ip:port` the peer announced. */
	addr: string;
	/** `windows` / `linux` / `macos`. */
	platform: string;
}

/** Result of starting a remote-play session. */
export interface PlayInfo {
	/** Play/tab id — used to address input + stop for this session. */
	id: number;
	/** How the link was made. */
	transport: string;
	/** Loopback WebSocket port the webview opens to receive the RTP video. */
	ws_port: number;
	/** True if the host is this same machine — control is disabled (feedback loop). */
	local: boolean;
}

export const isTauri =
	typeof window !== 'undefined' &&
	'__TAURI_INTERNALS__' in (window as unknown as Record<string, unknown>);

const DEFAULT_CONFIG: Config = {
	relay: '127.0.0.1:21116',
	network_mode: 'auto',
	device_name: 'Bu Cihaz',
	language: 'tr',
	unattended_access: false
};

let mockConfig: Config = { ...DEFAULT_CONFIG };

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	if (isTauri) {
		const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
		return tauriInvoke<T>(cmd, args);
	}
	return mock<T>(cmd, args);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function mock<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	switch (cmd) {
		case 'get_config':
			return Promise.resolve(mockConfig as unknown as T);
		case 'set_config':
			mockConfig = args?.config as Config;
			return Promise.resolve(undefined as unknown as T);
		case 'go_online':
			return Promise.resolve('482 913 056' as unknown as T);
		case 'session_password':
			return Promise.resolve('7yf2-qk' as unknown as T);
		case 'new_password':
			return Promise.resolve('m4kp-zd' as unknown as T);
		case 'connect': {
			// Mirror the core's behavior: relay-only tunnels, otherwise direct.
			const transport = mockConfig.network_mode === 'relay-only' ? 'relay' : 'direct';
			return Promise.resolve({ transport, peer: String(args?.target ?? '') } as unknown as T);
		}
		case 'lan_devices':
			// Sample devices so the browser preview shows the discovery section.
			return Promise.resolve([
				{ id: '719 204 663', has_id: true, name: 'Salon PC', addr: '192.168.1.42:50311', platform: 'windows' },
				{ id: '305 881 027', has_id: true, name: 'OrangePi', addr: '192.168.1.77:50990', platform: 'linux' }
			] as unknown as T);
		case 'controllers':
			return Promise.resolve([] as unknown as T);
		case 'scan_folder':
			// No real filesystem in the browser mock.
			return Promise.resolve([] as unknown as T);
		case 'run_command':
			return Promise.resolve(undefined as unknown as T);
		case 'publish_games':
			return Promise.resolve(undefined as unknown as T);
		case 'list_remote_games':
			// No real host in the browser mock.
			return Promise.resolve([] as unknown as T);
		case 'launch_remote_game':
			return Promise.resolve(undefined as unknown as T);
		case 'available_encoders':
			return Promise.resolve(['software'] as unknown as T);
		case 'start_remote_play':
			return Promise.resolve({ id: 0, transport: 'direct', ws_port: 0, local: false } as unknown as T);
		case 'respond_request':
		case 'submit_password':
		case 'disconnect_peer':
			return Promise.resolve(undefined as unknown as T);
		case 'set_stream_settings':
		case 'stop_stream':
		case 'input_pointer':
		case 'input_button':
		case 'input_scroll':
		case 'input_key':
		case 'send_clipboard':
		case 'send_chat':
		case 'host_send_chat':
		case 'send_file':
		case 'mic_start':
		case 'mic_stop':
			return Promise.resolve(undefined as unknown as T);
		default:
			return Promise.reject(new Error(`unknown command: ${cmd}`));
	}
}

export const api = {
	getConfig: () => invoke<Config>('get_config'),
	setConfig: (config: Config) => invoke<void>('set_config', { config }),
	/** Bind the node and register with the relay; returns this device's ID. */
	goOnline: () => invoke<string>('go_online'),
	connect: (target: string) => invoke<ConnInfo>('connect', { target }),
	/** Pulsar devices auto-discovered on the local network (multicast beacon). */
	lanDevices: () => invoke<LanDevice[]>('lan_devices'),
	controllers: () => invoke<ControllerInfo[]>('controllers'),
	/** Scan a folder for launchable apps (host side). */
	scanFolder: (path: string) => invoke<ScannedApp[]>('scan_folder', { path }),
	/** Run a host-side prep command (session start/stop hook). */
	runCommand: (command: string) => invoke<void>('run_command', { command }),
	/** Publish this host's games so connecting clients can list/launch them. */
	publishGames: (games: unknown[]) => invoke<void>('publish_games', { games }),
	/** This host's current one-time password (clients must enter it to connect). */
	sessionPassword: () => invoke<string>('session_password'),
	/** Roll a fresh one-time password (invalidates the previous one). */
	newPassword: () => invoke<string>('new_password'),
	/** Approval popup → resolve an incoming connection request (Allow/Deny). */
	respondRequest: (id: number, allow: boolean) =>
		invoke<void>('respond_request', { id, allow }),
	/** Client password prompt → reply (null = cancelled). */
	submitPassword: (req: number, password: string | null) =>
		invoke<void>('submit_password', { req, password }),
	/** Host: kick a connected client by its peer id. */
	disconnectPeer: (peer: string) => invoke<void>('disconnect_peer', { peer }),
	/** List the games published by the host at `target`. */
	listRemoteGames: (target: string) => invoke<GameInfo[]>('list_remote_games', { target }),
	/** Ask the host at `target` to launch one of its games. */
	launchRemoteGame: (target: string, gameId: string) =>
		invoke<void>('launch_remote_game', { target, gameId }),
	/** Hardware encoders ffmpeg reports as available on this machine. */
	availableEncoders: () => invoke<string[]>('available_encoders'),
	/** Push host stream settings (resolution/fps/bitrate/encoder/display) to the core. */
	setStreamSettings: (cfg: Record<string, unknown>) =>
		invoke<void>('set_stream_settings', { cfg }),
	/**
	 * Client: connect to a host, open its video (ffplay window), and optionally
	 * stream local controller input — over one session held open until stopStream.
	 * Returns the transport used (`direct`/`relay`).
	 */
	startRemotePlay: (
		target: string,
		gameId: string,
		port: number,
		codec: string,
		encoder: string,
		gamepad: boolean
	) => invoke<PlayInfo>('start_remote_play', { target, gameId, port, codec, encoder, gamepad }),
	/** Stop one remote-play session (tab) by id. */
	stopStream: (id: number) => invoke<void>('stop_stream', { id }),
	/** Control: absolute pointer motion, normalized 0..1 over the remote screen. */
	inputPointer: (id: number, x: number, y: number) =>
		invoke<void>('input_pointer', { id, x, y }),
	/** Control: mouse button (0=left, 1=right, 2=middle) press/release. */
	inputButton: (id: number, button: number, down: boolean) =>
		invoke<void>('input_button', { id, button, down }),
	/** Control: scroll delta. */
	inputScroll: (id: number, dx: number, dy: number) =>
		invoke<void>('input_scroll', { id, dx, dy }),
	/** Control: keyboard evdev keycode press/release. */
	inputKey: (id: number, code: number, down: boolean) =>
		invoke<void>('input_key', { id, code, down }),
	/** Client → host: push clipboard text to the remote. */
	sendClipboard: (id: number, text: string) => invoke<void>('send_clipboard', { id, text }),
	/** Client → host: send a chat line. */
	sendChat: (id: number, text: string) => invoke<void>('send_chat', { id, text }),
	/** Host → client: reply to a connected peer's chat. */
	hostSendChat: (peer: string, text: string) => invoke<void>('host_send_chat', { peer, text }),
	/** Client → host: send a file (raw bytes, chunked + saved on the host). */
	sendFile: (id: number, name: string, data: number[]) =>
		invoke<void>('send_file', { id, name, data }),
	/** Client: start streaming the microphone to the host. */
	micStart: (id: number) => invoke<void>('mic_start', { id }),
	/** Client: stop streaming the microphone. */
	micStop: (id: number) => invoke<void>('mic_stop', { id })
};

/** A side-channel text message: `peer` is the host-side peer id, or (for client
 * events) the play id as a string. */
export interface DataText {
	peer: string;
	text: string;
}

/** A finished (or failed) inbound file transfer, host side. */
export interface FileRecv {
	peer: string;
	name: string;
	bytes: number;
	ok: boolean;
}

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

export interface SessionEvent {
	kind: string;
	peer: string;
	detail: string;
}

/** Subscribe to host-side session activity (connect/launch/stream/disconnect). */
export async function onSessionEvent(cb: (e: SessionEvent) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<SessionEvent>('session', (e) => cb(e.payload));
}

/** A host is asking this client for a password — show a prompt and reply via
 * `api.submitPassword(req, …)`. */
export interface AuthPrompt {
	req: number;
	peer: string;
}

/** Subscribe to host password requests (client side). */
export async function onAuthPrompt(cb: (e: AuthPrompt) => void): Promise<() => void> {
	if (!isTauri) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<AuthPrompt>('auth-prompt', (e) => cb(e.payload));
}

/** Copy text to the clipboard. Uses the async Clipboard API, falling back to a
 * hidden textarea + execCommand for webviews that block it. Returns success. */
export async function copyText(text: string): Promise<boolean> {
	try {
		if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
			await navigator.clipboard.writeText(text);
			return true;
		}
	} catch {
		/* fall through to the legacy path */
	}
	try {
		const ta = document.createElement('textarea');
		ta.value = text;
		ta.setAttribute('readonly', '');
		ta.style.position = 'fixed';
		ta.style.top = '-1000px';
		ta.style.opacity = '0';
		document.body.appendChild(ta);
		ta.select();
		const ok = document.execCommand('copy');
		document.body.removeChild(ta);
		return ok;
	} catch {
		return false;
	}
}

/** Toggle OS-window fullscreen (no-op outside Tauri). */
export async function setFullscreen(on: boolean): Promise<void> {
	if (!isTauri) return;
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	await getCurrentWindow().setFullscreen(on);
}

/** Read the local clipboard text (for "paste to remote"). */
export async function readClipboard(): Promise<string> {
	try {
		if (typeof navigator !== 'undefined' && navigator.clipboard?.readText) {
			return await navigator.clipboard.readText();
		}
	} catch {
		/* ignore */
	}
	return '';
}

/** Control the frameless OS window (no-op outside Tauri, e.g. browser dev). */
export async function windowControl(action: 'minimize' | 'maximize' | 'close') {
	if (!isTauri) return;
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	const w = getCurrentWindow();
	if (action === 'minimize') await w.minimize();
	else if (action === 'maximize') await w.toggleMaximize();
	else await w.close();
}
