//! Linux (X11) native renderer plumbing: window-id resolution, the moonlight-style
//! single-surface (libmpv→GtkGLArea) install/teardown, the mpv stats pollers (render
//! API + `--wid` IPC), and the host-rotation → vidsink re-spawn. The stdout line
//! readers (`vidsink-fps …` / `ov …`) live in the sibling `render_stats` module.
//!
//! Everything here is `#[cfg(all(unix, not(target_os = "macos")))]` — it has no
//! counterpart on Windows/macOS (those keep the in-webview WebCodecs path).
#![cfg(all(unix, not(target_os = "macos")))]

use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager};

use crate::events::PlayVStats;
use crate::native_view;
use crate::state::AppState;

/// Resolve the Pulsar **main window's X11 window id** (Linux/X11) so the native mpv
/// renderer can embed *inside* it via `--wid` (in-app HW-decoded video). `gtk_window()`
/// is main-thread-only, so hop onto the GTK main thread, read the GdkX11 xid, and send it
/// back. Returns None off X11 / before the window is realized (→ mpv falls back to its own
/// fullscreen window).
pub(crate) async fn window_xid(app: &AppHandle) -> Option<u64> {
	let (tx, rx) = tokio::sync::oneshot::channel::<Option<u64>>();
	let app2 = app.clone();
	let posted = app.run_on_main_thread(move || {
		use gtk::glib::Cast;
		use gtk::prelude::WidgetExt;
		let w = app2.get_webview_window("main");
		let gw = w.and_then(|w| w.gtk_window().ok());
		// The GdkWindow only exists once the widget is realized; force it (the window is
		// shown by session time, but be defensive) and pump pending GTK work.
		if let Some(ref g) = gw {
			if !g.is_realized() {
				g.realize();
			}
		}
		let gdkw = gw.as_ref().and_then(|gw| gw.window());
		let x11 = gdkw
			.as_ref()
			.and_then(|gdkw| gdkw.clone().downcast::<gdkx11::X11Window>().ok());
		let xid = x11.as_ref().map(|x11| x11.xid() as u64);
		let _ = tx.send(xid);
	});
	if posted.is_err() {
		return None;
	}
	rx.await.ok().flatten()
}

