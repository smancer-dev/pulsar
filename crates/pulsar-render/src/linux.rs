//! Linux/X11 single-surface renderer: rkmpp video + egui overlay in ONE GL context, in a CHILD
//! window of the Tauri window (`--wid`). Because it's a child window the overlay moves/clips/
//! stacks WITH the app automatically (the override-redirect-top-level approach desynced on move
//! and floated above other apps). Video is drawn every frame; egui is composited on top of it in
//! the same framebuffer while the overlay is open — no compositor transparency needed.

use crate::overlay::{self, Mode, OverlayCmd, OverlayState};
use crate::video;
use std::ffi::c_void;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use x11::xlib;

use khronos_egl as egl;

static OPEN: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// Set by a `hide` line on stdin (sent by the app when a session ends). While idle the render
/// UNMAPS its video window — revealing the WebKitGTK webview underneath — but keeps the process
/// + EGL context ALIVE. Destroying this GL context (process exit) corrupts WebKit's shared Mali
/// GL on RK3588 and wedges the webview; staying resident-but-hidden avoids that. `show` re-maps.
static IDLE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr(sig: libc::c_int) {
    OPEN.store(sig == libc::SIGUSR1, Ordering::SeqCst);
}
extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
    // Async-signal-safe: only set atomics here. The decode loop checks video::STOP and exits;
    // the actual drain+free (video::stop_decode) is done from the main thread after the render
    // loop ends (see real_run) — calling it here can deadlock on the MBX mutex the main thread
    // holds every frame in Presenter::draw.
    video::signal_stop();
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u64 = 0;
    let mut mode = Mode::Game;
    let mut sdp = String::new();
    // Frame-pacing startup default: ON (Moonlight per-vblank metering — kills the gap→teleport
    // and beats the old EMA pacer). `PULSAR_PACE=0` forces newest-wins for A/B; the `--pace`
    // flag and the live `pace 0|1` stdin toggle still override. The frontend persists the choice.
    let mut pace = std::env::var("PULSAR_PACE").map(|v| v == "1" || v == "on" || v == "true").unwrap_or(true);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--wid" => {
                if let Some(s) = args.get(i + 1) {
                    let s = s.trim_start_matches("0x");
                    wid = u64::from_str_radix(s, 16).or_else(|_| s.parse()).unwrap_or(0);
                    i += 1;
                }
            }
            "--mode" => {
                if let Some(s) = args.get(i + 1) {
                    mode = if s == "remote" { Mode::Remote } else { Mode::Game };
                    i += 1;
                }
            }
            "--pace" => {
                if let Some(s) = args.get(i + 1) {
                    pace = s == "on" || s == "1" || s == "true";
                    i += 1;
                }
            }
            a if !a.starts_with("--") && sdp.is_empty() => sdp = a.to_string(),
            _ => {}
        }
        i += 1;
    }

    unsafe {
        libc::signal(libc::SIGUSR1, on_usr as *const () as usize);
        libc::signal(libc::SIGUSR2, on_usr as *const () as usize);
        libc::signal(libc::SIGINT, on_stop as *const () as usize);
        libc::signal(libc::SIGTERM, on_stop as *const () as usize);
    }

    video::set_pace(pace);
    // Adaptive pacing ceiling by mode: game prizes min latency (buffer ≤2), remote tolerates one
    // more frame for smoothness (≤3). Within the QCAP hard cap; the pacer trims toward 1 at rest.
    video::set_pace_ceiling(match mode {
        Mode::Game => 2,
        Mode::Remote => 3,
    });
    // Live pacing toggles arrive as `pace 0|1` lines on stdin (the same stdin the HUD `stat …`
    // lines use); read them on a side thread so the frontend Settings/overlay can flip pacing
    // with no respawn. Non-`pace` lines (HUD stat, etc.) are tolerated and ignored.
    std::thread::spawn(|| {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let mut it = line.split_whitespace();
            match it.next() {
                Some("pace") => {
                    if let Some(v) = it.next() {
                        video::set_pace(v == "1" || v == "on" || v == "true");
                    }
                }
                // Session ended: hide the video window (reveal the webview) + idle, WITHOUT
                // exiting — keeps our EGL context alive so WebKit's shared Mali GL isn't corrupted.
                Some("hide") => IDLE.store(true, Ordering::SeqCst),
                Some("show") => IDLE.store(false, Ordering::SeqCst),
                _ => {}
            }
        }
    });
    if !sdp.is_empty() {
        video::start_decode(&sdp);
    }
    unsafe { real_run(wid, mode) };
}

