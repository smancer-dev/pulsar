// Floating-menu + game-overlay UI controller for a session, lifted out of Session.svelte.
// Owns the dock/menu open + floating-drag state and the Ctrl+Shift+M game overlay's
// open/close serialization (the Linux set_overlay kills/respawns the embedded mpv, so toggles
// are debounced + guarded). Behaviour is identical to the original inline script.
//
// Instantiated at component init so its Escape-key $effect scopes to + tears down with the
// component. Cross-cutting actions (release control, end the tab, toggle fullscreen, reset the
// menu's active panel) are passed in as callbacks; `playId` is a getter.

import { api } from '$lib/api';

type Inputs = {
	playId: () => number;
	stopControl: () => void;
	onEnd: () => void;
	onToggleFullscreen: () => void;
	resetPanel: () => void;
};

export class SessionUi {
	#in: Inputs;

	// Parsec-style floating control menu. A handle that expands to all session actions. It
	// auto-hides while controlling and reappears once you release control; it can be docked
	// (top-center) or floating (dragged anywhere). Opening the menu drops out of control mode.
	menuOpen = $state(false);
	statsHover = $state(false);
	floating = $state(false);
	pos = $state({ x: 0, y: 0 });

	// Game-only overlay (Ctrl+Shift+M, NOT the leave combo — it does not end the session).
	// Opening releases control so the local OS/webview drive the overlay, and asks the host to
	// suspend input grab + pause the embedded mpv on Linux; closing resumes both.
	overlayOpen = $state(false);
	// Overlay toggles are CONVERGENT, not dropped: `#overlayWant` is the latest desired state
	// and `#pumpOverlay` serializes set_overlay calls until applied == desired. The old guard
	// IGNORED toggles while one was in flight — a swallowed close left the native overlay
	// closed but evdev suspended + pass-through off (input dead, "stuck") when the combo was
	// spammed. Now a burst just settles on the last press. A small debounce still absorbs
	// key-repeat bursts of the combo itself.
	#overlayWant = false;
	#overlayApplied = false;
	#overlayBusy = false;
	#lastOverlayAt = 0;

	// Handle drag (when floating) vs click (toggle menu) — a small threshold tells them apart
	// so a tiny jitter still counts as a click.
	#pdown = false;
	#dragMoved = false;
	#dragOrig = { x: 0, y: 0, px: 0, py: 0 };

	constructor(inputs: Inputs) {
		this.#in = inputs;

		// Close the menu or game overlay on Escape (when it isn't being used to leave control).
		$effect(() => {
			if ((!this.menuOpen && !this.overlayOpen) || typeof window === 'undefined') return;
			const onEsc = (e: KeyboardEvent) => {
				if (e.key === 'Escape') {
					e.stopPropagation();
					if (this.overlayOpen) this.closeOverlay();
					else this.closeMenu();
				}
			};
			window.addEventListener('keydown', onEsc, true);
			return () => window.removeEventListener('keydown', onEsc, true);
		});
	}

	// Drain desired → applied, ONE set_overlay in flight at a time. A toggle that lands
	// mid-flight is not dropped — the loop re-runs until the states converge, so the native
	// overlay (signal), the evdev suspend and the container pass-through always end on the
	// SAME final state no matter how fast the combo was spammed.
	#pumpOverlay = async () => {
		if (this.#overlayBusy) return;
		this.#overlayBusy = true;
		// EVERYTHING inside try/finally: a throw in stopControl() (or setOverlay) used to leak
		// overlayBusy=true forever → every later Ctrl+Shift+M was swallowed by the guard and the
		// overlay stuck open. finally guarantees the busy flag always clears.
		try {
			while (this.#overlayWant !== this.#overlayApplied) {
				const open = this.#overlayWant;
				this.overlayOpen = open;
				if (open) this.#in.stopControl();
				const playId = this.#in.playId();
				if (playId >= 0) await api.setOverlay(playId, open);
				this.#overlayApplied = open;
			}
		} catch {
			// Resync the tracker with what the UI shows; the next toggle re-pumps.
			this.#overlayApplied = this.overlayOpen;
		} finally {
			this.#overlayBusy = false;
		}
	};
	toggleOverlay = () => {
		// Debounce only the combo bursts — a queued (not dropped) desire makes longer
		// serialization unnecessary.
		const now = Date.now();
		if (now - this.#lastOverlayAt < 250) return;
		this.#lastOverlayAt = now;
		this.#overlayWant = !this.#overlayWant;
		void this.#pumpOverlay();
	};
	closeOverlay = () => {
		// NEVER swallowed (no busy/debounce gate): the native overlay may already be closed
		// on its side (`ov close`), and skipping this would strand the suspend/pass-through.
		this.#overlayWant = false;
		void this.#pumpOverlay();
	};

	onHandleDown = (e: PointerEvent) => {
		if (!this.floating) return;
		this.#pdown = true;
		this.#dragMoved = false;
		this.#dragOrig = { x: e.clientX, y: e.clientY, px: this.pos.x, py: this.pos.y };
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	};
	onHandleMove = (e: PointerEvent) => {
		if (!this.#pdown) return;
		const dx = e.clientX - this.#dragOrig.x;
		const dy = e.clientY - this.#dragOrig.y;
		if (!this.#dragMoved && (Math.abs(dx) > 3 || Math.abs(dy) > 3)) this.#dragMoved = true;
		if (this.#dragMoved) {
			this.pos = {
				x: Math.max(0, Math.min(window.innerWidth - 60, this.#dragOrig.px + dx)),
				y: Math.max(0, Math.min(window.innerHeight - 30, this.#dragOrig.py + dy))
			};
		}
	};
	onHandleUp = (e: PointerEvent) => {
		if (!this.#pdown) return;
		this.#pdown = false;
		try {
			(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
		} catch {
			/* pointer already released */
		}
	};
	// Cancelled pointer (touch gesture takeover / capture loss): no pointerup or click
	// follows, so end the drag here AND clear the drag flag — a stale #pdown would make
	// plain hovers keep dragging the dock, and a stale #dragMoved would swallow the
	// next genuine click in handleClick.
	onHandleCancel = (e: PointerEvent) => {
		if (!this.#pdown) return;
		this.onHandleUp(e);
		this.#dragMoved = false;
	};
	handleClick = () => {
		if (this.#dragMoved) {
			this.#dragMoved = false; // this was a drag, not a click
			return;
		}
		this.toggleMenu();
	};
	toggleFloating = () => {
		this.floating = !this.floating;
		if (this.floating && typeof window !== 'undefined') {
			this.pos = { x: Math.max(8, Math.round(window.innerWidth / 2 - 70)), y: 6 };
		}
	};

	toggleMenu = () => {
		this.menuOpen = !this.menuOpen;
		if (this.menuOpen) this.#in.stopControl();
	};
	closeMenu = () => {
		this.menuOpen = false;
		this.#in.resetPanel();
	};
	endSession = () => {
		this.closeMenu();
		this.#in.onEnd();
	};
	doFullscreen = () => {
		this.closeMenu();
		this.#in.onToggleFullscreen();
	};
}
