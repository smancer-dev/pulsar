# Pulsar desktop app

Cross-platform (Windows/macOS/Linux) remote-desktop + game-streaming app. See
`../CLAUDE.md` (umbrella dir) for the product and multi-repo architecture.

**Stack:** **Tauri 2** shell · **SvelteKit (Svelte 5)** UI · **Rust**. One app is
both the *client* (connect to others) and the *host* (share this machine).

> **The shared engine is NOT in this repo.** `pulsar-core`, `pulsar-proto` and
> `pulsar-relay` are **git dependencies** (`Cargo.toml` `[workspace.dependencies]`),
> plus `[patch.crates-io] vigem-client = { git = …/pulsar-core }`. After pushing
> pulsar-core/proto, bump this repo with `cargo update`. A sibling checkout lives at
> `../pulsar-core/` but is *not* a path dep.

> This repo has its **own `rust-toolchain.toml`** (stable; some deps need
> edition 2024 — run `rustup update stable` if you hit an edition error). Use
> **`bun run tauri dev`** to run the app — `cargo tauri` would need a
> separately-installed `cargo-tauri`. **Package manager: bun, never npm.**

## Layout

```
pulsar-desktop/
  crates/pulsar-capture/  # Windows-only native capture+encode: DXGI Desktop
                          #   Duplication (+ WGC) → NVENC SDK → hand-rolled RTP
                          #   (the Sunshine technique, no ffmpeg). Also
                          #   nvenc_codecs() real-silicon probe via nvEncodeAPI64.dll.
  crates/pulsar-render/   # native renderer PROCESS for ALL platforms: Linux
                          #   rkmpp/EGL, Windows Media Foundation + D3D11 zero-copy
                          #   decode, egui in-session overlay, `--probe` decoder probe.
  crates/pulsar-setup/    # egui branded Windows bootstrapper installer
                          #   (Discord-style, embedded payload, silent/uninstall).
  src-tauri/              # `pulsar-tauri` crate (bin name `pulsar`)
    src/lib.rs            #   command registration / glue
    src/host.rs,host/     #   host side; handlers.rs = capture/encode/audio spawn
    src/caps.rs           #   startup capability probe (see below)
    src/process.rs        #   ffmpeg spawning, ffmpeg_bin(), encoder validation
    src/viewer.rs         #   UDP→WebSocket relay (now audio-only path to webview)
    src/native_view*/     #   embedded native video (spawn.rs: spawn_mpv & co.)
    src/kbdhook.rs,kbdhook/ # OS-key capture: Windows Interception/LL-hook, Linux evdev
    src/relay_mode.rs     #   `pulsar --relay …` — the app can run as a relay
    src/{auth,avatar,connections,files,fs_browse,audio_io,io_cmds,…}.rs
    tauri.conf.json       #   frameless 1200×780 window; updater plugin (minisign)
  src/                    # SvelteKit frontend
    routes/+page.svelte   #   shell; routes/page/{Tabs,SplitPicker,Chrome,Sidebar,
                          #     GamingShell,UpdateModal,…}.svelte — multi-session
                          #     tabs + split view
    lib/api.ts            #   bridge, split into api.{commands,dom,events,invoke,types}.ts
    lib/i18n/             #   4-language lazy i18n: en/tr/ru/kk (+ src-tauri/lang/)
    lib/screens/          #   Home, Devices, Settings/(4 tabs), Connecting, Session/,
                          #     Gaming/(6 comps), Games/, Approve, Connections,
                          #     FilesWindow, HostChat
  scripts/                # fetch-ffmpeg.mjs, fetch-drivers.mjs, fetch-player.mjs,
                          #   gen-update-manifest.mjs, bump-version.mjs
```

## Core concepts

- **`Node`** (pulsar-core `connection`) is the heart: `register()` gets the
  relay-assigned ID; `connect()` does rendezvous + hole-punch and returns a
  `Session` (`Direct` or `Relay` transport, E2E encrypted).
- **Configurable relay**: `Config.relay` + `Config.network_mode`
  (`auto`/`p2p-only`/`relay-only`), user-editable in Settings → Ağ, persisted.
- **UI ↔ core**: `lib/api.ts` calls Tauri commands in Tauri, falls back to a
  deterministic mock so `vite dev` + tests work without the native shell.

## Run & build

```bash
bun install
bun run tauri dev      # the real app (NOT `cargo tauri`)
bun run tauri build    # installers (bundles ffmpeg — see below)
bun run dev            # UI only in a browser (mock)
bun run build          # static SPA → build/
```

