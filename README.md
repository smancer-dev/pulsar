# Pulsar Desktop

**Free, open-source remote desktop and game streaming in one app** for Windows, macOS
and Linux (x64 and ARM64, including Orange Pi 5 and Raspberry Pi).

Every install works both ways: as a **client** you connect to other machines, and as a
**host** you share your own. You pick a mode each time you connect:

| | Remote Desktop | Game Streaming |
| --- | --- | --- |
| Similar to | AnyDesk, RustDesk | Moonlight, Parsec |
| Tuned for | image quality and full control | lowest possible latency |
| In-session tools | file transfer, clipboard, chat, mic and voice call, multi-monitor | codec, bitrate, fps, resolution, controllers |
| Overlay | connection info strip | performance HUD (latency, fps, bitrate) |

Built with **Tauri 2**, **SvelteKit (Svelte 5)** and **Rust**.

## Features

- **Connects anywhere.** Peer-to-peer with NAT hole-punching, and a relay when a direct
  path fails. You can run your own relay and choose *auto*, *P2P-only* or *relay-only*
  in Settings.
- **End-to-end encrypted.** X25519 key exchange and ChaCha20-Poly1305. Each device keeps
  a stable 9-digit ID. Access uses one-time passwords or unattended access, with an
  approve or view-only prompt on the host.
- **Hardware video, rendered natively.** Direct NVENC (the Sunshine approach), AMF,
  QSV, VAAPI, VideoToolbox and Rockchip MPP. Codecs are checked on real hardware at
  startup, and every path falls back to software.
- **Adaptive streaming.** Bitrate, resolution and frame rate follow the network, with
  Reed-Solomon FEC and loss recovery.
- **Game-ready input.** Controllers appear on the host as virtual Xbox 360 or
  DualShock 4 pads (ViGEmBus on Windows, uinput on Linux). System keys such as Alt+Tab
  and Win are forwarded. The UI can be driven with a gamepad, and there is a games
  library.
- **Audio.** Host audio streams as Opus (WASAPI loopback on Windows, so no virtual cable
  is needed).
- **Multi-session.** Tabs and split view, LAN device discovery, signed auto-updates, and
  a UI in English, Türkçe, Русский and Қазақша.

## Platform support

If no hardware path works on a machine, the host falls back to software encode and the
viewer to software decode. If a session still shows no video, the viewer says why, and
the app log has details (Settings → General → *Open log folder*).

