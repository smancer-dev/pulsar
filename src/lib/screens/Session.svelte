<script lang="ts" module>
	// Which play currently OWNS the global keyboard/mouse capture (kbdhook is one
	// per app, last call wins). On a tab switch the effect re-run order across
	// sibling Session components isn't guaranteed, so a deactivating tab must not
	// disarm a capture another tab just re-armed — only the recorded owner may stop.
	let captureOwner = -1;
</script>

<script lang="ts">
	import VideoStatus from './Session/VideoStatus.svelte';
	import { SessionMedia } from './Session/media.svelte';
	import {
		api,
		onKbdLeave,
		onKbdEngaged,
		onKbdReleased,
		onOverlayToggle,
		onOverlayCmd,
		onOverlayEnd,
		onOverlayClose,
		onOverlayChat,
		onOverlayFs,
		onOverlayFiles,
		onFsEntries,
		onPeerAvatar,
		onPeerName,
		onChatMsg,
		onPlayDims,
		onPlayEnded
	} from '$lib/api';
	import { listenScope } from '$lib/api.events';
	import { setPeerIdentity } from '$lib/peers.svelte';
	import { SessionInput } from './Session/input.svelte';
	import { SessionControls } from './Session/controls.svelte';
	import { SessionSideChannels } from './Session/sidechannels.svelte';
	import { SessionUi } from './Session/ui.svelte';
	import { ui, saveUi, applyCtrlSwap, type Encoder, type VideoCodec } from '$lib/settings.svelte';
	import { t } from '$lib/i18n.svelte';

	type Target = { name: string; id: string };
	type Props = {
		playId: number;
		target: Target;
		mode: 'remote' | 'game';
		conn: 'direct' | 'relay';
		wsPort?: number;
		audioWsPort?: number;
		selfId?: string;
		native?: boolean;
		embedded?: boolean;
		/** Host's validated stream caps (QueryStreamCaps) — gate the menu options. */
		hostCodecs?: string[];
		hostEncoders?: string[];
		/** Host's streamable monitors (primary first); the session menu's screen picker. */
		hostDisplays?: import('$lib/api.types').HostDisplay[];
		fullscreen?: boolean;
		/** True while this is the ACTIVE tab. Inactive session tabs stay mounted (just
		 * CSS-hidden), so the GLOBAL keyboard/mouse capture and the payload-less combo
		 * events (kbd-leave / overlay-toggle / kbd-engaged…) must only be owned/handled
		 * by the visible tab — otherwise one combo ends/changes EVERY open session. */
		active?: boolean;
		/** Split mode: this session is one pane of a tiled grid (not a fullscreen tab). In
		 * split, `active` means THIS is the FOCUSED pane; non-focused panes stay live (no
		 * rect-zeroing / data-occluded) and the embedded --wid container path is forced so
		 * the renderers compose side-by-side. */
		split?: boolean;
		/** Split mode: clicking this pane's video focuses the pane (routes input to it). */
		onPaneFocus?: () => void;
		/** A shell modal (e.g. the SplitPicker) is open ON TOP — hide this pane's native render
		 * window (which on Linux composites OVER the webview, hiding the modal) while true. */
		occludeNative?: boolean;
		onToggleFullscreen: () => void;
		onEnd?: () => void;
	};
	let {
		playId,
		target,
		mode,
		conn,
		wsPort = 0,
		audioWsPort = 0,
		selfId = '',
		native = false,
		embedded = false,
		hostCodecs = [],
		hostEncoders = [],
		hostDisplays = [],
		fullscreen = false,
		active = true,
		split = false,
		onPaneFocus,
		occludeNative = false,
		onToggleFullscreen,
		onEnd = () => {}
	}: Props = $props();

	// Reverse direction: ask the host to connect back to us so roles swap.
	function reverse() {
		if (playId >= 0 && selfId) api.reversePlay(playId, selfId.replace(/\s/g, '')).catch(() => {});
	}

	const connLabel = $derived(conn === 'relay' ? 'Relay' : 'P2P');

	let canvas: HTMLCanvasElement;
	// The video + audio + live-stats engine (canvas WebSocket sink, mpv vstats, RTT/host
	// summary, the 1s fps/stall timer). Owns all the streaming metric state + derivations;
	// see Session/media.svelte.ts. Created at component init so its effects scope to + tear
	// down with this component. Inputs are getters so the effects re-run exactly as before.
	const media = new SessionMedia(
		{
			playId: () => playId,
			wsPort: () => wsPort,
			audioWsPort: () => audioWsPort,
			native: () => native,
			embedded: () => embedded,
			// `controls` is declared just below; this getter is only read later from the 1 s
			// stall timer (after init), so the lazy reference is safe and lets the stall
			// detector suppress itself while a codec/encoder/resolution switch is in flight.
			switching: () => controls.switching
		},
		() => canvas
	);

	// The host's ACTIVE codec+encoder (its Stats push) — shown faintly under the
	// selectors so the request vs. reality is always visible.
	const activeInfo = $derived(
		media.hostCodec && media.hostEncoder ? `${media.hostCodec} · ${media.hostEncoder}` : ''
	);

	// How the video fills the viewport (AnyDesk-style): fit (keep aspect),
	// stretch (fill, may distort), or original (1:1 native pixels).
	let fitMode = $state<'fit' | 'stretch' | 'original'>('fit');
	// Host-stream control state + live setters (resolution/fps/bitrate/quality/pacing/audio +
	// codec/encoder). See Session/controls.svelte.ts. Created at component init so its
	// persisted frame-pacing one-shot effect scopes to the component. (The decoder is
	// auto-selected by the renderer and shown read-only.)
	const controls = new SessionControls({
		playId: () => playId,
		native: () => native,
		mode: () => mode
	});

	// Native renderer: video is in the native render window, so skip the webview canvas/WS
	// path entirely. ARM the capture watcher now, but it starts DISENGAGED (click-to-engage):
	// the devices are only grabbed once the user clicks the session video — never
	// automatically at session start, so a broken/erroring session can't trap the keyboard.
	// Gated on `active`: kbdhook::enable is GLOBAL (last call wins), so only the visible
	// tab may own it — capture re-arms on tab activation and stops on deactivation.
	$effect(() => {
		if (!native || playId < 0 || !active) return;
		captureOwner = playId;
		// true = also capture mouse. Game mode = Moonlight/Parsec-style "control immediately
			// on connect": engage right after the watcher is armed (remote stays click-to-engage
			// so a manual remote connect never grabs the local desktop unasked).
			api.kbdCaptureStart(playId, true)
				.then(() => (mode === 'game' ? api.kbdEngage() : undefined))
				.catch(() => {});
		return () => {
			if (captureOwner !== playId) return; // another tab re-armed it for itself
			captureOwner = -1;
			api.kbdCaptureStop().catch(() => {});
		};
	});

	// The HOST's pushed identity (image + display name): shown in the session menu
	// AND cached against the target id so recents/LAN/devices keep the face+name.
	let peerAvatar = $state('');
	let peerName = $state('');
	const displayTarget = $derived(peerName ? { name: peerName, id: target.id } : target);
	$effect(() => {
		const scope = listenScope();
		scope.add(
			onPeerAvatar((e) => {
				if (e.peer !== String(playId)) return;
				peerAvatar = e.dataUrl;
				setPeerIdentity(target.id, { avatar: e.dataUrl });
			}),
			onPeerName((e) => {
				if (e.peer !== String(playId)) return;
				peerName = e.name;
				setPeerIdentity(target.id, { name: e.name });
			}),
			// Inbound chat: surface as a renderer toast (visible while the overlay is
			// closed) AND into the overlay's native Chat log.
			onChatMsg((e) => {
				if (e.peer !== String(playId) || !native) return;
				const who = peerName || target.name;
				api.renderToast(playId, `${who}: ${e.text}`).catch(() => {});
				api.renderChat(playId, 'in', e.text).catch(() => {});
			})
		);
		return scope.dispose;
	});

	// Live engage state of the native capture (drives the status hint): a video click
	// engages (kbd-engaged), the release combo (Ctrl+Alt+Z / 3×RightCtrl) or focus loss
	// disengages. Each edge also pushes a transient helper tooltip onto the renderer
	// (bottom-center): "how to release" on engage, "click to control" on release.
	let nativeEngaged = $state(false);
	$effect(() => {
		const scope = listenScope();
		// Payload-less edges from the GLOBAL capture — only the active tab owns it.
		scope.add(
			onKbdEngaged(() => {
				if (!active) return;
				nativeEngaged = true;
				if (native && playId >= 0) api.renderHint(playId, 'engage').catch(() => {});
			}),
			onKbdReleased(() => {
				if (!active) return;
				nativeEngaged = false;
				// Always sent (even while the overlay is open): the renderer also derives the
				// CURSOR visibility from this edge, not just the tooltip.
				if (native && playId >= 0) api.renderHint(playId, 'click').catch(() => {});
			})
		);
		return scope.dispose;
	});

	// In-app native video (Linux): the renderer is embedded in a pass-through container
	// window the Rust side positions over THIS screen's rect — the video renders inside
	// the session tab (chrome/tabs stay visible) instead of covering the window. Report
	// the rect on layout/resize; the tab going inactive (display:none) yields a 0×0 rect
	// → container unmaps. Cleanup reports 0×0 so an unmounted tab never leaves video up.
	// Split mode forces the embedded per-pane `--wid` container path (rect-positioned over
	// the grid cell) and NEVER the single fullscreen GtkGLArea surface — 4 renderers must
	// compose side-by-side, so `embedded` (the single-surface mode) is treated as off here.
	const useEmbedded = $derived(embedded && !split);
	let screenEl = $state<HTMLDivElement | null>(null);
	$effect(() => {
		const el = screenEl;
		// Track the fullscreen prop: while the native video is up the webview is
		// render-suspended (data-occluded), and WebKitGTK then starves the
		// ResizeObserver — a fullscreen toggle resized the window but the video
		// container KEPT its old rect (small video with bars in fullscreen, an
		// oversized one back in windowed). Re-running this effect on the toggle
		// (plus the window resize listener + settle-delayed reports below) pushes
		// the fresh rect without relying on RO delivery.
		void fullscreen;
		if (!native || useEmbedded || playId < 0 || !el) return;
		// A shell modal (the SplitPicker) is open ON TOP. On Linux the native --wid render window
		// composites OVER the webview, so it would hide the modal. Unmap it (0×0) while occluded;
		// reading `occludeNative` makes this effect re-run + re-report the real rect when it closes.
		if (occludeNative) {
			api.nativeViewRect(playId, 0, 0, 0, 0).catch(() => {});
			return;
		}
		const report = () => {
			const r = el.getBoundingClientRect();
			api
				.nativeViewRect(
					playId,
					Math.round(r.x),
					Math.round(r.y),
					Math.round(r.width),
					Math.round(r.height)
				)
				.catch(() => {});
		};
		const ro = new ResizeObserver(report);
		ro.observe(el);
		window.addEventListener('resize', report);
		report();
		// The OS fullscreen transition lands a few frames later than the prop flip;
		// re-measure after it settles (and once more for slow WMs/animations).
		const t1 = setTimeout(report, 120);
		const t2 = setTimeout(report, 450);
		return () => {
			clearTimeout(t1);
			clearTimeout(t2);
			ro.disconnect();
			window.removeEventListener('resize', report);
		};
	});
	// The 0×0 "unmap" report lives in its OWN effect (no `fullscreen` dependency):
	// the reporter above re-runs on every fullscreen toggle, and unmapping the
	// container from ITS cleanup blanked the video for a frame on each toggle.
	$effect(() => {
		if (!native || useEmbedded || playId < 0) return;
		return () => {
			api.nativeViewRect(playId, 0, 0, 0, 0).catch(() => {});
		};
	});

	// Overlay-open button hotspot: position + drag. The renderer draws the button at
	// ui.overlayBtnPos (egui POINTS); the hotspot mirrors it in CSS px (pt × 1.25,
	// the renderer's fixed pixels-per-point; -1.5 centers the 48px hotspot over the
	// 45px visual). Dragging streams `ovbtnpos` to the renderer live and persists the
	// final spot; a press that never crosses the threshold stays a click (toggle).
	const BTN_PPP = 1.25;
	const BTN_HOT = 48; // hotspot square, CSS px
	const btnCss = $derived({
		x: (ui.overlayBtnPos?.x ?? 90) * BTN_PPP - 1.5,
		y: (ui.overlayBtnPos?.y ?? 70) * BTN_PPP - 1.5
	});
	let btnDrag: { px: number; py: number; ox: number; oy: number; moved: boolean } | null = null;
	let btnPosPushed = 0;
	function pushBtnPos(x: number, y: number) {
		// Live stream, rAF-throttled (pointermove can fire >120 Hz).
		const now = performance.now();
		if (now - btnPosPushed < 16) return;
		btnPosPushed = now;
		if (playId >= 0) api.setOverlayButtonPos(playId, x, y).catch(() => {});
	}
	function onBtnDown(e: PointerEvent) {
		e.stopPropagation();
		btnDrag = { px: e.clientX, py: e.clientY, ox: btnCss.x, oy: btnCss.y, moved: false };
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	}
	function onBtnMove(e: PointerEvent) {
		if (!btnDrag) return;
		const dx = e.clientX - btnDrag.px;
		const dy = e.clientY - btnDrag.py;
		if (!btnDrag.moved && Math.abs(dx) < 4 && Math.abs(dy) < 4) return;
		btnDrag.moved = true;
		// Clamp inside the video area (.screen — same rect the renderer container fills).
		const r = screenEl?.getBoundingClientRect();
		const maxX = Math.max(0, (r?.width ?? window.innerWidth) - BTN_HOT);
		const maxY = Math.max(0, (r?.height ?? window.innerHeight) - BTN_HOT);
		const cx = Math.min(maxX, Math.max(0, btnDrag.ox + dx));
		const cy = Math.min(maxY, Math.max(0, btnDrag.oy + dy));
		const pt = { x: (cx + 1.5) / BTN_PPP, y: (cy + 1.5) / BTN_PPP };
		ui.overlayBtnPos = pt; // reactive: the hotspot follows
		pushBtnPos(pt.x, pt.y);
	}
	function onBtnUp(e: PointerEvent) {
		if (!btnDrag) return;
		const wasDrag = btnDrag.moved;
		btnDrag = null;
		try {
			(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
		} catch {
			/* already released */
		}
		if (wasDrag) {
			// Persist + make sure the renderer has the FINAL spot (the throttle may
			// have eaten the last move).
			saveUi();
			const p = ui.overlayBtnPos;
			if (playId >= 0) api.setOverlayButtonPos(playId, p.x, p.y).catch(() => {});
		} else {
			dock.toggleOverlay();
		}
	}
	// Cancelled pointer (touch gesture takeover / capture loss): no pointerup follows, so
	// clear the drag here — otherwise the next plain HOVER over the hotspot keeps dragging
	// the button with no press. Finalize a real drag (persist + final renderer pos), but
	// never toggle the overlay from a cancel.
	function onBtnCancel(e: PointerEvent) {
		if (!btnDrag) return;
		const wasDrag = btnDrag.moved;
		btnDrag = null;
		try {
			(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
		} catch {
			/* already released */
		}
		if (wasDrag) {
			saveUi();
			const p = ui.overlayBtnPos;
			if (playId >= 0) api.setOverlayButtonPos(playId, p.x, p.y).catch(() => {});
		}
	}

	// Click-to-engage: the container is input pass-through, so clicks on the video land
	// HERE. Capture phase on the whole screen — in native mode everything under the video
	// is occluded anyway, so any click in the session area means "take control".
	function onScreenDown(e: PointerEvent) {
		// The overlay-button hotspot opens the overlay; it must never count as the
		// click-to-engage opt-in.
		if ((e.target as HTMLElement | null)?.closest?.('.ovbtn-hotspot')) return;
		// Split mode: a click anywhere in this pane focuses it (routes keyboard/mouse +
		// unlocked controllers here) before the engage/pointer handling below.
		onPaneFocus?.();
		// GAME mode only: engage the evdev grab (relative mouselook) on the click. REMOTE mode
		// drives control through the canvas VNC path (input.onDown → absolute pointer + buttons)
		// instead — the grab is fragile (needs its capture thread up) and useless for a phone
		// host, and its relative motion never reached hosts where the grab didn't engage.
		if (native && mode === 'game' && !nativeEngaged) {
			api.kbdEngage().catch(() => {});
			// Start the host cursor where the user clicked: map the click into the video
			// rect (normalized 0..1) and send it as one absolute move before the relative
			// capture takes over — so control begins at the clicked spot, not wherever the
			// host pointer happened to be.
			const el = screenEl;
			if (el && playId >= 0) {
				const r = el.getBoundingClientRect();
				if (r.width > 0 && r.height > 0) {
					const nx = Math.min(1, Math.max(0, (e.clientX - r.x) / r.width));
					const ny = Math.min(1, Math.max(0, (e.clientY - r.y) / r.height));
					api.inputPointer(playId, nx, ny).catch(() => {});
				}
			}
		}
	}

	// Size the windowed session to the HOST's aspect ratio on the first decoded frame
	// (`play-dims` from the native renderer): keep the current window width, set the
	// height so the video area matches the stream's w:h exactly (16:9 host → 16:9
	// video, 4:3 → 4:3). Re-fires when the host's ORIENTATION flips (portrait↔landscape) —
	// e.g. a phone host rotating mid-session re-encodes at swapped dims, and the window
	// should follow so the video isn't stuck letterboxed in the old shape. Skipped in
	// fullscreen/maximized (and for minor AR jitter — only a portrait↔landscape flip re-sizes).
	// Live decoded stream dimensions (from `play-dims`), used by the input engine to map the
	// cursor over the real video rect (letterbox-aware).
	let streamW = $state(0);
	let streamH = $state(0);
	let sizedToHost = false;
	let sizedLandscape: boolean | null = null;
	$effect(() => {
		if (!native || playId < 0) return;
		const scope = listenScope();
		scope.add(onPlayDims(async (e) => {
			if (e.id !== playId || e.w <= 0 || e.h <= 0) return;
			// Track the live stream dims so the input engine maps the cursor over the ACTUAL
			// video letterbox rect (a portrait phone in a wider window is pillarboxed — mapping
			// over the whole canvas would over-scale one axis). Always updated, even fullscreen.
			if (streamW !== e.w || streamH !== e.h) {
				console.warn(`[session] stream dims ${e.w}x${e.h} (input letterbox mapping active)`);
			}
			streamW = e.w;
			streamH = e.h;
			if (fullscreen) return;
			const landscape = e.w >= e.h;
			// First frame, or an orientation flip → (re)size; otherwise leave the window alone
			// so the user's manual resize sticks.
			if (sizedToHost && sizedLandscape === landscape) return;
			sizedToHost = true;
			sizedLandscape = landscape;
			try {
				const { getCurrentWindow } = await import('@tauri-apps/api/window');
				const { LogicalSize } = await import('@tauri-apps/api/dpi');
				const win = getCurrentWindow();
				if ((await win.isFullscreen()) || (await win.isMaximized())) return;
				// Chrome = everything around the video area (titlebar + tabs).
				const chrome = Math.max(
					0,
					window.innerHeight - (screenEl?.getBoundingClientRect().height ?? window.innerHeight)
				);
				let w = window.innerWidth;
				let h = Math.round(chrome + (w * e.h) / e.w);
				const maxH = Math.round((window.screen?.availHeight ?? 1080) * 0.92);
				if (h > maxH) {
					h = maxH;
					w = Math.round(((h - chrome) * e.w) / e.h);
				}
				await win.setSize(new LogicalSize(w, h));
			} catch {
				/* not in Tauri / no window control — keep the current size */
			}
		}));
		return scope.dispose;
	});

	// Single-surface (Linux): the rkmpp video is a GtkGLArea BEHIND this webview, so the
	// whole page must be transparent over the video — flag <html> so the app frame
	// (`.window`) goes transparent and the GLArea shows through. Chrome/dock stay opaque.
	$effect(() => {
		if (typeof document === 'undefined') return;
		document.documentElement.toggleAttribute('data-embedded', useEmbedded);
		return () => document.documentElement.removeAttribute('data-embedded');
	});

	// Remote-control input engine (the `controlling` flag, absolute-positioning pointer/key
	// forwarding, the rAF pump). See Session/input.svelte.ts. Created at component init so its
	// effect scopes to + tears down with this component; inputs are getters so it tracks them.
	const input = new SessionInput({
		playId: () => playId,
		wsPort: () => wsPort,
		canvas: () => canvas,
		mode: () => mode,
		native: () => native,
		streamW: () => streamW,
		streamH: () => streamH,
		fitMode: () => fitMode
	});
	function stopControl() {
		input.stopControl();
	}

	// Side channels — clipboard, file transfer, two-way chat, microphone (remote-desktop only).
	// Owns the chat/clipboard/file/mic state + the inbound-events effect; the active menu body
	// (`panel`) lives here too (chat is a side channel). See Session/sidechannels.svelte.ts.
	const sidechan = new SessionSideChannels({
		playId: () => playId,
		menuOpen: () => dock.menuOpen
	});

	// Floating-menu + game-overlay UI controller (dock open/floating-drag, the Ctrl+Shift+M
	// overlay's debounced open/close, Escape-to-close). See Session/ui.svelte.ts. Cross-cutting
	// actions are passed as callbacks (release control, end the tab, fullscreen, reset panel).
	const dock = new SessionUi({
		playId: () => playId,
		stopControl,
		onEnd: () => onEnd(),
		onToggleFullscreen: () => onToggleFullscreen(),
		resetPanel: () => (sidechan.panel = 'menu')
	});

	// Keyboard relay for the overlay's NATIVE Chat composer: while the overlay is
	// open the evdev grab is suspended, so keys land in this (focused) webview —
	// pipe them to the renderer (its child window can't take X focus without
	// killing the focus-gated combos). Shortcuts (Ctrl/Alt/Meta chords) pass through.
	$effect(() => {
		if (!native || playId < 0 || typeof window === 'undefined' || !dock.overlayOpen) return;
		const navDirs: Record<string, string> = {
			ArrowUp: 'up',
			ArrowDown: 'down',
			ArrowLeft: 'left',
			ArrowRight: 'right',
			Escape: 'escape'
		};
		const onKey = (e: KeyboardEvent) => {
			if (e.ctrlKey || e.altKey || e.metaKey) return;
			const dir = navDirs[e.key];
			if (dir) {
				// Arrows / Escape drive the overlay nav on the SAME `k <dir>` channel the pad
				// uses → keyboard navigates the menus (up/down/left/right) like a controller.
				api.renderNav(playId, dir).catch(() => {});
			} else if (e.key === 'Enter') {
				// Enter both activates the focused nav item (non-chat views) AND sends the
				// chat composer (chat view) — each view consumes only its own channel.
				api.renderNav(playId, 'go').catch(() => {});
				api.renderKin(playId, 'k', 'enter').catch(() => {});
			} else if (e.key.length === 1) {
				api.renderKin(playId, 't', e.key).catch(() => {});
			} else if (e.key === 'Backspace') {
				api.renderKin(playId, 'k', 'backspace').catch(() => {});
			} else {
				return;
			}
			e.preventDefault();
			e.stopPropagation();
		};
		window.addEventListener('keydown', onKey, true);
		return () => window.removeEventListener('keydown', onKey, true);
	});

	// Auto-fade the in-control hint after a couple of seconds (Parsec-style); it
	// reappears each time control is (re-)taken.
	let hintFade = $state(false);
	$effect(() => {
		if (!input.controlling) {
			hintFade = false;
			return;
		}
		hintFade = false;
		const tmr = setTimeout(() => (hintFade = true), 2500);
		return () => clearTimeout(tmr);
	});

	// The OS-level keyboard hook / evdev capture saw the LEAVE combo (Ctrl+Shift+Q — the
	// webview never gets those keys while capture suppresses them). Native: END the
	// session (the user's exit combo); webview: just drop the canvas control. Releasing
	// control WITHOUT ending is 3×RightCtrl → kbd-released (handled above). Payload-less
	// global event: only the active tab may act, or one combo would end every session.
	$effect(() => {
		const scope = listenScope();
		scope.add(
			onKbdLeave(() => {
				if (!active) return;
				if (native) onEnd();
				else stopControl();
			})
		);
		return scope.dispose;
	});

	// Host closed the session (or a network error) — the hold-loop emits `play-ended`. Release
	// the input grab and end the tab so the native path doesn't freeze on mpv's last frame with
	// the keyboard/mouse still captured (you'd be stuck needing an SSH kill). kbdCaptureStop is
	// called directly (not via stopControl) so it ungrabs regardless of the `controlling` flag.
	$effect(() => {
		const scope = listenScope();
		scope.add(
			onPlayEnded((eid) => {
				if (eid !== playId) return;
				// Only the capture owner disarms — a BACKGROUND tab's death must not
				// drop the active tab's grab.
				if (captureOwner === playId) {
					captureOwner = -1;
					api.kbdCaptureStop().catch(() => {});
				}
				nativeEngaged = false;
				onEnd();
			})
		);
		return scope.dispose;
	});

	// Ctrl+Shift+M (from the OS-level keyboard hook / evdev capture) toggles the game
	// overlay without ending the session. Payload-less, like kbd-leave; applies to this
	// active play tab.
	$effect(() => {
		const scope = listenScope();
		// The OS keyboard hook / evdev emits overlay-toggle whenever capture is active, with no
		// knowledge of mode. The webview overlay only renders in game mode (`{#if mode === 'game'}`),
		// so on Windows/macOS remote sessions (native=false) a toggle would just drop control with
		// nothing shown. Only honor it in game mode, or when a native renderer (Linux) is up — there
		// the overlay is drawn on the video and works in any mode. Payload-less global event:
		// only the active tab toggles, never every mounted game/native tab at once.
		scope.add(
			onOverlayToggle(() => {
				if (active && (mode === 'game' || native)) dock.toggleOverlay();
			})
		);
		return scope.dispose;
	});

	// Linux native overlay (`pulsar-render`): the egui overlay is the real UI on Linux (the
	// webview overlay is occluded by the video), so its interactions arrive as events and apply
	// through the SAME setters — codec/encoder/fps/etc go to the host, End/Close mirror locally.
	$effect(() => {
		const scope = listenScope();
		scope.add(onOverlayCmd((id, field, val) => {
			if (id !== playId) return;
			switch (field) {
				case 'codec': controls.setCodec(val as VideoCodec); break;
				case 'encoder': controls.setEncoder(val as Encoder); break;
				case 'res': controls.setRes(val as 'auto' | '1080p' | '1440p' | '4K'); break;
				case 'fps': controls.setFps(val as 'auto' | '30' | '60' | '120'); break;
				case 'display': controls.setMonitor(Number(val) || 0); break;
				case 'bitrate': controls.setBitrate(Number(val) || 0); break;
				case 'quality': controls.setQuality(val as 'latency' | 'quality'); break;
				case 'pace': controls.setFramePacing(val === 'on' || val === '1' || val === 'true'); break;
				case 'statshud': controls.setStatsHud(val === 'on' || val === '1' || val === 'true'); break;
				case 'ovbtn': controls.setOverlayButton(val === 'on' || val === '1' || val === 'true'); break;
				// Renderer-side button drag (Windows: the webview hotspot is buried under
				// the video child): mirror + persist the new position, like the hotspot drag.
				case 'btnpos': {
					const [bx, by] = val.split(',').map(Number);
					if (Number.isFinite(bx) && Number.isFinite(by)) {
						ui.overlayBtnPos = { x: bx, y: by };
						api.setOverlayButtonPos(playId, bx, by).catch(() => {});
					}
					break;
				}
				// View-fit is applied renderer-side instantly; mirror it for the webview
				// canvas path so both presenters agree.
				case 'fit': fitMode = (val as 'fit' | 'stretch' | 'original') ?? 'fit'; break;
				// Ses section: host-audio transmit / host-mute / mic — reuse the menu's
				// toggles (flip only when the desired state differs).
				case 'atx': if ((val === 'on') !== controls.transmitAudio) controls.toggleTransmit(); break;
				case 'amute': if ((val === 'on') !== controls.muteHost) controls.toggleMute(); break;
				case 'mic': if ((val === 'on') !== sidechan.micOn) sidechan.toggleMic(); break;
				// Voice call: pair mic (us → host) with host audio (host → us) in one switch;
				// turning it off only drops the mic (host audio stays as the user had it).
				case 'call': {
					const on = val === 'on';
					if (on && !controls.transmitAudio) controls.toggleTransmit();
					if (on !== sidechan.micOn) sidechan.toggleMic();
					break;
				}
				case 'sendclip': sidechan.sendClipboard(); break;
				// 'pickfile' is handled Rust-side (rfd native dialog) — the renderer
				// intercepts the cmd before emitting overlay-cmd so this case is never
				// reached; kept as a no-op guard against stale renderer builds.
				case 'pickfile': break;
				// Overlay controller swap: the egui list's ▲/▼ buttons emit
				// `ov set ctrlswap i,j` where i and j are row indices (= player slots).
				// Seed any missing slots from the live controller list first so the
				// indices always map, then swap the two UUIDs in controllerOrder,
				// persist, and push the new order to the Rust reader (which will
				// re-emit the updated `ctrls` line on the next 16 ms tick).
				case 'ctrlswap': {
					const [ci, cj] = val.split(',').map(Number);
					if (!Number.isFinite(ci) || !Number.isFinite(cj) || ci === cj) break;
					api.controllers().then((pads) => {
						const connected = pads.filter((p) => p.connected);
						const order = [...ui.controllerOrder];
						// applyCtrlSwap extends `order` for missing slots (seeded from the
						// live connected pad list) then swaps ci↔cj in place.
						const swapped = applyCtrlSwap(
							order,
							(slot) => connected.find((p) => !order.includes(p.uuid))?.uuid ?? '',
							ci,
							cj
						);
						if (!swapped) return;
						ui.controllerOrder.length = 0;
						ui.controllerOrder.push(...order);
						saveUi();
						api.setControllerOrder($state.snapshot(ui.controllerOrder) as string[]).catch(() => {});
					}).catch(() => {});
					break;
				}
				// Overlay emulation-target picker: the egui seg buttons emit
				// `ov set ctrlemu uuid,target` where target is 'auto'/'xbox360'/'ds4'.
				// Persist to ui.controllerEmulation and push the new map to the Rust reader.
				case 'ctrlemu': {
					const comma = val.indexOf(',');
					if (comma < 0) break;
					const uuid = val.slice(0, comma);
					const target = val.slice(comma + 1) as 'auto' | 'xbox' | 'xbox360' | 'ds4';
					// Normalise 'xbox360' token from the renderer to 'xbox' used by the settings map.
					const normalised: 'auto' | 'xbox' | 'ds4' = target === 'xbox360' ? 'xbox' : target;
					if (uuid) {
						ui.controllerEmulation[uuid] = normalised;
						saveUi();
						api.setControllerEmulation($state.snapshot(ui.controllerEmulation) as Record<string, string>).catch(() => {});
					}
					break;
				}
				// Overlay PER-PAD vibration picker: the egui Controllers view emits
				// `ov set ctrlrumble uuid,level` (off/weak/medium/strong). Persist to
				// ui.controllerRumble[uuid] and push the map to the SDL pad manager (live).
				case 'ctrlrumble': {
					const comma = val.indexOf(',');
					if (comma < 0) break;
					const uuid = val.slice(0, comma);
					const lvl = val.slice(comma + 1).trim();
					if (uuid && (lvl === 'off' || lvl === 'weak' || lvl === 'medium' || lvl === 'strong')) {
						ui.controllerRumble[uuid] = lvl;
						saveUi();
						api.setControllerRumble($state.snapshot(ui.controllerRumble) as Record<string, string>).catch(() => {});
					}
					break;
				}
				// Overlay enable/disable SET: the egui Controllers view emits
				// `ov set ctrldisable uuid,state` (state 1 = disabled, 0 = enabled).
				case 'ctrldisable': {
					const comma = val.indexOf(',');
					if (comma < 0) break;
					const uuid = val.slice(0, comma);
					const state = val.slice(comma + 1).trim();
					if (uuid) {
						if (state === '1') ui.controllerDisabled[uuid] = true;
						else delete ui.controllerDisabled[uuid];
						saveUi();
						const list = Object.keys($state.snapshot(ui.controllerDisabled));
						api.setDisabledControllers(list).catch(() => {});
					}
					break;
				}
				// Overlay controller-LOCK SET (split mode): the egui Controllers view emits
				// `ov set ctrllock uuid,state` (state 1 = locked to this pane, 0 = unlocked).
				// Mirrors `ctrldisable` exactly; forwarded with this session's play id so the
				// backend can scope the lock owner.
				case 'ctrllock': {
					const comma = val.indexOf(',');
					if (comma < 0) break;
					const uuid = val.slice(0, comma);
					const state = val.slice(comma + 1).trim();
					if (uuid) {
						api.setControllerLock(uuid, playId, state === '1').catch(() => {});
					}
					break;
				}
				// Overlay vibration TEST: `ov set ctrltest <uuid>` → one-shot rumble pulse on
				// that pad at its current level so the player can feel it.
				case 'ctrltest': {
					const uuid = val.trim();
					if (uuid) api.testControllerRumble(uuid).catch(() => {});
					break;
				}
				case 'reverse': reverse(); break;
				case 'fullscreen': onToggleFullscreen(); break;
				// Overlay "Ekran uyarlama" (screen adaptation): `ov set adapt <w>x<h>` turns it on
				// with the pane's pixel size, `ov set adapt off` turns it off. The host switches the
				// captured monitor to the best-fit mode (and reverts on exit).
				case 'adapt': {
					if (val === 'off') {
						controls.setAdapt(false, 0, 0);
					} else {
						const m = /^(\d+)x(\d+)$/.exec(val);
						if (m) controls.setAdapt(true, Number(m[1]), Number(m[2]));
					}
					break;
				}
			}
		}));
		// Native Chat: composer line from the overlay → host + echo back so the
		// renderer's log shows it.
		scope.add(onOverlayChat((e) => {
			if (e.id !== playId) return;
			api.sendChat(playId, e.text).catch(() => {});
			api.renderChat(playId, 'out', e.text).catch(() => {});
		}));
		// Native Files: remote-pane requests → the session's fs commands; replies
		// flow back via onFsEntries → renderFs below.
		scope.add(onOverlayFs((e) => {
			if (e.id !== playId) return;
			if (e.op === 'fsls') api.fsList(playId, e.path).catch(() => {});
			else if (e.op === 'fsget') api.fsGet(playId, e.path).catch(() => {});
			else if (e.op === 'fssend') api.sendFilePath(playId, e.path).catch(() => {});
		}));
		scope.add(onFsEntries((e) => {
			if (e.id !== playId) return;
			api
				.renderFs(playId, JSON.stringify({ path: e.path, entries: e.entries }))
				.catch(() => {});
		}));
		// Overlay Files box → the dedicated per-session file window (we own the
		// peer label; the renderer only signals the click).
		scope.add(onOverlayFiles((id) => {
			if (id === playId) api.openFilesWindow(playId, displayTarget.name).catch(() => {});
		}));
		// End/Close carry the play id — gate like onOverlayChat/onOverlayFs, or one
		// native overlay's End would terminate EVERY mounted session tab.
		scope.add(onOverlayEnd((id) => {
			if (id === playId) dock.endSession();
		}));
		scope.add(onOverlayClose((id) => {
			if (id === playId) dock.closeOverlay();
		}));
		// NOTE: focus loss must NOT close the overlay (alt-tabbing away and back kept
		// losing it). It can't strand either: a click on the overlay refocuses the
		// app (renderer XSetInputFocus), so the close combo + scrim click keep working.
		return scope.dispose;
	});

</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="screen" class:embedded={useEmbedded} bind:this={screenEl} onpointerdowncapture={onScreenDown}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<canvas
		bind:this={canvas}
		class="video {fitMode}"
		class:on={media.hasVideo}
		class:nativeinput={native}
		class:control={input.controlling}
		tabindex="0"
		onpointerdown={input.onDown}
		onpointermove={input.onMove}
		onpointerup={input.onUp}
		onwheel={input.onWheel}
		onpointerleave={input.clearMove}
		onblur={stopControl}
		oncontextmenu={(e) => e.preventDefault()}
	></canvas>

	<VideoStatus
		{native}
		{embedded}
		{mode}
		{target}
		{nativeEngaged}
		hasVideo={media.hasVideo}
		videoErr={media.videoErr}
		stalled={media.stalled}
		controlling={input.controlling}
		{hintFade}
		onStartControl={input.startControl}
	/>

	<!-- The in-session UI (menu/overlay/HUD) is the NATIVE egui overlay drawn by
	     pulsar-render over the video on every platform — `native` is always true, so the
	     old webview Menu/Overlay surfaces were removed. Everything else here is non-visual
	     plumbing (input capture, viewrect, engage, overlay-cmd handlers). -->

	<!-- Parsec-style overlay-open hotspot: the VISUAL button is drawn by the native
	     renderer (egui, over the video); this invisible webview button sits at the
	     exact same spot and receives the pointer (the video container is input
	     pass-through on Linux). Click = open the overlay; drag = move the button
	     (live-streamed to the renderer, persisted). Hidden while open / disabled. -->
	{#if native && ui.overlayButton && !dock.overlayOpen}
		<button
			class="ovbtn-hotspot"
			aria-label={t('session.overlayBtn')}
			style="left:{btnCss.x}px; top:{btnCss.y}px"
			onpointerdown={onBtnDown}
			onpointermove={onBtnMove}
			onpointerup={onBtnUp}
			onpointercancel={onBtnCancel}
			onlostpointercapture={onBtnCancel}
		></button>
	{/if}

	<!-- Codec/encoder switch veil: the Rust side hides the native video container for
	     the restart window, so this brief loading screen is what the user sees. -->
	{#if controls.switching}
		<div class="switchveil">
			<div class="pulse" aria-hidden="true"><span></span><span></span><span></span></div>
			<div class="swmsg">{t('session.switching')}</div>
		</div>
	{/if}

	<!-- hidden picker for "send file" -->
	<input class="filepick" type="file" bind:this={sidechan.fileInput} onchange={sidechan.onFilePicked} />
</div>

<style>
	.screen {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		background:
			radial-gradient(700px 380px at 50% 30%, oklch(0.3 0.06 272 / 0.3), transparent 70%),
			#0c0d12;
		overflow: hidden;
	}
	/* Single-surface (Linux): the video is a GtkGLArea BEHIND this webview, so the screen
	   must be transparent to show it through; the dock/menu/hints stay opaque on top. */
	.screen.embedded {
		background: transparent;
	}
	.video {
		display: none;
		background: #000;
	}
	.video.on {
		display: block;
	}
	/* Native (Linux) renderer: the canvas is never drawn to (the video is a native child
	   window composited over the webview) but it IS the VNC input surface — the pointer
	   handlers live on it. Without this it was an unsized 300×150 box, hidden until vstats
	   flowed (display:none) — clicks/moves mostly missed it, so control never started and
	   the phone cursor never appeared. Make it a transparent full-bleed input layer that is
	   always hittable while a native session is up. */
	.video.nativeinput {
		display: block;
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		background: transparent;
	}
	/* AnyDesk-style fit modes */
	.video.fit {
		max-width: 100%;
		max-height: 100%;
	}
	/* "Doldur" = fill the height (vertical), preserving aspect; width overflows and
	   is cropped by the screen's overflow:hidden rather than distorting horizontally. */
	.video.stretch {
		height: 100%;
		width: auto;
		max-width: none;
		max-height: none;
	}
	.video.original {
		max-width: none;
		max-height: none;
	}
	.video:focus {
		outline: none;
	}
	.video.control {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
		/* While controlling, the HOST's cursor (rendered into the stream) is the pointer —
		   hide the local one over the video so there aren't two arrows fighting (and any
		   sub-frame offset between them is invisible). Leaving the video/stopping control
		   restores the normal cursor. */
		cursor: none;
	}
	.filepick {
		display: none;
	}
	/* invisible pointer target matching the renderer-drawn Pulsar-mark button
	   (drag-movable; left/top come inline from ui.overlayBtnPos in egui pt × 1.25
	   scale, button ≈45px → 48px hotspot) */
	.ovbtn-hotspot {
		position: absolute;
		width: 48px;
		height: 48px;
		border: none;
		background: transparent;
		cursor: pointer;
		z-index: 6;
		touch-action: none;
	}
	.switchveil {
		position: absolute;
		inset: 0;
		z-index: 5;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 10px;
		background: #0c0d12;
	}
	.switchveil .pulse {
		position: relative;
		width: 72px;
		height: 72px;
	}
	.switchveil .pulse span {
		position: absolute;
		inset: 0;
		margin: auto;
		width: 72px;
		height: 72px;
		border-radius: 50%;
		border: 2px solid var(--accent);
		opacity: 0;
		animation: swpulse 1.6s cubic-bezier(0.2, 0.6, 0.3, 1) infinite;
	}
	.switchveil .pulse span:nth-child(2) {
		animation-delay: 0.53s;
	}
	.switchveil .pulse span:nth-child(3) {
		animation-delay: 1.06s;
	}
	@keyframes swpulse {
		0% {
			transform: scale(0.25);
			opacity: 0.9;
		}
		100% {
			transform: scale(1);
			opacity: 0;
		}
	}
	.swmsg {
		font-size: 13px;
		color: oklch(0.8 0.01 265);
	}
</style>
