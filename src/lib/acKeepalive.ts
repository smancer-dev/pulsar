// AC present-keepalive — opi5 / RK3588 / WebKitGTK only, and ONLY when accelerated (GPU)
// compositing is forced on (PULSAR_FORCE_AC=1).
//
// With AC on, WebKitGTK on Mali INTERMITTENTLY stops presenting frames once the page goes
// idle (no running animation): JS keeps running and the DOM updates, but the screen freezes
// on a stale frame (e.g. switching gaming-mode tabs froze the display). The compositor's
// frame clock goes idle and doesn't reliably restart on the next content change.
//
// Fix: keep the page perpetually "animating" with a continuous requestAnimationFrame that
// nudges a SUB-PIXEL transform on a tiny, invisible, own-layer (`will-change:transform`)
// element every frame. That keeps the GDK frame clock requesting frames, so the compositor
// never goes idle → never stalls. No visible effect (2px, opacity ~0, sub-pixel motion).
//
// Cost: one composited transform per frame — negligible on the GPU path, and the whole point
// is to keep that path alive. Gated to AC-on (the default software path doesn't need it and a
// per-frame repaint there would be expensive).
let started = false;

export function startAcKeepalive(): void {
	if (started || typeof document === 'undefined' || typeof requestAnimationFrame === 'undefined')
		return;
	started = true;
	const el = document.createElement('div');
	el.setAttribute('aria-hidden', 'true');
	el.style.cssText =
		'position:fixed;top:0;left:0;width:2px;height:2px;pointer-events:none;' +
		'opacity:0.01;z-index:-1;will-change:transform;contain:strict;';
	const attach = () => {
		(document.body ?? document.documentElement).appendChild(el);
		let f = 0;
		const tick = () => {
			// Alternate a half-pixel translate so each frame is a real (composited) change.
			f ^= 1;
			el.style.transform = `translate3d(${f * 0.5}px,0,0)`;
			requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	};
	if (document.body) attach();
	else document.addEventListener('DOMContentLoaded', attach, { once: true });
}
