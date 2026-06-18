//! Wayland screen capture via the XDG **ScreenCast** desktop portal + GStreamer.
//!
//! On a Wayland session (KDE/GNOME) there is no global X root window to grab:
//! `x11grab` of the rootless Xwayland display only ever captures black. The portal
//! hands back a **PipeWire** video node we feed to GStreamer, encode to RTP/H.264,
//! and send to the client's WebCodecs viewer. (Input injection for remote control
//! is handled separately by uinput — see [`crate::input::DesktopInput`] — because
//! KDE's RemoteDesktop portal `Start` hangs without showing a dialog here.)
//!
//! Linux-only; the rest of the app calls [`is_wayland`] to decide between this and
//! the ffmpeg capture path in [`crate::pipeline`].
#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Child;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::{PersistMode, Session};
use ashpd::WindowIdentifier;

/// True when running under Wayland, where `x11grab` would capture a black
/// (rootless Xwayland) screen and we must use the portal instead.
pub fn is_wayland() -> bool {
	std::env::var("XDG_SESSION_TYPE")
		.map(|v| v.eq_ignore_ascii_case("wayland"))
		.unwrap_or(false)
		|| std::env::var("WAYLAND_DISPLAY")
			.map(|v| !v.is_empty())
			.unwrap_or(false)
}

/// A running portal capture: the GStreamer child streaming to the client, the
/// PipeWire remote fd kept open for its lifetime, and the **portal ScreenCast
/// session** — which must be explicitly closed (ashpd does *not* close it on drop)
/// or the compositor keeps showing "your screen is being shared" forever.
pub struct WaylandCapture {
	child: Child,
	session: Session<'static, Screencast<'static>>,
	_pw_fd: OwnedFd,
}

impl WaylandCapture {
	/// Stop the capture: kill GStreamer and **close the portal session** so the
	/// compositor's screen-sharing indicator (KDE/GNOME) actually goes away. Just
	/// killing gst / dropping the fd is not enough — the portal session lingers.
	pub async fn stop(mut self) {
		let _ = self.child.kill();
		// Reap the child so the SIGKILLed gst-launch does not linger as a
		// <defunct> zombie until the whole app exits.  wait() on an already-dead
		// process returns immediately (the kernel already holds the exit status).
		let _ = self.child.wait();
		let _ = self.session.close().await;
	}
}

/// Clear `FD_CLOEXEC` so a spawned child inherits the PipeWire fd.
fn clear_cloexec(fd: i32) -> std::io::Result<()> {
	// SAFETY: `fd` is a valid borrowed descriptor for the duration of the call.
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
	if flags < 0 {
		return Err(std::io::Error::last_os_error());
	}
	if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
		return Err(std::io::Error::last_os_error());
	}
	Ok(())
}

/// Start a portal screencast and pipe the screen to `udp://ip:port` as RTP.
/// `encoder_fragment` is a prebuilt gst encode→parse→rtp-payload fragment from
/// [`crate::pipeline::gst::encoder_fragment`] — the codec/encoder choice (and thus
/// what the client's SDP must declare) is the CALLER's, made against its validated
/// gst caps. Shows the compositor's share dialog the first time; pass a stored
/// `restore_token` to skip it on later calls. Returns the running capture and a
/// (possibly new) restore token to persist.
pub async fn start(
	ip: &str,
	port: u16,
	encoder_fragment: &str,
	restore_token: Option<String>,
) -> anyhow::Result<(WaylandCapture, Option<String>)> {
	let proxy: Screencast<'static> = Screencast::new().await?;
	let session: Session<'static, Screencast<'static>> = proxy.create_session().await?;
	// Everything past `create_session` can fail with the portal cast already live
	// (the realistic case: gstreamer not installed). ashpd does NOT close the
	// session on drop (see the struct docs above), so a bare `?` here would leave
	// the compositor showing "your screen is being shared" forever with no stream
	// behind it — run the fallible tail in a block and close the session on error.
	let res = async {
		proxy
			.select_sources(
				&session,
				CursorMode::Embedded,
				SourceType::Monitor | SourceType::Window,
				false,
				restore_token.as_deref(),
				PersistMode::Application,
			)
			.await?;
		let response = proxy
			.start(&session, &WindowIdentifier::default())
			.await?
			.response()?;
		let stream = response
			.streams()
			.first()
			.ok_or_else(|| anyhow::anyhow!("portal returned no screencast stream"))?;
		let node_id = stream.pipe_wire_node_id();
		let token = response.restore_token().map(|s| s.to_string());

		let pw_fd: OwnedFd = proxy.open_pipe_wire_remote(&session).await?;
		clear_cloexec(pw_fd.as_raw_fd())?;

		// Latency: the builder's `leaky=downstream` queue drops stale frames if the
		// encoder can't keep up with the monitor's refresh, so end-to-end lag stays
		// bounded (effective fps drops instead of latency growing).
		let pipeline = crate::pipeline::gst::wayland_pipeline(
			pw_fd.as_raw_fd(),
			node_id,
			encoder_fragment,
			ip,
			port,
		);
		let mut cmd = std::process::Command::new("gst-launch-1.0");
		cmd.arg("-q").args(pipeline.split_whitespace());
		// Die if our process dies, so an orphaned gst-launch never keeps the screen
		// "being shared" (KDE tray) after the app/session goes away.
		unsafe {
			cmd.pre_exec(|| {
				// SAFETY: async-signal-safe libc calls only.
				libc::prctl(
					libc::PR_SET_PDEATHSIG,
					libc::SIGKILL as libc::c_ulong,
					0,
					0,
					0,
				);
				if libc::getppid() == 1 {
					libc::_exit(0); // parent already gone between fork and here
				}
				Ok(())
			});
		}
		let child = cmd.spawn().map_err(|e| {
			anyhow::anyhow!("gst-launch-1.0 başlatılamadı (gstreamer kurulu mu?): {e}")
		})?;
		Ok::<_, anyhow::Error>((child, pw_fd, token))
	}
	.await;

	match res {
		Ok((child, pw_fd, token)) => Ok((
			WaylandCapture {
				child,
				session,
				_pw_fd: pw_fd,
			},
			token,
		)),
		Err(e) => {
			let _ = session.close().await;
			Err(e)
		}
	}
}
