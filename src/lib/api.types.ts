// Shared types for the SvelteKit ↔ Rust core (pulsar-core) bridge. Pure type
// definitions, re-exported by `api.ts` so importers are unaffected.

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
	/** Loopback WebSocket port the webview opens to receive the Opus audio (0 = none). */
	audio_ws_port: number;
	/** True if the host is this same machine — control is disabled (feedback loop). */
	local: boolean;
	/** True when the native ffplay renderer is in use (no webview video canvas). */
	native: boolean;
	/** True when the Linux single-surface renderer is active (video in a GLArea behind
	 * this webview); the session screen must be transparent to show it through. */
	embedded: boolean;
}

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

export interface SessionEvent {
	kind: string;
	peer: string;
	detail: string;
}

/** A host is asking this client for a password — show a prompt and reply via
 * `api.submitPassword(req, …)`. */
export interface AuthPrompt {
	req: number;
	peer: string;
}
