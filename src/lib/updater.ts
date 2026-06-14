import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

/**
 * Silently check for a newer release on this build's channel and, if found,
 * download + install + relaunch. Appliance/kiosk UX: no prompt. NEVER call this
 * while a session is live or about to auto-connect — it must not interrupt
 * remote control. Any failure (offline, signature, download, no Tauri context)
 * is swallowed so a broken updater can never block launch.
 */
export async function silentUpdateCheck(): Promise<void> {
	try {
		const update = await check();
		if (!update) return; // already up to date
		await update.downloadAndInstall();
		// Installed; relaunch into the new version.
		await relaunch();
	} catch (e) {
		// Offline / no endpoint / signature mismatch / browser mock: log + continue.
		// Never throw into launch.
		console.warn('[updater] skipped:', e);
	}
}
