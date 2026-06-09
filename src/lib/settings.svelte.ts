// Locally-persisted UI/host preferences — including the video encoding selection.
// (Relay + network mode live in the core Config; these display/security/general
// prefs persist here until the streaming backend consumes them.)

export type VideoCodec = 'auto' | 'h264' | 'h265' | 'av1';
export type Encoder =
	| 'auto'
	| 'nvenc'
	| 'quicksync'
	| 'amf'
	| 'videotoolbox'
	| 'vaapi'
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
	/** Client-side hardware decoder (same families as encoders). */
	decoder: Encoder;
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
	{ value: 'quicksync', label: 'Intel QuickSync' },
	{ value: 'amf', label: 'AMD AMF' },
	{ value: 'videotoolbox', label: 'Apple VideoToolbox' },
	{ value: 'vaapi', label: 'VA-API (Linux)' },
	{ value: 'software', label: 'Yazılım (CPU)' }
];

// Decode uses the same hardware families.
export const DECODERS: { value: Encoder; label: string }[] = ENCODERS;

const DEFAULTS: UiSettings = {
	quality: 'auto',
	res: '1440p',
	fps: 60,
	bitrate: 0,
	codec: 'auto',
	encoder: 'auto',
	decoder: 'auto',
	hdr: false,
	bwlimit: false,
	unattended: true,
	twofa: true,
	record: false,
	startup: true,
	tray: true,
	debug: false,
	keepVisible: true,
	framePacing: true
};

const KEY = 'pulsar.ui.v1';
const hasLS = typeof localStorage !== 'undefined';

function load(): UiSettings {
	if (!hasLS) return { ...DEFAULTS };
	try {
		const raw = localStorage.getItem(KEY);
		return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as Partial<UiSettings>) } : { ...DEFAULTS };
	} catch {
		return { ...DEFAULTS };
	}
}

export const ui = $state<UiSettings>(load());

export function saveUi() {
	if (hasLS) localStorage.setItem(KEY, JSON.stringify($state.snapshot(ui)));
}
