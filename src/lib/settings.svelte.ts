// Locally-persisted UI/host preferences — including the video encoding selection.
// (Relay + network mode live in the core Config; these display/security/general
// prefs persist here until the streaming backend consumes them.)

export type VideoCodec = 'auto' | 'h264' | 'h265' | 'av1';
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
	bwlimit: boolean;
	unattended: boolean;
	twofa: boolean;
	record: boolean;
	startup: boolean;
	tray: boolean;
	/** Show the verbose host activity log under "connected devices". */
	debug: boolean;
	/** Keep the in-session control handle/menu visible at all times (even fullscreen /
	 * while controlling). Default on; turn off to let it auto-hide while controlling. */
	keepVisible: boolean;
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
	bwlimit: false,
	unattended: true,
	twofa: true,
	record: false,
	startup: true,
	tray: true,
	debug: false,
	keepVisible: true,
	framePacing: true,
	statsHud: false,
	overlayButton: true,
	overlayBtnPos: { x: 90, y: 70 }
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

export function saveUi() {
	if (hasLS) localStorage.setItem(KEY, JSON.stringify($state.snapshot(ui)));
	saveTick.n++;
}
