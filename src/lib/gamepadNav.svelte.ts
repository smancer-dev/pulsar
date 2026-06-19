// Controller + keyboard navigation for the gaming-mode shell. A small, framework-light
// roving-focus controller: gaming-shell widgets register themselves with the `item`
// Svelte action, and this drives a single focus that moves SPATIALLY (geometry-based, so
// no manual row/col wiring) with the D-pad / left stick, activates with A, goes back with
// B, and jumps between sections with the bumpers. A keyboard fallback (arrows / Enter /
// Esc) mirrors it so the shell is fully usable without a pad — and on platforms where the
// webview Gamepad API is unavailable (e.g. WebKitGTK on RK3588).
//
// Menu navigation uses the browser Gamepad API (not the gilrs reader, which is for
// in-session forwarding): it only runs while the app shell — not a live session — is
// focused, so it never competes with controller input being sent to a host.
//
// Modal scoping: when a `[data-navmodal]` element is in the DOM (the connect pop-up),
// focus is confined to items inside it, and B/Esc run the modal's back handler (pushed
// via pushBack) instead of the shell's — so the pop-up is fully pad-navigable and the
// background can't be reached behind it.

import type { NavInput } from '$lib/api';

/** Standard-mapping button indices (Gamepad API "standard" layout). X = West (□ / X). */
const BTN = { A: 0, B: 1, X: 2, LB: 4, RB: 5, UP: 12, DOWN: 13, LEFT: 14, RIGHT: 15 } as const;
type Dir = 'up' | 'down' | 'left' | 'right';
const AXIS_DEADZONE = 0.55;
/** Delay (ms) before a held direction auto-repeats, and the repeat interval after. */
const REPEAT_DELAY = 360;
const REPEAT_RATE = 130;
/** Pad poll interval (ms). We poll with setTimeout (not requestAnimationFrame) — ~60 Hz is
 * plenty for menu nav and a plain timer keeps us off the compositor's frame clock. */
const POLL_MS = 16;
/** Linux client = WebKitGTK. This build ships libmanette, so touching the webview Gamepad
 * API (`navigator.getGamepads()` / a `gamepadconnected` listener) starts libmanette's
 * gamepad monitor — which, on RK3588 with a DualSense, busy-loops the GTK main thread at
 * ~100% the moment a pad connects (froze the whole gaming home; verified the process loads
 * libmanette-0.2 and the spin is in the GDK main loop with the X queue empty). We never need
 * it on Linux — the gilrs→SDL bridge (api.onGamepadNav) is the pad-nav source there — so the
 * browser-Gamepad-API fallback is hard-disabled on Linux. */
const IS_LINUX = typeof navigator !== 'undefined' && /Linux|X11/i.test(navigator.userAgent);

type Opts = {
	/** Called on B / Escape when no modal back-handler is active (e.g. leave a sub-view). */
	onBack?: () => void;
	/** Called on LB(-1) / RB(+1) — section/tab jump (e.g. cycle the bottom dock). */
	onBumper?: (dir: -1 | 1) => void;
};

export class GamepadNav {
	#items = new Set<HTMLElement>();
	#current: HTMLElement | null = null;
	#opts: Opts;
	/** Stack of back handlers (e.g. an open modal pushes its close fn). The top wins. */
	#backStack: (() => void)[] = [];
	#timer: ReturnType<typeof setTimeout> | 0 = 0;
	#running = false;
	#prevInp: NavInput | null = null;
	#dirHeld: Dir | null = null;
	#nextRepeat = 0;
	/** Latest input from the gilrs→webview bridge (api.onGamepadNav) + when it arrived.
	 * This is the ONLY pad-nav source on Linux (no webview Gamepad API) and the preferred
	 * one everywhere; the browser Gamepad API is a fallback used only when no bridge event
	 * has arrived recently. */
	#bridge: NavInput | null = null;
	#bridgeAt = 0;
	/** Resting axis values for the browser-Gamepad-API fallback, snapshotted when a pad
	 * first appears, so a D-pad reported as an axis is read as DEVIATION from rest (a
	 * trigger that rests off-center can't masquerade as a held direction). */
	#axisRest: number[] | null = null;
	/** Whether the user has STARTED keyboard/controller navigation. The focus ring
	 * (`data-navfocus`) stays HIDDEN until the FIRST directional/activate input, so a mouse/touch
	 * user never sees a stray selection border. The first input "wakes" nav at the default action
	 * (`data-navdefault`, e.g. "Connect to a host") — not whatever registered first. Reset on stop(). */
	#navActive = false;
	#now = () => (typeof performance !== 'undefined' ? performance.now() : 0);

