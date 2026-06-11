// Startup-probed LOCAL capabilities (the `local-caps` event / `local_caps` command),
// shared as a reactive store: Settings disables encoder/codec options that this
// machine can't actually use, per the maintainer's rule — show ONLY the current
// platform's families, DISABLE the platform-native-but-unavailable ones, and keep
// software always available as the universal fallback.

import { api, onLocalCaps } from '$lib/api';
import type { LocalCaps } from '$lib/api.types';

export const caps = $state<{ v: LocalCaps | null }>({ v: null });

let started = false;

/** Start listening for the startup probe result (idempotent; call from the shell). */
export function initCaps() {
	if (started || typeof window === 'undefined') return;
	started = true;
	api
		.localCaps()
		.then((c) => {
			if (c) caps.v = c;
		})
		.catch(() => {});
	onLocalCaps((c) => (caps.v = c));
}

/** Is this encoder family usable on this machine? Unknown caps (probe still
 * running / old backend) → everything enabled; `auto`/`software` always are. */
export function encoderAvailable(id: string): boolean {
	if (id === 'auto' || id === 'software') return true;
	const c = caps.v;
	if (!c) return true;
	return c.encoders.some((e) => e.id === id);
}

/** Can this machine DECODE the codec (client role)? `auto` always allowed. */
export function decodeAvailable(codec: string): boolean {
	if (codec === 'auto') return true;
	const c = caps.v;
	if (!c) return true;
	return c.decoders.some((d) => d.codec === codec && d.ok);
}
