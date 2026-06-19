//! Top-level window enumeration for the per-window (WGC) capture target (Phase 2b co-op).
//!
//! Two jobs, both built on `EnumWindows`:
//! - [`list_windows`] — the host advertises its visible, titled, non-tool top-level windows
//!   so the client can pick one as a [`crate::CaptureConfig::window_hwnd`] WGC capture target
//!   (mirrors [`crate::list_displays`] for the window case).
//! - [`find_window_for_launch`] — after the host launches a game/app, resolve the launched
//!   process's top-level window HWND from its PID (the game then WGC-captures itself). Games
//!   create their window asynchronously and Steam/other launchers re-parent into a child
//!   process, so this matches the launched PID *and its descendant PIDs* and is meant to be
//!   polled with a retry budget by the caller (see the doc on `find_window_for_launch`).
//!
//! All Windows-only: the non-Windows stubs return empty/None so the workspace builds
//! everywhere and the host's display path is used instead (no per-window source off Windows).
#![allow(clippy::missing_safety_doc)]

/// One enumerated host window: `(hwnd, title)`. `hwnd` is the raw Win32 `HWND` as an
/// `isize` — the same token [`crate::CaptureConfig::window_hwnd`] / `wgc::WgcCaptureDevice`
/// expect (cast to/from `i64` on the wire). `title` is the window caption.
pub type WindowDesc = (isize, String);

/// Enumerate the host's visible, user-facing top-level windows (Windows). Each entry is a
/// candidate per-window capture target. Filtered to windows that are: visible
/// (`IsWindowVisible`), un-owned (top-level, not a dialog/popup of another window), have a
/// non-empty title, are not zero-sized, and are not tool windows (`WS_EX_TOOLWINDOW` — e.g.
/// floating toolbars / tray helpers). Empty on non-Windows or if enumeration fails — the
/// caller then advertises no window picker.
#[cfg(windows)]
pub fn list_windows() -> Vec<WindowDesc> {
	use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
	use windows::Win32::UI::WindowsAndMessaging::{
		EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW,
		GetWindowTextW, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
	};

	// The callback appends to this Vec via the LPARAM cookie. EnumWindows is synchronous on
	// the calling thread, so a stack Vec borrowed through a raw pointer is safe (no escape).
	let mut out: Vec<WindowDesc> = Vec::new();

	unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
		let out = &mut *(lparam.0 as *mut Vec<WindowDesc>);
		// Visible only.
		if !IsWindowVisible(hwnd).as_bool() {
			return BOOL(1); // keep enumerating
		}
		// Top-level only: skip windows owned by another window (dialogs, tool palettes).
		if GetWindow(hwnd, GW_OWNER).map(|h| !h.is_invalid()).unwrap_or(false) {
			return BOOL(1);
		}
		// Skip tool windows (taskbar-less helper windows).
		let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
		if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
			return BOOL(1);
		}
		// Non-empty title.
		let len = GetWindowTextLengthW(hwnd);
		if len <= 0 {
			return BOOL(1);
		}
		// Skip zero-sized windows (e.g. a 0×0 message-only-ish leftover).
		let mut rect = RECT::default();
		if GetWindowRect(hwnd, &mut rect).is_ok() {
			let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
			if w <= 0 || h <= 0 {
				return BOOL(1);
			}
		}
		// Read the title (len + NUL).
		let mut buf = vec![0u16; (len + 1) as usize];
		let n = GetWindowTextW(hwnd, &mut buf);
		if n <= 0 {
			return BOOL(1);
		}
		let title = String::from_utf16_lossy(&buf[..n as usize]);
		if title.trim().is_empty() {
			return BOOL(1);
		}
		out.push((hwnd.0 as isize, title));
		BOOL(1) // continue
	}

	unsafe {
		let lparam = LPARAM(&mut out as *mut Vec<WindowDesc> as isize);
		// Returns Err when the callback stopped early (it never does here, always returns TRUE)
		// or on failure; either way `out` holds whatever was collected, so ignore the result.
		let _ = EnumWindows(Some(cb), lparam);
	}
	out
}

/// Non-Windows stub (see [`list_windows`]).
#[cfg(not(windows))]
pub fn list_windows() -> Vec<WindowDesc> {
	Vec::new()
}

/// Resolve the top-level capture-able window HWND for a launched app, given the PID of the
/// process the host spawned. Returns the first visible, titled, non-zero-size top-level
/// window owned by `pid` **or any of its descendant PIDs** — descendants because Steam and
/// other launchers spawn the real game in a child process whose window is what should be
/// captured, while the spawned process is just a stub that exits.
///
/// This is a SINGLE attempt: games create their window asynchronously (seconds after the
/// process starts), so the caller must POLL this with a retry budget (e.g. ~10 s, 250 ms
/// apart) until it returns `Some` — see `process::resolve_launched_window`. Returns `None`
/// when no matching window exists yet (retry) or the PID is gone.
#[cfg(windows)]
pub fn find_window_for_launch(pid: u32) -> Option<isize> {
	let mut pids = descendant_pids(pid);
	pids.push(pid);
	find_top_window_for_pids(&pids)
}

