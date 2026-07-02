/**
 * tauri.js — single chokepoint over window.__TAURI__
 *
 * The "Tauri yok" guard lives here and nowhere else. All other modules
 * import { invoke, listen, hasTauri } from './tauri.js'.
 *
 * Tauri config: withGlobalTauri:true — so window.__TAURI__ is available
 * synchronously once the webview starts. No bundler; plain ES module.
 */

/** @type {boolean} */
export const hasTauri = !!(window.__TAURI__ && window.__TAURI__.core);

const _core  = hasTauri ? window.__TAURI__.core  : null;
const _event = hasTauri ? window.__TAURI__.event : null;

/**
 * Invoke a Tauri command.
 * @template T
 * @param {string} cmd
 * @param {object} [args]
 * @returns {Promise<T>}
 */
export async function invoke(cmd, args) {
	if (!_core) {
		// In a plain browser (dev/test without Tauri) — log and return a mock null.
		console.warn('[tauri] window.__TAURI__ yok — invoke("' + cmd + '") atlandı');
		return null;
	}
	return _core.invoke(cmd, args);
}

/**
 * Subscribe to a Tauri event.
 * @param {string} name
 * @param {(payload: any) => void} cb
 * @returns {Promise<() => void>} unlisten function
 */
export async function listen(name, cb) {
	if (!_event) {
		console.warn('[tauri] window.__TAURI__ yok — listen("' + name + '") atlandı');
		return () => {};
	}
	return _event.listen(name, (e) => cb(e.payload));
}

/**
 * Write text to the system clipboard (falls back to navigator.clipboard).
 * @param {string} text
 * @returns {Promise<void>}
 */
export async function clipboard(text) {
	try {
		if (navigator.clipboard && navigator.clipboard.writeText) {
			await navigator.clipboard.writeText(text);
			return;
		}
	} catch (_) { /* fall through */ }
	console.warn('[tauri] clipboard yazma desteklenmiyor');
}

/**
 * Native share sheet (falls back to clipboard copy).
 * @param {{ title?: string, text?: string, url?: string }} data
 * @returns {Promise<void>}
 */
export async function share(data) {
	if (navigator.share) {
		try { await navigator.share(data); return; } catch (_) { /* user cancelled */ }
	}
	if (data.text) await clipboard(data.text);
}
