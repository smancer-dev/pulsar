# Pulsar desktop app

Cross-platform (Windows/macOS/Linux) remote-desktop + game-streaming app. See
`../CLAUDE.md` for the product and architecture.

**Stack:** **Tauri 2** shell · **SvelteKit (Svelte 5)** UI · **Rust** core
(`crates/pulsar-core`). One app is both the *client* (connect to others) and the
*host* (share this machine).

> `cargo` here is a rustup proxy that honors `../rust-toolchain.toml`, so it
> resolves to current stable automatically (run `rustup update stable` once if you
> hit an edition-2024 error). Use **`bun run tauri dev`** to run the app — `cargo
> tauri` would need a separately-installed `cargo-tauri`.

> **Package manager: bun, not npm** (project-wide — see the root `CLAUDE.md`).
> The Tauri config's `beforeDevCommand`/`beforeBuildCommand` call `bun run …`.

## Layout

```
desktop-app/
  crates/pulsar-core/   # the performant core (lib, fully unit + e2e tested)
    src/config.rs       #   configurable relay + network mode, persisted
    src/crypto.rs       #   X25519 + ChaCha20-Poly1305 E2E
    src/connection.rs   #   Node: register→ID→P2P hole-punch→relay fallback
    src/input.rs        #   controllers: DS3/4/5/Xbox/standard + virtual pad
    src/media.rs        #   capture→encode→transport→decode→present pipeline
    tests/e2e.rs        #   relay + 2 nodes: P2P, relay-only, relay-down survival
    tests/streaming.rs  #   frames streamed over the encrypted session
  src-tauri/            # Tauri commands bridging UI ↔ pulsar-core
    src/lib.rs          #   get/set_config, go_online, connect, controllers, …
    tauri.conf.json     #   frameless 1200×780 window
  src/                  # SvelteKit frontend
    routes/+page.svelte #   app shell: frameless chrome + sidebar + screen router
    lib/api.ts          #   Tauri invoke bridge (+ browser mock for `vite dev`)
    lib/screens/*.svelte#   Home, Devices, Settings, Connecting, Session, Gaming
```

## Core concepts

- **`Node`** (`connection.rs`) is the heart: `register()` gets the relay-assigned
  ID; `connect()` does the rendezvous + hole-punch and returns a `Session` whose
  `transport()` is `Direct` or `Relay`. `Session::send/recv` are encrypted.
- **Configurable relay**: `Config.relay` (host:port) + `Config.network_mode`
  (`auto` / `p2p-only` / `relay-only`) are user-editable in Settings → Ağ and
  persisted to the app config dir.
- **UI ↔ core**: `lib/api.ts` calls Tauri commands when running in Tauri, and
  falls back to a deterministic mock otherwise, so `vite dev` + tests work without
  the native shell.

## Run & build

```bash
bun install
bun run tauri dev      # run the real app (Rust + webview). NOT `cargo tauri` —
                       # that needs a separately-installed cargo-tauri binary;
                       # we ship the JS CLI via @tauri-apps/cli.
bun run tauri build    # package installers (bundles ffmpeg — see below)

bun run dev            # UI only in a browser (uses the mock)
bun run build          # static SPA → build/ (Tauri frontendDist)
```

> **Bundled ffmpeg:** the host captures+encodes the screen with ffmpeg, which is
> **shipped inside the app** (no user install, works offline). The binary lives at
> `src-tauri/resources/ffmpeg[.exe]` — git-ignored and fetched per-platform by
> `scripts/fetch-ffmpeg.mjs` (run it once before `tauri build`; CI runs it
> automatically). At runtime `ffmpeg_bin()` (in `src-tauri/src/lib.rs`) resolves the
> bundled copy first, falling back to a system `ffmpeg` on PATH.

> Plain `cargo` here is a rustup proxy that honors `../rust-toolchain.toml`, so it
> already resolves to stable (≥1.85). If you ever see an edition-2024 error, run
> `rustup update stable` once.

## Test

```bash
# Rust core (fast, headless): connection, crypto, controllers, media, e2e
cargo test -p pulsar-core
# UI components
bun run test:unit
# Tauri bridge compiles against the core
cargo check -p pulsar-tauri
```

## Screen capture (host side)

The host picks a capture method by platform/session:

- **X11 / Windows / macOS** — ffmpeg (`pipeline.rs`): `x11grab` / `gdigrab` /
  `avfoundation` → HW encode (NVENC via `prime-run`, VAAPI, QSV, VideoToolbox) or
  `libx264`, sent as MPEG-TS over UDP to the client's `ffplay`.
