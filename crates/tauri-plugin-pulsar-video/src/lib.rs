use tauri::{
  plugin::{Builder, TauriPlugin},
  Manager, Runtime,
};

pub use models::*;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

mod commands;
mod error;
mod models;

pub use error::{Error, Result};

#[cfg(desktop)]
use desktop::PulsarVideo;
#[cfg(mobile)]
use mobile::PulsarVideo;

// W5-native extension trait (HDR / pane layout / decoder enumeration) — mobile-only.
#[cfg(mobile)]
pub use mobile::PulsarVideoW5Ext;

/// Extensions to [`tauri::App`], [`tauri::AppHandle`] and [`tauri::Window`] to access the pulsar-video APIs.
pub trait PulsarVideoExt<R: Runtime> {
  fn pulsar_video(&self) -> &PulsarVideo<R>;
}

impl<R: Runtime, T: Manager<R>> crate::PulsarVideoExt<R> for T {
  fn pulsar_video(&self) -> &PulsarVideo<R> {
    self.state::<PulsarVideo<R>>().inner()
  }
}

/// Initializes the plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("pulsar-video")
    .invoke_handler(tauri::generate_handler![
      commands::attach,
      commands::detach,
      commands::play_test,
      commands::set_audio_muted,
      commands::set_aspect,
      commands::set_video_transform,
      commands::set_orientation,
      commands::set_status_bar,
      commands::read_clipboard,
      commands::screen_refresh_rate,
      commands::gamepad_battery,
      commands::get_video_size,
      commands::get_wallpaper_avatar,
      commands::mic_start,
      commands::mic_stop,
    ])
    .setup(|app, api| {
      #[cfg(mobile)]
      let pulsar_video = mobile::init(app, api)?;
      #[cfg(desktop)]
      let pulsar_video = desktop::init(app, api)?;
      app.manage(pulsar_video);
      Ok(())
    })
    .build()
}
