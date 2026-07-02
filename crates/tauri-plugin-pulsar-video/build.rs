const COMMANDS: &[&str] = &[
  "attach",
  "detach",
  "play_test",
  "set_audio_muted",
  "set_aspect",
  "set_video_transform",
  "set_orientation",
  "set_status_bar",
  "read_clipboard",
  "screen_refresh_rate",
  "gamepad_battery",
  "get_video_size",
  "get_wallpaper_avatar",
  "mic_start",
  "mic_stop",
];

fn main() {
  tauri_plugin::Builder::new(COMMANDS)
    .android_path("android")
    .build();
}