/// Live single-surface renderers, keyed by play id. `SharedMpv` is `!Send`, so this lives
/// on the GTK main thread only; the `GLArea` is kept alongside so teardown can make its GL
/// context current before freeing the mpv render context.
thread_local! {
	static GL_RENDERERS: std::cell::RefCell<
		std::collections::HashMap<u64, (gtk::GLArea, native_view::mpvgl::SharedMpv)>,
	> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Build the moonlight-style single surface (Linux/X11): reparent the WebKitGTK webview on
/// top of a `GtkGLArea` via a `GtkOverlay`, drive the GLArea with libmpv's render API
/// (rkmpp), and make the webview transparent so the video shows through. MUST run on the
/// GTK main thread. Returns Err on any step so the caller can fall back to `spawn_mpv --wid`.
pub(crate) fn install_single_surface(
	app: &AppHandle,
	id: u64,
	sdp_path: String,
) -> Result<(), String> {
	use gtk::glib;
	use gtk::glib::object::Cast;
	use gtk::glib::translate::ToGlibPtr;
	use gtk::prelude::*;
	use native_view::mpvgl::{MpvGl, SharedMpv};

	let w = app.get_webview_window("main").ok_or("no main window")?;
	let vbox = w.default_vbox().map_err(|e| e.to_string())?;

	// Give the GTK window an RGBA visual so the webview's drawing surface has an alpha
	// channel — without it, set_background_color's alpha is dropped and transparent page
	// regions render OPAQUE BLACK over the GLArea instead of revealing the video.
	if let Ok(gtk_win) = w.gtk_window() {
		if let Some(screen) = WidgetExt::screen(&gtk_win) {
			if let Some(rgba) = screen.rgba_visual() {
				gtk_win.set_visual(Some(&rgba));
			}
		}
	}
	// wry packs exactly one WebKitWebView into the vbox; find it by GType name.
	let webview = vbox
		.children()
		.into_iter()
		.find(|c| c.type_().name().contains("WebView") || c.type_().name().contains("WebKit"))
		.ok_or_else(|| {
			let names: Vec<String> = vbox
				.children()
				.iter()
				.map(|c| c.type_().name().to_string())
				.collect();
			format!("WebKitWebView not in vbox; children={names:?}")
		})?;

	// Make the webview transparent BEFORE reparenting: the reparent re-realizes the webview,
	// rebuilding its accelerated-compositing surface — if the background is already alpha-0 the
	// new surface is transparent (setting it afterwards leaves the surface opaque → the GLArea
	// behind never shows). Also force a small GTK pump so the property lands before the remove.
	let _ = w.with_webview(|pw| {
		use webkit2gtk::WebViewExt;
		pw.inner()
			.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
	});

	// Hold a strong ref across remove() so the webview can't finalize before re-rooting.
	let keepalive = webview.clone();
	vbox.remove(&webview);

	let overlay = gtk::Overlay::new();
	let gl = gtk::GLArea::new();
	gl.set_has_depth_buffer(false);
	gl.set_has_stencil_buffer(false);
	gl.set_hexpand(true);
	gl.set_vexpand(true);
	gl.set_can_focus(false);
	gl.set_sensitive(false); // never steals input; the webview on top handles it
	overlay.add(&gl); // base layer = video
	overlay.add_overlay(&webview); // top layer = UI (transparent over the video)
	overlay.set_overlay_pass_through(&webview, false); // webview keeps input
	vbox.pack_start(&overlay, true, true, 0);
	overlay.show();
	gl.show();
	webview.show();
	drop(keepalive);

	// Make the webview transparent so the GLArea video shows through the video region.
	let _ = w.with_webview(|pw| {
		use webkit2gtk::WebViewExt;
		pw.inner()
			.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
	});

	// X11 Display* — required for rkmpp EGL import on RK3588.
	let x11_display = gtk::gdk::Display::default()
		.and_then(|d| d.downcast::<gdkx11::X11Display>().ok())
		.map(|d| {
			// Keep the GObject alive across the ffi call; pick the *mut GdkX11Display impl.
			let stash =
				ToGlibPtr::<*mut gdkx11::ffi::GdkX11Display>::to_glib_none(&d);
			unsafe {
				gdkx11::ffi::gdk_x11_display_get_xdisplay(stash.0) as *mut std::ffi::c_void
			}
		})
		.unwrap_or(std::ptr::null_mut());
	let x11_usize = x11_display as usize; // carry into the 'static realize closure

	let handle = MpvGl::new()?;
	let handle_usize = handle as usize;
	let shared: SharedMpv = std::rc::Rc::new(std::cell::RefCell::new(None));

	{
		let shared = shared.clone();
		let sdp = sdp_path.clone();
		gl.connect_realize(move |a| {
			a.make_current();
			if a.error().is_some() {
				return;
			}
			match MpvGl::attach(handle_usize as *mut _, a, x11_usize as *mut std::ffi::c_void) {
				Ok(r) => {
					r.load_sdp(&sdp);
					*shared.borrow_mut() = Some(r);
				}
				Err(_) => {}
			}
		});
	}
	{
		let shared = shared.clone();
		gl.connect_render(move |a, _| {
			let s = a.scale_factor();
			let (w, h) = (a.allocated_width() * s, a.allocated_height() * s);
			if let Some(r) = shared.borrow().as_ref() {
				r.render(w, h);
			}
			glib::Propagation::Stop
		});
	}
	{
		let shared = shared.clone();
		gl.connect_unrealize(move |a| {
			a.make_current();
			if let Some(r) = shared.borrow_mut().take() {
				r.teardown();
			}
		});
	}

	// Drive the GLArea at ~60fps: mpv's update callback alone doesn't reliably trigger
	// GtkGLArea redraws (it only renders when GTK draws the widget), so queue a render on a
	// frame-clock timer; on_render then presents the latest decoded frame. Stops with the area.
	{
		let gl_weak = gl.downgrade();
		gtk::glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
			match gl_weak.upgrade() {
				Some(a) => {
					a.queue_render();
					gtk::glib::ControlFlow::Continue
				}
				None => gtk::glib::ControlFlow::Break,
			}
		});
	}

	GL_RENDERERS.with(|m| m.borrow_mut().insert(id, (gl, shared)));
	start_mpv_stats(app, id);
	Ok(())
}

/// Poll mpv (on the GTK main thread, which owns the handle) once a second and push real
/// video stats to the overlay's perf panel. Stops when the renderer for `id` is gone.
fn start_mpv_stats(app: &AppHandle, id: u64) {
	use tauri::Emitter;
	let app = app.clone();
	gtk::glib::timeout_add_seconds_local(1, move || {
		let alive = GL_RENDERERS.with(|m| {
			let map = m.borrow();
			let Some((_, shared)) = map.get(&id) else {
				return false;
			};
			let guard = shared.borrow(); // named so it drops before `map`
			let Some(r) = guard.as_ref() else {
				return true; // not attached yet; keep polling
			};
			r.drain_log(); // diagnostics: surface mpv decode/hwdec/VO messages
			let fps = r.prop_f64("estimated-vf-fps").unwrap_or(0.0);
			// Align with the `--wid` IPC poller + OSD: decoder-side drops.
			let drops = r.prop_f64("decoder-frame-drop-count").unwrap_or(0.0) as i64;
			let mbps = r.prop_f64("video-bitrate").unwrap_or(0.0) / 1e6;
			// Truthful per-frame GPU render cost, timed directly around
			// mpv_render_context_render on this path; 0.0 until the first frame. (D1)
			let decode_ms = r.render_ms();
			let _ = app.emit(
				"play-vstats",
				PlayVStats {
					id,
					fps,
					drops,
					mbps,
					decode_ms,
				},
			);
			true
		});
		if alive {
			gtk::glib::ControlFlow::Continue
		} else {
			gtk::glib::ControlFlow::Break
		}
	});
}