/// Non-Windows stub (see [`find_window_for_launch`]).
#[cfg(not(windows))]
pub fn find_window_for_launch(_pid: u32) -> Option<isize> {
	None
}

/// The process id that OWNS a window (Windows), via `GetWindowThreadProcessId`.
///
/// Used by the per-process audio loopback (Phase 4 same-host co-op): a session that
/// WGC-captures one app window (`find_window_for_launch` → HWND) resolves the EXACT
/// owning process here so its WASAPI process-loopback taps only that app's render
/// audio (and its child processes), not the whole system mix. Deriving the PID from
/// the captured window — rather than the launcher PID `launch_host_game` returns —
/// keeps the audio target identical to the video target and avoids capturing a whole
/// launcher's audio tree (e.g. Steam's PID would pull in the overlay / other games).
/// Returns `None` for an invalid window or a window whose PID can't be read.
#[cfg(windows)]
pub fn window_pid(hwnd: isize) -> Option<u32> {
	use windows::Win32::Foundation::HWND;
	use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
	let h = HWND(hwnd as *mut std::ffi::c_void);
	let mut pid = 0u32;
	// GetWindowThreadProcessId returns the owning thread id (0 on failure) and writes the PID.
	let thread = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
	if thread == 0 || pid == 0 {
		None
	} else {
		Some(pid)
	}
}

/// Non-Windows stub (see [`window_pid`]).
#[cfg(not(windows))]
pub fn window_pid(_hwnd: isize) -> Option<u32> {
	None
}

/// Best top-level window owned by any PID in `pids` (Windows). Same visibility/title/size/
/// tool-window filtering as [`list_windows`]; among matches it prefers the LARGEST window
/// (by area) so a game's main window wins over an incidental small splash/helper window.
#[cfg(windows)]
fn find_top_window_for_pids(pids: &[u32]) -> Option<isize> {
	use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
	use windows::Win32::UI::WindowsAndMessaging::{
		EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW,
		GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
	};

	// (pid set, best (hwnd, area)) carried through the callback.
	struct Ctx<'a> {
		pids: &'a [u32],
		best: Option<(isize, i64)>,
	}
	let mut ctx = Ctx { pids, best: None };

	unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
		let ctx = &mut *(lparam.0 as *mut Ctx);
		let mut wpid = 0u32;
		GetWindowThreadProcessId(hwnd, Some(&mut wpid));
		if wpid == 0 || !ctx.pids.contains(&wpid) {
			return BOOL(1);
		}
		if !IsWindowVisible(hwnd).as_bool() {
			return BOOL(1);
		}
		if GetWindow(hwnd, GW_OWNER).map(|h| !h.is_invalid()).unwrap_or(false) {
			return BOOL(1);
		}
		let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
		if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
			return BOOL(1);
		}
		if GetWindowTextLengthW(hwnd) <= 0 {
			return BOOL(1);
		}
		let mut rect = RECT::default();
		if GetWindowRect(hwnd, &mut rect).is_err() {
			return BOOL(1);
		}
		let (w, h) = ((rect.right - rect.left) as i64, (rect.bottom - rect.top) as i64);
		if w <= 0 || h <= 0 {
			return BOOL(1);
		}
		let area = w * h;
		match ctx.best {
			Some((_, best_area)) if best_area >= area => {}
			_ => ctx.best = Some((hwnd.0 as isize, area)),
		}
		BOOL(1)
	}

	unsafe {
		let lparam = LPARAM(&mut ctx as *mut Ctx as isize);
		let _ = EnumWindows(Some(cb), lparam);
	}
	ctx.best.map(|(hwnd, _)| hwnd)
}

/// All descendant PIDs of `root` (Windows), via one toolhelp process snapshot walked
/// transitively. Used so a launcher (Steam, an Epic helper, a `.bat`) that spawns the real
/// game in a child still resolves to the game's window. Bounded (one snapshot, fixed-point
/// expansion over the snapshot's parent→child edges). Empty on failure.
#[cfg(windows)]
fn descendant_pids(root: u32) -> Vec<u32> {
	use windows::Win32::Foundation::CloseHandle;
	use windows::Win32::System::Diagnostics::ToolHelp::{
		CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
		TH32CS_SNAPPROCESS,
	};

	// Collect (pid, parent_pid) for every process, then expand the descendant set of `root`.
	let mut edges: Vec<(u32, u32)> = Vec::new();
	unsafe {
		let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
			return Vec::new();
		};
		let mut entry = PROCESSENTRY32W {
			dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
			..Default::default()
		};
		if Process32FirstW(snap, &mut entry).is_ok() {
			loop {
				edges.push((entry.th32ProcessID, entry.th32ParentProcessID));
				entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
				if Process32NextW(snap, &mut entry).is_err() {
					break;
				}
			}
		}
		let _ = CloseHandle(snap);
	}

	// Transitive closure: repeatedly pull in any pid whose parent is already in the set.
	let mut set: Vec<u32> = Vec::new();
	let mut frontier = vec![root];
	while let Some(p) = frontier.pop() {
		for &(pid, parent) in &edges {
			if parent == p && pid != root && !set.contains(&pid) {
				set.push(pid);
				frontier.push(pid);
			}
		}
	}
	set
}