- **Wayland (KDE/GNOME)** — `capture.rs`: the **XDG ScreenCast portal** (`ashpd`)
  → a PipeWire node fed to **GStreamer** (`pipewiresrc ! queue leaky=downstream !
  x264enc(zerolatency,bframes=0) ! rtph264pay ! udpsink`). `x11grab` of rootless
  Xwayland is **always black**, so this is required. First connect shows the share
  dialog; a restore token (in `AppState`) skips it after. No gst HW-encoder plugins
  here → **software x264** — the leaky queue drops stale frames so latency stays
  bounded when the CPU can't keep up (fps drops instead of lag growing).
  **Input injection is via uinput** (`input::DesktopInput`: absolute pointer +
  keyboard), NOT the RemoteDesktop portal — that portal's `Start` hangs with no
  dialog on this KDE. uinput works on Wayland/X11 (kernel-level); needs the user
  in the `input` group.

Both paths emit **RTP/H.264 over UDP** to the connecting client. `is_wayland()`
(in `capture.rs`) selects between them; `lib.rs`'s `on_stream` branches on it.

## Client video — embedded WebCodecs (no separate window)

The remote screen renders **inside the app**, not a separate `ffplay` window:

- `src-tauri/src/viewer.rs` runs a local **UDP→WebSocket relay**: it binds an
  ephemeral UDP port (where the host streams RTP), and re-broadcasts each datagram
  over a loopback WebSocket. `start_remote_play` returns that `ws_port`.
- `src/lib/h264.ts` + `Session.svelte`: the webview opens the WebSocket, runs an
  RTP/H.264 depacketizer (single-NAL / STAP-A / FU-A; derives the `avc1.*` codec
  string from the SPS), and decodes each access unit with **WebCodecs
  `VideoDecoder`** onto a `<canvas>`. Low latency, hardware-accelerated.
- Why not WebRTC: this WebKitGTK is **compiled without WebRTC** (`RTCPeerConnection`
  is undefined even with `enable-webrtc`). WebCodecs gives equivalent low latency.
  Verified: a real RTP/H.264 stream decoded 60/60 frames, 0 errors, in the webview.

## Windows drivers: keyboard capture + virtual gamepad

Two host/client features need kernel drivers on Windows; both are **bundled and
auto-installed** so the user never installs anything by hand (the GPLv3 build is
license-clear to redistribute them — see below).

- **Keyboard capture under ASTER** (`src-tauri/src/kbdhook.rs`). The client must
  capture OS-reserved keys (Win, Alt+Tab, Ctrl+Esc) to forward + suppress them.
  `WH_KEYBOARD_LL` works on normal machines but **ASTER multiseat injects physical
  keys below the LL-hook chain**, so it's bypassed there. We load the
  **Interception** driver's `interception.dll` at runtime (via `libloading`) and
  capture below the hook layer (proven to see ASTER's physical keys). It's the
  primary path when the driver is present; otherwise we fall back to
  `WH_KEYBOARD_LL`. Set-1 scancodes → evdev via `scancode_to_evdev`; the
  Ctrl+Alt+Shift leave combo + suppress/forward logic is shared (`handle_key`) with
  the hook path. Interception is **LGPL-3.0 for non-commercial use with explicit
  redistribution rights for the driver+installer** — fine for GPLv3 Pulsar; a
  commercial Pulsar would need Interception's commercial license.
- **Virtual gamepad** (`pulsar-core::input`, `vigem_backend`). The host replays the
  client's controller via **ViGEmBus** (Xbox 360 emulation, read through XInput by
  every game), using the pure-Rust `vigem-client`. Linux uses uinput; macOS + ARM
  Windows fall back to a recording stub. Buttons map through `xinput_buttons`
  (XInput bitmask) — pure + unit-tested.

**Driver bundling / auto-install:** `interception.dll` is shipped next to the exe
(loaded at runtime). The **NSIS installer** runs the Interception + ViGEmBus
silent installers during Pulsar's install (it already elevates) and shows a
**"restart required"** notice (kernel drivers need one reboot). Driver payloads are
fetched by `scripts/fetch-drivers.mjs` (like `fetch-ffmpeg.mjs`) into
`src-tauri/resources/` so a dev clone + CI build needs no manual step.

## Audio streaming (host → client)

A second Opus/RTP stream runs **parallel to the video** (not over the JSON control
channel, which would bloat with PCM): the host captures its system audio, encodes
**Opus**, and sends **RTP** to a second UDP port. `viewer.rs` relays it over a second
loopback WebSocket; `src/lib/opus-audio.ts` decodes with WebCodecs `AudioDecoder` and
plays via WebAudio (degrades to silent video if the webview lacks audio WebCodecs).

