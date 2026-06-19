// The Tauri invoke bridge + the deterministic browser/test mock. When running
// outside Tauri (e.g. `vite dev` in a browser, or component tests) it falls back
// to a mock so the UI is fully usable without the native shell.

import type { Config } from './types';

export const isTauri =
	typeof window !== 'undefined' &&
	'__TAURI_INTERNALS__' in (window as unknown as Record<string, unknown>);

const DEFAULT_CONFIG: Config = {
	relay: '127.0.0.1:21116',
	network_mode: 'auto',
	device_name: 'Bu Cihaz',
	language: 'tr',
	unattended_access: false,
	connect_password: '',
	transmit_audio: true,
	mute_host_audio: false,
	audio_input: '',
	node_port: 0,
	avatar_mode: 'user',
	native_player: false,
	request_admin: true
};

let mockConfig: Config = { ...DEFAULT_CONFIG };

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
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
		case 'list_input_devices':
			return Promise.resolve([] as unknown as T);
		case 'set_kbm_lock':
			return Promise.resolve(undefined as unknown as T);
		case 'local_ip':
			return Promise.resolve('192.168.1.42' as unknown as T);
		case 'node_port':
			return Promise.resolve(21118 as unknown as T);
		case 'self_avatar':
			// No OS account image in the browser mock — the UI keeps its textual chip.
			return Promise.resolve(null as unknown as T);
		case 'device_user_name':
			return Promise.resolve('Deniz Yılmaz' as unknown as T);
		case 'steam_path':
			return Promise.resolve('' as unknown as T);
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
		case 'host_window_list':
			// Deterministic sample windows so the "Pencere" picker is browsable in the
			// browser preview (real hosts reply over a live session; the mock has none).
			return Promise.resolve([
				{ hwnd: 65772, title: 'Not Defteri' },
				{ hwnd: 131308, title: 'Hesap Makinesi' },
				{ hwnd: 198844, title: 'Firefox' }
			] as unknown as T);
		case 'available_encoders':
			return Promise.resolve(['software'] as unknown as T);
		case 'list_audio_sources':
			// Sample sources so the browser preview shows the dropdown populated.
			return Promise.resolve([
				'alsa_output.pci-0000_00_1f.3.analog-stereo.monitor',
				'alsa_input.pci-0000_00_1f.3.analog-stereo'
			] as unknown as T);
		case 'start_remote_play':
			return Promise.resolve({ id: 0, transport: 'direct', ws_port: 0, audio_ws_port: 0, local: false, native: false, embedded: false, host_codecs: ['h264'], host_encoders: ['software'], host_displays: [{ idx: 0, name: 'DISPLAY1', width: 2560, height: 1440, primary: true }, { idx: 1, name: 'DISPLAY2', width: 1920, height: 1080, primary: false }], client_codecs: ['h264', 'h265'] } as unknown as T);
		case 'local_caps':
			return Promise.resolve({
				platform: 'linux',
				encoders: [
					{ id: 'vaapi', backend: 'ffmpeg', codecs: ['h264', 'h265'] },
					{ id: 'software', backend: 'ffmpeg', codecs: ['h264', 'h265', 'av1'] }
				],
				decoders: [
					{ codec: 'h264', ok: true, name: 'h264', hw: false, tier: 'software' },
					{ codec: 'h265', ok: true, name: 'hevc', hw: false, tier: 'software' },
					{ codec: 'av1', ok: true, name: 'libdav1d', hw: false, tier: 'software' }
				]
			} as unknown as T);
		case 'auto_connect_target':
			// No CLI auto-connect target in the browser mock (silences the reject log).
			return Promise.resolve(null as unknown as T);
		case 'self_update_possible':
			// The browser mock isn't an AppImage; pretend self-update is possible so the
			// preview behaves like a normal in-place-updatable build.
			return Promise.resolve(true as unknown as T);
		case 'list_connections':
			return Promise.resolve([] as unknown as T);
		case 'respond_request':
		case 'submit_password':
		case 'disconnect_peer':
		case 'show_connections':
		case 'relaunch_to_home':
			return Promise.resolve(undefined as unknown as T);
		case 'set_controller_order':
		case 'set_controller_emulation':
		case 'set_controller_rumble':
		case 'set_disabled_controllers':
		case 'set_tray':
		case 'set_host_serving':
		case 'gamepad_nav_start':
		case 'gamepad_nav_stop':
		case 'set_stream_settings':
		case 'stop_stream':
		case 'set_play_resolution':
		case 'set_overlay':
		case 'set_active_session':
		case 'set_pane_count':
		case 'set_controller_lock':
		case 'set_play_bitrate':
		case 'set_play_quality':
		case 'set_play_monitor':
		case 'set_frame_pacing':
		case 'set_stats_hud':
		case 'set_overlay_button':
		case 'set_overlay_button_pos':
		case 'reverse_play':
		case 'set_window_fullscreen':
		case 'input_pointer':
		case 'input_button':
		case 'input_scroll':
		case 'input_key':
		case 'input_char':
		case 'kbd_capture_start':
		case 'kbd_capture_stop':
		case 'kbd_engage':
		case 'native_view_rect':
		case 'send_clipboard':
		case 'send_chat':
		case 'host_send_chat':
		case 'send_file':
		case 'send_file_path':
		case 'fs_list':
		case 'fs_get':
		case 'mic_start':
		case 'mic_stop':
		case 'set_language':
			return Promise.resolve(undefined as unknown as T);
		case 'local_ls':
			// Deterministic sample listing so the file panel is browsable in the
			// browser preview (dirs first, alphabetical — like the real command).
			return Promise.resolve(
				(args?.path
					? []
					: [
							{ name: 'Belgeler', dir: true, size: 0 },
							{ name: 'İndirilenler', dir: true, size: 0 },
							{ name: 'notlar.txt', dir: false, size: 2048 }
						]) as unknown as T
			);
		default:
			return Promise.reject(new Error(`unknown command: ${cmd}`));
	}
}
