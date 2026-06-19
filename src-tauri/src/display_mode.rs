//! Host-side "screen adaptation" (Parsec-style): switch a captured monitor to the
//! display mode that best fits a client split-pane, and revert it on teardown.
//!
//! When a client streams to a small split pane (e.g. a 1280×800 renderer window) the
//! host's native 3840×2160 capture gets downscaled/letterboxed into that pane — wasting
//! encode bandwidth on pixels the pane can't show and giving the wrong aspect. Parsec
//! instead temporarily changes the HOST monitor's resolution to one that matches the
//! pane's shape, so the captured framebuffer is already the right size/aspect. We do the
//! same: [`set_best_mode`] picks the closest-aspect available mode and applies it; the
//! returned [`PrevMode`] captures the monitor's ORIGINAL mode so [`revert`] can restore it
//! exactly when the session tears down (or when a later `StreamReq` sends `adapt: None`).
//!
//! IMPORTANT — robustness: the host must NEVER be left stuck at a wrong resolution. The
//! revert is the guarantee; it is driven from `SessionCleanupGuard::drop` (host.rs), which
//! fires on BOTH a normal session end and an `abort()`-path teardown. We snapshot the
//! revert target (`ENUM_CURRENT_SETTINGS`) BEFORE applying the change, and only return a
//! `PrevMode` when the apply actually succeeded — so we never store a bogus target.
//!
//! Windows-only. On other platforms [`set_best_mode`] is a no-op returning `None` (X11
//! mode-switching would be xrandr, not wired here) so the rest of the app compiles and the
//! adapt logic in handlers.rs/host.rs is inert.

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub use win::{revert, set_best_mode, PrevMode};

/// Per-session screen-adaptation state, shared (via `Arc<Mutex<_>>`) between the StreamReq
/// handler (`make_on_stream`) and the session's [`SessionCleanupGuard`](crate::host) so the
/// teardown can revert whatever the handler applied.
///
/// * `prev` — the monitor's original [`PrevMode`] when we currently hold an applied change
///   (`Some`), or `None` when the monitor is at its native mode. Teardown `take()`s and
///   reverts it (idempotent — a second drop sees `None`).
/// * `last` — the last `(display_idx, w, h)` adapt TARGET we acted on, so a repeated StreamReq
///   carrying the same target is a no-op instead of re-running the best-fit every frame. It is
///   set even when [`set_best_mode`] returns `None` (current mode already best / apply failed),
///   so we don't retry a no-op target on each restream.
#[derive(Default)]
pub struct AdaptState {
	pub prev: Option<PrevMode>,
	pub last: Option<(u32, u32, u32)>,
}

#[cfg(windows)]
mod win {
	use windows_sys::Win32::Graphics::Gdi::{
		ChangeDisplaySettingsExW, EnumDisplaySettingsW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
		ENUM_CURRENT_SETTINGS,
	};

	/// The monitor's display mode BEFORE we changed it, kept so the session teardown can
	/// restore it byte-for-byte. We keep the full original `DEVMODEW` (queried via
	/// `ENUM_CURRENT_SETTINGS`) so the revert re-applies the exact same width/height/depth/
	/// frequency/orientation the user had — not an approximation we'd have to reconstruct.
	/// `device` is the GDI device name (`\\.\DISPLAYn`) the change/revert target. `DEVMODEW`
	/// is `Copy` + plain-old-data (no pointers), so this is `Send` and safe to stash in an
	/// `Arc<Mutex<Option<PrevMode>>>` shared with the cleanup guard.
	#[derive(Clone)]
	pub struct PrevMode {
		device: Vec<u16>,
		devmode: DEVMODEW,
	}

	/// Resolve the GDI device name (`\\.\DISPLAYn`) for `display_idx` as a NUL-terminated wide
	/// string. We reuse `pulsar_capture::list_displays()` — the SAME enumeration `host_displays`
	/// and the capture path index by — so `display_idx` here means the exact monitor the client
	/// selected (its `name` is the DXGI `DeviceName`, i.e. the GDI `\\.\DISPLAYn` trimmed of the
	/// `\\.\` prefix; we re-prepend it, matching `util::display_rotation_detect`). `None` when the
	/// index is out of range (stale picker / monitor unplugged).
	fn device_name_wide(display_idx: u32) -> Option<Vec<u16>> {
		let displays = pulsar_capture::list_displays();
		let (_idx, name, _w, _h, _primary) = displays.into_iter().nth(display_idx as usize)?;
		let full = format!(r"\\.\{name}");
		Some(full.encode_utf16().chain(std::iter::once(0u16)).collect())
	}

