//! OS/timing primitives for the pacing loop: thread priority + display keep-alive,
//! the high-resolution waitable timer (the pacing sleep), and the QPC monotonic clock.
//! All unsafe Win32 FFI; moved verbatim from the original `dxgi.rs` (behaviour unchanged).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Power::{
	SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
	EXECUTION_STATE,
};
use windows::Win32::System::Threading::{
	CreateWaitableTimerExW, GetCurrentThread, SetThreadPriority, SetWaitableTimer,
	WaitForSingleObject, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, THREAD_PRIORITY_TIME_CRITICAL,
	TIMER_ALL_ACCESS,
};

// ---------------------------------------------------------------------------
// Thread priority + display keep-alive (called from lib.rs's thread body)
// ---------------------------------------------------------------------------

/// Pin the capture thread to `TIME_CRITICAL` so the pacing loop isn't preempted mid
/// frame interval (the documented cause of the "73 fps irregular" symptom). Best-effort;
/// a failure just means we run at normal priority.
pub unsafe fn raise_thread_priority() {
	// GetCurrentThread() returns a pseudo-handle (no need to close it).
	let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
}

/// RAII guard that asks Windows to keep the display + system awake while streaming, and
/// clears the request on drop. Mirrors what every screen-share tool does so the host
/// doesn't blank/sleep mid-session.
pub struct DisplayKeepAlive;

impl DisplayKeepAlive {
	pub unsafe fn engage() -> Self {
		// ES_CONTINUOUS makes the request sticky until the next call; combine with
		// DISPLAY|SYSTEM so neither the monitor nor the box sleeps.
		let _ = SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
		DisplayKeepAlive
	}
}

impl Drop for DisplayKeepAlive {
	fn drop(&mut self) {
		// Drop the keep-awake request — ES_CONTINUOUS alone resets to the default policy.
		unsafe {
			let _: EXECUTION_STATE = SetThreadExecutionState(ES_CONTINUOUS);
		}
	}
}

// ---------------------------------------------------------------------------
// High-resolution waitable timer (the pacing primitive)
// ---------------------------------------------------------------------------

/// ~0.5 ms-granularity sleep, far better than `thread::sleep` (15.6 ms default tick →
/// a ~64 fps ceiling). Backed by `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`.
pub(crate) struct HiResTimer {
	handle: HANDLE,
}

// HANDLE is a raw OS handle (isize); the timer is single-owner, so it's safe to move the
// timer to the RTP sender thread (Stage-1 packet pacing reuses it). No shared access.
unsafe impl Send for HiResTimer {}

impl HiResTimer {
	pub(crate) unsafe fn new() -> windows::core::Result<Self> {
		// High-resolution waitable timer; unnamed (PCWSTR::null), TIMER_ALL_ACCESS so we
		// can SetWaitableTimer it. `.0` unwraps SYNCHRONIZATION_ACCESS_RIGHTS → u32.
		let handle = CreateWaitableTimerExW(
			None,
			PCWSTR::null(),
			CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
			TIMER_ALL_ACCESS.0,
		)?;
		Ok(HiResTimer { handle })
	}

	/// Block the calling thread for `dur` using the high-res timer. `dur == 0` returns
	/// immediately (the "we're already past the deadline" path).
	pub(crate) unsafe fn sleep_for(&self, dur: std::time::Duration) {
		let ns = dur.as_nanos();
		if ns == 0 {
			return;
		}
		// SetWaitableTimer's due-time (lpduetime: *const i64) is a *relative* value when
		// NEGATIVE, in 100-ns units. So -(ns/100). Classic foot-gun: a POSITIVE value is an
		// absolute FILETIME and would wait ~forever. Saturate so a huge dur can't overflow.
		let hundred_ns = (ns / 100).min(i64::MAX as u128) as i64;
		let due: i64 = -hundred_ns;
		if SetWaitableTimer(self.handle, &due, 0, None, None, false).is_ok() {
			// INFINITE (u32::MAX) wait — the timer itself bounds the duration.
			let _ = WaitForSingleObject(self.handle, u32::MAX);
		}
	}
}

impl Drop for HiResTimer {
	fn drop(&mut self) {
		if !self.handle.is_invalid() {
			unsafe {
				let _ = CloseHandle(self.handle);
			}
		}
	}
}

// ---------------------------------------------------------------------------
// QPC clock — monotonic anchor for the pacing math (QPC → ns)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(super) struct Qpc {
	freq: i64,
}

impl Qpc {
	pub(super) unsafe fn new() -> Self {
		let mut freq = 0i64;
		// QueryPerformanceFrequency never fails on XP+; default to 1 to avoid div-by-0.
		let _ = QueryPerformanceFrequency(&mut freq);
		if freq == 0 {
			freq = 1;
		}
		Qpc { freq }
	}

	pub(super) unsafe fn now_ns(&self) -> i64 {
		let mut c = 0i64;
		let _ = QueryPerformanceCounter(&mut c);
		// Multiply before divide; do it in i128 so a multi-hour counter can't overflow.
		((c as i128) * 1_000_000_000i128 / (self.freq as i128)) as i64
	}
}
