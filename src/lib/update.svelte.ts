// Update-available state shared by the chrome badge, the update modal and the
// updater flow (updater.ts writes it). Deliberately UI-side: whether an update
// exists / is installing is presentation state, not core config.

import type { Update } from '@tauri-apps/plugin-updater';

export type UpdatePhase = 'idle' | 'downloading' | 'installing' | 'restarting' | 'error';

class UpdateStore {
	/** A newer release exists on this build's channel. Stays true even when this
	 * install can't self-update (package manager / non-FUSE AppImage) — the user
	 * must still SEE that they're outdated. */
	available = $state(false);
	/** Currently running version (no leading v). */
	from = $state('');
	/** Version offered by the manifest. */
	to = $state('');
	/** Release notes from the update manifest (plain text / markdown-ish). */
	notes = $state('');
	/** Whether THIS install can self-update (false → flatpak/package-manager/raw
	 * binary: the install button is disabled and manual instructions show). */
	installable = $state(true);
	phase = $state<UpdatePhase>('idle');
	/** Download progress in bytes (total may be 0 when the server omits Content-Length). */
	received = $state(0);
	total = $state(0);
	error = $state('');
	/** Update modal visibility (badge click opens it). */
	open = $state(false);
	/** The plugin's Update handle for the pending update (non-reactive — it's a
	 * resource wrapper, not display state). */
	handle: Update | null = null;

	get progressPct(): number {
		return this.total > 0 ? Math.min(100, Math.round((this.received / this.total) * 100)) : 0;
	}
}

export const update = new UpdateStore();

// Dev-only design hook: drive the badge/modal states from the browser console in the
// `vite dev` mock (no Tauri, no real updater), e.g.
//   __pulsarUpdate.mock('downloading')   // 'available' | 'downloading' | 'installing' |
//                                        // 'restarting' | 'error' | 'noSelfUpdate' | 'off'
if (import.meta.env.DEV && typeof window !== 'undefined') {
	(window as unknown as { __pulsarUpdate: unknown }).__pulsarUpdate = {
		store: update,
		mock(state: string) {
			update.from = '0.7.3';
			update.to = '0.8.0';
			update.notes =
				'• Windows: encoder fallback chain — a dead NVENC/AMF pick no longer leaves a black screen\n' +
				'• Viewer shows why there is no video (host verdict, first-frame watchdog)\n' +
				'• Daily log file, openable from Settings → General\n' +
				'• macOS: bundled ffplay fallback when mpv is not installed';
			update.installable = state !== 'noSelfUpdate';
			update.error = state === 'error' ? 'signature verification failed (updater-latest/latest.json)' : '';
			update.received = state === 'downloading' ? 37 * 1024 * 1024 : 0;
			update.total = state === 'downloading' ? 86 * 1024 * 1024 : 0;
			update.phase =
				state === 'downloading' || state === 'installing' || state === 'restarting' || state === 'error'
					? state
					: 'idle';
			update.available = state !== 'off';
			update.open = state !== 'off' && state !== 'badge';
		}
	};
	// `?mockUpdate=<state>` applies a state on load (headless screenshots / design review).
	const q = new URLSearchParams(window.location.search).get('mockUpdate');
	if (q) (window as unknown as { __pulsarUpdate: { mock(s: string): void } }).__pulsarUpdate.mock(q);
}