	/// Read the monitor's CURRENT mode (`ENUM_CURRENT_SETTINGS`). `None` on failure.
	unsafe fn current_mode(device: &[u16]) -> Option<DEVMODEW> {
		let mut dm: DEVMODEW = std::mem::zeroed();
		dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
		if EnumDisplaySettingsW(device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
			Some(dm)
		} else {
			None
		}
	}

	/// Score a candidate mode against the target pane size; LOWER is better. Primary key is the
	/// aspect-ratio distance (so the captured frame matches the pane's shape — the whole point of
	/// adaptation). Among near-equal aspect (within ~1%) we tie-break on area distance to the pane,
	/// with a mild bias toward "tall enough" modes (height ≥ 1080) so adaptation doesn't drop a
	/// big monitor to a tiny letterbox-equivalent resolution. `native_area` lets us avoid exceeding
	/// the monitor's native resolution when a smaller equally-good-aspect mode exists.
	fn score(
		mw: u32,
		mh: u32,
		target_w: u32,
		target_h: u32,
		native_area: u64,
	) -> (i64, i64) {
		// Aspect distance, scaled to an integer so the tuple can be Ord-compared. We compare
		// w*th vs h*tw (cross-multiply) to avoid float division entirely.
		let mode_cross = mw as i64 * target_h as i64;
		let target_cross = mh as i64 * target_w as i64;
		// Normalize the cross-product difference so it's comparable across resolutions: divide by
		// the target dimension product magnitude. Multiply up first to keep integer precision.
		let denom = (target_w as i64 * target_h as i64).max(1);
		let aspect_dist = ((mode_cross - target_cross).abs() * 10_000) / denom;
		// Area distance to the pane (secondary). A mode that EXCEEDS native is penalized so an
		// equally-shaped non-native mode is preferred (don't push the monitor past its panel).
		let area = mw as i64 * mh as i64;
		let target_area = target_w as i64 * target_h as i64;
		let mut area_dist = (area - target_area).abs();
		// Mild preference for ≥1080 lines (keeps adaptation from collapsing to a tiny mode when
		// several share the pane's aspect): bump the area_dist of sub-1080 modes.
		if mh < 1080 {
			area_dist += target_area / 2;
		}
		// Penalize exceeding native area (we'd rather not over-drive the panel).
		if area as u64 > native_area {
			area_dist += area - native_area as i64;
		}
		(aspect_dist, area_dist)
	}

