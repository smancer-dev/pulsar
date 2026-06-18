//! Single-surface renderer (Linux/X11): libmpv's RENDER API draws rkmpp-decoded frames into
//! a `GtkGLArea` we own, so the WebKitGTK webview can be composited transparently ON TOP —
//! the moonlight-style "video + overlay in one window" (no separate window, control via the
//! focused webview's existing JS path). Validated on RK3588/Panfrost with a standalone C
//! probe: desktop-GL context + the `X11_DISPLAY` param → rkmpp decode + webview overlay.

use gtk::glib;
use gtk::prelude::*;
use libmpv_sys as mpv;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr;
use std::rc::Rc;

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

// mpv's get_proc_address resolves GL functions for the current context. We resolve
// eglGetProcAddress / glXGetProcAddressARB from **libepoxy at runtime** (dlopen) rather
// than linking -lEGL/-lGL — direct linking perturbs WebKitGTK's own GL/epoxy loading and
// wedges its compositor (blank webview). libepoxy is already in-process (GTK uses it) and
// exports both resolvers; eglGetProcAddress handles core GL even for GLX contexts on Mesa.
type GetProcFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
fn dlsym_getproc(
	slot: &std::sync::OnceLock<Option<GetProcFn>>,
	libs: &[&str],
	sym: &[u8],
) -> Option<GetProcFn> {
	*slot.get_or_init(|| unsafe {
		for name in libs {
			if let Ok(lib) = libloading::Library::new(*name) {
				if let Ok(f) = lib.get::<GetProcFn>(sym) {
					let f = *f;
					std::mem::forget(lib); // keep it resident for the process lifetime
					return Some(f);
				}
			}
		}
		None
	})
}
unsafe extern "C" fn get_proc(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
	// Real libEGL/libGL first (what the GLArea's Mesa context actually uses); libepoxy
	// as a last resort. dlopen'd at runtime so we never perturb WebKit's link-time GL.
	static EGL: std::sync::OnceLock<Option<GetProcFn>> = std::sync::OnceLock::new();
	static GLX: std::sync::OnceLock<Option<GetProcFn>> = std::sync::OnceLock::new();
	if let Some(f) = dlsym_getproc(
		&EGL,
		&["libEGL.so.1", "libEGL.so", "libepoxy.so.0"],
		b"eglGetProcAddress\0",
	) {
		let p = f(name);
		if !p.is_null() {
			return p;
		}
	}
	if let Some(f) = dlsym_getproc(
		&GLX,
		&["libGL.so.1", "libGL.so", "libepoxy.so.0"],
		b"glXGetProcAddressARB\0",
	) {
		return f(name);
	}
	std::ptr::null_mut()
}

fn set_opt(h: *mut mpv::mpv_handle, k: &str, v: &str) {
	if let (Ok(ck), Ok(cv)) = (CString::new(k), CString::new(v)) {
		unsafe {
			mpv::mpv_set_option_string(h, ck.as_ptr(), cv.as_ptr());
		}
	}
}

// The update callback runs on mpv's render thread; bounce to the GTK main thread and
// queue a redraw. `SendWeakRef` is Send; `MainContext::invoke` is callable from any thread.
struct Bridge {
	area: glib::SendWeakRef<gtk::GLArea>,
	ctx: glib::MainContext,
}
unsafe extern "C" fn on_update(cb: *mut c_void) {
	let b = &*(cb as *const Bridge);
	let area = b.area.clone();
	b.ctx.invoke(move || {
		if let Some(a) = area.upgrade() {
			a.queue_render();
		}
	});
}

pub struct MpvGl {
	handle: *mut mpv::mpv_handle,
	render_ctx: *mut mpv::mpv_render_context,
	bridge: *mut Bridge,
	/// Rolling average (EMA) of the wall-clock cost of `mpv_render_context_render`, in
	/// milliseconds. This is the ONLY truthful per-frame timing we can measure on the
	/// single-surface path (the `--wid` child reports an IPC `vo-delay` proxy instead).
	/// `Cell` because `render()` takes `&self` (called from the GLArea draw signal); the
	/// whole type is `!Send`/main-thread-only so no sync is needed.
	render_ms: Cell<f64>,
}

