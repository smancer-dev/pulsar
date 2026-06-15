import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { api } from './api.commands';

/**
 * Silently check for a newer release on this build's channel and, if found,
 * download + install + relaunch. Appliance/kiosk UX: no prompt. NEVER call this
 * while a session is live or about to auto-connect — it must not interrupt
 * remote control. Any failure (offline, signature, download, no Tauri context)
 * is swallowed so a broken updater can never block launch.
 *
 * `timeoutMs` bounds only the manifest HTTP request (the Tauri plugin applies it
 * to `check()`). `overallTimeoutMs` bounds the ENTIRE flow including the
 * download+install phase, which the plugin otherwise runs with no timeout — so a
 * stalled download can't hang a boot path that sequences `connect` after this.
 *
 * `isBusy` is re-evaluated at two points inside `run()`: (1) after the slow
 * `check()` round-trip, before committing to `downloadAndInstall()`; and (2)
 * immediately before `relaunch()`, after the download completes. This covers the
 * boot path (where `overallTimeoutMs` causes `Promise.race` to reject and boot to
 * continue, but the still-running `run()` promise finishes the download later) and
 * the idle path (where there is no overall cap but a download can be slow). If a
 * session is live at either guard point, the update is abandoned — the next boot
 * will pick it up. The installed-but-not-relaunched state is safe: the new binary
 * is in place and will be used on the next normal launch.
 */
export async function silentUpdateCheck(opts?: {
	timeoutMs?: number;
	overallTimeoutMs?: number;
	isBusy?: () => boolean;
}): Promise<void> {
	const run = async () => {
		// On Linux the updater self-replaces the file pointed to by $APPIMAGE. When the
		// AppImage is launched without FUSE (e.g. `--appimage-extract-and-run`, or the raw
		// `--no-bundle` dev binary), $APPIMAGE is unset and the plugin falls back to a
		// throwaway extracted temp binary — so downloadAndInstall() would silently rewrite a
		// temp file (or error) instead of the deployed AppImage, leaving the appliance stuck
		// on the old version while the updater appears to "succeed". Skip loudly instead.
		if (!(await api.selfUpdatePossible().catch(() => true))) {
			console.warn(
				'[updater] skipped: self-update unavailable (running without $APPIMAGE — ' +
					'launch the AppImage with FUSE, not --appimage-extract-and-run, to enable updates)'
			);
			return;
		}
		const update = await check(opts?.timeoutMs ? { timeout: opts.timeoutMs } : undefined);
		if (!update) return; // already up to date
		// A session may have started during the (slow) manifest fetch above. Re-check
		// before committing to the download so we don't interrupt remote control.
		if (opts?.isBusy?.()) {
			console.warn('[updater] skipped: a session started during the update check');
			return;
		}
		await update.downloadAndInstall();
		// Re-check busy state BEFORE the destructive relaunch: the download may have
		// taken longer than overallTimeoutMs (boot path) or longer than the user took
		// to start a session (idle path). Either way, if a session is now live we must
		// NOT tear it down — defer silently and let the next boot pick up the update.
		if (opts?.isBusy?.()) {
			console.warn('[updater] update downloaded but a session is live — deferring relaunch to next boot');
			return;
		}
		// Relaunch into the new version. macOS/Linux only: on Windows
		// downloadAndInstall() spawns the (now `quiet`) NSIS installer and calls
		// process::exit(0), so it never returns here — the installer's /R flag
		// auto-relaunches the app. This line is therefore unreachable on Windows
		// (harmless) and is the actual relaunch path on macOS/Linux.
		await relaunch();
	};
	try {
		if (opts?.overallTimeoutMs) {
			// Cap the whole check+download+install so a stalled download can never
			// block boot. On timeout we give up on this update and continue launch.
			await Promise.race([
				run(),
				new Promise<void>((_, reject) =>
					setTimeout(
						() => reject(new Error(`updater timed out after ${opts.overallTimeoutMs}ms`)),
						opts.overallTimeoutMs
					)
				)
			]);
		} else {
			await run();
		}
	} catch (e) {
		// Offline / no endpoint / signature mismatch / download stall / browser mock:
		// log + continue. Never throw into launch.
		console.warn('[updater] skipped:', e);
	}
}