Capture has two paths:

- **Windows (default): WASAPI loopback** of the *default render endpoint*
  (`audio::run_loopback_capture` — `IAudioClient` + `AUDCLNT_STREAMFLAGS_LOOPBACK`),
  the OBS/Sunshine approach. It taps whatever is playing on the host's output, so it
  **works with no `virtual-audio-capturer` / Stereo Mix device installed** (the old
  dshow default silently produced nothing on machines lacking that device). A Rust
  thread writes the endpoint's raw mix PCM into an ffmpeg reading `pipe:0`
  (`spawn_loopback_audio` in `src-tauri/src/lib.rs`); ffmpeg does the Opus/RTP encode
  (`audio::opus_rtp_output`, shared with the dshow path). Silence is filled so the
  audio timeline tracks wall-clock and never drifts ahead of the video.
- **dshow / Pulse `.monitor` / AVFoundation** (`audio::audio_command`): used on
  Linux/macOS, and on Windows when the user names a specific capture device in
  Settings (`Config::audio_input` non-empty → `audio_loopback()` is false).

Two `Config` toggles drive it: `transmit_audio` (send host→client) and
`mute_host_audio` (silence the host's local output via Core Audio on Windows /
`pactl` on Linux — `audio::set_host_muted`). **Game mode forces both on**
(`AudioSettings::policy`, unit-tested) so a remote game's sound moves entirely to
the player; the host is un-muted on session teardown.

## Stable device ID (identity persistence)

