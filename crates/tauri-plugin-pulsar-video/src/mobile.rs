use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{
  plugin::{PluginApi, PluginHandle},
  AppHandle, Runtime,
};

use crate::models::*;

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_pulsar_video);

// initializes the Kotlin or Swift plugin classes
pub fn init<R: Runtime, C: DeserializeOwned>(
  _app: &AppHandle<R>,
  api: PluginApi<R, C>,
) -> crate::Result<PulsarVideo<R>> {
  #[cfg(target_os = "android")]
  let handle = api.register_android_plugin("dev.pulsar.video", "PulsarVideoPlugin")?;
  #[cfg(target_os = "ios")]
  let handle = api.register_ios_plugin(init_plugin_pulsar_video)?;
  Ok(PulsarVideo(handle))
}

#[derive(Serialize)]
struct MimeArgs<'a> {
  mime: &'a str,
  slot: u8,
}

#[derive(Serialize)]
struct DataArgs {
  data: String,
  slot: u8,
}

#[derive(Serialize)]
struct SlotArgs {
  slot: u8,
}

/// Access to the pulsar-video APIs.
pub struct PulsarVideo<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> PulsarVideo<R> {
  /// Force the webview transparent and insert a native video surface beneath it.
  pub fn attach(&self, payload: AttachRequest) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("attach", payload).map_err(Into::into)
  }

  /// Remove the native surface and make the webview opaque again.
  pub fn detach(&self) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("detach", ()).map_err(Into::into)
  }

  /// Decode the bundled H.265 test clip onto the native surface (M3 proof).
  pub fn play_test(&self) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("playTest", ()).map_err(Into::into)
  }

  /// Begin a live stream: attach the surface and arm the decoder for `mime`
  /// (`video/avc` or `video/hevc`). The decoder configures itself from the first
  /// access unit's parameter sets. (M4)
  pub fn start_stream(&self, mime: &str, slot: u8) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("startStream", MimeArgs { mime, slot })
      .map_err(Into::into)
  }

  /// Feed one Annex-B access unit to a slot's live decoder. Called per-frame from
  /// the session read loop; the bytes are base64'd for the JSON bridge. (M4/M8)
  pub fn feed_au(&self, au: &[u8], slot: u8) -> crate::Result<AttachResponse> {
    let data = base64::engine::general_purpose::STANDARD.encode(au);
    self
      .0
      .run_mobile_plugin("feedAu", DataArgs { data, slot })
      .map_err(Into::into)
  }

  /// Feed one Opus packet to the (shared) live audio decoder (M5).
  pub fn feed_audio(&self, packet: &[u8]) -> crate::Result<AttachResponse> {
    let data = base64::engine::general_purpose::STANDARD.encode(packet);
    self
      .0
      .run_mobile_plugin("feedAudio", DataArgs { data, slot: 0 })
      .map_err(Into::into)
  }

  /// Stop + release a slot's live decoder. (M4/M8)
  pub fn stop_stream(&self, slot: u8) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("stopStream", SlotArgs { slot })
      .map_err(Into::into)
  }

  /// Mobile HOST (M16): capture this device's screen (MediaProjection), encode
  /// (MediaCodec), packetize to RTP, and send it to `port` on loopback.
  pub fn start_host(&self, port: u16, audio_port: u16, codec: &str, width: u32, height: u32, fps: u32, bitrate_kbps: u32) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("startHost", HostArgs { port, audio_port, codec, width, height, fps, bitrate_kbps })
      .map_err(Into::into)
  }

  pub fn stop_host(&self) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("stopHost", ()).map_err(Into::into)
  }

  /// Inject a tap/swipe into this device (normalized 0..1 coords) via the
  /// AccessibilityService — used by the mobile host's `on_input` (M16 polish).
  pub fn host_gesture(&self, x1: f64, y1: f64, x2: f64, y2: f64) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("hostGesture", GestureArgs { x1, y1, x2, y2 })
      .map_err(Into::into)
  }

  /// Open Android's Accessibility settings so the user can enable Pulsar control.
  pub fn open_a11y_settings(&self) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("openA11ySettings", ()).map_err(Into::into)
  }

  /// `ok` reflects whether the control AccessibilityService is currently enabled.
  pub fn a11y_enabled(&self) -> crate::Result<AttachResponse> {
    self.0.run_mobile_plugin("a11yEnabled", ()).map_err(Into::into)
  }

  /// W6-host-notify: post a high-priority heads-up notification for an incoming
  /// connection request, so the user is alerted even when the app is backgrounded
  /// or the screen is off (the in-app approval sheet is invisible then). Called
  /// from the host `approval_race` alongside the `session-request` event.
  pub fn notify_request(&self, peer: &str) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("notifyRequest", NotifyRequestArgs { peer })
      .map_err(Into::into)
  }

  // ---- W3-media-native additions ----

  /// Mute (`muted: true`) or unmute (`false`) the remote audio AudioTrack.
  pub fn set_audio_muted(&self, muted: bool) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("setAudioMuted", AudioMutedArgs { muted })
      .map_err(Into::into)
  }

  /// Set video surface aspect mode for `slot`: `"fit"` | `"fill"` | `"stretch"`.
  pub fn set_aspect(&self, slot: u8, mode: &str) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("setAspect", AspectArgs { slot, mode: mode.to_string() })
      .map_err(Into::into)
  }

  /// Apply a pinch-zoom/pan transform: the video's destination rect on screen,
  /// normalized to the surface [0..1] (`w`/`h` > 1 = zoomed in). DPR-independent.
  pub fn set_video_transform(&self, slot: u8, x: f32, y: f32, w: f32, h: f32) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("setVideoTransform", VideoTransformArgs { slot, x, y, w, h })
      .map_err(Into::into)
  }

  /// Lock screen orientation: `landscape: true` → landscape, `false` → portrait.
  pub fn set_orientation(&self, landscape: bool) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("setOrientation", OrientationArgs { landscape })
      .map_err(Into::into)
  }

  /// Status / nav bar icon colour vs the app theme: `light_theme: true` → dark
  /// icons (light bg); `false` → light icons (dark bg).
  pub fn set_status_bar(&self, light_theme: bool) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("setStatusBar", StatusBarArgs { light_theme })
      .map_err(Into::into)
  }

  /// Read the system clipboard text via the native `ClipboardManager`
  /// (returned in `AttachResponse.detail`).
  pub fn read_clipboard(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("readClipboard", ())
      .map_err(Into::into)
  }

  /// Display refresh rate in Hz (returned in `AttachResponse.detail`).
  pub fn screen_refresh_rate(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("screenRefreshRate", ())
      .map_err(Into::into)
  }

  /// Connected gamepads + battery (JSON in `AttachResponse.detail`).
  /// Last decoded video size for `slot` (`detail = "<vw>x<vh>"`, "0x0" until first frame).
  pub fn get_video_size(&self, slot: u8) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("getVideoSize", SlotArgs { slot })
      .map_err(Into::into)
  }

  pub fn gamepad_battery(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("gamepadBattery", ())
      .map_err(Into::into)
  }

  /// Poll+clear the slot's decoder-failed flag. `true` = the decode path threw and the
  /// codec was rebuilt since the last poll — the caller should nudge the host for an IDR
  /// (`MediaNack([0])`) so the fresh decoder gets a reference frame immediately.
  pub fn decoder_failed(&self, slot: u8) -> crate::Result<bool> {
    self
      .0
      .run_mobile_plugin::<AttachResponse>("decoderStatus", SlotArgs { slot })
      .map(|r| r.detail == "failed")
      .map_err(Into::into)
  }

  /// The home-screen wallpaper as a base64 JPEG (≤14 KB) in
  /// `AttachResponse.detail`; `ok:false` when it can't be read. Used as this
  /// device's identity image pushed to peers (mobile analogue of the desktop
  /// `avatar_mode = "wallpaper"`).
  pub fn get_wallpaper_avatar(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("getWallpaperAvatar", ())
      .map_err(Into::into)
  }

  // ---- W4-mic additions -------------------------------------------------------

  /// Start capturing microphone audio via Android AudioRecord (VOICE_COMMUNICATION,
  /// 48 kHz mono s16le). PCM is buffered internally; W4-rust-client's `mic_start`
  /// Tauri command drains it and forwards `DataMsg::Audio` frames to the host.
  pub fn mic_start(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("micStart", ())
      .map_err(Into::into)
  }

  /// Stop the AudioRecord capture. Any remaining buffered PCM is still readable
  /// by the caller before sending `DataMsg::AudioEnd`.
  pub fn mic_stop(&self) -> crate::Result<AttachResponse> {
    self
      .0
      .run_mobile_plugin("micStop", ())
      .map_err(Into::into)
  }

  /// Poll one ~20 ms PCM frame (48 kHz mono s16le, 1920 bytes) from the native mic
  /// buffer, returning the raw decoded s16le bytes — or an empty `Vec` when the
  /// buffer has no frame ready. The Kotlin side base64-encodes the PCM into the
  /// response `detail` (`{ok:false, detail:""}` when empty); we decode it here so
  /// callers can forward the bytes straight into `DataMsg::Audio`. Called in a tight
  /// loop by the W4-rust-client `mic_start` Tauri command task.
  ///
  /// This is a **native-only** bridge method — it is NOT registered as a JS
  /// command in `lib.rs`/`default.toml`. Only Rust code calls it.
  pub fn poll_mic_frame(&self) -> crate::Result<Vec<u8>> {
    let resp = self
      .0
      .run_mobile_plugin::<AttachResponse>("pollMicFrame", ())?;
    if resp.detail.is_empty() {
      return Ok(Vec::new());
    }
    Ok(
      base64::engine::general_purpose::STANDARD
        .decode(resp.detail)
        .unwrap_or_default(),
    )
  }
}

