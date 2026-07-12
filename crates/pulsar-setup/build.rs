// Embeds the app payload zip. CI sets PULSAR_SETUP_PAYLOAD to the zip it
// assembled from the built app (pulsar.exe + resources/); dev builds without the
// env get a tiny placeholder so the crate always compiles (the UI then refuses
// to install with a clear "no payload" error).
//
// Windows resource: the exe icon (taskbar / Explorer / Add-Remove-Programs).

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
	println!("cargo:rerun-if-env-changed=PULSAR_SETUP_PAYLOAD");
	let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("payload.zip");
	match env::var("PULSAR_SETUP_PAYLOAD") {
		Ok(src) if !src.is_empty() => {
			println!("cargo:rerun-if-changed={src}");
			fs::copy(&src, &out).expect("PULSAR_SETUP_PAYLOAD copy failed");
		}
		_ => {
			// Placeholder: an EMPTY file (not a valid zip) — runtime detects and refuses.
			fs::write(&out, b"").unwrap();
		}
	}

	#[cfg(windows)]
	{
		let ico = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
			.join("../../src-tauri/icons/icon.ico");
		if ico.exists() {
			let mut res = winresource::WindowsResource::new();
			res.set_icon(ico.to_str().unwrap());
			res.set("ProductName", "Pulsar Setup");
			res.set("FileDescription", "Pulsar Kurulumu");
			let _ = res.compile();
		}
	}
}
