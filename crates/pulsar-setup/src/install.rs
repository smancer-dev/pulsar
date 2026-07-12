//! The install/uninstall engine — everything except the UI. Runs on a worker
//! thread and reports progress over an mpsc channel.
//!
//! Windows specifics (registry uninstall entry, .lnk shortcuts, WebView2 probe)
//! are `#[cfg(windows)]`; on other OSes they're no-ops so the crate compiles and
//! the extract path stays locally testable.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// The embedded app payload (zip of pulsar.exe + resources/). CI sets
/// `PULSAR_SETUP_PAYLOAD`; a dev build embeds an empty placeholder instead.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zip"));

pub const APP_NAME: &str = "Pulsar";
pub const UNINSTALL_EXE: &str = "pulsar-setup.exe";

/// Progress events streamed to the UI.
pub enum Msg {
	/// Extraction started; total entry count.
	Total(usize),
	/// Entry `i` (0-based) extracted (name shown in the progress line).
	Entry(usize, String),
	/// Post-extract bookkeeping stage (shortcuts / registry / uninstaller copy).
	Finishing,
	Done,
	Err(String),
}

pub fn have_payload() -> bool {
	!PAYLOAD.is_empty()
}

pub fn version() -> &'static str {
	env!("CARGO_PKG_VERSION")
}

/// Default per-user install dir: `%LOCALAPPDATA%\Programs\Pulsar` (the Discord /
/// VS Code user-install convention — no UAC needed).
pub fn default_install_dir() -> PathBuf {
	#[cfg(windows)]
	{
		let base = std::env::var_os("LOCALAPPDATA")
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from(r"C:\"));
		base.join("Programs").join(APP_NAME)
	}
	#[cfg(not(windows))]
	{
		std::env::temp_dir().join("pulsar-setup-test").join(APP_NAME)
	}
}

/// The full install: stop a running Pulsar, extract the payload, copy this exe in
/// as the uninstaller, write shortcuts + the uninstall registry entry. Blocking —
/// run on a worker thread.
pub fn install(dir: &Path, desktop_shortcut: bool, tx: &Sender<Msg>) {
	let r = (|| -> Result<(), String> {
		if !have_payload() {
			return Err("bu kurulum dosyasında uygulama paketi yok (geliştirme derlemesi)".into());
		}
		kill_running();
		std::fs::create_dir_all(dir).map_err(|e| format!("klasör oluşturulamadı: {e}"))?;

		let mut zip =
			zip::ZipArchive::new(Cursor::new(PAYLOAD)).map_err(|e| format!("paket bozuk: {e}"))?;
		let total = zip.len();
		let _ = tx.send(Msg::Total(total));
		for i in 0..total {
			let mut entry = zip.by_index(i).map_err(|e| format!("paket okunamadı: {e}"))?;
			let Some(rel) = entry.enclosed_name() else { continue };
			let out = dir.join(rel);
			if entry.is_dir() {
				std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
			} else {
				if let Some(parent) = out.parent() {
					std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
				}
				let mut buf = Vec::with_capacity(entry.size() as usize);
				entry
					.read_to_end(&mut buf)
					.map_err(|e| format!("{}: {e}", entry.name()))?;
				std::fs::write(&out, &buf).map_err(|e| format!("{}: {e}", out.display()))?;
			}
			let _ = tx.send(Msg::Entry(i, entry.name().to_string()));
		}

		let _ = tx.send(Msg::Finishing);
		// Copy THIS exe into the install dir as the uninstaller (registry points at it).
		if let Ok(me) = std::env::current_exe() {
			let dst = dir.join(UNINSTALL_EXE);
			if me != dst {
				let _ = std::fs::copy(&me, &dst);
			}
		}
		write_shortcuts(dir, desktop_shortcut);
		write_uninstall_registry(dir);
		Ok(())
	})();
	let _ = tx.send(match r {
		Ok(()) => Msg::Done,
		Err(e) => Msg::Err(e),
	});
}

/// Headless install for `/S` / `--silent` (updater / scripted): reuse the
/// registered InstallLocation when present, else the default dir. Keeps existing
/// shortcut choices (registry entry rewrite only refreshes metadata).
pub fn silent_install() -> Result<(), String> {
	let dir = registered_install_dir().unwrap_or_else(default_install_dir);
	let (tx, rx) = std::sync::mpsc::channel();
	install(&dir, false, &tx);
	// install() is synchronous — drain the channel for the outcome.
	let mut err = None;
	while let Ok(m) = rx.try_recv() {
		if let Msg::Err(e) = m {
			err = Some(e);
		}
	}
	match err {
		None => {
			launch_app(&dir);
			Ok(())
		}
		Some(e) => Err(e),
	}
}

/// Remove the app: kill it, delete shortcuts + the registry entry, then delete the
/// install dir via a detached `cmd` (this exe LIVES in that dir, so it can't remove
/// itself while running — the shell does it after we exit).
pub fn uninstall() {
	let dir = registered_install_dir()
		.or_else(|| std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)))
		.unwrap_or_else(default_install_dir);
	kill_running();
	remove_shortcuts();
	remove_uninstall_registry();
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		const DETACHED_PROCESS: u32 = 0x0000_0008;
		let _ = std::process::Command::new("cmd")
			.args([
				"/C",
				&format!(
					"ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"",
					dir.display()
				),
			])
			.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
			.spawn();
	}
	#[cfg(not(windows))]
	{
		let _ = std::fs::remove_dir_all(&dir);
	}
}