/// Poll the embedded `--wid` mpv child over its JSON IPC socket (~1 Hz) and emit real
/// `play-vstats` to the overlay HUD. This is the DEFAULT Linux path (no WebCodecs sink,
/// no single-surface render context), so without it the perf numbers stay zero. Runs as
/// a plain tokio task (it's just socket I/O, not GTK main-thread work). The socket only
/// appears once mpv has started, so connect-refused on the first polls is tolerated
/// (mpv_ipc_get_f64 returns None). Stops when the play session for `id` is gone or no
/// longer running.
pub(crate) fn start_mpv_ipc_stats(app: &AppHandle, id: u64, sock: std::path::PathBuf) {
	use tauri::Emitter;
	let app = app.clone();
	tokio::spawn(async move {
		let state = app.state::<AppState>();
		let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
		loop {
			tick.tick().await;
			// Stop once this session is gone (or has been marked not-running).
			let alive = state
				.plays
				.lock()
				.unwrap()
				.get(&id)
				.map(|p| p.running.load(Ordering::SeqCst))
				.unwrap_or(false);
			if !alive {
				break;
			}
			// Read from mpv; None (socket not ready / property missing) → 0, never faked.
			let get = |prop: &str| native_view::mpv_ipc_get_f64(&sock, prop);
			// mpv 0.34 on --wid can't report client fps with our low-latency flags
			// (estimated-vf-fps is unavailable → 0); the UI falls back to the host's real
			// encode fps. drops + bitrate ARE real here.
			let fps = get("estimated-vf-fps").unwrap_or(0.0);
			let drops = get("decoder-frame-drop-count").unwrap_or(0.0) as i64;
			let mbps = get("video-bitrate").unwrap_or(0.0) / 1e6;
			// Real pipeline-buffer latency (demuxer-cache-duration, seconds → ms): how much
			// video is buffered ahead (~one frame with cache=no). `vo-delay` doesn't exist in
			// mpv 0.34, so this is the honest local-latency number; 0 if unavailable. (D1)
			let decode_ms = get("demuxer-cache-duration").map(|s| s * 1000.0).unwrap_or(0.0);
			let _ = app.emit(
				"play-vstats",
				PlayVStats {
					id,
					fps,
					drops,
					mbps,
					decode_ms,
				},
			);
		}
	});
}

/// Tear down the single-surface renderer for `id` (mpv stop) on the GTK main thread.
pub(crate) async fn teardown_single_surface(app: &AppHandle, id: u64) {
	use gtk::prelude::*;
	let (tx, rx) = tokio::sync::oneshot::channel::<()>();
	let posted = app.run_on_main_thread(move || {
		GL_RENDERERS.with(|m| {
			if let Some((gl, shared)) = m.borrow_mut().remove(&id) {
				gl.make_current();
				if let Some(r) = shared.borrow_mut().take() {
					r.teardown();
				}
			}
		});
		let _ = tx.send(());
	});
	if posted.is_ok() {
		let _ = rx.await;
	}
}

/// Client (Linux/vidsink): apply the host's reported display rotation by respawning the vidsink
/// with the inverse `--rotate` so the video shows upright. A manual `PULSAR_ROTATE` override
/// wins (and disables auto). No-op if the rotation already matches or there's no vidsink.
pub(crate) fn apply_vidsink_rotation(app: &AppHandle, id: u64, host_deg: u32) {
	// Manual override (PULSAR_ROTATE) disables auto-detect.
	if std::env::var("PULSAR_ROTATE")
		.ok()
		.and_then(|s| s.parse::<u32>().ok())
		.map(|d| d % 360 != 0)
		.unwrap_or(false)
	{
		return;
	}
	let target = (360 - host_deg % 360) % 360; // un-rotate
	let state = app.state::<AppState>();
	let (sdp, wid, bin, cur, ostdin) = {
		let plays = state.plays.lock().unwrap();
		match plays.get(&id) {
			Some(p) => (
				p.mpv_sdp.clone(),
				p.mpv_wid,
				p.vidsink_bin.clone(),
				p.vidsink_rotate,
				p.render_stdin.clone(),
			),
			None => return,
		}
	};
	let (Some(sdp), Some(bin)) = (sdp, bin) else { return };
	if target == cur {
		return; // already applied
	}
	if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
		if let Some(mut c) = p.ffplay.take() {
			let _ = c.kill();
			let _ = c.wait();
		}
	}
	let mut child = native_view::spawn_vidsink(&bin, &sdp, wid, target);
	if let Some(c) = child.as_mut() {
		if let Some(out) = c.stdout.take() {
			crate::render_stats::start_vidsink_stats(app, id, out, ostdin);
		}
	}
	if let Some(c) = child {
		if let Some(p) = state.plays.lock().unwrap().get_mut(&id) {
			p.ffplay = Some(c);
			p.vidsink_rotate = target;
		}
	}
}