impl MpvGl {
	/// Create + initialize the core (main thread, before the GLArea realizes). `vo=libmpv`
	/// routes frames to the render API; options mirror the proven `spawn_mpv` flags.
	pub fn new() -> Result<*mut mpv::mpv_handle, String> {
		// mpv requires LC_NUMERIC=C; GTK's gtk_init() sets the system locale (e.g.
		// en_US.UTF-8), under which mpv_create() refuses. Force it back to C.
		unsafe {
			libc::setlocale(libc::LC_NUMERIC, b"C\0".as_ptr() as *const libc::c_char);
		}
		let h = unsafe { mpv::mpv_create() };
		if h.is_null() {
			return Err("mpv_create failed".into());
		}
		set_opt(h, "vo", "libmpv");
		set_opt(h, "hwdec", "rkmpp");
		set_opt(h, "profile", "low-latency");
		set_opt(h, "cache", "no");
		set_opt(h, "demuxer-readahead-secs", "0");
		set_opt(h, "vd-lavc-threads", "1");
		set_opt(h, "framedrop", "decoder");
		set_opt(h, "audio", "no");
		set_opt(h, "input-default-bindings", "no");
		set_opt(h, "input-vo-keyboard", "no");
		set_opt(h, "input-cursor", "no");
		set_opt(
			h,
			"demuxer-lavf-o",
			"protocol_whitelist=%29%file,udp,rtp,rtcp,crypto,data,buffer_size=8388608,fifo_size=1000000,overrun_nonfatal=1,fflags=+nobuffer+discardcorrupt,probesize=32,analyzeduration=0,max_delay=0",
		);
		set_opt(h, "demuxer-lavf-probe-info", "no");
		if unsafe { mpv::mpv_initialize(h) } < 0 {
			unsafe {
				mpv::mpv_terminate_destroy(h);
			}
			return Err("mpv_initialize failed".into());
		}
		unsafe {
			mpv::mpv_request_log_messages(h, b"v\0".as_ptr() as *const libc::c_char);
		}
		Ok(h)
	}

	/// Drain mpv events (decode/hwdec/VO status) so the event queue doesn't stall.
	pub fn drain_log(&self) {
		loop {
			let e = unsafe { mpv::mpv_wait_event(self.handle, 0.0) };
			if e.is_null() {
				break;
			}
			let ev = unsafe { &*e };
			if ev.event_id == mpv::mpv_event_id_MPV_EVENT_NONE {
				break;
			}
		}
	}

	/// In the GLArea `realize` handler (GL context current). `x11_display` from
	/// `gdk_x11_display_get_xdisplay` — required for rkmpp on RK3588.
	pub fn attach(
		handle: *mut mpv::mpv_handle,
		area: &gtk::GLArea,
		x11_display: *mut c_void,
	) -> Result<Self, String> {
		let mut gl_init = mpv::mpv_opengl_init_params {
			get_proc_address: Some(get_proc),
			get_proc_address_ctx: ptr::null_mut(),
			extra_exts: ptr::null(),
		};
		let mut params = [
			mpv::mpv_render_param {
				type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
				data: mpv::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
			},
			mpv::mpv_render_param {
				type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
				data: &mut gl_init as *mut _ as *mut c_void,
			},
			mpv::mpv_render_param {
				type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_X11_DISPLAY,
				data: x11_display,
			},
			mpv::mpv_render_param {
				type_: 0,
				data: ptr::null_mut(),
			},
		];
		let mut render_ctx: *mut mpv::mpv_render_context = ptr::null_mut();
		let rc =
			unsafe { mpv::mpv_render_context_create(&mut render_ctx, handle, params.as_mut_ptr()) };
		if rc < 0 {
			return Err(format!("render_context_create failed: {rc}"));
		}
		let bridge = Box::into_raw(Box::new(Bridge {
			area: glib::SendWeakRef::from(area.downgrade()),
			ctx: glib::MainContext::default(),
		}));
		unsafe {
			mpv::mpv_render_context_set_update_callback(
				render_ctx,
				Some(on_update),
				bridge as *mut c_void,
			);
		}
		Ok(Self {
			handle,
			render_ctx,
			bridge,
			render_ms: Cell::new(0.0),
		})
	}

	pub fn load_sdp(&self, sdp: &str) {
		if let (Ok(c0), Ok(a)) = (CString::new("loadfile"), CString::new(sdp)) {
			let mut argv = [c0.as_ptr(), a.as_ptr(), ptr::null()];
			unsafe {
				mpv::mpv_command(self.handle, argv.as_mut_ptr());
			}
		}
	}