> **Bundled ffmpeg:** host capture/encode fallback ships ffmpeg inside the app at
> `src-tauri/resources/ffmpeg[.exe]` — git-ignored, fetched per-platform by
> `scripts/fetch-ffmpeg.mjs` (CI runs it automatically). At runtime `ffmpeg_bin()`
> (`src-tauri/src/process.rs`) resolves the bundled copy first, then PATH.

## Test

```bash
cargo check -p pulsar-tauri     # Tauri bridge compiles against the engine
cargo test -p pulsar-capture    # workspace default-member tests
bun run test:unit               # UI components (vitest)
bun run check                   # svelte-check
# engine tests live in ../pulsar-core (its own repo): cargo test there
```

## Host capture/encode paths

- **Windows, NVENC present** — `crates/pulsar-capture`: DXGI Desktop Duplication
  (or WGC) → NVENC SDK directly → hand-rolled RTP packetizer. No ffmpeg in the hot
  path; falls back to ffmpeg if init fails.
- **Linux X11** — the ffmpeg **libraries in-process** (`src-tauri/src/host/libav.rs`:
  x11grab → libavfilter → libavcodec x264/x265/SVT-AV1/NVENC → libavformat RTP; live
  bitrate, forced IDR on request, short-GOP recovery; `PULSAR_LIBAV_HOST=0` disables).
  Falls back to the ffmpeg CLI below when it cannot start or for VA-API/Vulkan/HDR/4:4:4.
- **Windows (non-NVENC) / macOS** (and the Linux fallback) — ffmpeg CLI (`ddagrab`/
  `gdigrab`/`avfoundation`/`x11grab` → HW encode or libx264 → RTP). Arg builders are pure
  functions in pulsar-core `pipeline/` (unit-tested); spawning is here (`process.rs`, `host/`).
- **Wayland (KDE/GNOME)** — pulsar-core `capture` (Linux-only module): XDG
  ScreenCast portal (`ashpd`) → PipeWire → **in-process** GStreamer
  (`pipewiresrc ! queue leaky=downstream ! x264enc(zerolatency) name=venc ! rtph264pay`;
  bitrate/keyframe/short-GOP applied live, no restart).
  `x11grab` of rootless Xwayland is **always black**, so this is required. Restore
  token skips the share dialog after first connect. Software x264 (no gst HW
  plugins); leaky queue bounds latency. **Input injection is uinput**
  (`DesktopInput`), NOT the RemoteDesktop portal (its `Start` hangs on this KDE);
  needs the user in the `input` group.

All paths emit **RTP/H.264(+HEVC/AV1) over UDP**.

## Capability probe (`src-tauri/src/caps.rs`) — read before touching codecs

Moonlight-model startup probe, re-run every launch, no persistence:

- Windows NVENC: `pulsar_capture::nvenc_codecs()` opens a real NVENC session and
  enumerates codec GUIDs — **instead of** the ffmpeg one-frame probe, which targets
  the display GPU (wrong on hybrid laptops) and lists encoders on GPUs with no
  NVENC silicon (GP108/MX). Advertising HEVC there → client SDP says HEVC, host
  degrades wire to H.264 → **permanent black screen**. Families with zero
  validated codecs are dropped.
- Decoders: `pulsar-render --probe` decodes real keyframes headlessly (replaced
  the old hardcoded "MF decodes X" assumption).
- `DecoderCap.incompatible_with` blacklists bad host-encoder × client-decoder
  combos (e.g. rkmpp-HEVC × nvenc on RK3588).
- `process.rs` keeps a Sunshine-style one-frame `probe_encoder_codec` +
  `resolve_codec_validated` with an unreliable-probe guard for hybrid laptops.

## Client video — native renderer everywhere (webview video is GONE)

