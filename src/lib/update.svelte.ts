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
