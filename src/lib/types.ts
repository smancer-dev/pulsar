// Shapes mirrored from pulsar-core (serde JSON). Keep field names in sync with
// the Rust `Config` (snake_case) and `NetworkMode` (kebab-case) serialization.

export type NetworkMode = 'auto' | 'p2p-only' | 'relay-only';
export type Language = 'tr' | 'en';

export interface Config {
	relay: string;
	network_mode: NetworkMode;
	device_name: string;
	language: Language;
	unattended_access: boolean;
	/** Stream this host's audio to the client (host → client). */
	transmit_audio: boolean;
	/** Silence this host's local speakers while streaming. */
	mute_host_audio: boolean;
	/** Audio capture source override (empty = platform default). */
	audio_input: string;
	/** Local node listen port for direct/P2P (0 = pick automatically). */
	node_port: number;
	/** Identity presented to peers: 'user' (OS photo) | 'wallpaper' | 'anonymous'. */
	avatar_mode: string;
	/** Use the native (ffplay) hardware-decoded renderer instead of the webview. */
	native_player: boolean;
}

export type Transport = 'direct' | 'relay';

export interface ConnInfo {
	transport: Transport;
	peer: string;
}

/** CLI-parsed auto-connect target (from `pulsar --connect …`), widened to carry
 * the session mode and target app. `mode` defaults to 'remote'; `app` is the
 * host app/game id-or-name ('' = Desktop). Returned by `auto_connect_target`. */
export interface AutoConnectTarget {
	id: string;
	pw: string;
	mode: 'remote' | 'game';
	app: string;
}

export type DeviceCategory = 'pc' | 'server' | 'console';

export interface Device {
	name: string;
	id: string;
	cat: DeviceCategory;
	online: boolean;
	fav: boolean;
	lastSeen: string;
}

export interface ControllerInfo {
	/** Positional index in the backend list. */
	index: number;
	/** OS/driver-reported name, e.g. "Wireless Controller". */
	name: string;
	/** Detected family tag, e.g. "Ds4" / "Xbox". */
	kind: string;
	/** Human label, e.g. "DualShock 4". */
	label: string;
	/** Connected + forwardable right now. */
	connected: boolean;
}

export interface SessionStats {
	fps: number;
	latency: number;
	bitrate: number;
}
