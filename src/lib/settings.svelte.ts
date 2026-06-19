// Locally-persisted UI/host preferences — including the video encoding selection.
// (Relay + network mode live in the core Config; these display/security/general
// prefs persist here until the streaming backend consumes them.)

export type VideoCodec = 'auto' | 'h264' | 'h265' | 'av1';
export type RumbleStrength = 'off' | 'weak' | 'medium' | 'strong';
export type Encoder =
	| 'auto'
	| 'nvenc'
	| 'qsv'
	| 'amf'
	| 'videotoolbox'
	| 'vaapi'
	| 'rkmpp'
	| 'software';

export interface UiSettings {
	quality: string;
	res: string;
	fps: number;
	/** Game-overlay video bitrate in Mbit (0 = automatic / host default). */
	bitrate: number;
	codec: VideoCodec;
	/** Host-side hardware encoder. */
	encoder: Encoder;
	hdr: boolean;
	unattended: boolean;
	twofa: boolean;
	record: boolean;
	tray: boolean;
	/** Show the verbose host activity log under "connected devices". */
	debug: boolean;
	/** Moonlight-style frame pacing on the Linux native renderer: buffer ~1-2 frames and
	 * present at a steady cadence to smooth network/decode jitter (slightly higher latency).
	 * Off = present newest immediately (lowest latency). */
	framePacing: boolean;
	/** Always-on mini stats HUD over the video while the overlay is closed. */
	statsHud: boolean;
	/** Parsec-style always-visible overlay-open button (Pulsar mark) over the video. */
	overlayButton: boolean;
	/** Overlay-open button position — egui POINTS from the video's top-left (the
	 * renderer draws there; the webview hotspot mirrors it as CSS px ×1.25).
	 * Drag-movable in-session; default mirrors overlay.rs BTN_POS_DEFAULT. */
	overlayBtnPos: { x: number; y: number };
	/** Feature 1: whether to forward physical controllers to the host (both remote
	 * and game mode). When false the client reads controllers locally but does not
	 * send Gamepad frames. Default true. */
	forwardControllers: boolean;
	/** Feature 3: stable ordering of controllers by gilrs uuid hex. Index = player
	 * slot (0=Player1 …). Unknown UUIDs are appended at the end. Default [] (natural
	 * gilrs enumeration order). */
	controllerOrder: string[];
	/** Per-controller emulation target keyed by gilrs uuid hex: 'auto' (resolve from
	 * detected kind), 'xbox' (Xbox 360), or 'ds4' (DualShock 4). Absent = 'auto'. */
	controllerEmulation: Record<string, 'auto' | 'xbox' | 'ds4'>;
	/** PER-CONTROLLER vibration (rumble) strength, keyed by gilrs/SDL uuid hex: the host's
	 * motor magnitudes are scaled per pad on the client (off = motors stay still, strong =
	 * full). Pushed to the SDL pad manager via `set_controller_rumble`. Absent uuid =
	 * 'medium'. */
	controllerRumble: Record<string, RumbleStrength>;
	/** Per-controller DISABLED flag keyed by uuid hex (true = off). A disabled pad isn't
	 * forwarded to the host and doesn't rumble. Absent = enabled. */
	controllerDisabled: Record<string, boolean>;
	/** Feature 2 Piece B: treat the DS4/DS5 touchpad as a relative mouse (pointer
	 * moves + left-click). Default true. */
	touchpadAsMouse: boolean;
	/** App-level personality (persisted): 'remote' = the general remote-desktop app
	 * (left sidebar, host capability); 'game' = the controller-first game-streaming
	 * shell (bottom dock, centered ID + numpad, no hosting). Toggled from the top bar;
	 * the CLI `--mode game` overrides it on launch. Default 'remote'. */
	appMode: 'remote' | 'game';
	/** Whether GAMING mode was left in fullscreen — persisted so the app reopens
	 * fullscreen in game mode the way the user last had it (the bottom-dock button /
	 * F11 toggle writes this; restored at startup only when appMode==='game'). Scoped
	 * to game mode so a fullscreen remote session never reopens fullscreen. Default false. */
	gamingFullscreen: boolean;
}

export const CODECS: { value: VideoCodec; label: string }[] = [
	{ value: 'auto', label: 'Otomatik' },
	{ value: 'h264', label: 'H.264' },
	{ value: 'h265', label: 'H.265' },
	{ value: 'av1', label: 'AV1' }
];

export const ENCODERS: { value: Encoder; label: string }[] = [
	{ value: 'auto', label: 'Otomatik (en iyi donanım)' },
	{ value: 'nvenc', label: 'NVIDIA NVENC' },
	{ value: 'qsv', label: 'Intel QuickSync' },
	{ value: 'amf', label: 'AMD AMF' },
	{ value: 'videotoolbox', label: 'Apple VideoToolbox' },
	{ value: 'vaapi', label: 'VA-API (Linux)' },
	{ value: 'rkmpp', label: 'Rockchip MPP' },
	{ value: 'software', label: 'Yazılım (CPU)' }
];

