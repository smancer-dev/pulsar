<script lang="ts" module>
	// Which play currently OWNS the global keyboard/mouse capture (kbdhook is one
	// per app, last call wins). On a tab switch the effect re-run order across
	// sibling Session components isn't guaranteed, so a deactivating tab must not
	// disarm a capture another tab just re-armed — only the recorded owner may stop.
	let captureOwner = -1;
</script>

<script lang="ts">
	import Menu from './Session/Menu.svelte';
	import Overlay from './Session/Overlay.svelte';
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
	import { ui, saveUi, type Encoder, type VideoCodec } from '$lib/settings.svelte';
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
		fullscreen?: boolean;
		/** True while this is the ACTIVE tab. Inactive session tabs stay mounted (just
		 * CSS-hidden), so the GLOBAL keyboard/mouse capture and the payload-less combo
		 * events (kbd-leave / overlay-toggle / kbd-engaged…) must only be owned/handled
		 * by the visible tab — otherwise one combo ends/changes EVERY open session. */
		active?: boolean;
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
		fullscreen = false,
		active = true,
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
			embedded: () => embedded
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
		api.kbdCaptureStart(playId, true).catch(() => {}); // true = also capture mouse
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
	let screenEl = $state<HTMLDivElement | null>(null);
	$effect(() => {
		const el = screenEl;
		if (!native || embedded || playId < 0 || !el) return;
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
		report();
		return () => {
			ro.disconnect();
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
		if (native && !nativeEngaged) api.kbdEngage().catch(() => {});
	}

	// Size the windowed session to the HOST's aspect ratio on the first decoded frame
	// (`play-dims` from the native renderer): keep the current window width, set the
	// height so the video area matches the stream's w:h exactly (16:9 host → 16:9
	// video, 4:3 → 4:3). Once per session; skipped in fullscreen/maximized.
	let sizedToHost = false;
	$effect(() => {
		if (!native || playId < 0) return;
		const scope = listenScope();
		scope.add(onPlayDims(async (e) => {
			if (e.id !== playId || sizedToHost || fullscreen || e.w <= 0 || e.h <= 0) return;
			sizedToHost = true;
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
		document.documentElement.toggleAttribute('data-embedded', embedded);
		return () => document.documentElement.removeAttribute('data-embedded');
	});

	// Remote-control input engine (the `controlling` flag, absolute-positioning pointer/key
	// forwarding, the rAF pump). See Session/input.svelte.ts. Created at component init so its
	// effect scopes to + tears down with this component; inputs are getters so it tracks them.
	const input = new SessionInput({
		playId: () => playId,
		wsPort: () => wsPort,
		canvas: () => canvas
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
		const onKey = (e: KeyboardEvent) => {
			if (e.ctrlKey || e.altKey || e.metaKey) return;
			if (e.key.length === 1) {
				api.renderKin(playId, 't', e.key).catch(() => {});
			} else if (e.key === 'Backspace') {
				api.renderKin(playId, 'k', 'backspace').catch(() => {});
			} else if (e.key === 'Enter') {
				api.renderKin(playId, 'k', 'enter').catch(() => {});
			} else if (e.key === 'ArrowLeft') {
				api.renderKin(playId, 'k', 'left').catch(() => {});
			} else if (e.key === 'ArrowRight') {
				api.renderKin(playId, 'k', 'right').catch(() => {});
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
				// Tools shortcuts that live app-side: the OS file dialog (its own
				// toplevel — visible over the video), reverse direction, fullscreen.
				case 'pickfile': sidechan.pickFile(); break;
				case 'reverse': reverse(); break;
				case 'fullscreen': onToggleFullscreen(); break;
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
<div class="screen" class:embedded bind:this={screenEl} onpointerdowncapture={onScreenDown}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<canvas
		bind:this={canvas}
		class="video {fitMode}"
		class:on={media.hasVideo}
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

	<!-- Floating control handle + expandable menu — WEBVIEW path only (Windows/macOS).
	     On the Linux NATIVE path this whole surface is retired: the egui overlay now
	     owns every feature (Yayın/Görüntü/Ses/Sohbet/Dosyalar/Araçlar), and the old
	     dock handle would sit uselessly under the opaque video anyway. -->
	{#if !native}
	<Menu
		{playId}
		menuOpen={dock.menuOpen}
		controlling={input.controlling}
		floating={dock.floating}
		pos={dock.pos}
		keepVisible={ui.keepVisible}
		bind:statsHover={dock.statsHover}
		netClass={media.netClass}
		fps={media.fps}
		latencyMs={media.latencyMs}
		spark={media.spark}
		hostCodec={media.hostCodec}
		hostRes={media.hostRes}
		hostEncoder={media.hostEncoder}
		hostFps={media.hostFps}
		decoderCodec={media.decoderCodec}
		decodeMs={media.decodeMs}
		{connLabel}
		rttMs={media.rttMs}
		jitterMs={media.jitterMs}
		lossPct={media.lossPct}
		mbps={media.mbps}
		target={displayTarget}
		{peerAvatar}
		{mode}
		{fullscreen}
		bind:panel={sidechan.panel}
		messages={sidechan.messages}
		bind:chatInput={sidechan.chatInput}
		bind:chatBox={sidechan.chatBox}
		unread={sidechan.unread}
		note={sidechan.note}
		micOn={sidechan.micOn}
		transmitAudio={controls.transmitAudio}
		muteHost={controls.muteHost}
		framePacing={controls.framePacing}
		bind:fitMode
		codec={ui.codec}
		encoder={ui.encoder}
		{hostCodecs}
		{hostEncoders}
		{activeInfo}
		streamRes={controls.streamRes}
		streamFps={controls.streamFps}
		streamBitrate={controls.streamBitrate}
		streamQuality={controls.streamQuality}
		onCloseMenu={dock.closeMenu}
		onHandleClick={dock.handleClick}
		onHandleDown={dock.onHandleDown}
		onHandleMove={dock.onHandleMove}
		onHandleUp={dock.onHandleUp}
		onHandleCancel={dock.onHandleCancel}
		onCodec={controls.setCodec}
		onEncoder={controls.setEncoder}
		onRes={controls.setRes}
		onFps={controls.setFps}
		onBitrate={controls.setBitrate}
		onQuality={controls.setQuality}
		onFullscreen={dock.doFullscreen}
		onSendClipboard={sidechan.sendClipboard}
		onPickFile={sidechan.pickFile}
		onToggleMic={sidechan.toggleMic}
		onOpenChat={sidechan.openChat}
		onToggleFloating={dock.toggleFloating}
		onReverse={reverse}
		onToggleTransmit={controls.toggleTransmit}
		onToggleMute={controls.toggleMute}
		onToggleKeepVisible={controls.toggleKeepVisible}
		onToggleFramePacing={controls.toggleFramePacing}
		onSendChat={sidechan.sendChatLine}
		onEnd={dock.endSession}
	/>
	{/if}

	<!-- Game-only overlay (Ctrl+Shift+M). Opaque dialog so it stays visible while mpv is
	     paused on Linux. Perf HUD + the slim game controls (codec/encoder/decoder/res/fps/
	     bitrate/quality) + controllers + end — NO file/clipboard/mic/chat (remote-only). -->
	{#if mode === 'game' && dock.overlayOpen}
		<Overlay
			{target}
			{connLabel}
			netClass={media.netClass}
			fps={media.fps}
			latencyMs={media.latencyMs}
			decodeMs={media.decodeMs}
			mbps={media.mbps}
			codec={ui.codec}
			encoder={ui.encoder}
			{hostCodecs}
			{hostEncoders}
			decoderInfo={media.decoderCodec}
			{activeInfo}
			activeFps={media.hostFps}
			activeRes={media.hostRes}
			streamRes={controls.streamRes}
			streamFps={controls.streamFps}
			streamBitrate={controls.streamBitrate}
			streamQuality={controls.streamQuality}
			framePacing={controls.framePacing}
			onCodec={controls.setCodec}
			onEncoder={controls.setEncoder}
			onRes={controls.setRes}
			onFps={controls.setFps}
			onBitrate={controls.setBitrate}
			onQuality={controls.setQuality}
			onFramePacing={controls.setFramePacing}
			onClose={dock.closeOverlay}
			onEnd={dock.endSession}
		/>
	{/if}

	<!-- Parsec-style overlay-open hotspot: the VISUAL button is drawn by the native
	     renderer (egui, over the video); this invisible webview button sits at the
	     exact same spot and receives the pointer (the video container is input
	     pass-through on Linux). Click = open the overlay; drag = move the button
	     (live-streamed to the renderer, persisted). Hidden while open / disabled. -->
	{#if native && ui.overlayButton && !dock.overlayOpen}
		<button
			class="ovbtn-hotspot"
			aria-label="Overlay"
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
