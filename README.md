# pulsar-desktop

The **desktop** app for [Pulsar](https://github.com/PulsarDesk) — the free,
open-source remote-desktop + game-streaming platform. Runs on **Windows, macOS and
Linux** (x64 and ARM64 for Orange Pi / Raspberry Pi).

**Stack:** **Tauri 2** shell · **SvelteKit (Svelte 5)** UI · **Rust**. One app is
both the *client* (connect to others) and the *host* (share this machine), with two
mode-aware personalities: **Remote Desktop** and **Game Streaming**.

## Platform support

Every host degrades to software encode and every viewer to software decode when no
hardware path validates on that machine; a session that still shows no video says
why on the viewer (host verdict, first-frame watchdog) and in the app log
(Settings → General → *Open log folder*).

| Platform | Host (capture + encode) | Viewer (decode) | Status |
| --- | --- | --- | --- |
| Linux x64 (X11 / Wayland) | ffmpeg / GStreamer: NVENC, VAAPI, Vulkan, x264 — one-frame probe-validated | pulsar-render: CUDA, VAAPI, Vulkan, software; runtime fallback on zero output | verified (AMD iGPU + NVIDIA hybrid) |
| Linux ARM64 (Raspberry Pi, Orange Pi / RK3588) | rkmpp (RK3588), x264; KMS zero-copy path optional | pulsar-render: DRM_PRIME zero-copy (rkmpp, v4l2) or software | exercised on RK3588 / Pi during development |
| Windows x64 | native DXGI + NVENC (SDK-validated), AMF, QSV, Media Foundation, x264 via ffmpeg; runtime fallback chain to software | pulsar-render: Media Foundation (DXVA) with sync-MFT rebuild; bundled ffplay fallback | verified on NVIDIA + Intel; AMD via the same probe path |
| Windows ARM64 (Snapdragon X) | Media Foundation (`h264_mf`), x264 | Media Foundation inbox decoder | best-effort CI build, no test hardware yet |
| macOS (Apple Silicon, Intel) | avfoundation + VideoToolbox, x264 via ffmpeg | mpv when installed (VideoToolbox), otherwise bundled ffplay (software) | builds in CI, not yet verified on hardware |
| Android | — | MediaCodec ([pulsar-mobile](https://github.com/PulsarDesk/pulsar-mobile)) | verified |
| iOS | — | VideoToolbox (pulsar-mobile) | not yet verified |

## Layout

```
pulsar-desktop/
  src/                    # SvelteKit frontend (app shell, screens, WebCodecs viewer)
  src-tauri/              # Tauri commands bridging the UI to the engine (pulsar-tauri)
  crates/
    pulsar-render/        # native video renderer (mpv/ffmpeg, zero-copy decode)
    pulsar-capture/       # screen capture + encode (DXGI/WGC/NVENC on Windows)
  scripts/                # fetch bundled ffmpeg / drivers, version stamp, update manifest
```

The shared engine is a git dependency —
[`pulsar-core`](https://github.com/PulsarDesk/pulsar-core) (with
[`pulsar-proto`](https://github.com/PulsarDesk/pulsar-proto) and
[`relay`](https://github.com/PulsarDesk/relay)).

## Run & build

```bash
bun install
bun run tauri dev        # run the real app (Rust + webview) — NOT `cargo tauri`
bun run tauri build      # package installers (bundles ffmpeg — fetched by scripts/)
bun run dev              # UI only in a browser (uses the mock)
```

## Test

```bash
cargo test               # headless crate suite (pulsar-capture)
bun run test:unit        # SvelteKit components
```

## Releases

`release.yml` builds signed installers for Windows / macOS / Linux (x64 + ARM64) and
a rolling auto-update manifest, then attaches them to a GitHub Release cut by
semantic-release (tag + release, no commit-back). The installer matrix is opt-in —
set repo variable `ENABLE_DESKTOP_RELEASE=true` and the `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