export const EMULATION_TARGETS = [
	{ value: 'auto', label: 'Otomatik' },
	{ value: 'xbox', label: 'Xbox 360' },
	{ value: 'ds4', label: 'DualShock 4' }
] as const;

export const RUMBLE_LEVELS: { value: RumbleStrength; label: string }[] = [
	{ value: 'off', label: 'Kapalı' },
	{ value: 'weak', label: 'Zayıf' },
	{ value: 'medium', label: 'Orta' },
	{ value: 'strong', label: 'Güçlü' }
];

/** Per-platform encoder families: a platform shows ONLY its own backends (foreign
 * entries never render); availability within the family is a separate, probe-driven
 * disabled state (see `caps.svelte.ts`). `auto` + `software` exist everywhere —
 * software is the universal terminal fallback. */
const PLATFORM_ENCODERS: Record<string, Encoder[]> = {
	linux: ['auto', 'nvenc', 'vaapi', 'qsv', 'rkmpp', 'software'],
	windows: ['auto', 'nvenc', 'amf', 'qsv', 'software'],
	macos: ['auto', 'videotoolbox', 'software']
};

export function encodersForPlatform(platform: string): { value: Encoder; label: string }[] {
	const family = PLATFORM_ENCODERS[platform] ?? PLATFORM_ENCODERS.linux;
	return ENCODERS.filter((e) => family.includes(e.value));
}

const DEFAULTS: UiSettings = {
	quality: 'auto',
	res: '1440p',
	fps: 60,
	bitrate: 0,
	codec: 'auto',
	encoder: 'auto',
	hdr: false,
	unattended: true,
	twofa: true,
	record: false,
	tray: true,
	debug: false,
	framePacing: true,
	statsHud: false,
	overlayButton: true,
	overlayBtnPos: { x: 90, y: 70 },
	forwardControllers: true,
	controllerOrder: [],
	controllerEmulation: {},
	controllerRumble: {},
	controllerDisabled: {},
	touchpadAsMouse: true,
	appMode: 'remote',
	gamingFullscreen: false
};

const KEY = 'pulsar.ui.v1';
const hasLS = typeof localStorage !== 'undefined';

function load(): UiSettings {
	if (!hasLS) return { ...DEFAULTS };
	try {
		const raw = localStorage.getItem(KEY);
		if (!raw) return { ...DEFAULTS };
		const saved = JSON.parse(raw) as Partial<UiSettings> & { decoder?: string };
		delete saved.decoder; // removed: the decoder is auto-selected and shown read-only
		// 'quicksync' was the old UI value; the wire/host vocabulary is 'qsv'.
		if ((saved.encoder as string) === 'quicksync') saved.encoder = 'qsv';
		return { ...DEFAULTS, ...saved };
	} catch {
		return { ...DEFAULTS };
	}
}

export const ui = $state<UiSettings>(load());

// A monotonically-rising tick bumped on every successful save. The Settings
// screen watches it (via $effect) to surface the "saved" toast — the tabs call
// saveUi() directly, so this is the only channel back up to the screen.
export const saveTick = $state({ n: 0 });

// A separate tick bumped ONLY when the CORE config (relay/unattended/avatar/audio…)
// is persisted — distinct from saveTick, which fires on every UI-only twiddle too
// (overlay-button drag, in-session stream controls). The shell (+page) re-fetches its
// config copy off THIS, so it doesn't churn IPC on UI-only saves.
export const configTick = $state({ n: 0 });

export function saveUi() {
	if (hasLS) localStorage.setItem(KEY, JSON.stringify($state.snapshot(ui)));
	saveTick.n++;
}

/** Return the player-slot index (0-based) for the given controller UUID.
 * Reads the live `ui.controllerOrder` permutation so reorders apply without a
 * page reload. Returns `ui.controllerOrder.length` (i.e. "append") for UUIDs
 * not yet in the list — callers should treat that as "last slot + 1". */
export function slotOf(uuid: string): number {
	const i = ui.controllerOrder.indexOf(uuid);
	return i >= 0 ? i : ui.controllerOrder.length;
}

/**
 * Swap two slot indices in a controller order array. Pure helper — no side effects.
 * Used by the overlay `ctrlswap` handler (Session.svelte) so the logic can be
 * unit-tested without a DOM or Tauri context.
 *
 * @param order  - Current order array (UUIDs at each slot position). Mutated in place.
 * @param seedFn - Called for each slot index that is missing from `order`; should
 *                 return the UUID of the pad that currently occupies that slot, or ''
 *                 if unknown. Called once per missing slot, in ascending slot order.
 * @param i      - First slot index to swap.
 * @param j      - Second slot index to swap.
 * @returns true if the swap was applied, false if i === j or indices are invalid.
 */
export function applyCtrlSwap(
	order: string[],
	seedFn: (slot: number) => string,
	i: number,
	j: number
): boolean {
	if (!Number.isFinite(i) || !Number.isFinite(j) || i === j || i < 0 || j < 0) return false;
	const maxSlot = Math.max(i, j);
	while (order.length <= maxSlot) {
		order.push(seedFn(order.length));
	}
	[order[i], order[j]] = [order[j], order[i]];
	return true;
}
