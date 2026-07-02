use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::models::*;

pub fn init<R: Runtime, C: DeserializeOwned>(
  app: &AppHandle<R>,
  _api: PluginApi<R, C>,
) -> crate::Result<PulsarVideo<R>> {
  Ok(PulsarVideo(app.clone()))
}

/// Access to the pulsar-video APIs.
pub struct PulsarVideo<R: Runtime>(AppHandle<R>);

/// Desktop has no native-surface-under-webview path — the desktop app renders
/// video in the separate `pulsar-render` binary. These are no-ops so the plugin
/// still compiles for desktop sanity builds of the mobile crate (`cargo run`).
impl<R: Runtime> PulsarVideo<R> {
  fn noop(detail: &str) -> crate::Result<AttachResponse> {
    Ok(AttachResponse { ok: false, detail: detail.into() })
  }

  pub fn attach(&self, _payload: AttachRequest) -> crate::Result<AttachResponse> {
    Self::noop("no native video surface on desktop")
  }
  pub fn detach(&self) -> crate::Result<AttachResponse> {
    Self::noop("no native video surface on desktop")
  }
  pub fn play_test(&self) -> crate::Result<AttachResponse> {
    Self::noop("no native decode on desktop")
  }
  pub fn start_stream(&self, _mime: &str, _slot: u8) -> crate::Result<AttachResponse> {
    Self::noop("no native decode on desktop")
  }
  pub fn feed_au(&self, _au: &[u8], _slot: u8) -> crate::Result<AttachResponse> {
    Self::noop("no native decode on desktop")
  }
  pub fn feed_audio(&self, _packet: &[u8]) -> crate::Result<AttachResponse> {
    Self::noop("no native audio on desktop")
  }
  pub fn stop_stream(&self, _slot: u8) -> crate::Result<AttachResponse> {
    Self::noop("no native decode on desktop")
  }
  pub fn decoder_failed(&self, _slot: u8) -> crate::Result<bool> {
    Ok(false)
  }
  pub fn start_host(&self, _port: u16, _audio_port: u16, _codec: &str, _w: u32, _h: u32, _fps: u32, _kbps: u32) -> crate::Result<AttachResponse> {
    Self::noop("no screen capture host on desktop")
  }
  pub fn stop_host(&self) -> crate::Result<AttachResponse> {
    Self::noop("no screen capture host on desktop")
  }
  pub fn host_gesture(&self, _x1: f64, _y1: f64, _x2: f64, _y2: f64) -> crate::Result<AttachResponse> {
    Self::noop("no gesture injection on desktop")
  }
  pub fn open_a11y_settings(&self) -> crate::Result<AttachResponse> {
    Self::noop("no accessibility settings on desktop")
  }
  pub fn a11y_enabled(&self) -> crate::Result<AttachResponse> {
    Self::noop("no accessibility on desktop")
  }
  pub fn notify_request(&self, _peer: &str) -> crate::Result<AttachResponse> {
    Self::noop("no request notification on desktop")
  }

  // ---- W3-media-native no-ops (desktop has no native AudioTrack / SurfaceView) ----

  pub fn set_audio_muted(&self, _muted: bool) -> crate::Result<AttachResponse> {
    Self::noop("no native audio on desktop")
  }
  pub fn set_aspect(&self, _slot: u8, _mode: &str) -> crate::Result<AttachResponse> {
    Self::noop("no native surface on desktop")
  }

  pub fn set_video_transform(&self, _slot: u8, _x: f32, _y: f32, _w: f32, _h: f32) -> crate::Result<AttachResponse> {
    Self::noop("no native surface on desktop")
  }
  pub fn set_orientation(&self, _landscape: bool) -> crate::Result<AttachResponse> {
    Self::noop("no orientation lock on desktop")
  }
  pub fn set_status_bar(&self, _light_theme: bool) -> crate::Result<AttachResponse> {
    Self::noop("no system bars on desktop")
  }
  pub fn read_clipboard(&self) -> crate::Result<AttachResponse> {
    Self::noop("no native clipboard on desktop")
  }
  pub fn screen_refresh_rate(&self) -> crate::Result<AttachResponse> {
    Self::noop("no native display query on desktop")
  }
  pub fn gamepad_battery(&self) -> crate::Result<AttachResponse> {
    Self::noop("no native gamepad battery on desktop")
  }
  pub fn get_video_size(&self, _slot: u8) -> crate::Result<AttachResponse> {
    Self::noop("no native surface on desktop")
  }
  pub fn get_wallpaper_avatar(&self) -> crate::Result<AttachResponse> {
    Self::noop("no wallpaper avatar on desktop")
  }

  // ---- W4-mic no-ops (desktop has no AudioRecord) ----------------------------

  pub fn mic_start(&self) -> crate::Result<AttachResponse> {
    Self::noop("no microphone capture on desktop")
  }

  pub fn mic_stop(&self) -> crate::Result<AttachResponse> {
    Self::noop("no microphone capture on desktop")
  }

  pub fn poll_mic_frame(&self) -> crate::Result<Vec<u8>> {
    Ok(Vec::new())
  }
}
