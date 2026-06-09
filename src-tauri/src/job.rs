//! Windows Job Object — ties every spawned media child (ffmpeg encode/audio, ffplay)
//! to Pulsar's process lifetime so it can NEVER outlive the app.
//!
//! Why: on Windows a child does NOT die when its parent dies. Pulsar kills its ffmpeg
//! children on normal session teardown (lib.rs: re-stream and `tokio::select!` end),
//! but those paths do not run when the process dies abnormally — a crash, an external
//! `taskkill`, or the tray "Çıkış" (`app.exit(0)` calls `std::process::exit`, which does
//! not unwind the tokio task holding the procs). The encoders then leak: each keeps an
//! NVENC session open, so the GPU sits at ~100% and the consumer-GeForce NVENC-session
//! limit fills up — the next connection's encoder fails to open and the client gets no
//! video. (Confirmed on the maintainer's RTX-3080 box: three orphaned ffmpeg.exe with
//! dead parents, NVENC pegged, no video.)
//!
//! Fix: a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. We hold the only handle to it
//! for the whole process lifetime and assign every spawned child to it. When Pulsar's
//! process exits for ANY reason, the OS closes that last handle and kills every member.
//! This is additive insurance — the explicit per-session kills stay as the prompt path
//! that frees the GPU between sessions while Pulsar keeps running.
//!
//! Nested jobs: on Win8+ a process already inside a job (Windows Terminal / VS Code /
//! `bun run tauri dev`) can still create and assign a nested job, so this works in those
//! launch contexts. If a restrictive ancestor forbids it, the assign fails with
//! ACCESS_DENIED — we log and continue (rare on Win11).

use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
	AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
	SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

// HANDLE is `*mut c_void` (`!Send + !Sync`), so a `static OnceLock<HANDLE>` would not be
// `Sync` and would NOT compile. Store the handle as an `isize` (a kernel handle value is
// safe to share) and cast back to HANDLE at the call sites. `0` means "creation failed".
static JOB: OnceLock<isize> = OnceLock::new();

/// Get (creating once) the process-wide job handle, or `0` if creation failed. The handle
/// is intentionally never closed — closing it would trigger KILL_ON_JOB_CLOSE and kill the
/// live stream. It is reclaimed by the OS on process exit, which is exactly when we want
/// the members killed.
fn job() -> isize {
	*JOB.get_or_init(|| unsafe {
		// Unnamed, non-inheritable job (the child must not inherit a handle that would
		// keep the job — and thus the encoders — alive past Pulsar's death).
		let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
		if job.is_null() {
			return 0;
		}
		let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
		info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
		// An unconfigured job won't kill-on-close, but keeping the handle is harmless.
		SetInformationJobObject(
			job,
			JobObjectExtendedLimitInformation,
			&info as *const _ as *const core::ffi::c_void,
			std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
		);
		job as isize
	})
}

/// Assign a freshly-spawned child to the job so it dies with Pulsar. Best-effort: a
/// failure (e.g. a restrictive ancestor job) is logged, never fatal — the explicit
/// per-session kill still covers normal teardown.
pub fn assign(child: &std::process::Child) {
	let job = job();
	if job == 0 {
		return;
	}
	unsafe {
		let h = child.as_raw_handle() as HANDLE;
		AssignProcessToJobObject(job as HANDLE, h);
	}
}
