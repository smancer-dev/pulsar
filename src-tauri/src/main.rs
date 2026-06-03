// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
	// `pulsar --relay …` runs a headless relay/rendezvous server instead of the GUI,
	// so the same install can self-host a relay.
	let args: Vec<String> = std::env::args().collect();
	if args.iter().any(|a| a == "--relay") {
		pulsar_tauri::run_relay(&args);
		return;
	}
	pulsar_tauri::run()
}