	/// Switch the monitor at `display_idx` to the available mode that best fits a `target_w ×
	/// target_h` pane, recording the original mode for [`revert`]. Returns `Some(PrevMode)` only
	/// when a real change was applied; `None` when there is nothing to do or revert:
	///   - the index is invalid / the device name can't be resolved,
	///   - the current mode is already the best fit (no change → nothing to revert),
	///   - the `ChangeDisplaySettingsExW` apply did not return `DISP_CHANGE_SUCCESSFUL`.
	///
	/// Best-fit: smallest aspect-ratio distance to the pane, then closest area (see [`score`]),
	/// preferring ≥1080 lines and not over-driving the panel past its native resolution.
	pub fn set_best_mode(display_idx: u32, target_w: u32, target_h: u32) -> Option<PrevMode> {
		if target_w == 0 || target_h == 0 {
			return None;
		}
		let device = device_name_wide(display_idx)?;
		unsafe {
			// Snapshot the current mode FIRST — this is the exact revert target.
			let original = current_mode(&device)?;
			let cur_w = original.dmPelsWidth;
			let cur_h = original.dmPelsHeight;
			// Native = the monitor's current (largest sensible) resolution; used so we don't pick a
			// mode that over-drives the panel when an equally-shaped smaller mode exists.
			let native_area = cur_w as u64 * cur_h as u64;

			// Enumerate every mode at the current color depth and pick the best fit.
			let mut best: Option<((i64, i64), DEVMODEW)> = None;
			let mut i = 0u32;
			loop {
				let mut dm: DEVMODEW = std::mem::zeroed();
				dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
				if EnumDisplaySettingsW(device.as_ptr(), i, &mut dm) == 0 {
					break; // exhausted the mode list
				}
				i += 1;
				// Only consider modes at the CURRENT color depth (mixing bit depths would let a
				// lower-quality mode win on a near-tie, and we don't want to change the depth).
				if dm.dmBitsPerPel != original.dmBitsPerPel {
					continue;
				}
				let s = score(dm.dmPelsWidth, dm.dmPelsHeight, target_w, target_h, native_area);
				match &best {
					Some((bs, _)) if *bs <= s => {}
					_ => best = Some((s, dm)),
				}
			}

			let (_, mut chosen) = best?;
			// Nothing to do if the best fit is what we're already running (avoids a needless mode
			// flap + a bogus PrevMode that revert() would then re-apply for no reason).
			if chosen.dmPelsWidth == cur_w && chosen.dmPelsHeight == cur_h {
				return None;
			}
			// Apply the chosen resolution. We mask dmFields to exactly the geometry we set so the
			// driver doesn't try to honor stale/zeroed fields from our copy. flags = 0 = a dynamic
			// change that takes effect immediately (no reboot, persists for the session). NULL hwnd
			// + NULL lparam per the Win32 contract for a plain resolution change.
			use windows_sys::Win32::Graphics::Gdi::{
				DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
			};
			chosen.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
			let rc = ChangeDisplaySettingsExW(
				device.as_ptr(),
				&chosen as *const DEVMODEW,
				std::ptr::null_mut(),
				0,
				std::ptr::null(),
			);
			if rc != DISP_CHANGE_SUCCESSFUL {
				// Failed apply: do NOT store a PrevMode (there is no change to revert). The monitor
				// is untouched, so the stream just proceeds at the host's existing resolution.
				tracing::warn!(
					display_idx,
					target_w,
					target_h,
					new_w = chosen.dmPelsWidth,
					new_h = chosen.dmPelsHeight,
					rc,
					"screen-adapt: ChangeDisplaySettingsExW failed; leaving mode unchanged"
				);
				return None;
			}
			tracing::info!(
				display_idx,
				target_w,
				target_h,
				from = format!("{cur_w}x{cur_h}"),
				to = format!("{}x{}", chosen.dmPelsWidth, chosen.dmPelsHeight),
				"screen-adapt: switched host display mode for split pane"
			);
			Some(PrevMode { device, devmode: original })
		}
	}

	/// Restore the monitor's original mode recorded in `prev`. Best-effort (logs on failure):
	/// teardown must not panic, and there is no better fallback than leaving the user to fix the
	/// resolution manually if Windows refuses the restore (rare — re-applying a mode the panel was
	/// just at). flags = 0 = a dynamic change, same as the forward switch.
	pub fn revert(prev: PrevMode) {
		unsafe {
			let rc = ChangeDisplaySettingsExW(
				prev.device.as_ptr(),
				&prev.devmode as *const DEVMODEW,
				std::ptr::null_mut(),
				0,
				std::ptr::null(),
			);
			if rc != DISP_CHANGE_SUCCESSFUL {
				tracing::warn!(
					rc,
					w = prev.devmode.dmPelsWidth,
					h = prev.devmode.dmPelsHeight,
					"screen-adapt: revert ChangeDisplaySettingsExW failed (host left at adapted mode)"
				);
			} else {
				tracing::info!(
					w = prev.devmode.dmPelsWidth,
					h = prev.devmode.dmPelsHeight,
					"screen-adapt: reverted host display mode on teardown"
				);
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Non-Windows stubs — keep handlers.rs / host.rs compiling everywhere.
// ---------------------------------------------------------------------------
/// Placeholder so callers can name the type on every platform. Carries no state off Windows.
#[cfg(not(windows))]
pub struct PrevMode;

/// No-op: screen adaptation is Windows-only (X11 xrandr switching is not wired here).
#[cfg(not(windows))]
pub fn set_best_mode(_display_idx: u32, _target_w: u32, _target_h: u32) -> Option<PrevMode> {
	None
}

/// No-op revert off Windows (there is never a stored `PrevMode` to restore).
#[cfg(not(windows))]
pub fn revert(_prev: PrevMode) {}