The old in-webview WebCodecs video path (`h264.ts`) was **removed — too slow**.
Video renders **natively on every platform** via the separate `pulsar-render`
process: Linux rkmpp/EGL zero-copy, Windows MF+D3D11 zero-copy (DXVA), macOS
mpv/VideoToolbox. The egui overlay draws on the video natively. **Only Opus
audio still decodes in the webview** (`src/lib/opus-audio.ts`, fed by
`viewer.rs`'s UDP→WebSocket relay). Don't resurrect a webview video path.

## Windows drivers: keyboard capture + virtual gamepad

Both are **bundled and auto-installed** (GPLv3 build is license-clear):

- **Keyboard capture under ASTER** (`src-tauri/src/kbdhook.rs` + `kbdhook/imp.rs`).
  `WH_KEYBOARD_LL` misses ASTER-multiseat physical keys, so we load the
  **Interception** driver's `interception.dll` at runtime (`libloading`) and
  capture below the hook layer; LL-hook is the fallback. Set-1 scancodes → evdev
  via `scancode_to_evdev`; leave-combo + suppress/forward logic shared
  (`handle_key`). Interception: LGPL-3.0 non-commercial w/ redistribution — fine
  for GPLv3; commercial Pulsar would need their commercial license.
- **Virtual gamepad** (pulsar-core `input`, `vigem.rs`): **ViGEmBus** (X360 + DS4
  via the vendored `vigem-client` fork). Linux uses uinput; macOS is a no-op stub.

**Bundling:** `interception.dll` ships next to the exe; the NSIS installer runs
the Interception + ViGEmBus silent installers (elevated) with a "restart
required" notice. Payloads fetched by `scripts/fetch-drivers.mjs` into
`src-tauri/resources/`.

## Audio streaming (host → client)

A second Opus/RTP stream parallel to video. Implementation lives in
**pulsar-core `audio/`**; this repo spawns it from `src-tauri/src/host/handlers.rs`
(`spawn_loopback_audio`, `run_loopback_capture_tracking`, `opus_rtp_output_layout`,
`audio_command_layout`, `set_host_muted`).

- **Windows default: WASAPI loopback** of the default render endpoint (the
  OBS/Sunshine approach — no virtual-audio-capturer/Stereo Mix needed). Rust
  thread pipes PCM into ffmpeg `pipe:0`; ffmpeg does Opus/RTP. Silence-filled so
  the timeline tracks wall-clock.
- **dshow / Pulse `.monitor` / AVFoundation** on Linux/macOS, or when the user
  names a device (`Config::audio_input` non-empty → `audio_loopback()` false).

Config toggles: `transmit_audio`, `mute_host_audio`. **Game mode forces
`transmit` on but NOT `mute_host`** (`AudioSettings::policy`, unit-tested):
on common codecs the WASAPI loopback tap is post-mute/post-volume, so a capture
opened *into* a muted endpoint latches **pure silence** (verified live, −91 dBFS,
fixed 2026-06-13). Never force-mute the captured endpoint; user-set `mute_host`
is still honored (mid-session muting is safe). Windows mute = endpoint **mute
flag**, NOT volume-0. Un-muted on teardown. Client-side Pulse buffer capped
(~80 ms `-buffer_duration` in `native_view/spawn.rs::spawn_native_audio`).
Mic/voice-call side-channel PCM lives in `src-tauri/src/audio_io.rs`.

## Stable device ID

The relay maps **pubkey → id** (`by_pubkey` in the `relay` repo), so a returning
device keeps its 9-digit ID. Client persists its X25519 identity per-user
(`Identity::load_or_create` → `<app_config_dir>/identity.key`,
`Node::bind_with_identity`). ID is stable across restarts, distinct per OS user
(ASTER seats keep separate IDs). Per-user single-instance guard (Windows `Local\`
named mutex).

## Two usage modes (product direction — keep consistent everywhere)

Pulsar is ONE app with **two mode-aware personalities**, chosen at connect time
(`startConnect(target, mode: 'remote' | 'game')`). Mode drives **menu content,
overlay content, the look, and the encode profile**:

| | **Remote Desktop** (AnyDesk/RustDesk) | **Game Streaming** (Moonlight/Parsec) |
| - | - | - |
| Focus | general remote control + management | lowest latency, gaming |
| Menu | **full**: resolution/quality · codec/encoder · **file transfer · clipboard · multi-monitor** · chat · mic · reverse-direction · settings | **slim, game-only**: codec · bitrate (Mbit) · **fps** · resolution · quality/perf · encoder/decoder · controllers · end. **NO file/clipboard/mic/multi-monitor** |
| Overlay | thin info strip (connection/transport) | perf HUD (latency/fps/bitrate) + controller status + leave-combo hint |
| Look | neutral/general | gaming (cyan accent, immersive — `data-gaming`) |
| Encode | quality-focused | low-latency (mode-aware on the host) |

Established with the maintainer: **entering game streaming makes the whole app
gaming-focused; remote desktop makes it general remote-control-focused.**

## CLI / headless start (kiosk / appliance — esp. Orange Pi)

`pulsar --connect <id|ip> [--connect-pw <pw>] [--mode game]` auto-connects on
launch (splash, no home screen; `AppState.auto_connect`). Game mode's target app
defaults to **"Desktop"** — *always present, NOT deletable* (every host exposes a
"Desktop" entry). `pulsar --relay --session-rate 10mbit …` runs the app as a
relay (`relay_mode.rs`, same `pulsar_relay::Limits` flags as the relay repo).

## Gaming overlay (in-session, game mode)

Hidden by default, opened with a key combo. Drawn **natively by pulsar-render
(egui)** on the video — controller-navigable; rich game-only menu
(encoder/decoder, codec, fps, quality/perf, bitrate), **NOT**
file/mic/clipboard. Video may pause while the rich menu screen is open —
acceptable; priority is Moonlight-class latency during play.

## Linux / RK3588 (Orange Pi 5) renderer — the reality (IMPORTANT)

- **WebKitGTK can't hardware-decode** the stream. Video MUST be native:
  historically `mpv --wid=<XID>` with `--hwdec` → `h264_rkmpp`/`hevc_rkmpp`
  (`--untimed --no-correct-pts --video-sync=desync` are load-bearing — RTP has no
  usable PTS), now the `pulsar-render` rkmpp/EGL path. `native_view/spawn.rs`.
- **The webview can NOT be composited transparently over native video** on this
  GTK3/WebKitGTK stack (wry webview renders opaque black over GtkGLArea; proven
  with a magenta-clear probe). **Do NOT build a "rich webview UI over live
  video" path on Linux.** Moonlight's model: one native renderer, overlay drawn
  natively; the rich menu is a separate screen you toggle to. (A libmpv-render →
  GtkGLArea single-surface exists behind `PULSAR_SINGLE_SURFACE=1`, still blocked
  by the transparency wall.)
- **Control on Linux** = `kbdhook/linux.rs`: grabs local keyboard+mouse via
  **evdev (EVIOCGRAB)**, hotplug-aware. **Leave combo: Ctrl+Shift+Q** (F12
  unreliable on media-mode keyboards).
- **Rendezvous gotcha:** a host serving LAN clients must register with its
  **LAN IP**, not `127.0.0.1`, or the relay hands clients a loopback addr.
- **Auto-update → the appliance runs the AppImage** (`$APPIMAGE` self-replace;
  FUSE required). A `--connect` kiosk checks updates on boot before connecting
  (8 s timeout). Raw `--no-bundle` binary can't self-update. Updater UX in-app:
  `@tauri-apps/plugin-updater` + minisign key in `tauri.conf.json`,
  `src/lib/updater.ts`, `routes/page/UpdateModal.svelte`,
  `scripts/gen-update-manifest.mjs` (consent-based: badge, changelog, progress).

## What's complete vs. scaffolded

**Complete + tested:** relay protocol + heartbeats (10 s; relay evicts at 30 s),
register→ID→P2P→relay-fallback + relay-down survival, E2E crypto, one-time-password
auth (`unattended_access` skips), Approve/view-only consent flow, controller
detection/normalization + virtual pads (uinput + ViGEmBus), capture/encode paths
above, native render + overlay, **side channels — clipboard, file transfer (+
file browser `fs_browse.rs` + Files window), chat, mic, voice call**
(`Session/sidechannels.svelte.ts`), multi-session **tabs + split view**
(`sessions.svelte.ts`), 4-language i18n, LAN discovery UI (`Home/LanDevices`),
Games library + folder scan, gamepad-driven UI nav (`gamepadNav.svelte.ts`),
auto-updater, relay mode, VNC-mode client path (`io_cmds.rs`), the SvelteKit UI
and the Tauri bridge.

**Adaptive streaming (2026-09-03, local, awaiting the maintainer's test):** the shared
controller is `pulsar_core::adapt` (design + status in `../pulsar-core/docs/adaptive-streaming.md`).
Here: `src-tauri/src/play/hold.rs` measures (loss, 500 ms pings, per-frame arrivals, FEC
parity) and actuates (`StreamReq` bitrate / resolution / fps / `loss_recovery` / `fec`);
`adapt_memory.rs` remembers the last good rate per peer; the host (`host.rs`, `host/handlers.rs`)
sizes FEC parity from the client's `Stats` and encodes with intra-refresh / short GOP on request;
`pulsar-render` holds the last good frame on an unrepaired loss (`hold`), follows the RTT-derived
reorder wait (`maxdelay`), and on Windows/macOS has a reorder buffer. Test with
`scripts/netem.sh` + `scripts/validate-log.mjs`. Do not push behaviour changes untested.

**Scaffolded / known gaps:** macOS virtual gamepad (no-op stub); HW encode on
the Wayland/gst path (software x264 only); media-over-the-session for
symmetric-NAT (media is a direct UDP RTP flow today — fine for LAN/cone-NAT/
relay-direct). Windows↔Windows blackscreen debugging is ACTIVE WIP — see the
dirty-tree work around `caps.rs`/`pulsar-capture`/`win/decode.rs` and the
project memory note before touching encoder/decoder selection.
