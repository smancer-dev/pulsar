//! Host cursor side-channel (Linux/X11). The KMS zero-copy capture scans the screen out
//! WITHOUT the X hardware cursor (it lives on its own DRM plane), so a remote desktop session
//! over that path would show no pointer. Following Moonlight's model we separate the cursor
//! from the video: this module reads the X pointer position (~60 Hz) + shape (on change) via
//! XFixes and ships them out-of-band as [`DataMsg::CursorPos`]/[`CursorShape`]/[`CursorHidden`];
//! the client's native renderer draws the pointer over the video.
//!
//! Started only when the client advertised it can draw the cursor (`StreamReq::cursor_external`)
//! AND the host actually capture-without-cursor (the gated KMS path) — otherwise the cursor is
//! already in the frame and a side-channel one would double up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pulsar_core::service::DataMsg;
use tokio::sync::mpsc::Sender;

/// Spawn the X11 cursor poller on a dedicated OS thread (Xlib is not `Send`/async-friendly).
/// It runs until `alive` flips false (session teardown) or X errors. Returns immediately; all
/// work is on the spawned thread. `out` is the session's host→client `DataMsg` sender (the
/// same `stats_out` the encode summary uses) — `try_send` so a full queue never blocks capture.
pub(crate) fn spawn(out: Sender<DataMsg>, alive: Arc<AtomicBool>) {
	std::thread::Builder::new()
		.name("pulsar-cursor".into())
		.spawn(move || unsafe { run(out, alive) })
		.ok();
}

/// XFixes cursor poll loop. Position is read with `XQueryPointer` against the root window and
/// normalized to 0..1 by the root geometry; shape with `XFixesGetCursorImage` (sent only when
/// the cursor `serial` changes — i.e. the bitmap actually changed). A position outside the
/// screen or an absent cursor maps to [`DataMsg::CursorHidden`].
unsafe fn run(out: Sender<DataMsg>, alive: Arc<AtomicBool>) {
	use x11::{xfixes, xlib};

	let dpy = xlib::XOpenDisplay(std::ptr::null());
	if dpy.is_null() {
		tracing::warn!("cursor side-channel: XOpenDisplay failed");
		return;
	}
	// XFixes must be present for the shape; position works without it. Probe once.
	let (mut ev_base, mut err_base) = (0i32, 0i32);
	let have_xfixes = xfixes::XFixesQueryExtension(dpy, &mut ev_base, &mut err_base) != 0;
	let root = xlib::XDefaultRootWindow(dpy);

	// Root geometry → normalization basis. Re-read occasionally is overkill; the screen size is
	// stable for a session (a resolution change restarts the stream → restarts this thread).
	let (mut rw, mut rh) = root_size(dpy, root);
	if rw == 0 || rh == 0 {
		(rw, rh) = (1, 1);
	}

	let mut last_pos: Option<(f32, f32)> = None;
	let mut last_serial: u64 = 0;
	let mut last_hidden = false;
	// ~60 Hz poll. Cheap (one round-trip); the shape fetch only runs when the serial moves.
	let period = std::time::Duration::from_millis(16);

	while alive.load(Ordering::SeqCst) {
		let mut root_ret = 0u64;
		let mut child_ret = 0u64;
		let (mut rx, mut ry, mut wx, mut wy) = (0i32, 0i32, 0i32, 0i32);
		let mut mask = 0u32;
		let on_screen = xlib::XQueryPointer(
			dpy, root, &mut root_ret, &mut child_ret, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask,
		) != 0
			&& rx >= 0 && ry >= 0
			&& (rx as u32) < rw
			&& (ry as u32) < rh;

		// The session sender being closed means the session is gone — stop the thread so
		// it can't outlive the session (a stale poller would leak until X errored).
		if out.is_closed() {
			break;
		}
		if !on_screen {
			if !last_hidden {
				last_hidden = true;
				let _ = out.try_send(DataMsg::CursorHidden);
			}
		} else {
			last_hidden = false;
			// Shape change (caret/resize/etc): only when the serial moved — keeps the
			// side-channel to two f32 per tick in the common case.
			if have_xfixes {
				let img = xfixes::XFixesGetCursorImage(dpy);
				if !img.is_null() {
					let serial = (*img).cursor_serial as u64;
					if serial != last_serial {
						last_serial = serial;
						if let Some(msg) = encode_shape(img) {
							let _ = out.try_send(msg);
						}
					}
					xlib::XFree(img as *mut _);
				}
			}
			let nx = (rx as f32 / rw as f32).clamp(0.0, 1.0);
			let ny = (ry as f32 / rh as f32).clamp(0.0, 1.0);
			// Only send on real movement (>~0.5 px at 1080p) so a still pointer is silent.
			let moved = match last_pos {
				Some((px, py)) => (nx - px).abs() > 0.0003 || (ny - py).abs() > 0.0003,
				None => true,
			};
			if moved {
				last_pos = Some((nx, ny));
				let _ = out.try_send(DataMsg::CursorPos { x: nx, y: ny });
			}
		}
		std::thread::sleep(period);
	}
	xlib::XCloseDisplay(dpy);
}

