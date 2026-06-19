// The `api` object: typed wrappers over the Tauri commands exposed by the Rust
// core. Re-exported by `api.ts`.

import type { AutoConnectTarget, Config, ConnInfo, ControllerInfo } from './types';
import type { FsEntry, LocalCaps } from './api.types';
import type { GameInfo, LanDevice, PlayInfo, ScannedApp, WindowInfo } from './api.types';
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
	/** Persist the controller slot permutation. order[n] = gilrs uuid hex of the pad
	 * assigned to player-slot n. Written by the controller reorder UI; the play reader
	 * reads it each tick so changes apply live without reconnect. */
	setControllerOrder: (order: string[]) => invoke<void>('set_controller_order', { order }),
	/** Persist the per-controller emulation target. map[uuid] = 'auto'|'xbox'|'ds4'. The play
	 * reader reads it live so changes apply without reconnect. */
	setControllerEmulation: (map: Record<string, string>) =>
		invoke<void>('set_controller_emulation', { map }),
	/** Set PER-CONTROLLER vibration strength. map[uuid] = 'off'|'weak'|'medium'|'strong'. The
	 * SDL pad manager scales each pad's host rumble by its level live. */
	setControllerRumble: (map: Record<string, string>) =>
		invoke<void>('set_controller_rumble', { map }),
	/** Set the disabled-controller set (uuid hex list). Disabled pads aren't forwarded. */
	setDisabledControllers: (uuids: string[]) =>
		invoke<void>('set_disabled_controllers', { uuids }),
	/** Fire a one-shot rumble pulse on one pad (by uuid) at its configured level so the
	 * user can FEEL the vibration setting. No-op if the level is 'off' or the pad is gone. */
	testControllerRumble: (uuid: string) =>
		invoke<void>('test_controller_rumble', { uuid }),
	/** Whether GPU compositing is forced on (opi5). The shell runs the present-keepalive only
	 * when true. */
	forceAc: () => invoke<boolean>('force_ac'),
	/** Per-device DEFAULT for app-UI hardware acceleration (off on Orange Pi 5, on elsewhere) —
	 * shown by the Settings toggle when no explicit preference is set. */
	defaultUiHwaccel: () => invoke<boolean>('default_ui_hwaccel'),
	/** Whether the webview is software-painted (AC off) this session — the root gets `data-sw-ui`
	 * so the gaming CSS strips expensive blurred shadows only where needed. */
	webviewSwPainted: () => invoke<boolean>('webview_sw_painted'),
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
	/** Host: kick a connected SESSION by its session id (so one pane of a same-host
	 * co-op pair can be kicked without ending the other). */
	disconnectPeer: (sid: number) => invoke<void>('disconnect_peer', { sid }),
	/** Host: kick EVERY connected client (authoritative) — used when entering gaming mode. */
	disconnectAllPeers: () => invoke<void>('disconnect_all_peers'),
	/** Host: snapshot of active inbound connections (the connections window's initial list). */
	listConnections: () =>
		invoke<
			{
				sid: number;
				peer: string;
				since_ms: number;
				mode: 'remote' | 'game';
				view_only: boolean;
				name: string | null;
				avatar: string | null;
				client_id: string | null;
			}[]
		>('list_connections'),
	/** Host: reveal/focus the dedicated connections window (sidebar button). */
	showConnections: () => invoke<void>('show_connections'),
	/** Client: open (or focus) the per-session file-manager window for play `id`;
	 * `peer` decorates the title/header so multi-session windows stay tellable apart. */
	openFilesWindow: (id: number, peer: string) => invoke<void>('open_files_window', { id, peer }),
	/** Host: revoke/restore a connected SESSION's CONTROL ("Sadece izleme") — its
	 * input is dropped while set; the stream keeps running. Keyed by session id. */
	setViewOnly: (sid: number, on: boolean) => invoke<void>('set_view_only', { sid, on }),
	/** Host: chat backlog [(peer, text, me)] — seeds the connections window's message
	 * modal with lines from before that window existed. */
	chatLog: () => invoke<[string, string, boolean][]>('chat_log'),
	/** List the games published by the host at `target`. */
	listRemoteGames: (target: string) => invoke<GameInfo[]>('list_remote_games', { target }),
	/** Ask the host at `target` to launch one of its games. */
	launchRemoteGame: (target: string, gameId: string) =>
		invoke<void>('launch_remote_game', { target, gameId }),
	/** List the host's capture-able windows (Phase 2b co-op "Pencere" picker), routed
	 * through an EXISTING live session's control channel (`id` = its Rust play id). The
	 * user picks one and its `hwnd` becomes the new pane's window-capture target. Empty on
	 * a non-Windows / old host (no per-window WGC source). */
	hostWindowList: (id: number) => invoke<WindowInfo[]>('host_window_list', { id }),
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
	 *
	 * `gamepad` is independent of `gameMode`: controllers can be forwarded in both
	 * remote-desktop and game-streaming sessions. It reflects the user's
	 * `ui.forwardControllers` preference, not the session mode.
	 */
	startRemotePlay: (
		target: string,
		gameId: string,
		port: number,
		codec: string,
		encoder: string,
		gamepad: boolean,
		gameMode = false,
		/** 'auto' | 'hq' | 'fast' — from Settings → Display 'Varsayılan kalite'. */
		quality?: string,
		/** Treat the DS4/DS5 touchpad as a relative mouse (Linux only; Feature 2B). */
		touchpadAsMouse = true,
		/** Initial host monitor to capture (0 = primary; null = host default). A second
		 * same-host split pane passes a FREE display so two panes capture DIFFERENT
		 * monitors and dodge the host's DXGI same-monitor collision. */
		displayIdx?: number,
		/** Initial per-WINDOW capture target (Phase 2b co-op). A raw Win32 HWND (from
		 * `hostWindowList`, or the host-resolved game window) makes the host WGC-capture
		 * that single window instead of a whole monitor — so two panes can share ONE
		 * monitor. Wins over `displayIdx` on the host when set; null = monitor capture. */
		windowHwnd?: number
	) =>
		invoke<PlayInfo>('start_remote_play', {
			target,
			gameId,
			port,
			codec,
			encoder,
			gamepad,
			gameMode,
			quality: quality ?? null,
			touchpadAsMouse,
			displayIdx: displayIdx ?? null,
			windowHwnd: windowHwnd ?? null
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
	/** Whether an in-app self-update can actually replace the running binary on this platform.
	 * False on Linux when launched without FUSE (no $APPIMAGE: extract-and-run / raw dev binary),
	 * where the updater would silently rewrite a throwaway temp file instead of the deployed
	 * AppImage. Used to skip the update with a clear warning rather than no-op'ing. */
	selfUpdatePossible: () => invoke<boolean>('self_update_possible'),
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
		invoke<void>('set_play_monitor', { id, displayIdx }),
	/** Toggle Parsec-style screen adaptation: when `on`, the host switches the captured monitor
	 *  to the mode best fitting a `w×h` pane and reverts on exit (no-op on a non-Windows host). */
	setPlayAdapt: (id: number, w: number, h: number, on: boolean) =>
		invoke<void>('set_play_adapt', { id, w, h, on }),
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
	/** Relay a NAV keypress to the overlay (the same `k <dir>` channel the pad uses) so a
	 * physical keyboard drives the menus like a controller. dir: up/down/left/right/go/escape. */
	renderNav: (id: number, dir: string) => invoke<void>('render_nav', { id, dir }),
	/** Toggle host audio transmit + host-mute live (session-menu audio options). */
	setPlayAudio: (id: number, transmit: boolean, mute: boolean) =>
		invoke<void>('set_play_audio', { id, transmit, mute }),
	/** Open/close the in-session gaming overlay (Linux: ungrabs input + pauses the
	 * native mpv; Windows/macOS: no-op pause, overlay floats over the live canvas). */
	setOverlay: (id: number, open: boolean) => invoke<void>('set_overlay', { id, open }),
	/** Split mode: tell the backend which play OWNS keyboard/mouse + the unlocked
	 * controllers (AppState.focused_session). Called on pane focus so input follows
	 * the focused pane. `playId` is the session's Rust play id. */
	setActiveSession: (playId: number) => invoke<void>('set_active_session', { playId }),
	/** Split mode: tell the backend how many panes are active (AppState.split_pane_count;
	 * default 1). Used as the resident-pool reap cap so up to 4 live renderers aren't
	 * reaped as "excess". Called after every pane-count change. */
	setPaneCount: (count: number) => invoke<void>('set_pane_count', { count }),
	/** Split mode: apply a controller-lock toggle from a session overlay. `locked=true`
	 * makes only this play forward that pad; `locked=false` releases it (if this play owns
	 * the lock). Mirrors the overlay `ctrllock` "uuid,0|1" wire. */
	setControllerLock: (uuid: string, playId: number, locked: boolean) =>
		invoke<void>('set_controller_lock', { uuid, playId, locked }),
	/** Split couch-coop: lock a physical KEYBOARD/MOUSE (by its stable key from listInputDevices)
	 * to a pane's session so two players can use separate kb+mouse, or unlock it. Mirrors
	 * setControllerLock. */
	setKbmLock: (devKey: string, playId: number, locked: boolean) =>
		invoke<void>('set_kbm_lock', { devKey, playId, locked }),
	/** Split: the keyboards/mice the local capture is holding (stable keys), for per-pane
	 * assignment. Linux-only content; empty elsewhere. */
	listInputDevices: () => invoke<string[]>('list_input_devices'),
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
	/** Control: type a resolved Unicode character verbatim (layout-independent). Sent for
	 * printable keys with no shortcut modifier so non-US layouts + AltGr symbols land
	 * correctly on the host regardless of ITS active layout. */
	inputChar: (id: number, ch: string) => invoke<void>('input_char', { id, ch }),
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
	/** Host → client: reply to a connected SESSION's chat (routed by session id so
	 * two panes from one client device get the reply on the right pane). */
	hostSendChat: (sid: number, text: string) => invoke<void>('host_send_chat', { sid, text }),
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
	micStop: (id: number) => invoke<void>('mic_stop', { id }),
	/** Sync the "run in system tray" preference to the backend so the CloseRequested
	 * handler knows whether to hide-to-tray (enabled=true) or quit (enabled=false). */
	setTray: (enabled: boolean) => invoke<void>('set_tray', { enabled }),
	/** Enable/disable this device's HOST role. `serving=false` (set when the app enters
	 * gaming mode) makes the host reject every inbound connection at auth time — nobody
	 * can connect to this machine. Outbound connects + relay registration are unaffected. */
	setHostServing: (serving: boolean) => invoke<void>('set_host_serving', { serving }),
	/** Start the gilrs→webview controller-nav bridge (gaming-mode menu navigation). Emits
	 * `gamepad-nav` events (see onGamepadNav). The ONLY pad-nav path on Linux (no webview
	 * Gamepad API there) and the preferred one everywhere. */
	gamepadNavStart: () => invoke<void>('gamepad_nav_start'),
	gamepadNavStop: () => invoke<void>('gamepad_nav_stop')
};
