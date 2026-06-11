//! Drag-release repro injector: creates a VIRTUAL uinput mouse and performs a long
//! steady drag (BTN_LEFT held + relative wiggle). The Pulsar client's evdev capture
//! grabs it like a real mouse and forwards the events to the host — if the host-side
//! drag releases by itself mid-run, the bug reproduced without touching the real mouse.
//!
//! Run (client machine, while a session is ENGAGED):
//!   cargo run -p pulsar-core --example drag_repro -- [seconds]

#[cfg(target_os = "linux")]
fn main() {
	let secs: u64 = std::env::args()
		.nth(1)
		.and_then(|s| s.parse().ok())
		.unwrap_or(30);
	let mut d = pulsar_core::input::DesktopInput::new().expect("uinput (input group?)");
	// Give the capture thread's 1 s device rescan time to grab this device.
	eprintln!("virtual mouse up; waiting for the evdev grab…");
	std::thread::sleep(std::time::Duration::from_millis(2500));
	eprintln!("BTN_LEFT down — dragging for {secs}s");
	d.button(0, true);
	let t0 = std::time::Instant::now();
	let mut i = 0u64;
	while t0.elapsed().as_secs() < secs {
		let dx = if i % 2 == 0 { 3.0 } else { -3.0 };
		d.pointer_relative(dx, if i % 7 == 0 { 1.0 } else { -0.0 });
		i += 1;
		std::thread::sleep(std::time::Duration::from_millis(16));
	}
	d.button(0, false);
	eprintln!("BTN_LEFT up — done ({i} moves)");
}

#[cfg(not(target_os = "linux"))]
fn main() {
	eprintln!("linux only");
}
