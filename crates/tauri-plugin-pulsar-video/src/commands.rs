use tauri::{command, AppHandle, Runtime};

use crate::models::*;
use crate::PulsarVideoExt;
use crate::Result;

#[command]
pub(crate) async fn attach<R: Runtime>(
    app: AppHandle<R>,
    payload: AttachRequest,
) -> Result<AttachResponse> {
    app.pulsar_video().attach(payload)
}

#[command]
pub(crate) async fn detach<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().detach()
}

#[command]
pub(crate) async fn play_test<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().play_test()
}

/// Mute or unmute the remote audio output on the native side.
/// `muted: true` silences the AudioTrack (volume 0); `false` restores gain.
/// JS-invokable as `plugin:pulsar-video|setAudioMuted`.
#[command]
pub(crate) async fn set_audio_muted<R: Runtime>(
    app: AppHandle<R>,
    muted: bool,
) -> Result<AttachResponse> {
    app.pulsar_video().set_audio_muted(muted)
}

/// Set the video surface aspect mode: `"fit"` (default letterbox), `"fill"`
/// (crop to fill) or `"stretch"` (distort to fill). Applies to the active slot
/// (slot 0 single / slot picked by UI). JS-invokable as
/// `plugin:pulsar-video|setAspect`.
#[command]
pub(crate) async fn set_aspect<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    mode: String,
) -> Result<AttachResponse> {
    app.pulsar_video().set_aspect(slot, &mode)
}

/// Apply a local pinch-zoom/pan transform: the video's destination rect on screen,
/// normalized to the surface [0..1] (`w`/`h` > 1 = zoomed in). DPR-independent.
/// JS-invokable as `plugin:pulsar-video|setVideoTransform`.
#[command]
pub(crate) async fn set_video_transform<R: Runtime>(
    app: AppHandle<R>,
    slot: u8,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Result<AttachResponse> {
    app.pulsar_video().set_video_transform(slot, x, y, w, h)
}

/// Force portrait (`landscape: false`) or landscape (`landscape: true`) on the
/// Android activity. JS-invokable as `plugin:pulsar-video|setOrientation`.
#[command]
pub(crate) async fn set_orientation<R: Runtime>(
    app: AppHandle<R>,
    landscape: bool,
) -> Result<AttachResponse> {
    app.pulsar_video().set_orientation(landscape)
}

/// Match the system status / navigation bar icon colour to the app theme:
/// `light_theme: true` (light background) → DARK icons; `false` (dark theme) →
/// LIGHT icons. JS-invokable as `plugin:pulsar-video|setStatusBar`.
#[command]
pub(crate) async fn set_status_bar<R: Runtime>(
    app: AppHandle<R>,
    light_theme: bool,
) -> Result<AttachResponse> {
    app.pulsar_video().set_status_bar(light_theme)
}

/// Read the system clipboard text. The Android WebView denies
/// `navigator.clipboard.readText()` ("Read permission denied"), so the paste
/// button goes through the native `ClipboardManager` instead. The text is
/// returned in `AttachResponse.detail`. JS-invokable as
/// `plugin:pulsar-video|read_clipboard`.
#[command]
pub(crate) async fn read_clipboard<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().read_clipboard()
}

/// The display refresh rate in Hz (in `AttachResponse.detail`). JS-invokable as
/// `plugin:pulsar-video|screen_refresh_rate`.
#[command]
pub(crate) async fn screen_refresh_rate<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().screen_refresh_rate()
}

/// Connected gamepads + battery (JSON array in `AttachResponse.detail`).
/// JS-invokable as `plugin:pulsar-video|gamepad_battery`.
#[command]
pub(crate) async fn gamepad_battery<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().gamepad_battery()
}

/// Last decoded video size for `slot` as `"<vw>x<vh>"` in `AttachResponse.detail`.
/// JS-invokable as `plugin:pulsar-video|get_video_size`.
#[command]
pub(crate) async fn get_video_size<R: Runtime>(app: AppHandle<R>, slot: u8) -> Result<AttachResponse> {
    app.pulsar_video().get_video_size(slot)
}

/// The device's home-screen wallpaper as a small (96×96, ≤14 KB) JPEG, base64'd
/// in `AttachResponse.detail` (`ok:false` when the wallpaper can't be read, e.g.
/// permission denied or a live wallpaper). Used as this phone's identity image
/// pushed to peers, mirroring the desktop `avatar_mode = "wallpaper"`.
/// JS-invokable as `plugin:pulsar-video|get_wallpaper_avatar`.
#[command]
pub(crate) async fn get_wallpaper_avatar<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().get_wallpaper_avatar()
}

// ---- W4-mic plugin commands --------------------------------------------------

/// Start capturing microphone audio via Android `AudioRecord` (VOICE_COMMUNICATION
/// source, 48 kHz mono s16le). The plugin loops PCM into a shared ring buffer;
/// the `mic_start` Tauri command (in W4-rust-client's `client.rs`) drains it and
/// sends `DataMsg::Audio` frames to the host.
/// JS-invokable as `plugin:pulsar-video|micStart`.
#[command]
pub(crate) async fn mic_start<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().mic_start()
}

/// Stop the `AudioRecord` capture started by `micStart`. Drains any remaining
/// buffered PCM so the caller can flush a final `DataMsg::Audio` + `AudioEnd`.
/// JS-invokable as `plugin:pulsar-video|micStop`.
#[command]
pub(crate) async fn mic_stop<R: Runtime>(app: AppHandle<R>) -> Result<AttachResponse> {
    app.pulsar_video().mic_stop()
}