#[derive(Serialize)]
struct AudioMutedArgs {
  muted: bool,
}

#[derive(Serialize)]
struct AspectArgs {
  slot: u8,
  mode: String,
}

#[derive(Serialize)]
struct VideoTransformArgs {
  slot: u8,
  x: f32,
  y: f32,
  w: f32,
  h: f32,
}

#[derive(Serialize)]
struct OrientationArgs {
  landscape: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusBarArgs {
  light_theme: bool,
}

#[derive(Serialize)]
struct GestureArgs {
  x1: f64,
  y1: f64,
  x2: f64,
  y2: f64,
}

#[derive(Serialize)]
struct HostArgs<'a> {
  port: u16,
  audio_port: u16,
  codec: &'a str,
  width: u32,
  height: u32,
  fps: u32,
  bitrate_kbps: u32,
}

#[derive(Serialize)]
struct NotifyRequestArgs<'a> {
  peer: &'a str,
}

// ---- W5-native additions -------------------------------------------------------

/// Response from `enumerateDecoders` / `enumerateHostCodecs` — a list of codec
/// name strings (e.g. `["h265", "h264", "av1"]`).
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CodecListResponse {
  pub ok: bool,
  pub codecs: Vec<String>,
}

/// Response from `positionPanes` — echoes the applied layout.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct LayoutResponse {
  pub ok: bool,
  pub layout: String,
}