/// Launch the installed app (detached).
pub fn launch_app(dir: &Path) {
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const DETACHED_PROCESS: u32 = 0x0000_0008;
		let _ = std::process::Command::new(dir.join("pulsar.exe"))
			.current_dir(dir)
			.creation_flags(DETACHED_PROCESS)
			.spawn();
	}
	#[cfg(not(windows))]
	{
		let _ = std::process::Command::new(dir.join("pulsar")).current_dir(dir).spawn();
	}
}

/// Whether the WebView2 runtime is present (the app's UI needs it). Probed so the
/// done-screen can warn + offer the Microsoft bootstrapper when it's missing —
/// on Win10 it usually IS present (ships with Edge), Win11 always.
pub fn webview2_present() -> bool {
	#[cfg(windows)]
	{
		// Evergreen runtime registers this client GUID under HKLM (per-machine) or
		// HKCU (per-user); either counts.
		const KEY: &str = r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
		reg_key_exists(windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE, KEY)
			|| reg_key_exists(windows_sys::Win32::System::Registry::HKEY_CURRENT_USER, KEY)
			|| reg_key_exists(
				windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE,
				r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
			)
	}
	#[cfg(not(windows))]
	{
		true
	}
}

/// Best-effort: download + run Microsoft's Evergreen WebView2 bootstrapper
/// (needs network; the payload itself stays fully offline-installable).
pub fn install_webview2() {
	#[cfg(windows)]
	{
		let tmp = std::env::temp_dir().join("MicrosoftEdgeWebview2Setup.exe");
		let script = format!(
			"Invoke-WebRequest -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile '{}'; Start-Process -Wait '{}'",
			tmp.display(),
			tmp.display()
		);
		let _ = std::process::Command::new("powershell")
			.args(["-NoProfile", "-NonInteractive", "-Command", &script])
			.status();
	}
}

fn kill_running() {
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		for exe in ["pulsar.exe", "pulsar-render.exe"] {
			let _ = std::process::Command::new("taskkill")
				.args(["/F", "/IM", exe])
				.creation_flags(CREATE_NO_WINDOW)
				.status();
		}
	}
}

// ── Shortcuts (Start Menu + optional Desktop) ─────────────────────────────────

#[cfg(windows)]
fn shortcut_paths() -> Vec<(PathBuf, bool)> {
	// (lnk path, is_desktop)
	let mut v = Vec::new();
	if let Some(appdata) = std::env::var_os("APPDATA") {
		v.push((
			PathBuf::from(appdata)
				.join(r"Microsoft\Windows\Start Menu\Programs")
				.join(format!("{APP_NAME}.lnk")),
			false,
		));
	}
	if let Some(profile) = std::env::var_os("USERPROFILE") {
		v.push((PathBuf::from(profile).join("Desktop").join(format!("{APP_NAME}.lnk")), true));
	}
	v
}