The relay assigns the 9-digit ID, but it now **maps pubkey → id** (`by_pubkey` in
`relay/src/lib.rs`) so a returning device keeps the same ID. The client persists
its X25519 identity per-user via `Identity::load_or_create` (in
`<app_config_dir>/identity.key`), passed to `Node::bind_with_identity`. Result: the
ID is **stable across restarts** and **distinct per OS user** (ASTER seats keep
separate IDs). A per-session single-instance guard (Windows `Local\` named mutex)
stops a second Pulsar per user while still allowing one per seat/user.

## Two usage modes (product direction — keep consistent everywhere)

Pulsar is ONE app with **two mode-aware personalities**, chosen at connect time
(`startConnect(target, mode: 'remote' | 'game')`). The mode drives **menu content,
overlay content, the look, and the encode profile**:

| | **Remote Desktop** (AnyDesk/RustDesk) | **Game Streaming** (Moonlight/Parsec) |
| - | - | - |
| Focus | general remote control + management | lowest latency, gaming |
| Menu | **full**: resolution/quality · codec/encoder · **file transfer · clipboard · multi-monitor** · chat · mic · reverse-direction · settings | **slim, game-only**: codec · bitrate (Mbit) · **fps** · resolution · quality/perf · encoder/decoder · controllers · end. **NO file/clipboard/mic/multi-monitor** (irrelevant in-game) |
| Overlay | thin info strip (connection/transport) | perf HUD (latency/fps/bitrate) + controller status + leave-combo hint |
| Look | neutral/general | gaming (cyan accent, minimal, immersive — `data-gaming`) |
| Encode | quality-focused | low-latency (already mode-aware on the host) |

Established earlier with the maintainer: **entering game streaming makes the whole
app gaming-focused; remote desktop makes it general remote-control-focused.**

## CLI / headless start (kiosk / appliance — esp. Orange Pi)

`pulsar --connect <id|ip> [--connect-pw <pw>]` auto-connects on launch — **splash
shows, but NO home screen**; it goes straight into the connection (headless-style;
already wired via `AppState.auto_connect` + `+page.svelte` onMount). Today this starts
in **remote** mode. PLANNED CLI surface:
- a **mode** flag so the CLI can start a **game** session (default is **remote**);
- for game mode, a **target app** to launch — **default is "Desktop"**. *Desktop is
  always present and is NOT deletable* (established earlier — every host always exposes
  a "Desktop" entry to stream the whole desktop).

## Gaming overlay (in-session, game mode)

The advanced overlay is **hidden by default and opened with a key combo** (so it never
clutters gameplay). When open it shows a **rich, game-focused menu** — encoder/decoder
selection, codec, **fps**, quality/performance, **bitrate (Mbit)**, etc. — and
explicitly **NOT** file manager / mic / clipboard (those are remote-desktop-only).
While the overlay is open the **video may pause/freeze and resume after** — acceptable;
the priority is **Moonlight-class low latency + performance** during play. The overlay
may even be *injected* onto the video to display it **as long as it costs no
performance**.

## Linux / RK3588 (Orange Pi 5) renderer — the reality (IMPORTANT)

On Linux the webview path is NOT viable for video and the in-app overlay differs from
Windows — keep this straight:

- **WebKitGTK can't hardware-decode** the stream (no usable WebCodecs HW path on RK3588;
  it would software-decode + glitch). So on Linux the video MUST be **native**:
  **mpv with `--hwdec` → `h264_rkmpp`/`hevc_rkmpp`** (zero-copy EGL), the same decode
  Moonlight uses. Default is embedded **`mpv --wid=<app window XID>`** (renders inside the
  Pulsar window). `--untimed --no-correct-pts --video-sync=desync` are load-bearing for
  low latency — RTP has no usable PTS, so without them mpv paces to a made-up 30fps
  (adds latency). `native_view.rs::spawn_mpv`.
- **The webview can NOT be composited transparently over the native video** on this
  GTK3/WebKitGTK stack (the reparented wry webview renders OPAQUE black over a GtkGLArea
  even with `set_background_color(0)` + an RGBA window visual — a fresh webview works,
  wry's drops alpha). Proven with a magenta-clear probe. **So do NOT build a "rich webview
  UI over the live video" path on Linux.** Follow **Moonlight's model** (verified in
  `_ref/moonlight-qt`): one native renderer for *both* windowed and fullscreen (just a
  window flag), and the overlay is **drawn natively on the video** (`OverlayManager` →
  `renderOverlay()`), NOT a rich UI composite. The rich menu is a separate screen you
  toggle to (video pauses) — exactly the gaming-overlay-via-combo above.
  - A libmpv-render-API → `GtkGLArea` "single surface" is implemented but gated **opt-in**
    behind `PULSAR_SINGLE_SURFACE=1` (it renders rkmpp + controls, but the webview-overlay
    is blocked by the transparency wall above).
  - **Windows/macOS keep the in-webview WebCodecs path** (WebView2/WKWebView HW-decode), so
    there the rich menu CAN live over the video. The two-mode UX is shared; only the
    video+overlay *mechanism* is platform-specific.
- **Control on Linux** = `kbdhook.rs` Linux module: grabs local keyboard+mouse via
  **evdev (EVIOCGRAB)**, hotplug-aware, forwards `InputEvent` to the host (analog of the
  Windows Interception path). **Leave combo: Ctrl+Shift+Q** (F12 is unreliable — media-mode
  keyboards like Logitech MX Keys don't emit `KEY_F12`).
- **Rendezvous gotcha:** a host that serves LAN clients must register with the relay using
  its **LAN IP**, not `127.0.0.1` — otherwise the relay hands clients a loopback address and
  P2P/auth never completes (`Config.relay`).

## What's complete vs. scaffolded

**Complete + tested:** relay protocol, register→ID→P2P→relay-fallback (incl.
relay-down survival), **client heartbeats** (every 10s; the relay evicts devices
after `DEVICE_TTL`=30s, so without these `connect()` fails with `BadToken`), E2E
crypto, configurable relay, **one-time-password auth** on the session (host issues
a `7yf2-qk`-style password on `go_online`; client must send a matching `Auth`
first or the host refuses — `unattended_access` skips it), controller detection +
state normalization + virtual-pad trait (real uinput backend on Linux), the
streaming pipeline (X11 ffmpeg + Wayland portal/GStreamer; gst→ffmpeg loopback
verified), the service protocol over the encrypted session (auth, list/launch
games, start stream, controller input), the SvelteKit UI, and the Tauri bridge.

Remote control: the client captures mouse/keyboard over the video canvas
(`Session.svelte`, evdev keymap in `keymap.ts`) → `input_*` Tauri commands →
`InputEvent` over the held session → host injects (Wayland RemoteDesktop portal;
gamepad via uinput). Input forwarding is unified through one `mpsc<InputEvent>`
per remote-play session.

**Scaffolded / known gaps:** Windows/macOS `VirtualGamepad` + mouse/keyboard
injection (Linux is real — uinput pad + Wayland portal; X11/Win/Mac input
injection not yet wired); HW-accelerated encode on the Wayland path (needs gst
VAAPI/NVENC plugins, else software x264); media-over-the-session for symmetric-NAT
(today media is a direct UDP RTP flow to the peer addr, fine for LAN/cone-NAT/
relay-direct); the session toolbar's clipboard/file/chat/mic buttons are disabled
placeholders (only End works).
