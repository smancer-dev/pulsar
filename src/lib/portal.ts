// Svelte action: relocate a node (e.g. a modal's backdrop) to document.body so its
// `position: fixed` / centering is relative to the VIEWPORT, never trapped by an ancestor
// that establishes a containing block (a split pane's `position:relative` / any
// transform/contain/will-change). Without this, a modal opened from inside a split pane is
// confined to that pane and clips (e.g. the connect numpad in a stacked half-height pane).
// The node stays owned by Svelte (reactivity + events intact) — it's just re-parented.
export function portal(node: HTMLElement) {
	const target = typeof document !== 'undefined' ? document.body : null;
	if (target) target.appendChild(node);
	return {
		destroy() {
			if (target && node.parentNode === target) target.removeChild(node);
		}
	};
}
