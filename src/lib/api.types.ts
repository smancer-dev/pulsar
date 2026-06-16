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
	/** The HOST's validated stream caps: codecs + encoder backends it can really emit
	 * (QueryStreamCaps). Empty = unknown (old host / timeout) — no menu gating then. */
	host_codecs: string[];
	host_encoders: string[];
	/** The host's streamable monitors (primary at index 0). The session menu lists
	 * these so the user can pick which screen to view. Empty = no picker. */
	host_displays: HostDisplay[];
	/** This client's own decodable codecs (startup probe). */
	client_codecs: string[];
}

/** One host monitor advertised by QueryStreamCaps. `idx` is what `setPlayMonitor`
 * echoes back (0 = primary); `name` is human-facing; `width`/`height` are pixels. */
export interface HostDisplay {
	idx: number;
	name: string;
	width: number;
	height: number;
	primary: boolean;
}

/** A side-channel text message: `peer` is the host-side peer id, or (for client
 * events) the play id as a string. */
export interface DataText {
	peer: string;
	text: string;
}

/** A finished (or failed) inbound file transfer, host side — and client side for
 * file-manager downloads (`peer` = play id as string then).
 * `xferId` is the host-assigned transfer id (mirrors `FileBegin.xferId`) so the
 * client UI can key pending completions by transfer id rather than by filename,
 * preventing a timed-out same-name download from draining a different in-flight
 * download's concurrency slot (C21 fix). */
export interface FileRecv {
	peer: string;
	name: string;
	bytes: number;
	ok: boolean;
	xferId: number;
}

/** Emitted to the client UI when the host starts streaming a file-manager download
 * (`FileBegin` datagram received). `peer` = play id as string. Signals that the
 * concurrency slot must NOT be released on the short wall-clock timeout — the
 * transfer is legitimately in flight.
 * `xferId` is the host-assigned transfer id used to associate this event with
 * the queued `download()` call for the same filename (C21 fix). */
export interface FileBegin {
	peer: string;
	name: string;
	xferId: number;
}

/** One entry of a directory listing (file manager). Same JSON shape for the
 * remote (`fs-entries` event) and local (`local_ls`) sides. */
export interface FsEntry {
	name: string;
	dir: boolean;
	/** Byte size for files; 0 for directories. */
	size: number;
}

/** A host directory listing for the file panel (the `fs-entries` event):
 * the play id it belongs to, the echoed HOME-relative path, and its entries
 * (dirs first, alphabetical; empty = rejected/unreadable path). */
export interface FsEntries {
	id: number;
	path: string;
	entries: FsEntry[];
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

/** Startup-probed local capabilities (the `local-caps` event / `local_caps` command). */
export interface LocalCaps {
	platform: 'linux' | 'windows' | 'macos';
	encoders: { id: string; backend: 'ffmpeg' | 'gst'; codecs: string[] }[];
	decoders: { codec: string; ok: boolean; name: string; hw: boolean; tier: string }[];
}