fn write_shortcuts(dir: &Path, desktop: bool) {
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		let target = dir.join("pulsar.exe");
		for (lnk, is_desktop) in shortcut_paths() {
			if is_desktop && !desktop {
				continue;
			}
			// WScript.Shell COM via powershell — no extra COM plumbing in Rust for a
			// two-line shortcut write.
			let script = format!(
				"$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath='{}'; $s.WorkingDirectory='{}'; $s.Description='Pulsar'; $s.Save()",
				lnk.display(),
				target.display(),
				dir.display()
			);
			let _ = std::process::Command::new("powershell")
				.args(["-NoProfile", "-NonInteractive", "-Command", &script])
				.creation_flags(CREATE_NO_WINDOW)
				.status();
		}
	}
	#[cfg(not(windows))]
	{
		let _ = (dir, desktop);
	}
}

fn remove_shortcuts() {
	#[cfg(windows)]
	for (lnk, _) in shortcut_paths() {
		let _ = std::fs::remove_file(lnk);
	}
}

// ── Registry: HKCU uninstall entry ────────────────────────────────────────────

#[cfg(windows)]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Pulsar";

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
	s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn reg_key_exists(root: windows_sys::Win32::System::Registry::HKEY, path: &str) -> bool {
	use windows_sys::Win32::System::Registry::*;
	let mut h: HKEY = std::ptr::null_mut();
	let ok = unsafe { RegOpenKeyExW(root, wide(path).as_ptr(), 0, KEY_READ, &mut h) } == 0;
	if ok {
		unsafe { RegCloseKey(h) };
	}
	ok
}

fn write_uninstall_registry(dir: &Path) {
	#[cfg(windows)]
	unsafe {
		use windows_sys::Win32::System::Registry::*;
		let mut h: HKEY = std::ptr::null_mut();
		if RegCreateKeyExW(
			HKEY_CURRENT_USER,
			wide(UNINSTALL_KEY).as_ptr(),
			0,
			std::ptr::null(),
			0,
			KEY_WRITE,
			std::ptr::null(),
			&mut h,
			std::ptr::null_mut(),
		) != 0
		{
			return;
		}
		let set = |name: &str, val: &str| {
			let v = wide(val);
			RegSetValueExW(
				h,
				wide(name).as_ptr(),
				0,
				REG_SZ,
				v.as_ptr() as *const u8,
				(v.len() * 2) as u32,
			);
		};
		let exe = dir.join("pulsar.exe");
		let unins = dir.join(UNINSTALL_EXE);
		set("DisplayName", APP_NAME);
		set("DisplayVersion", version());
		set("Publisher", "Pulsar contributors");
		set("InstallLocation", &dir.display().to_string());
		set("DisplayIcon", &exe.display().to_string());
		set("UninstallString", &format!("\"{}\" --uninstall", unins.display()));
		RegCloseKey(h);
	}
	#[cfg(not(windows))]
	let _ = dir;
}

fn remove_uninstall_registry() {
	#[cfg(windows)]
	unsafe {
		use windows_sys::Win32::System::Registry::*;
		RegDeleteKeyW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr());
	}
}

/// InstallLocation from a previous install's registry entry (silent updates and
/// uninstall reuse it so a custom dir survives).
pub fn registered_install_dir() -> Option<PathBuf> {
	#[cfg(windows)]
	unsafe {
		use windows_sys::Win32::System::Registry::*;
		let mut h: HKEY = std::ptr::null_mut();
		if RegOpenKeyExW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr(), 0, KEY_READ, &mut h) != 0 {
			return None;
		}
		let mut buf = [0u16; 1024];
		let mut len = (buf.len() * 2) as u32;
		let mut ty = 0u32;
		let ok = RegQueryValueExW(
			h,
			wide("InstallLocation").as_ptr(),
			std::ptr::null_mut(),
			&mut ty,
			buf.as_mut_ptr() as *mut u8,
			&mut len,
		) == 0;
		RegCloseKey(h);
		if !ok || ty != REG_SZ {
			return None;
		}
		let n = (len as usize / 2).saturating_sub(1);
		let s = String::from_utf16_lossy(&buf[..n]);
		if s.is_empty() {
			None
		} else {
			Some(PathBuf::from(s))
		}
	}
	#[cfg(not(windows))]
	{
		None
	}
}
