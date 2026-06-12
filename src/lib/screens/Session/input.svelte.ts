// Remote-control input engine for a session, lifted out of Session.svelte. Owns the
// `controlling` flag, the absolute-positioning pointer forwarding (VNC-style, not pointer
// lock — which froze the webview), key forwarding with held-key tracking, and the rAF pump
// that sends the latest pointer position at ≈refresh rate. Behaviour is identical to the
// original inline script; inputs are getters so the pump effect tracks them as it did inline.
//
// Instantiated once at component init so its $effect() scopes to + tears down with the
// component (canvas/playId are read lazily through getters).

import { api } from '$lib/api';
import { evdevCode } from '$lib/keymap';

type Inputs = {
	playId: () => number;
	wsPort: () => number;
	canvas: () => HTMLCanvasElement;
};

export class SessionInput {
	#in: Inputs;

	// Control via absolute positioning (VNC-style). Click the screen to start controlling;
	// the remote cursor follows yours over the canvas.
	controlling = $state(false);
	// Controllable once the video relay is up. (We used to disable control whenever the host
	// was reached over loopback — but that wrongly blocked legitimate cases like ASTER
	// multiseat: a *different* seat on the same box is fine to drive.) A getter (not a
	// $derived field) so it doesn't dereference #in before the constructor assigns it.
	get controllable() {
		return this.#in.wsPort() > 0;
	}

	#moveDirty = false;
	#nx = 0;
	#ny = 0;
	#armingClick = false;
	#lastScroll = 0;
	// Every key we've forwarded as "down" and not yet released. We release EXACTLY these
	// (no blind modifier list) so a key can never stay stuck on the host — including the
	// Win key. The old code excluded Win to avoid a spurious lone Win-up popping Start,
	// but since we only release keys actually held, releasing Win here is correct.
	#heldKeys = new Set<number>();

	constructor(inputs: Inputs) {
		this.#in = inputs;

		// While controlling: a rAF pump sends the latest pointer position at ≈refresh
		// rate (never per raw event), plus window-level keyboard capture.
		$effect(() => {
			if (!this.controlling || typeof window === 'undefined') return;
			const playId = this.#in.playId();
			const pump = () => {
				if (this.#moveDirty) {
					this.#moveDirty = false;
					api.inputPointer(playId, this.#nx, this.#ny).catch(() => {});
				}
				raf = requestAnimationFrame(pump);
			};
			let raf = requestAnimationFrame(pump);
			const kd = (e: KeyboardEvent) => this.onKey(e, true);
			const ku = (e: KeyboardEvent) => this.onKey(e, false);
			// Focus stolen (Start menu opened by Win, Alt+Tab, etc.) → the webview won't get
			// the key-ups, so release everything held on the host to avoid stuck keys.
			const onBlur = () => {
				this.#releaseButtons();
				this.#releaseHeldKeys();
			};
			window.addEventListener('keydown', kd, true);
			window.addEventListener('keyup', ku, true);
			window.addEventListener('blur', onBlur);
			return () => {
				cancelAnimationFrame(raf);
				window.removeEventListener('keydown', kd, true);
				window.removeEventListener('keyup', ku, true);
				window.removeEventListener('blur', onBlur);
			};
		});
	}

	#norm(e: PointerEvent) {
		const r = this.#in.canvas().getBoundingClientRect();
		this.#nx = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
		this.#ny = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height));
	}
	#releaseButtons() {
		const playId = this.#in.playId();
		if (playId < 0) return;
		api.inputButton(playId, 0, false).catch(() => {});
		api.inputButton(playId, 1, false).catch(() => {});
		api.inputButton(playId, 2, false).catch(() => {});
	}
	#releaseHeldKeys() {
		const playId = this.#in.playId();
		if (playId < 0) {
			this.#heldKeys.clear();
			return;
		}
		for (const c of this.#heldKeys) api.inputKey(playId, c, false).catch(() => {});
		this.#heldKeys.clear();
	}
	startControl = () => {
		if (this.controllable && !this.controlling) {
			this.controlling = true;
			this.#in.canvas().focus();
			// Windows: capture OS-reserved keys (Win/Alt+Tab/Ctrl+Esc) for the remote.
			const playId = this.#in.playId();
			if (playId >= 0) api.kbdCaptureStart(playId).catch(() => {});
		}
	};
	stopControl = () => {
		if (this.controlling) {
			this.controlling = false;
			api.kbdCaptureStop().catch(() => {});
			this.#releaseButtons();
			this.#releaseHeldKeys();
		}
	};
	#hostButton = (b: number) => (b === 2 ? 1 : b === 1 ? 2 : 0);

	onMove = (e: PointerEvent) => {
		if (!this.controlling) return;
		this.#norm(e);
		this.#moveDirty = true;
	};
	// Pointer left the canvas → stop re-sending the last position.
	clearMove = () => {
		this.#moveDirty = false;
	};
	// The click that takes control must not reach the host AT ALL — neither its down
	// (swallowed below) nor its up (which would otherwise fire a stray button-up,
	// e.g. a right-click context menu on the host).
	onDown = (e: PointerEvent) => {
		if (!this.controlling) {
			this.startControl();
			this.#armingClick = true;
			return; // the focusing click isn't forwarded
		}
		e.preventDefault();
		this.#norm(e);
		this.#moveDirty = true;
		api.inputButton(this.#in.playId(), this.#hostButton(e.button), true).catch(() => {});
	};
	onUp = (e: PointerEvent) => {
		if (this.#armingClick) {
			this.#armingClick = false;
			return; // swallow the focusing click's release
		}
		if (!this.controlling) return;
		e.preventDefault();
		api.inputButton(this.#in.playId(), this.#hostButton(e.button), false).catch(() => {});
	};
	onWheel = (e: WheelEvent) => {
		if (!this.controlling) return;
		e.preventDefault();
		const now = performance.now();
		if (now - this.#lastScroll < 30) return;
		this.#lastScroll = now;
		api.inputScroll(this.#in.playId(), e.deltaX, e.deltaY).catch(() => {});
	};
	onKey(e: KeyboardEvent, down: boolean) {
		if (!this.controlling) return;
		// Parsec-style leave combo: Ctrl+Shift+F12 (or Cmd+Shift+F12 for mac users)
		// releases control. preventDefault also stops F12 from opening webview devtools.
		if (down && (e.ctrlKey || e.metaKey) && e.shiftKey && e.code === 'F12') {
			e.preventDefault();
			this.stopControl();
			return;
		}
		const code = evdevCode(e.code);
		if (!code) return;
		e.preventDefault();
		// Track held keys so focus loss (the Win key popping Start, Alt+Tab) can release
		// them on the host — otherwise the key-up never arrives (the webview lost focus)
		// and the key stays stuck, e.g. Win held → every letter becomes a Win+letter shortcut.
		if (down) this.#heldKeys.add(code);
		else this.#heldKeys.delete(code);
		api.inputKey(this.#in.playId(), code, down).catch(() => {});
	}
}