	constructor(opts: Opts = {}) {
		this.#opts = opts;
		this.onKeydown = this.onKeydown.bind(this);
	}

	/** Update the back/bumper handlers after construction — used when the nav is a shared
	 * singleton (so the gaming shell can wire its own view-specific handlers into it). */
	setOpts(opts: Opts) {
		this.#opts = opts;
	}

	/** Svelte action: `use:nav.item`. Registers the node as focusable; unregisters on
	 * destroy. The first registered visible item becomes the initial focus (unless a
	 * modal is open — then focus is driven by enterModal). */
	item = (node: HTMLElement) => {
		this.#items.add(node);
		node.setAttribute('data-navitem', '');
		if (!node.hasAttribute('tabindex') && !/^(a|button|input|select|textarea)$/i.test(node.tagName))
			node.tabIndex = -1;
		// NO auto-focus on registration: the ring must not appear (and nothing should grab focus)
		// until the user actually navigates with a key/pad. #wake() picks the start element on the
		// first input. (Previously this focused the first-registered item — the stray border bug.)
		return {
			destroy: () => {
				this.#items.delete(node);
				node.removeAttribute('data-navitem');
				node.removeAttribute('data-navfocus');
				if (this.#current === node) this.#current = null;
			}
		};
	};

	start() {
		if (this.#running || typeof window === 'undefined') return;
		this.#running = true;
		window.addEventListener('keydown', this.onKeydown);
		this.#loop();
	}

	stop() {
		this.#running = false;
		if (typeof window !== 'undefined') window.removeEventListener('keydown', this.onKeydown);
		if (this.#timer) clearTimeout(this.#timer);
		this.#timer = 0;
		if (this.#current) this.#current.removeAttribute('data-navfocus');
		// Re-entering gaming mode should again start ring-less until the user navigates.
		this.#navActive = false;
	}

	/** Feed an input snapshot from the gilrs→webview bridge (api.onGamepadNav). */
	ingestBridge(s: NavInput) {
		this.#bridge = s;
		this.#bridgeAt = this.#now();
	}

	/** Push a back handler (a modal's close fn). B / Esc will call this until popped. */
	pushBack(fn: () => void) {
		this.#backStack.push(fn);
	}
	/** Remove a previously-pushed back handler (call on modal destroy). */
	popBack(fn?: () => void) {
		if (fn) this.#backStack = this.#backStack.filter((f) => f !== fn);
		else this.#backStack.pop();
	}

	/** Force focus to a specific registered element (e.g. the modal's ID box on open). */
	focus(node: HTMLElement | null) {
		if (node && this.#items.has(node) && this.#focusable(node)) this.#setCurrent(node);
	}

	/** Focus the first focusable item in the current scope (used when a modal opens, and
	 * when it closes to return focus to the shell). */
	focusFirst() {
		const c = this.#candidates();
		if (c.length) this.#setCurrent(c[0]);
		else if (this.#current) {
			this.#current.removeAttribute('data-navfocus');
			this.#current = null;
		}
	}

	// ---- focus management -------------------------------------------------

	#modalEl(): HTMLElement | null {
		if (typeof document === 'undefined') return null;
		return document.querySelector('[data-navmodal]');
	}

	#focusable(el: HTMLElement): boolean {
		if (!el.isConnected) return false;
		const r = el.getBoundingClientRect();
		// NOTE: deliberately NOT reading `el.offsetParent` here — it's a second forced
		// synchronous layout per element, and under `content-visibility` it forces WebKitGTK
		// to resolve geometry it was skipping (brutal on opi5's software-painted webview when
		// move() runs this over every registered control). rect width/height + isConnected
		// already gate real visibility.
		if (r.width <= 0 || r.height <= 0) return false;
		if (el.closest('[inert]')) return false;
		return true;
	}

	/** Focusable items in the active scope: confined to the open modal if one exists. */
	#candidates(): HTMLElement[] {
		const modal = this.#modalEl();
		const all = [...this.#items].filter((el) => this.#focusable(el));
		return modal ? all.filter((el) => modal.contains(el)) : all;
	}

	#setCurrent(el: HTMLElement) {
		if (this.#current && this.#current !== el) this.#current.removeAttribute('data-navfocus');
		this.#current = el;
		// The VISIBLE selection ring only shows once the user is navigating (mouse/touch users
		// never see it). el.focus() still runs so a woken pad/keyboard user can activate it, and
		// so a modal's first field is focused for typing — but no ring until #navActive.
		if (this.#navActive) el.setAttribute('data-navfocus', '');
		try {
			el.focus({ preventScroll: true });
		} catch {
			el.focus();
		}
		el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
	}

	/** The element marked as the default focus (the primary action — e.g. "Connect to a host",
	 * tagged `data-navdefault`). Null → #wake falls back to the first candidate. */
	#defaultItem(): HTMLElement | null {
		for (const el of this.#items)
			if (el.hasAttribute('data-navdefault') && this.#focusable(el)) return el;
		return null;
	}

	/** The first keyboard/controller input "wakes" navigation: reveal the focus ring, STARTING at
	 * the default primary action (or, in a modal, its first item; else the first candidate).
	 * Returns true when it just woke — so a directional input reveals-without-moving the first time. */
	#wake(): boolean {
		if (this.#navActive) return false;
		this.#navActive = true;
		const start = this.#modalEl()
			? this.#candidates()[0]
			: (this.#defaultItem() ?? this.#current ?? this.#candidates()[0]);
		if (start) this.#setCurrent(start);
		return true;
	}

	/** Move focus to the nearest item in `dir` within the active scope (geometric scoring).
	 * Reads each candidate's rect EXACTLY ONCE (center cached) — the old path called
	 * getBoundingClientRect twice per item (#candidates→#focusable, then #center) plus an
	 * offsetParent read, i.e. ~3 forced layouts × every registered control per nav step. On
	 * opi5/WebKitGTK (software paint, no batching) that pegged the main thread; one rect each
	 * roughly thirds the layout work. */
	move(dir: Dir) {
		if (this.#wake()) return; // first nav input only REVEALS the ring at the default action
		// A native <select> can't be value-edited by a pad's d-pad — focusing it would be a
		// dead end. So when the FOCUSED element is a <select>, left/right CYCLE its value
		// (prev/next non-disabled option) instead of moving focus, and we fire a native
		// `change` event so Svelte's onchange runs. Up/down still move geometrically, so the
		// user navigates away vertically. Clamps at the ends; a single enabled option no-ops.
		const cur = this.#current;
		if (cur && cur.tagName === 'SELECT' && (dir === 'left' || dir === 'right')) {
			this.#cycleSelect(cur as HTMLSelectElement, dir === 'right' ? 1 : -1);
			return;
		}
		const modal = this.#modalEl();
		const cands: { el: HTMLElement; cx: number; cy: number }[] = [];
		for (const el of this.#items) {
			if (!el.isConnected) continue;
			if (modal && !modal.contains(el)) continue;
			if (el.closest('[inert]')) continue;
			const r = el.getBoundingClientRect();
			if (r.width <= 0 || r.height <= 0) continue;
			cands.push({ el, cx: r.left + r.width / 2, cy: r.top + r.height / 2 });
		}
		if (cands.length === 0) return;
		// No current, or current left the scope (e.g. a modal just opened) → snap to first.
		const from = this.#current ? cands.find((c) => c.el === this.#current) : undefined;
		if (!from) {
			this.#setCurrent(cands[0].el);
			return;
		}
		let best: HTMLElement | null = null;
		let bestScore = Infinity;
		for (const c of cands) {
			if (c.el === this.#current) continue;
			const dx = c.cx - from.cx;
			const dy = c.cy - from.cy;
			let primary: number, cross: number;
			if (dir === 'right') { primary = dx; cross = Math.abs(dy); }
			else if (dir === 'left') { primary = -dx; cross = Math.abs(dy); }
			else if (dir === 'down') { primary = dy; cross = Math.abs(dx); }
			else { primary = -dy; cross = Math.abs(dx); }
			if (primary <= 1) continue;
			const score = primary + cross * 2;
			if (score < bestScore) { bestScore = score; best = c.el; }
		}
		if (best) this.#setCurrent(best);
	}

	/** Step a focused <select>'s value to the prev/next NON-disabled option (step ±1),
	 * skipping disabled entries and clamping at the ends, then dispatch a native `change`
	 * event so Svelte's onchange fires. No-ops gracefully if there's no enabled option in
	 * that direction (e.g. a select with a single enabled option). */
	#cycleSelect(sel: HTMLSelectElement, step: 1 | -1) {
		const opts = [...sel.options];
		let i = sel.selectedIndex + step;
		while (i >= 0 && i < opts.length && opts[i].disabled) i += step; // skip disabled
		if (i < 0 || i >= opts.length || opts[i].disabled) return; // clamp / nothing to pick
		sel.selectedIndex = i;
		sel.dispatchEvent(new Event('change', { bubbles: true }));
	}

	activate() {
		// First input wakes nav onto the default action; then we click whatever is current — so a
		// cold A/Enter activates "Connect to a host" directly (no stray ring beforehand).
		this.#wake();
		const el = this.#current;
		if (el && this.#focusable(el)) el.click();
	}

	/** Click the first element marked with `sel` in the active scope (e.g. the numpad
	 * delete key via West/Square). Used for context shortcuts that aren't the focused item. */
	#clickMarked(sel: string) {
		const root: ParentNode = this.#modalEl() ?? (typeof document !== 'undefined' ? document : null)!;
		if (!root) return;
		const el = root.querySelector<HTMLElement>(sel);
		if (el && this.#focusable(el)) el.click();
	}

	#back() {
		const fn = this.#backStack.at(-1) ?? this.#opts.onBack;
		fn?.();
	}

	// ---- keyboard fallback ------------------------------------------------

	onKeydown(e: KeyboardEvent) {
		const tgt = e.target as HTMLElement | null;
		const typing =
			!!tgt && (tgt.isContentEditable || /^(input|textarea|select)$/i.test(tgt.tagName));
		if (e.key === 'Escape') {
			e.preventDefault();
			this.#back();
			return;
		}
		if (typing) return; // let fields own arrows/Enter (caret/submit)
		switch (e.key) {
			case 'ArrowUp': e.preventDefault(); this.move('up'); break;
			case 'ArrowDown': e.preventDefault(); this.move('down'); break;
			case 'ArrowLeft': e.preventDefault(); this.move('left'); break;
			case 'ArrowRight': e.preventDefault(); this.move('right'); break;
			case 'Enter':
			case ' ': e.preventDefault(); this.activate(); break;
		}
	}

	// ---- gamepad polling --------------------------------------------------

	#loop() {
		if (!this.#running) return;
		this.#tick();
		// setTimeout, NOT requestAnimationFrame — see POLL_MS: rAF drives the GDK frame
		// clock on WebKitGTK and pins the main thread when a pad's touchpad device is present.
		this.#timer = setTimeout(() => this.#loop(), POLL_MS);
	}

	#pads(): (Gamepad | null)[] {
		// NEVER call getGamepads() on Linux: it starts WebKitGTK's libmanette monitor, which
		// freezes the GTK main thread on a pad connect (see IS_LINUX). The bridge is the only
		// pad-nav source there anyway.
		if (IS_LINUX || typeof navigator === 'undefined' || !navigator.getGamepads) return [];
		try {
			return navigator.getGamepads();
		} catch {
			return [];
		}
	}

	/** Read the browser Gamepad API into the unified NavInput shape (fallback path —
	 * Windows/macOS where the webview exposes it; absent on Linux WebKitGTK). */
	#readBrowserPad(): NavInput | null {
		const pad = this.#pads().find((p): p is Gamepad => !!p && p.connected);
		if (!pad) {
			this.#axisRest = null; // re-calibrate on reconnect
			return null;
		}
		const pressed = (i: number) => !!pad.buttons[i]?.pressed;
		if (!this.#axisRest) this.#axisRest = [...pad.axes];
		const ax = pad.axes[0] ?? 0;
		const ay = pad.axes[1] ?? 0;
		// D-pad-as-axes (axes 6/7) read as deviation from rest, ignored if the axis rests
		// off-center (a trigger, not a D-pad — that once froze nav).
		const dpadAxis = (i: number) => {
			const rest = this.#axisRest?.[i] ?? 0;
			if (Math.abs(rest) >= 0.4) return 0;
			return (pad.axes[i] ?? 0) - rest;
		};
		const dpx = dpadAxis(6);
		const dpy = dpadAxis(7);
		return {
			up: pressed(BTN.UP) || ay < -AXIS_DEADZONE || dpy < -AXIS_DEADZONE,
			down: pressed(BTN.DOWN) || ay > AXIS_DEADZONE || dpy > AXIS_DEADZONE,
			left: pressed(BTN.LEFT) || ax < -AXIS_DEADZONE || dpx < -AXIS_DEADZONE,
			right: pressed(BTN.RIGHT) || ax > AXIS_DEADZONE || dpx > AXIS_DEADZONE,
			a: pressed(BTN.A),
			b: pressed(BTN.B),
			x: pressed(BTN.X),
			lb: pressed(BTN.LB),
			rb: pressed(BTN.RB)
		};
	}

	#tick() {
		const now = this.#now();
		// Prefer the gilrs bridge (works on every platform; the only path on Linux). Fall
		// back to the browser Gamepad API only if no bridge event has arrived recently.
		const inp =
			this.#bridge && now - this.#bridgeAt < 600 ? this.#bridge : this.#readBrowserPad();
		if (!inp) {
			this.#dirHeld = null;
			this.#prevInp = null;
			return;
		}
		const dir: Dir | null = inp.up ? 'up' : inp.down ? 'down' : inp.left ? 'left' : inp.right ? 'right' : null;
		if (dir) {
			if (dir !== this.#dirHeld) {
				this.move(dir);
				this.#dirHeld = dir;
				this.#nextRepeat = now + REPEAT_DELAY;
			} else if (now >= this.#nextRepeat) {
				this.move(dir);
				this.#nextRepeat = now + REPEAT_RATE;
			}
		} else {
			this.#dirHeld = null;
		}

		const p = this.#prevInp;
		const edge = (k: keyof NavInput) => inp[k] && !p?.[k];
		if (edge('a')) this.activate();
		if (edge('b')) this.#back();
		// West / Square = delete: click the scope's delete key (numpad backspace) if any.
		if (edge('x')) this.#clickMarked('[data-navdelete]');
		if (edge('lb')) this.#opts.onBumper?.(-1);
		if (edge('rb')) this.#opts.onBumper?.(1);
		this.#prevInp = inp;
	}
}

/** The single gaming-mode nav instance, shared across the gaming shell AND the top-bar
 * chrome (so the controller can also reach the gaming/language/theme buttons). The shell
 * wires its view-specific handlers via `setOpts` and drives start/stop + bridge input. */
export const gamingNav = new GamepadNav();

/** Svelte action: auto-register EVERY focusable control inside `node` with the gaming nav,
 * kept in sync as the DOM changes (MutationObserver). Lets a whole panel (e.g. Settings, with
 * its many native controls across tab components) become controller-navigable WITHOUT tagging
 * each control by hand. `enabled=false` is a no-op (e.g. when the panel is shown in remote
 * mode). Native <select>/number <input> can be focused + activated but not value-edited by a
 * pad — buttons/toggles/seg-buttons are fully operable. */
const NAV_SEL =
	'button, select, input:not([type="hidden"]):not([type="range"]), a[href], [role="button"], [tabindex]:not([tabindex="-1"])';
export function navContainer(node: HTMLElement, enabled: boolean) {
	// DIFF-based registration: keep a map of el→unregister so a re-scan only ADDS newly
	// appeared controls and REMOVES vanished ones — existing items (and the current focus)
	// are left untouched. The old code destroyed+re-registered EVERY control on each scan,
	// which reset gamingNav's focus to the first item and yanked the scroll position; with a
	// MutationObserver firing on every Svelte childList change that made the gaming-mode
	// Settings panel stutter (focus/scroll fighting the user). Scans are also DEBOUNCED into
	// one trailing pass per mutation burst (a tab switch fires many childList mutations).
	const reg = new Map<HTMLElement, () => void>();
	let mo: MutationObserver | undefined;
	let on = false;
	let pending: ReturnType<typeof setTimeout> | 0 = 0;
	const doScan = () => {
		pending = 0;
		if (!on) return;
		const present = new Set<HTMLElement>();
		node.querySelectorAll<HTMLElement>(NAV_SEL).forEach((el) => {
			if (el.hasAttribute('disabled')) return;
			present.add(el);
			if (!reg.has(el)) {
				const r = gamingNav.item(el);
				reg.set(el, r && typeof r.destroy === 'function' ? r.destroy : () => {});
			}
		});
		for (const [el, off] of reg) {
			if (!present.has(el)) {
				off();
				reg.delete(el);
			}
		}
	};
	// Coalesce a burst of mutations into a single trailing scan (setTimeout, not rAF — rAF
	// pins the GDK frame clock on WebKitGTK, see POLL_MS).
	const scan = () => {
		if (pending || !on) return;
		pending = setTimeout(doScan, 50);
	};
	const clearAll = () => {
		for (const off of reg.values()) off();
		reg.clear();
	};
	const apply = (en: boolean) => {
		on = en;
		mo?.disconnect();
		mo = undefined;
		if (pending) {
			clearTimeout(pending);
			pending = 0;
		}
		if (en) {
			mo = new MutationObserver(scan);
			mo.observe(node, { childList: true, subtree: true });
			doScan(); // initial pass synchronously so first focus lands immediately
		} else {
			clearAll();
		}
	};
	apply(enabled);
	return {
		update: apply,
		destroy: () => {
			mo?.disconnect();
			if (pending) clearTimeout(pending);
			clearAll();
		}
	};
}
