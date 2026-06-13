// The `api` object: typed wrappers over the Tauri commands exposed by the Rust
// core. Re-exported by `api.ts`.

import type { AutoConnectTarget, Config, ConnInfo, ControllerInfo } from './types';
import type { FsEntry, LocalCaps } from './api.types';
import type { GameInfo, LanDevice, PlayInfo, ScannedApp } from './api.types';
import { invoke } from './api.invoke';

export const api = {
	getConfig: () => invoke<Config>('get_config'),
	setConfig: (config: Config) => invoke<void>('set_config', { config }),
	/** Bind the node and register with the relay; returns this device's ID. */
	goOnline: () => invoke<string>('go_online'),
	connect: (target: string) => invoke<ConnInfo>('connect', { target }),
	/** Pulsar devices auto-discovered on the local network (multicast beacon). */
	lanDevices: () => invoke<LanDevice[]>('lan_devices'),
	controllers: () => invoke<ControllerInfo[]>('controllers'),
	/** This machine's primary LAN IPv4 (for "connect to me by IP"); empty if none. */
	localIp: () => invoke<string>('local_ip'),
	/** The node's actual bound UDP port (0 = not online yet) — shown as "ip:port". */
	nodePort: () => invoke<number>('node_port'),
	/** The local user's avatar (account photo / wallpaper, per the avatar_mode
	 * setting) as a `data:image/png;base64,…` URL, or null when none resolves —
	 * the UI keeps its textual chip then. */
	selfAvatar: () => invoke<string | null>('self_avatar'),
	/** The OS user's display name (e.g. "Ahmet Enes Duruer") for the identity chip. */
	deviceUserName: () => invoke<string>('device_user_name'),
	/** Path to an installed Steam launcher, or empty if Steam isn't found. */
	steamPath: () => invoke<string>('steam_path'),
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
	/** Host: snapshot of active inbound connections (the connections window's initial list). */
	listConnections: () =>
		invoke<
			{
				peer: string;
				since_ms: number;
				mode: 'remote' | 'game';
				view_only: boolean;
				name: string | null;
				avatar: string | null;
			}[]
		>('list_connections'),
	/** Host: reveal/focus the dedicated connections window (sidebar button). */
	showConnections: () => invoke<void>('show_connections'),
	/** Client: open (or focus) the per-session file-manager window for play `id`;
	 * `peer` decorates the title/header so multi-session windows stay tellable apart. */
	openFilesWindow: (id: number, peer: string) => invoke<void>('open_files_window', { id, peer }),
	/** Host: revoke/restore a connected client's CONTROL ("Sadece izleme") — its
	 * input is dropped while set; the stream keeps running. */
	setViewOnly: (peer: string, on: boolean) => invoke<void>('set_view_only', { peer, on }),
	/** Host: chat backlog [(peer, text, me)] — seeds the connections window's message
	 * modal with lines from before that window existed. */
	chatLog: () => invoke<[string, string, boolean][]>('chat_log'),
	/** List the games published by the host at `target`. */
	listRemoteGames: (target: string) => invoke<GameInfo[]>('list_remote_games', { target }),
	/** Ask the host at `target` to launch one of its games. */
	launchRemoteGame: (target: string, gameId: string) =>
		invoke<void>('launch_remote_game', { target, gameId }),
	/** Startup-probed local caps (null while the probe is still running — listen to
	 * the `local-caps` event for the push). */
	localCaps: () => invoke<LocalCaps | null>('local_caps'),
	/** Hardware encoders ffmpeg reports as available on this machine. */
	availableEncoders: () => invoke<string[]>('available_encoders'),
	/** Audio capture devices this host can record from (for the Settings dropdown).
	 * Can change at runtime (USB unplug), so the Settings screen re-queries on mount
	 * and polls periodically. Windows = DirectShow device names; Linux = pactl
	 * source names; macOS = empty. */
	listAudioSources: () => invoke<string[]>('list_audio_sources'),
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
		gamepad: boolean,
		gameMode = false
	) =>
		invoke<PlayInfo>('start_remote_play', {
			target,
			gameId,
			port,
			codec,
			encoder,
			gamepad,
			gameMode
		}),
	/** CLI `--connect` auto-connect target (id/ip + password + mode + app), or null. */
	autoConnectTarget: () =>
		invoke<AutoConnectTarget | null>('auto_connect_target'),
	/** Stop one remote-play session (tab) by id. */
	stopStream: (id: number) => invoke<void>('stop_stream', { id }),
	/** Relaunch the app to a fresh, usable home after a direct-connect (kiosk) session ends.
	 * Linux only (the native renderer leaves WebKitGTK unable to process clicks once it tears
	 * down on the headless path); a new process is the reliable fix. Skips auto-connect. */
	relaunchToHome: () => invoke<void>('relaunch_to_home'),
	/** Change an active session's stream resolution live (0×0 = host default). */
	setPlayResolution: (id: number, width: number, height: number) =>
		invoke<void>('set_play_resolution', { id, width, height }),
	/** Switch the host's video encoder live (the host restarts ffmpeg with it). */
	setPlayEncoder: (id: number, encoder: string) =>
		invoke<void>('set_play_encoder', { id, encoder }),
	/** Switch the video codec live (h264/h265/av1; the host restarts ffmpeg with it). */
	setPlayCodec: (id: number, codec: string) => invoke<void>('set_play_codec', { id, codec }),
	/** Change the frame rate live (0 = host default). */
	setPlayFps: (id: number, fps: number) => invoke<void>('set_play_fps', { id, fps }),
	/** Switch which HOST monitor is streamed (session menu); idx into host_displays, 0 = primary. */
	setPlayMonitor: (id: number, displayIdx: number) =>
		invoke<void>('set_play_monitor', { id, display_idx: displayIdx }),
	/** Change the target bitrate live in kbps (0 = host default; UI converts Mbit→kbps). */
	setPlayBitrate: (id: number, kbps: number) => invoke<void>('set_play_bitrate', { id, kbps }),
	/** Switch the encode quality/perf profile live (the host restarts ffmpeg with it). */
	setPlayQuality: (id: number, quality: 'latency' | 'quality') =>
		invoke<void>('set_play_quality', { id, quality }),
	/** Toggle Moonlight-style frame pacing on the Linux native renderer (client-local;
	 * no host re-encode). No-op on Windows/macOS where there's no pulsar-render process. */
	setFramePacing: (id: number, on: boolean) => invoke<void>('set_frame_pacing', { id, on }),
	/** Toggle the always-on mini stats HUD on the native renderer (persisted in ui). */
	setStatsHud: (id: number, on: boolean) => invoke<void>('set_stats_hud', { id, on }),
	/** Toggle the always-visible overlay-open button on the native renderer. */
	setOverlayButton: (id: number, on: boolean) => invoke<void>('set_overlay_button', { id, on }),
	/** Move the overlay-open button (egui points from the renderer's top-left) — streamed
	 * live while the hotspot is dragged; the final spot is persisted in ui. */
	setOverlayButtonPos: (id: number, x: number, y: number) =>
		invoke<void>('set_overlay_button_pos', { id, x, y }),
	/** Push a transient helper tooltip to the native renderer ('engage' = how to release,
	 * 'click' = click-to-control), drawn bottom-center for ~3 s. */
	renderHint: (id: number, kind: 'engage' | 'click') => invoke<void>('render_hint', { id, kind }),
	/** Free-text toast on the native renderer (bottom-center, ~6 s) — inbound chat
	 * surfaces here because the video occludes the webview. */
	renderToast: (id: number, text: string) => invoke<void>('render_toast', { id, text }),
	/** One chat line into the native overlay's Chat view ('in' = from the host,
	 * 'out' = ours — both echoed so the renderer's log is the single truth). */
	renderChat: (id: number, dir: 'in' | 'out', text: string) =>
		invoke<void>('render_chat', { id, dir, text }),
	/** Push a host directory listing to the native Files view (one-line JSON). */
	renderFs: (id: number, json: string) => invoke<void>('render_fs', { id, json }),
	/** Relay a keyboard input to the overlay's Chat composer ('t' text / 'k' named key). */
	renderKin: (id: number, kind: 't' | 'k', data: string) =>
		invoke<void>('render_kin', { id, kind, data }),
	/** Toggle host audio transmit + host-mute live (session-menu audio options). */
	setPlayAudio: (id: number, transmit: boolean, mute: boolean) =>
		invoke<void>('set_play_audio', { id, transmit, mute }),
	/** Open/close the in-session gaming overlay (Linux: ungrabs input + pauses the
	 * native mpv; Windows/macOS: no-op pause, overlay floats over the live canvas). */
	setOverlay: (id: number, open: boolean) => invoke<void>('set_overlay', { id, open }),
	/** Ask the controlled host to reverse direction (it connects back to `myId`). */
	reversePlay: (id: number, myId: string) => invoke<void>('reverse_play', { id, myId }),
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
	/** Control (Windows): arm the low-level keyboard hook so OS-reserved keys
	 * (Win, Alt+Tab, Ctrl+Esc, media) go to the remote and are suppressed locally.
	 * No-op on non-Windows. */
	kbdCaptureStart: (id: number, mouse = false) =>
		invoke<void>('kbd_capture_start', { id, mouse }),
	/** Control: disarm the low-level keyboard hook (release / blur / leave combo). */
	kbdCaptureStop: () => invoke<void>('kbd_capture_stop'),
	/** Control (Linux native): explicit click-to-engage — the user clicked the session
	 * video (the pass-through container let the click reach the webview). */
	kbdEngage: () => invoke<void>('kbd_engage'),
	/** Client (Linux native): position the in-app video container over the session tab's
	 * content area (viewport CSS px). Zero area hides it (inactive tab / unmount). */
	nativeViewRect: (id: number, x: number, y: number, w: number, h: number) =>
		invoke<void>('native_view_rect', { id, x, y, w, h }),
	/** Client → host: push clipboard text to the remote. */
	sendClipboard: (id: number, text: string) => invoke<void>('send_clipboard', { id, text }),
	/** Client → host: send a chat line. */
	sendChat: (id: number, text: string) => invoke<void>('send_chat', { id, text }),
	/** Host → client: reply to a connected peer's chat. */
	hostSendChat: (peer: string, text: string) => invoke<void>('host_send_chat', { peer, text }),
	/** Client → host: send a file (raw bytes, chunked + saved on the host). */
	sendFile: (id: number, name: string, data: number[]) =>
		invoke<void>('send_file', { id, name, data }),
	/** Client → host: send a local file by its HOME-relative path (file panel's
	 * "gönder" — Rust streams it from disk, the webview never reads the bytes). */
	sendFilePath: (id: number, path: string) => invoke<void>('send_file_path', { id, path }),
	/** Client → host: list a host directory ("" = the host user's HOME). The reply
	 * arrives asynchronously as the `fs-entries` event (see onFsEntries). */
	fsList: (id: number, path: string) => invoke<void>('fs_list', { id, path }),
	/** Client → host: download the host file at this HOME-relative path — streamed
	 * back over the session and saved under "Pulsar Alınanlar" (`file-recv` event). */
	fsGet: (id: number, path: string) => invoke<void>('fs_get', { id, path }),
	/** List a LOCAL directory for the file panel's left pane (same shape + the
	 * same HOME jail as the remote listing; "" = this user's HOME). */
	localLs: (path: string) => invoke<FsEntry[]>('local_ls', { path }),
	/** Client: start streaming the microphone to the host. */
	micStart: (id: number) => invoke<void>('mic_start', { id }),
	/** Client: stop streaming the microphone. */
	micStop: (id: number) => invoke<void>('mic_stop', { id })
};
