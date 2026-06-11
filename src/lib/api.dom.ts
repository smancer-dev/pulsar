// Browser/OS-window helpers used by the UI: clipboard read/write, fullscreen, and
// frameless window controls. Re-exported by `api.ts`.

import { invoke, isTauri } from './api.invoke';

/** Copy text to the clipboard. Uses the async Clipboard API, falling back to a
 * hidden textarea + execCommand for webviews that block it. Returns success. */
export async function copyText(text: string): Promise<boolean> {
	// App-side first: the DOM Clipboard API silently fails in an occluded/unfocused
	// WebKitGTK (the Linux native-video path), so in Tauri the OS clipboard is owned
	// by Rust (io_cmds.rs). The DOM paths remain for browser dev.
	if (isTauri) {
		try {
			await invoke<void>('write_clipboard_text', { text });
			return true;
		} catch {
			/* fall through to the DOM paths */
		}
	}
	try {
		if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
			await navigator.clipboard.writeText(text);
			return true;
		}
	} catch {
		/* fall through to the legacy path */
	}
	try {
		const ta = document.createElement('textarea');
		ta.value = text;
		ta.setAttribute('readonly', '');
		ta.style.position = 'fixed';
		ta.style.top = '-1000px';
		ta.style.opacity = '0';
		document.body.appendChild(ta);
		ta.select();
		const ok = document.execCommand('copy');
		document.body.removeChild(ta);
		return ok;
	} catch {
		return false;
	}
}

/** Toggle true, taskbar-covering fullscreen (no-op outside Tauri). */
export async function setFullscreen(on: boolean): Promise<void> {
	if (!isTauri) return;
	// Done Rust-side: manual monitor-cover + always-on-top, which reliably hides the
	// taskbar for our transparent window (plain setFullscreen leaves it visible).
	await invoke<void>('set_window_fullscreen', { on });
}

/** Read the local clipboard text (for "paste to remote"). */
export async function readClipboard(): Promise<string> {
	// App-side first — see copyText (the DOM read is dead in occluded WebKitGTK).
	if (isTauri) {
		try {
			return await invoke<string>('read_clipboard_text');
		} catch {
			/* fall through to the DOM path */
		}
	}
	try {
		if (typeof navigator !== 'undefined' && navigator.clipboard?.readText) {
			return await navigator.clipboard.readText();
		}
	} catch {
		/* ignore */
	}
	return '';
}

/** Control the frameless OS window (no-op outside Tauri, e.g. browser dev). */
export async function windowControl(action: 'minimize' | 'maximize' | 'close') {
	if (!isTauri) return;
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	const w = getCurrentWindow();
	if (action === 'minimize') await w.minimize();
	else if (action === 'maximize') await w.toggleMaximize();
	else await w.close();
}