unsafe fn win_size(xd: *mut xlib::Display, win: u64) -> (u32, u32) {
    let mut root = 0u64;
    let (mut x, mut y, mut bw, mut depth) = (0i32, 0i32, 0u32, 0u32);
    let (mut w, mut h) = (0u32, 0u32);
    if xlib::XGetGeometry(xd, win, &mut root, &mut x, &mut y, &mut w, &mut h, &mut bw, &mut depth) != 0 {
        (w.max(1), h.max(1))
    } else {
        (1, 1)
    }
}

unsafe fn real_run(wid: u64, mode: Mode) {
    let xd = xlib::XOpenDisplay(std::ptr::null());
    if xd.is_null() {
        eprintln!("pulsar-render: XOpenDisplay failed");
        return;
    }
    let parent = if wid != 0 { wid } else { xlib::XDefaultRootWindow(xd) };
    let (mut w, mut h) = win_size(xd, parent);

    let egl = egl::Instance::new(egl::Static);
    let display = egl.get_display(xd as egl::NativeDisplayType).expect("egl display");
    egl.initialize(display).expect("egl init");

    let attribs = [
        egl::SURFACE_TYPE, egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT,
        egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8,
        egl::NONE,
    ];
    let config = egl.choose_first_config(display, &attribs).expect("choose").expect("no config");

    // Child window uses the EGL config's native (opaque) visual — like the vidsink. Opaque is
    // correct here: video + egui share THIS framebuffer, there's no sibling to composite against.
    let vid = egl.get_config_attrib(display, config, egl::NATIVE_VISUAL_ID).unwrap_or(0) as u64;
    let mut tmpl: xlib::XVisualInfo = std::mem::zeroed();
    tmpl.visualid = vid;
    let mut nret = 0i32;
    let vinfo = xlib::XGetVisualInfo(xd, xlib::VisualIDMask, &mut tmpl, &mut nret);
    let (visual, depth) = if !vinfo.is_null() && nret > 0 {
        ((*vinfo).visual, (*vinfo).depth)
    } else {
        let s = xlib::XDefaultScreen(xd);
        (xlib::XDefaultVisual(xd, s), xlib::XDefaultDepth(xd, s))
    };
    let cmap = xlib::XCreateColormap(xd, parent, visual, xlib::AllocNone);
    if !vinfo.is_null() {
        xlib::XFree(vinfo as *mut c_void);
    }

    let mut swa: xlib::XSetWindowAttributes = std::mem::zeroed();
    swa.colormap = cmap;
    swa.background_pixel = 0;
    swa.border_pixel = 0;
    // Start with NO pointer events (overlay closed): a click on the video then propagates to GTK
    // → Pulsar refocuses → the evdev grab re-engages. Pointer events are added when the overlay
    // opens (egui needs them) and removed again on close — see the open-transition block.
    swa.event_mask = xlib::ExposureMask | xlib::StructureNotifyMask;
    let valuemask = xlib::CWColormap | xlib::CWEventMask | xlib::CWBackPixel | xlib::CWBorderPixel;
    let win = xlib::XCreateWindow(xd, parent, 0, 0, w, h, 0, depth, xlib::InputOutput as u32, visual, valuemask, &mut swa);
    xlib::XMapWindow(xd, win);

    // Invisible cursor for the video window. While the overlay is CLOSED (gameplay) the local
    // pointer must not show over the video (input is forwarded to the host); while OPEN we restore
    // the default arrow so the user can click the egui overlay.
    let mut blank: xlib::XColor = std::mem::zeroed();
    let zero = [0u8];
    let pix = xlib::XCreateBitmapFromData(xd, win, zero.as_ptr(), 1, 1);
    let invisible = xlib::XCreatePixmapCursor(xd, pix, pix, &mut blank, &mut blank, 0, 0);
    xlib::XFreePixmap(xd, pix);
    xlib::XDefineCursor(xd, win, invisible); // start hidden (overlay closed)
    xlib::XSync(xd, xlib::False);

    egl.bind_api(egl::OPENGL_ES_API).ok();
    let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
    let context = egl.create_context(display, config, None, &ctx_attribs).expect("ctx");
    let surface = egl.create_window_surface(display, config, win as egl::NativeWindowType, None).expect("surface");
    egl.make_current(display, Some(surface), Some(surface), Some(context)).expect("make_current");
    // VSync: 1 = sync to vblank (default). Under the GNOME/mutter compositor a windowed GL
    // app's own vblank-sync can beat against the compositor's redraw → periodic judder
    // ("mouse jumps") on smooth motion. PULSAR_VSYNC=0 lets us present without blocking so the
    // compositor paces us instead — A/B knob for the windowed-stutter investigation.
    let vsync: i32 = std::env::var("PULSAR_VSYNC").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    egl.swap_interval(display, vsync).ok();

    let get_proc = |s: &str| -> *const c_void {
        match egl.get_proc_address(s) {
            Some(p) => p as *const c_void,
            None => std::ptr::null(),
        }
    };
    let gl = Arc::new(glow::Context::from_loader_function(|s| get_proc(s)));

    let egui_ctx = egui::Context::default();
    overlay::apply_theme(&egui_ctx);
    let ppp = 1.25_f32;
    egui_ctx.set_pixels_per_point(ppp);
    let mut painter = egui_glow::Painter::new(gl.clone(), "", None, false).expect("painter");

    let dpy_ptr = display.as_ptr();
    let mut presenter = video::Presenter::new(&gl, dpy_ptr, &get_proc);

    let mut state = OverlayState { mode, open: false, id: "—".into(), conn_label: "P2P".into(), ..Default::default() };

    let mut pointer = egui::pos2(0.0, 0.0);
    let mut geom_tick = 0u32;
    let mut last_stat = std::time::Instant::now();
    let mut prev_open = false;
    let mut prev_idle = false;

    while !STOP.load(Ordering::SeqCst) {
        // Idle (post-disconnect): unmap the video window so the WebKitGTK webview underneath is
        // revealed + interactive, but keep this process + its EGL context resident (exiting would
        // destroy the GL context and corrupt WebKit's shared Mali GL → the webview wedges with no
        // way back short of a reboot). Drain X events + sleep a frame; never draw/swap while idle.
        let idle = IDLE.load(Ordering::SeqCst);
        if idle != prev_idle {
            if idle {
                xlib::XUnmapWindow(xd, win);
            } else {
                xlib::XMapWindow(xd, win);
            }
            xlib::XSync(xd, xlib::False);
            prev_idle = idle;
        }
        if idle {
            while xlib::XPending(xd) > 0 {
                let mut ev: xlib::XEvent = std::mem::zeroed();
                xlib::XNextEvent(xd, &mut ev);
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
            continue;
        }
        let open = OPEN.load(Ordering::SeqCst);
        state.open = open;
        // Reflect the live pacing state (set by the egui toggle OR a `pace 0|1` from the
        // frontend) so the overlay's segmented control highlights the real value.
        state.pace = video::pace_on();
        // Show the cursor only while the overlay is open (so it can be clicked); hide it during
        // gameplay (the pointer is forwarded to the host, no local cursor over the video).
        if open != prev_open {
            if open {
                xlib::XUndefineCursor(xd, win);
                // Select pointer events so egui gets clicks/motion while the overlay is open.
                xlib::XSelectInput(
                    xd,
                    win,
                    xlib::ExposureMask
                        | xlib::StructureNotifyMask
                        | xlib::ButtonPressMask
                        | xlib::ButtonReleaseMask
                        | xlib::PointerMotionMask,
                );
            } else {
                xlib::XDefineCursor(xd, win, invisible);
                // Overlay CLOSED: stop selecting pointer events so a click on the video PROPAGATES
                // to the GTK window → Pulsar refocuses → the evdev grab re-engages (click-to-
                // capture). Otherwise this opaque child window ate the click and the user, once
                // unfocused, could never click back into the session (mouse/keyboard stayed free).
                xlib::XSelectInput(xd, win, xlib::ExposureMask | xlib::StructureNotifyMask);
            }
            prev_open = open;
        }

        // Track the parent size EVERY frame (XGetGeometry is ~µs) so the video follows a
        // fullscreen toggle INSTANTLY — the old 30-frame poll left the child the windowed size
        // for ~0.3 s after going fullscreen (visible glitch / wrong-size video). Moonlight-style:
        // the renderer fills whatever the app window currently is, with no lag.
        let _ = geom_tick;
        {
            let (nw, nh) = win_size(xd, parent);
            if nw != w || nh != h {
                w = nw;
                h = nh;
                xlib::XResizeWindow(xd, win, w, h);
            }
        }
        geom_tick = geom_tick.wrapping_add(1);

        // X events → egui input.
        let mut events: Vec<egui::Event> = Vec::new();
        while xlib::XPending(xd) > 0 {
            let mut ev: xlib::XEvent = std::mem::zeroed();
            xlib::XNextEvent(xd, &mut ev);
            match ev.get_type() {
                xlib::MotionNotify => {
                    let m = ev.motion;
                    pointer = egui::pos2(m.x as f32 / ppp, m.y as f32 / ppp);
                    events.push(egui::Event::PointerMoved(pointer));
                }
                xlib::ButtonPress | xlib::ButtonRelease => {
                    let b = ev.button;
                    let pressed = ev.get_type() == xlib::ButtonPress;
                    // A click into the overlay reaches THIS child window (it grabs pointer events
                    // while open), so it never propagates to GTK → the app stays unfocused if it
                    // was (e.g. the user alt-tabbed away). Without focus, FOCUSED stays false: the
                    // close combo (Ctrl+Shift+M, focus-gated) can't fire and, on close, the evdev
                    // grab never re-engages → the session is stranded with no input. Refocus the
                    // parent (Tauri GTK toplevel) on press so Focused(true) fires; SUSPENDED keeps
                    // the grab released while the overlay is open, so this is safe.
                    if pressed && parent != 0 {
                        xlib::XSetInputFocus(xd, parent, xlib::RevertToParent, xlib::CurrentTime);
                    }
                    pointer = egui::pos2(b.x as f32 / ppp, b.y as f32 / ppp);
                    let button = match b.button {
                        1 => Some(egui::PointerButton::Primary),
                        2 => Some(egui::PointerButton::Middle),
                        3 => Some(egui::PointerButton::Secondary),
                        _ => None,
                    };
                    if let Some(button) = button {
                        events.push(egui::Event::PointerButton { pos: pointer, button, pressed, modifiers: egui::Modifiers::default() });
                    }
                }
                _ => {}
            }
        }

        // Live stats from the video presenter ([fps, mbit, max_gap_ms]). Previously only fps +
        // mbps were fed, so the overlay's "Gecikme ms" + "Çözme ms" tiles were stuck at 0.
        {
            let s = *video::FPS.lock().unwrap();
            if s[0] > 0.0 {
                state.fps = s[0];
            }
            // Bitrate: measured received-stream Mbit/s.
            state.mbps = s[1];
            // Latency: worst present gap in the window (jitter/responsiveness, ms).
            state.latency_ms = s[2];
            // Decode: measured per-frame decode time (µs → ms).
            state.decode_ms = video::DEC_US.load(Ordering::Relaxed) as f32 / 1000.0;
        }

        // ---- Render: video first (opaque), then egui overlay on top (blended) ----
        use glow::HasContext;
        gl.disable(glow::BLEND);
        gl.viewport(0, 0, w as i32, h as i32);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let have_video = presenter.draw(&gl, w as i32, h as i32);

        if open {
            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32 / ppp, h as f32 / ppp))),
                events,
                ..Default::default()
            };
            let mut cmds: Vec<OverlayCmd> = Vec::new();
            let full = egui_ctx.run(raw_input, |ctx| cmds = overlay::draw(ctx, &state));
            for c in cmds {
                emit_cmd(&mut state, c);
            }
            gl.viewport(0, 0, w as i32, h as i32);
            let prims = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
            painter.paint_and_update_textures([w, h], full.pixels_per_point, &prims, &full.textures_delta);
        }

        egl.swap_buffers(display, surface).ok();
        let _ = have_video;

        // Emit a stats line for the host (~1 Hz): same format the lib.rs parser reads.
        if last_stat.elapsed().as_millis() >= 1000 {
            let s = *video::FPS.lock().unwrap();
            println!("vidsink-fps {:.1} {}x{} {:.1} {:.0}", s[0], w, h, s[1], s[2]);
            let _ = std::io::stdout().flush();
            last_stat = std::time::Instant::now();
        }
    }

    // The render loop has exited, so the main thread no longer holds MBX: do the real
    // drain+free here (the SIGINT/SIGTERM handler only set the atomic — see on_stop).
    video::stop_decode();
    painter.destroy();
    egl.make_current(display, None, None, None).ok();
    xlib::XDestroyWindow(xd, win);
    xlib::XCloseDisplay(xd);
}

fn emit_cmd(state: &mut OverlayState, c: OverlayCmd) {
    match c {
        OverlayCmd::Set(field, val) => {
            match field {
                "codec" => state.codec = val.clone(),
                "encoder" => state.encoder = val.clone(),
                "decoder" => state.decoder = val.clone(),
                "res" => state.res = val.clone(),
                "fps" => state.fps_sel = val.clone(),
                "bitrate" => state.bitrate = val.clone(),
                "quality" => state.quality = val.clone(),
                // Pacing flips the renderer locally AT ONCE (instant feel) and is also emitted
                // so the frontend persists it as the default for next session.
                "pace" => {
                    state.pace = val == "on";
                    video::set_pace(val == "on");
                }
                _ => {}
            }
            println!("ov set {field} {val}");
        }
        OverlayCmd::End => println!("ov end"),
        OverlayCmd::Close => {
            OPEN.store(false, Ordering::SeqCst);
            println!("ov close");
        }
    }
    let _ = std::io::stdout().flush();
}
