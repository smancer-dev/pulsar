// sheet-swipe.js — global swipe-down-to-dismiss for bottom sheets, app-wide.
//
// Every bottom sheet renders a grab handle (.sheet-handle) but only a backdrop tap
// closed them — dragging the handle did nothing. This wires the gesture ONCE at the
// document level (event delegation), so every `.sheet.open` (host approval / a11y,
// devices add/edit, session rename/auth, split pickers, …) — current or future —
// gets it without per-screen code.
//
// On release past the threshold it triggers the sheet's OWN dismiss by firing the
// backdrop's native close (so side effects like "deny the request" or overlay
// cleanup still run), then force-closes as a safety net. Shorter drags snap back.
// Touches that start on an interactive element (button/input/…) are ignored so taps
// and typing still work.

const THRESH = 90; // px of downward drag needed to dismiss

let sheet = null;
let startY = 0;
let dy = 0;

/** The backdrop/overlay element that closes `s`, or null. */
function backdropFor(s) {
	// devices.js openSheet: the sheet is wrapped in a .sheet-overlay that itself
	// closes on a pointerdown whose target is the overlay.
	const overlay = s.closest('.sheet-overlay');
	if (overlay) return overlay;
	// host / split / session: a separate open backdrop element (sibling) that closes
	// on click. Only one sheet is modal at a time, so the open one is ours.
	return document.querySelector(
		'.sheet-backdrop.open, .sheet-overlay.open, [id$="backdrop"].open'
	);
}

function dismiss(s) {
	const b = backdropFor(s);
	if (b) {
		// Different sheets listen on click vs pointerdown — fire both (idempotent).
		try { b.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true })); } catch (_) {}
		try { b.click(); } catch (_) {}
	}
	// Safety net: ensure it is visually closed even if no handler matched.
	s.classList.remove('open');
	if (b) b.classList.remove('open');
}

document.addEventListener('touchstart', (e) => {
	const s = e.target.closest('.sheet.open');
	if (!s) return;
	// Don't hijack taps/typing on interactive children.
	if (e.target.closest('button, a, input, textarea, select, [role="button"], .icon-chip, .chip')) return;
	sheet = s;
	startY = e.touches[0].clientY;
	dy = 0;
	sheet.style.transition = 'none';
}, { passive: true });

document.addEventListener('touchmove', (e) => {
	if (!sheet) return;
	dy = Math.max(0, e.touches[0].clientY - startY); // downward only
	sheet.style.transform = `translateY(${dy}px)`;
}, { passive: true });

function end() {
	if (!sheet) return;
	const s = sheet;
	sheet = null;
	s.style.transition = 'transform 0.25s ease';
	if (dy > THRESH) {
		// Past threshold → finish the slide down, THEN dismiss (so no upward flash).
		s.style.transform = 'translateY(100%)';
		setTimeout(() => { dismiss(s); s.style.transition = ''; s.style.transform = ''; }, 220);
	} else {
		// Snap back to the open position.
		s.style.transform = 'translateY(0)';
		setTimeout(() => { s.style.transition = ''; s.style.transform = ''; }, 220);
	}
}
document.addEventListener('touchend', end);
document.addEventListener('touchcancel', end);
