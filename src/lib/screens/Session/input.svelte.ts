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
	mode: () => 'remote' | 'game';
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
	// Physical codes (KeyboardEvent.code) we resolved to a Unicode char and forwarded as a
	// one-shot `Char` (layout-independent). These have NO held key-down on the host, so their
	// key-up is suppressed (mirrors the Linux native hook's char_keys) and they're never put in
	// #heldKeys — nothing to release on blur/disengage.
	#charKeys = new Set<string>();

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
		// Char keys are one-shot (no held VK on the host), so there's nothing to release —
		// just drop the tracking so a later key-up isn't wrongly suppressed.
		this.#charKeys.clear();
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
		const playId = this.#in.playId();
		// WYSIWYG / layout-independent text (mirrors the Linux native hook): a printable key
		// with no SHORTCUT modifier (Ctrl/Meta/Alt — but AltGr is NOT a shortcut) resolves via
		// THIS client's keyboard layout to a Unicode codepoint and is sent as `Char`, so the host
		// inserts that exact character regardless of its own active layout. Without this every key
		// goes through the positional evdev→VK table, so non-US layouts mistype and AltGr symbols
		// (@ € { }) become e.g. macOS Option dead-keys. Shortcuts and non-text keys (Enter, arrows,
		// F-keys, modifiers) still take the `Key` path so VK-level semantics are preserved.
		const altGraph = e.getModifierState && e.getModifierState('AltGraph');
		const isShortcut = !altGraph && (e.ctrlKey || e.metaKey || e.altKey);
		// Game mode must deliver raw scancodes/VK so games can see movement keys (WASD, digits,
		// space) via DirectInput / RawInput / GetAsyncKeyState — KEYEVENTF_UNICODE and CGEvent
		// Unicode injection are invisible to those APIs. The Char path is only correct for
		// remote-desktop mode where layout-independent text entry matters.
		const printable =
			this.#in.mode() === 'remote' &&
			e.key.length === 1 &&
			e.key.codePointAt(0)! >= 0x20 &&
			// Space and Tab carry positional/VK semantics (button activation, focus, pan, scroll,
			// page-down) that Unicode text injection (KEYEVENTF_UNICODE / CGEvent set_string)
			// cannot reproduce — Win32 controls and browsers react to VK_SPACE/VK_TAB key events,
			// not WM_CHAR. Exclude them so they take the evdev/VK Key path even in remote mode.
			e.code !== 'Space' &&
			e.code !== 'Tab' &&
			!isShortcut;
		// Char path is resolved BEFORE the evdev-code guard: international/ISO keys
		// (IntlBackslash, IntlRo, IntlYen, etc.) have no entry in the EVDEV table so
		// evdevCode returns 0 — but they carry a valid e.key printable character and must
		// still reach the host via inputChar. The evdev code is only required for the Key
		// (raw scancode/VK) path below.
		if (down && printable) {
			e.preventDefault();
			// If this physical key was previously held as a raw VK (e.g. the user pressed
			// Ctrl+'a' so the first down took the Key path, then released Ctrl while holding
			// 'a' so the auto-repeat now arrives here as printable), release the stale VK on
			// the host BEFORE switching to the Char path. Without this the held VK is never
			// released: the matching key-UP below will hit #charKeys.delete and return early,
			// leaving the host key stuck. Sending a key-up for a not-held VK is a harmless
			// host no-op (symmetric with C4's fix in the Linux native evdev path).
			const c = evdevCode(e.code);
			if (c && this.#heldKeys.delete(c)) {
				api.inputKey(playId, c, false).catch(() => {});
			}
			// One-shot char insert (no held VK on the host); remember it so the key-up is
			// suppressed and so blur/disengage doesn't try to release a key never held.
			this.#charKeys.add(e.code);
			api.inputChar(playId, e.key).catch(() => {});
			return;
		}
		// Key-up for a key that was sent as a one-shot Char: suppress it (no held VK to
		// release on the host). Also checked before the evdev guard for the same reason —
		// IntlBackslash key-ups would otherwise fall through and be dropped by !code.
		if (!down && this.#charKeys.delete(e.code)) return;
		const code = evdevCode(e.code);
		if (!code) return;
		e.preventDefault();
		if (down) {
			// Track held keys so focus loss (the Win key popping Start, Alt+Tab) can release
			// them on the host — otherwise the key-up never arrives (the webview lost focus)
			// and the key stays stuck, e.g. Win held → every letter becomes a Win+letter shortcut.
			// If this physical key was previously tracked as a one-shot Char (e.g. 'a' was held
			// without a modifier, then Ctrl was pressed mid-hold so the auto-repeat now carries
			// ctrlKey=true), drop the stale #charKeys entry. Without this the key-UP handler
			// hits the `#charKeys.delete` branch first and suppresses the matching
			// inputKey(code, false), leaving the host key stuck.
			this.#charKeys.delete(e.code);
			this.#heldKeys.add(code);
		} else {
			this.#heldKeys.delete(code);
		}
		api.inputKey(playId, code, down).catch(() => {});
	}
}