	/// Only from the GLArea `render` signal (GL context current, main thread).
	pub fn render(&self, w: i32, h: i32) {
		// Query the GLArea's currently-bound FBO via libepoxy's glGetIntegerv (context-
		// correct dispatch); resolving it through eglGetProcAddress can return a function
		// not bound to this GLX context → FBO reads 0 → mpv renders to the wrong target.
		let mut fbo: c_int = 0;
		unsafe {
			static GETINT: std::sync::OnceLock<Option<unsafe extern "C" fn(u32, *mut c_int)>> =
				std::sync::OnceLock::new();
			let f = GETINT.get_or_init(|| {
				for lib in ["libepoxy.so.0", "libepoxy.so", "libGL.so.1"] {
					if let Ok(l) = libloading::Library::new(lib) {
						if let Ok(s) =
							l.get::<unsafe extern "C" fn(u32, *mut c_int)>(b"glGetIntegerv\0")
						{
							let s = *s;
							std::mem::forget(l);
							return Some(s);
						}
					}
				}
				None
			});
			if let Some(f) = f {
				f(GL_FRAMEBUFFER_BINDING, &mut fbo);
			}
		}
		// DIAGNOSTIC: PULSAR_GLCLEAR=1 → clear the GLArea magenta + skip mpv, to test
		// whether the GLArea composites through the (transparent) webview at all.
		if std::env::var_os("PULSAR_GLCLEAR").is_some() {
			unsafe {
				type Cc = unsafe extern "C" fn(f32, f32, f32, f32);
				type Cl = unsafe extern "C" fn(u32);
				if let Ok(l) = libloading::Library::new("libepoxy.so.0") {
					if let (Ok(cc), Ok(cl)) =
						(l.get::<Cc>(b"glClearColor\0"), l.get::<Cl>(b"glClear\0"))
					{
						(*cc)(1.0, 0.0, 1.0, 1.0);
						(*cl)(0x4000); // GL_COLOR_BUFFER_BIT
					}
					std::mem::forget(l);
				}
			}
			return;
		}
		let mut gl_fbo = mpv::mpv_opengl_fbo {
			fbo,
			w,
			h,
			internal_format: 0,
		};
		let mut flip: c_int = 1; // GLArea default FBO is y-flipped vs mpv
		let mut params = [
			mpv::mpv_render_param {
				type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
				data: &mut gl_fbo as *mut _ as *mut c_void,
			},
			mpv::mpv_render_param {
				type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
				data: &mut flip as *mut _ as *mut c_void,
			},
			mpv::mpv_render_param {
				type_: 0,
				data: ptr::null_mut(),
			},
		];
		// Time the actual GPU submit so the perf HUD can show a TRUTHFUL render/output cost
		// (not a fabricated number). Smooth with an EMA (α=0.2) to avoid jitter; reported
		// via `render_ms()` to the stats poller.
		let t0 = std::time::Instant::now();
		unsafe {
			mpv::mpv_render_context_render(self.render_ctx, params.as_mut_ptr());
		}
		let dt = t0.elapsed().as_secs_f64() * 1000.0;
		let prev = self.render_ms.get();
		let ema = if prev > 0.0 {
			prev * 0.8 + dt * 0.2
		} else {
			dt
		};
		self.render_ms.set(ema);
	}

	/// Truthful rolling-average cost (ms) of the last few `mpv_render_context_render`
	/// calls — fed into the single-surface perf HUD as the decode/render-latency metric.
	/// 0.0 until the first frame is rendered. NEVER fabricated.
	pub fn render_ms(&self) -> f64 {
		self.render_ms.get()
	}

	/// Read a numeric mpv property for the perf overlay. None on error.
	pub fn prop_f64(&self, name: &str) -> Option<f64> {
		let cn = CString::new(name).ok()?;
		let mut out: f64 = 0.0;
		let rc = unsafe {
			mpv::mpv_get_property(
				self.handle,
				cn.as_ptr(),
				mpv::mpv_format_MPV_FORMAT_DOUBLE,
				&mut out as *mut f64 as *mut c_void,
			)
		};
		(rc >= 0).then_some(out)
	}

	/// Teardown order is load-bearing: free the render context (GL current) BEFORE
	/// destroying the core. Call from `unrealize` / session stop.
	pub fn teardown(self) {
		unsafe {
			mpv::mpv_render_context_set_update_callback(self.render_ctx, None, ptr::null_mut());
			mpv::mpv_render_context_free(self.render_ctx);
			mpv::mpv_terminate_destroy(self.handle);
			drop(Box::from_raw(self.bridge));
		}
	}
}

/// Destroy an mpv handle that was created by `MpvGl::new` but never passed to
/// `MpvGl::attach` (i.e. never wrapped in a `MpvGl`). Calling this on a handle
/// that IS already owned by an `MpvGl` is a double-free — the caller must zero
/// the tracked pointer afterwards (see the `Rc<Cell<usize>>` idiom in render.rs).
///
/// # Safety
/// `handle` must be a valid pointer returned by `mpv_create`/`mpv_initialize`,
/// and must not have been passed to `mpv_terminate_destroy` before.
pub unsafe fn destroy_handle(handle: *mut mpv::mpv_handle) {
    if !handle.is_null() {
        unsafe {
            mpv::mpv_terminate_destroy(handle);
        }
    }
}

/// Main-thread-only handle to the live renderer (it is `!Send`).
pub type SharedMpv = Rc<RefCell<Option<MpvGl>>>;
