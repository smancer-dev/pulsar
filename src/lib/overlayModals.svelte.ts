// Tracks how many webview modals (the ones portaled to <body> — the gaming connect numpad, the
// generic Modal) are currently open. The split-mode shell reads this to OCCLUDE every pane's
// native render window while a modal is up: on Linux the native video composites OVER the webview,
// so a live pane's video would otherwise hide a modal opened in ANOTHER pane (e.g. the connect
// numpad in an empty pane while a neighbour streams). Components increment on mount / decrement on
// destroy; `modalCount.n > 0` ⇒ hide the native renderers (Session.svelte reports a 0×0 rect).
export const modalCount = $state({ n: 0 });

/** A portaled modal appeared. */
export function openModal() {
	modalCount.n += 1;
}

/** A portaled modal was removed (clamped at 0 so an extra close can't drive it negative). */
export function closeModal() {
	modalCount.n = Math.max(0, modalCount.n - 1);
}
