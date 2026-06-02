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