/// Response from `setHdrMode` — echoes the applied mode.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct HdrResponse {
  pub ok: bool,
  pub slot: u8,
  pub mode: String,
}

#[derive(Serialize)]
struct PositionPanesArgs {
  layout: String,
}

#[derive(Serialize)]
struct SetHdrModeArgs {
  slot: u8,
  mode: String,
}

/// W5-native extension trait for additional `PulsarVideo<R>` methods. Rust allows
/// multiple `impl` blocks for the same type, but a trait keeps the W5 additions
/// clearly namespaced and avoids confusion with existing methods in the same
/// file. Import `PulsarVideoW5Ext` to call these.
pub trait PulsarVideoW5Ext<R: Runtime> {
  /// W5-native: Arrange the active video panes in a named layout.
  ///
  /// `layout`: `"single"` (default/fullscreen), `"left-right"` (landscape split),
  /// `"top-bottom"` (portrait/stacked split), `"quad"` (2×2 grid, 4 slots).
  ///
  /// Native-only — not a JS command.
  fn position_panes(&self, layout: &str) -> crate::Result<LayoutResponse>;

  /// W5-native: Enumerate video decoder MIME types supported by this device.
  ///
  /// Returns a `CodecListResponse` with names like `["h265", "h264", "av1"]` in
  /// preference order. Used by the `host_codecs` probe (W5-rust-host lane) and by
  /// the connect-time codec negotiation to exclude codecs the device cannot decode.
  ///
  /// Native-only — not a JS command.
  fn enumerate_decoders(&self) -> crate::Result<CodecListResponse>;

  /// W5-native: Set the HDR rendering mode for one video pane.
  ///
  /// `mode`: `"sdr"` (default), `"hdr10"`, `"hlg"`.
  ///
  /// Updates the Kotlin `Pane.hdrMediaFormatHints` and the `SurfaceView` color
  /// mode / Surface data space so the display pipeline routes the surface to the
  /// correct HW path. Takes effect on the next decoder configure (i.e. after the
  /// next keyframe / `arm()`).
  ///
  /// Native-only — not a JS command.
  fn set_hdr_mode(&self, slot: u8, mode: &str) -> crate::Result<HdrResponse>;
}

impl<R: Runtime> PulsarVideoW5Ext<R> for PulsarVideo<R> {
  fn position_panes(&self, layout: &str) -> crate::Result<LayoutResponse> {
    self
      .0
      .run_mobile_plugin("positionPanes", PositionPanesArgs { layout: layout.to_string() })
      .map_err(Into::into)
  }

  fn enumerate_decoders(&self) -> crate::Result<CodecListResponse> {
    self
      .0
      .run_mobile_plugin("enumerateDecoders", ())
      .map_err(Into::into)
  }

  fn set_hdr_mode(&self, slot: u8, mode: &str) -> crate::Result<HdrResponse> {
    self
      .0
      .run_mobile_plugin(
        "setHdrMode",
        SetHdrModeArgs { slot, mode: mode.to_string() },
      )
      .map_err(Into::into)
  }
}
