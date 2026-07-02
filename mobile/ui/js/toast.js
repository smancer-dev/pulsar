/**
 * toast.js — a single global toast, usable from any screen and in-session
 * (screen-local `showToast`s only work on their own screen). Reuses the `.toast`
 * component style; creates its element lazily on first use.
 */

let _el = null;
let _timer = null;

/** Show a brief message at the bottom of the screen. */
export function toast(msg, durationMs = 2600) {
	if (!_el) {
		_el = document.createElement('div');
		_el.className = 'toast';
		_el.id = 'global-toast';
		_el.setAttribute('role', 'status');
		_el.setAttribute('aria-live', 'polite');
		document.body.appendChild(_el);
	}
	_el.textContent = msg;
	// Force a reflow so re-showing an already-visible toast re-triggers the transition.
	void _el.offsetWidth;
	_el.classList.add('show');
	clearTimeout(_timer);
	_timer = setTimeout(() => { if (_el) _el.classList.remove('show'); }, durationMs);
}
