// Standalone host-side test of the WASAPI loopback capture (no Pulsar/Pi needed).
// Captures ~6s of the default render endpoint to a raw f32le file so we can verify the
// capture thread actually feeds data. Run while playing audio on the host:
//   cargo run --example loopback_test -p pulsar-core
// then: ffmpeg -f f32le -ar <rate> -ac <ch> -i loopback_out.raw -af volumedetect -f null -
fn main() {
	#[cfg(windows)]
	{
		let fmt = pulsar_core::audio::loopback_format().expect("loopback_format");
		eprintln!(
			"format: {} Hz, {} ch, {}",
			fmt.rate,
			fmt.channels,
			fmt.ffmpeg_sample_fmt()
		);
		let path = r"C:\Users\ahmet\loopback_out.raw";
		let f = std::fs::File::create(path).expect("create file");
		std::thread::spawn(move || {
			let r = pulsar_core::audio::run_loopback_capture(f);
			eprintln!("run_loopback_capture returned: {r:?}");
		});
		std::thread::sleep(std::time::Duration::from_secs(6));
		let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
		eprintln!(
			"captured {len} bytes to {path} (6s @ {} Hz {}ch f32 = ~{} bytes expected)",
			fmt.rate,
			fmt.channels,
			fmt.rate as u64 * fmt.channels as u64 * 4 * 6
		);
		std::process::exit(0);
	}
	#[cfg(not(windows))]
	eprintln!("windows only");
}
