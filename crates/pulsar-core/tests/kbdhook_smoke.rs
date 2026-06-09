//! Diagnostic: does `WH_KEYBOARD_LL` fire on THIS machine for a synthetic
//! `SendInput` keystroke? Answers whether low-level keyboard hooks work here at
//! all (vs. being bypassed by the input layer, e.g. ASTER multiseat). Windows-only.
#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
	SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
	CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
	TranslateMessage, UnhookWindowsHookEx, HHOOK, MSG, WH_KEYBOARD_LL, WM_QUIT,
};

static COUNT: AtomicU64 = AtomicU64::new(0);
static TID: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn smoke_proc(ncode: i32, w: WPARAM, l: LPARAM) -> LRESULT {
	if ncode >= 0 {
		COUNT.fetch_add(1, Ordering::SeqCst);
	}
	CallNextHookEx(std::ptr::null_mut(), ncode, w, l)
}

// Needs an interactive desktop (SendInput + a live input queue), so it's skipped
// in normal `cargo test` / CI; run it explicitly: `cargo test -p pulsar-core
// --test kbdhook_smoke -- --ignored --nocapture`.
#[test]
#[ignore = "requires an interactive desktop"]
fn ll_keyboard_hook_fires_for_sendinput() {
	// Install the hook on a dedicated thread with a message pump (required for LL).
	let t = std::thread::spawn(|| unsafe {
		let h: HHOOK = SetWindowsHookExW(
			WH_KEYBOARD_LL,
			Some(smoke_proc),
			GetModuleHandleW(std::ptr::null()),
			0,
		);
		assert!(!h.is_null(), "SetWindowsHookExW returned null");
		TID.store(GetCurrentThreadId() as u64, Ordering::SeqCst);
		let mut msg: MSG = std::mem::zeroed();
		while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
			TranslateMessage(&msg);
			DispatchMessageW(&msg);
		}
		UnhookWindowsHookEx(h);
	});
	std::thread::sleep(std::time::Duration::from_millis(250)); // let it install + pump

	// Inject a synthetic Space down+up.
	unsafe {
		let mk = |up: bool| INPUT {
			r#type: INPUT_KEYBOARD,
			Anonymous: INPUT_0 {
				ki: KEYBDINPUT {
					wVk: 0x20, // VK_SPACE
					wScan: 0,
					dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
					time: 0,
					dwExtraInfo: 0,
				},
			},
		};
		let inputs = [mk(false), mk(true)];
		SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
	}
	std::thread::sleep(std::time::Duration::from_millis(350)); // let the hook process

	let c = COUNT.load(Ordering::SeqCst);
	unsafe {
		PostThreadMessageW(TID.load(Ordering::SeqCst) as u32, WM_QUIT, 0, 0);
	}
	let _ = t.join();
	eprintln!("WH_KEYBOARD_LL fired {c} times for the synthetic SendInput");
	assert!(
		c > 0,
		"WH_KEYBOARD_LL callback NEVER fired for SendInput on this machine (c={c}) — \
		 low-level keyboard hooks are bypassed here"
	);
}