/// Root window size (the normalization basis for the pointer position).
unsafe fn root_size(dpy: *mut x11::xlib::Display, root: u64) -> (u32, u32) {
	use x11::xlib;
	let mut r = 0u64;
	let (mut x, mut y, mut bw, mut depth) = (0i32, 0i32, 0u32, 0u32);
	let (mut w, mut h) = (0u32, 0u32);
	if xlib::XGetGeometry(
		dpy, root, &mut r, &mut x, &mut y, &mut w, &mut h, &mut bw, &mut depth,
	) != 0
	{
		(w, h)
	} else {
		(0, 0)
	}
}

/// Turn an `XFixesCursorImage` (ARGB premultiplied, one `unsigned long` per pixel) into a
/// [`DataMsg::CursorShape`] with PNG-encoded RGBA — small (cursors are 32–64 px) and decodable
/// by the client. Returns `None` if the image is empty or PNG encoding fails.
unsafe fn encode_shape(img: *const x11::xfixes::XFixesCursorImage) -> Option<DataMsg> {
	let w = (*img).width as u32;
	let h = (*img).height as u32;
	if w == 0 || h == 0 || w > 256 || h > 256 {
		return None;
	}
	// XFixes packs each pixel into a native `unsigned long` (64-bit on this target) as
	// premultiplied ARGB in the low 32 bits. Convert to straight (un-premultiplied) RGBA8 so a
	// generic PNG decoder + the renderer's alpha-blend draw it correctly.
	let px = (*img).pixels;
	let n = (w * h) as usize;
	let mut rgba = vec![0u8; n * 4];
	for i in 0..n {
		let argb = *px.add(i) as u32;
		let a = ((argb >> 24) & 0xff) as u8;
		let mut r = ((argb >> 16) & 0xff) as u16;
		let mut g = ((argb >> 8) & 0xff) as u16;
		let mut b = (argb & 0xff) as u16;
		// Un-premultiply (XFixes alpha-premultiplies the channels).
		if a > 0 {
			r = (r * 255 / a as u16).min(255);
			g = (g * 255 / a as u16).min(255);
			b = (b * 255 / a as u16).min(255);
		}
		let o = i * 4;
		rgba[o] = r as u8;
		rgba[o + 1] = g as u8;
		rgba[o + 2] = b as u8;
		rgba[o + 3] = a;
	}
	let mut png = Vec::new();
	{
		use image::ImageEncoder as _;
		let enc = image::codecs::png::PngEncoder::new(&mut png);
		enc.write_image(&rgba, w, h, image::ExtendedColorType::Rgba8).ok()?;
	}
	Some(DataMsg::CursorShape {
		w,
		h,
		hot_x: (*img).xhot as u32,
		hot_y: (*img).yhot as u32,
		rgba_png: png,
	})
}
