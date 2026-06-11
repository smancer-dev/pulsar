//! Watch BTN_* events on an evdev node (the host-side "Pulsar Virtual Pointer")
//! and print each with a timestamp — proves whether the injected drag's button
//! stays held or gets spuriously released. Usage: btn_watch /dev/input/eventN

#[cfg(target_os = "linux")]
fn main() {
	let path = std::env::args()
		.nth(1)
		.expect("usage: btn_watch /dev/input/eventN");
	let mut dev = evdev::Device::open(&path).expect("open (input group?)");
	let t0 = std::time::Instant::now();
	eprintln!("watching {path}…");
	loop {
		for ev in dev.fetch_events().expect("fetch") {
			if let evdev::InputEventKind::Key(k) = ev.kind() {
				let c = k.code();
				if (272..=274).contains(&c) {
					println!(
						"{:8.3}s BTN code={} value={}",
						t0.elapsed().as_secs_f64(),
						c,
						ev.value()
					);
				}
			}
		}
	}
}

#[cfg(not(target_os = "linux"))]
fn main() {}