| Platform | Host (capture + encode) | Viewer (decode) | Status |
| --- | --- | --- | --- |
| Linux x64 (X11 / Wayland) | in-process libav (X11), GStreamer via PipeWire (Wayland), ffmpeg: NVENC, VAAPI, Vulkan, x264 | pulsar-render: CUDA, VAAPI, Vulkan, software | verified (AMD iGPU + NVIDIA hybrid) |
| Linux ARM64 (Orange Pi / RK3588, Raspberry Pi) | rkmpp (RK3588), x264 | pulsar-render: DRM_PRIME zero-copy (rkmpp, v4l2), software | tested on RK3588 and Pi |
| Windows x64 | DXGI / WGC + NVENC SDK, or ffmpeg with AMF, QSV, Media Foundation, x264 | pulsar-render: Media Foundation + D3D11 (DXVA), ffplay fallback | verified on NVIDIA + Intel |
| Windows ARM64 (Snapdragon X) | Media Foundation (`h264_mf`), x264 | Media Foundation | best-effort CI build, no test hardware yet |
| macOS (Apple Silicon, Intel) | AVFoundation + VideoToolbox, x264 | mpv (VideoToolbox) if installed, otherwise bundled ffplay | builds in CI, not yet tested on hardware |
| Android | n/a | MediaCodec ([pulsar-mobile](https://github.com/PulsarDesk/pulsar-mobile)) | verified |
| iOS | n/a | VideoToolbox (pulsar-mobile) | not yet verified |

## Build from source

### Prerequisites

- [Bun](https://bun.sh). This repo uses Bun as its package manager, not npm.
- [Rust](https://rustup.rs) stable. `rust-toolchain.toml` selects it; if you get an
  edition-2024 error, run `rustup update stable`.
- **Windows:** Visual Studio Build Tools with *Desktop development with C++*. WebView2
  is already part of Windows 10 and 11.
- **macOS:** Xcode Command Line Tools (`xcode-select --install`).
- **Linux (Debian / Ubuntu):** the packages below.

<details>
<summary>Linux packages</summary>

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf libudev-dev libmpv-dev libxkbcommon-dev \
  libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
  libavdevice-dev libavfilter-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  libegl1-mesa-dev libx11-dev libasound2-dev libpulse-dev \
  libxext-dev libxrandr-dev libxcursor-dev libxi-dev libxfixes-dev \
  libxss-dev libxtst-dev libgl1-mesa-dev libgles2-mesa-dev \
  libdrm-dev libgbm-dev libwayland-dev libdbus-1-dev
```

Add your user to the `input` group. Keyboard and mouse capture, and input injection on
the host, need it.
</details>

### Run

```bash
git clone https://github.com/smancer-dev/pulsar.git
cd pulsar
bun install
```

The app ships helper binaries from `src-tauri/resources/`, which is git-ignored. Stage
them once before your first build:

```bash
bun run fetch:ffmpeg                  # bundled ffmpeg (encode fallback, audio)
bun scripts/fetch-player.mjs          # Windows / macOS: ffplay fallback renderer
bun run fetch:drivers                 # Windows: Interception, ViGEmBus, virtual audio
cargo build --release -p pulsar-render
cp target/release/pulsar-render src-tauri/resources/   # pulsar-render.exe on Windows
```

Then:

```bash
bun run tauri dev      # run the app (use this, not `cargo tauri`)
bun run tauri build    # build installers
bun run dev            # UI only, in a browser, with a mock backend
```

## Command line

```bash
# Kiosk / appliance: skip the home screen and connect on launch
pulsar --connect <id|ip> [--connect-pw <password>] [--mode game|remote] [--app <name>] [--nofullscreen]

# Run the app as a relay server
pulsar --relay [--host 0.0.0.0] [--port <port>] \
  [--session-rate 10mbit] [--user-rate 10mbit] [--user-data 5gb] \
  [--auth-password <pw>] [--auth-totp <base32>] [--require-e2e]

# Create a 2FA secret for relay logins (prints a QR code)
pulsar --relay --generate-totp
```

In game mode, `--app` defaults to **Desktop**, an entry every host has. Each relay flag
can also be set with an environment variable (for example `--session-rate` →
`PULSAR_RELAY_SESSION_RATE`).

## Project layout

```
src/                  SvelteKit UI: screens, session tabs + split view, i18n
src-tauri/            the Tauri app (crate pulsar-tauri, binary `pulsar`):
                      host capture/encode, capability probe, input capture, relay mode
crates/
  pulsar-render/      native video renderer process for every platform, with an
                      egui in-session overlay
  pulsar-capture/     Windows capture + encode: DXGI / WGC → NVENC SDK → RTP
  pulsar-setup/       branded Windows installer (bootstrapper)
scripts/              fetch bundled binaries, version stamping, update manifest,
                      network-emulation test helpers
```

The streaming engine lives in separate repositories and is pulled in as git dependencies:
[`pulsar-core`](https://github.com/PulsarDesk/pulsar-core),
[`pulsar-proto`](https://github.com/PulsarDesk/pulsar-proto) and
[`relay`](https://github.com/PulsarDesk/relay).

## Tests

```bash
cargo test                        # pulsar-capture (default workspace member)
cargo check -p pulsar-tauri       # the app compiles against the engine
bun run test:unit                 # UI components (Vitest)
bun run check                     # svelte-check
```

Engine tests live in `pulsar-core`.

## Releases

On every push to `main`, `release.yml` uses semantic-release to work out the next version
from Conventional Commits. It then tags and publishes a GitHub Release; it never commits
back to the repo. Signed installers for Windows, macOS and Linux (x64 + ARM64) and the
auto-update manifest are built only when you set:

- the repository variable `ENABLE_DESKTOP_RELEASE=true`
- the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
