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
}

export type Transport = 'direct' | 'relay';

export interface ConnInfo {
	transport: Transport;
	peer: string;
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
	kind: string;
	label: string;
}

export interface SessionStats {
	fps: number;
	latency: number;
	bitrate: number;
}
