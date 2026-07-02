# Pulsar Mobile — Implementation Plan & Collision-Free Lane Contract

This is the single source of truth for bringing `desktop-app/mobile/` toward
feature parity with the desktop app. It turns `GAP-REPORT.json`'s 40-item roadmap
into a wave-by-wave plan that **parallel sonnet coder agents** execute. The hard
rule: **within a wave, no file is owned by two lanes**, so every lane in a wave
runs in parallel safely. Waves run sequentially.

Everything below was verified against real code (commit `0ebc000`). API names,
wire variants, and struct fields are only used here if they actually exist —
anything that needs a `pulsar-core` addition is **FLAGGED**.

---

## 0. Ground truth (verified APIs — do not invent beyond these)

**Mobile crate** (`desktop-app/mobile/src/`): `lib.rs` (the ONLY place that runs
`tauri::generate_handler!` + `.manage()` + `.plugin()`), `client.rs`
(`connect_host`, `send_pointer`, `send_button`, `InputSenders`), `host.rs`
(`go_online`, `open_a11y_settings`, `a11y_enabled`), `rtp.rs` (`Depacketizer`,
`Codec`, `rtp_payload`), `main.rs`.

**pulsar-core re-exports** (verified in `crates/pulsar-core/src/lib.rs`):
- `pulsar_core::proto` = the `pulsar-proto` crate. `proto::DeviceId(pub u32)`, `DeviceId::parse(&str) -> Option<DeviceId>`.
- `Identity` (`crypto.rs`): `Identity::generate()`, **`Identity::load_or_create(path) -> Identity`** (already exists — use it for W1-identity).
- `Node` (`connection/node.rs`): `bind_with_identity(local, relay, mode, name, identity) -> io::Result<Arc<Node>>`, `register(&Arc<Self>) -> Result<DeviceId>`, `connect(&Arc<Self>, DeviceId) -> Result<Session>`, **`connect_direct(&Arc<Self>, peer_addr: SocketAddr, peer_pubkey: Option<PublicKey>) -> Result<Session>`** (use for IP-direct, W2-ipconnect), `next_incoming(&self) -> Option<Session>`.
- `Session` (`connection/session.rs`): **`transport(&self) -> Transport`** (use for real P2P/relay feedback), `send(&self, &[u8])`, `sender(&self) -> SessionSender`, `recv(&mut self) -> Option<Vec<u8>>`.
- `Transport` enum: `Direct` | `Relay`.
- `SessionSender`: `Clone`, `send(&self, &[u8])`.
- `NetworkMode`: `Auto | P2pOnly | RelayOnly` (serde kebab-case: `auto`/`p2p-only`/`relay-only`).
- `Config` (`config.rs`): full struct (relay, network_mode, device_name, language, unattended_access, connect_password, …) + `Config::load(path)`, `Config::save(&self, path)`, `Config::relay_is_valid(&self) -> bool`, `DEFAULT_RELAY = "127.0.0.1:21116"`, `Language::{Tr,En}` (serde lowercase).
- `QualityPref`: `Latency | Balanced | Quality` (serde lowercase).

**Service protocol** (`crates/pulsar-core/src/service/`, all `pub use`d from `service`):
- Client helpers: `authenticate`, `query_stream_caps`, `query_windows`, `request_games`, `request_launch`, `request_stream`, `send_input`, **`send_input_via(&SessionSender, &InputEvent)`** (used by send_pointer/send_button today), `send_keepalive`, `send_bye`, **`send_data(&Session, &DataMsg)`** (takes `&Session`, NOT `&SessionSender` — see FLAG below), `decode_data(&[u8]) -> Option<DataMsg>`, `is_pong`.
- Auth race: `accept`, `reject`, `need_password`, `recv_auth`, `recv_client_auth`, `recv_host_auth`, `send_auth`, `AuthOutcome::{Accepted,Denied,NeedPassword}`, `ClientAuth::{Password,Keepalive,Gone}`, `HostAuth::{Ok,Denied,NeedPassword,Gone,Other}`.
- Host: `serve`, `serve_with`, `DataHandlers`, `gen_password()`.
- Media: `service::media::{parse, frame, TAG_VIDEO, TAG_AUDIO, FEAT_MOS}`.
- Wire types: `DataMsg`, `DisplayInfo`, `FsEntry`, `GameInfo`, `InputEvent`, `QualityPref`, `StreamCaps`, `StreamReq`, `WindowInfo`.

**`InputEvent` variants (verified, `wire.rs`)**: `Gamepad(GamepadState)`, `GamepadSlot{slot,kind,target,state}`, `GamepadDisconnect{slot}`, `PointerMotion{x,y}`, `PointerRelative{dx,dy}`, `PointerButton{button,down}`, `Scroll{dx,dy}`, `Key{code,down}`, `Char(char)`. (All `Copy`.)

**`DataMsg` variants (verified, `wire.rs`)**: `Clipboard(String)`, `Chat(String)`, `FileBegin{id,name,size,chunks}`, `FileChunk{id,index,data}`, `FileEnd{id}`, `Audio(Vec<u8>)`, `AudioEnd`, `Stats(String)`, `ReverseRequest(String)`, `DisplayRotation(u32)`, `MediaNack(Vec<u16>)`, `Avatar(Vec<u8>)`, `FsList{path}`, `FsEntries{path,entries}`, `FsGet{path}`, `PeerName(String)`, `PeerId(String)`, `CursorPos{x,y}`, `CursorShape{…}`, `CursorHidden`, `Rumble{slot,large,small}`. (Adding a new variant requires mirroring it in the private `DataMsgWire` shadow + `From` impl + the roundtrip test — but **we add NO new DataMsg variants**; everything mobile needs already exists.)

**`StreamReq` fields (verified)**: `port,codec,encoder,width,height,fps,audio_port,transmit_audio,mute_host,game_mode,bitrate_kbps,quality,hdr,yuv444,decode_codecs,media_over_session,cursor_external,display_idx,window_hwnd,adapt,audio_layout`. **`StreamCaps`**: `codecs,encoders,features,displays:Vec<DisplayInfo>`. **`DisplayInfo`**: `idx,name,width,height,primary,modes`.

**Native plugin** (`crates/tauri-plugin-pulsar-video/`):
- Rust→native bridge methods on `PulsarVideo<R>` (`mobile.rs`, called from `client.rs`/`host.rs` via `app.pulsar_video()`): `attach`, `detach`, `play_test`, `start_stream(mime,slot)`, `feed_au(&[u8],slot)`, `feed_audio(&[u8])`, `stop_stream(slot)`, `start_host(...)`, `stop_host`, `host_gesture(x1,y1,x2,y2)`, `open_a11y_settings`, `a11y_enabled`. Each calls `run_mobile_plugin("<cmdName>", argsStruct)`.
- JS→native plugin commands (the only ones invokable as `plugin:pulsar-video|<cmd>` from the webview): **only `attach`, `detach`, `play_test`** — these are the ones in `lib.rs`'s `generate_handler!` + `permissions/default.toml`. `index.html:366` already calls `plugin:pulsar-video|detach`. **Any NEW JS-invokable plugin command (setAudioMuted, setAspect, mic) must be added in 3 places: `commands.rs` (`#[command]` wrapper), `lib.rs` `generate_handler!`, AND `permissions/default.toml`.** Native-only methods (called from Rust, not JS) only need `mobile.rs` + `desktop.rs` no-op + the Kotlin `@Command`.
- Kotlin (`PulsarVideoPlugin.kt`): `@Command fun <name>(invoke: Invoke)` + `invoke.parseArgs(XArgs::class.java)`. Pane slot cap is `coerceIn(0,1)` (W5 must bump). `applyAspect(of: MediaFormat)` does min-scale fit today. `AudioTrack` under `audioLock`.

**Tauri config**: `withGlobalTauri: true`, `frontendDist: "ui"`, `csp: null`. So the
webview accesses `window.__TAURI__.core.invoke` / `window.__TAURI__.event.listen`
directly — **no bundler, ES modules only** (`<script type=module>`).

**Build/deploy**: rust target `x86_64-linux-android` (installed); app installs on
`emulator-5554` (verified connected).

### FLAGGED pulsar-core additions (small, additive, do in the "core" sub-lane of the owning wave)

1. **`send_data_via(&SessionSender, &DataMsg)`** in `crates/pulsar-core/src/service/client.rs` (+ `pub use` in `service.rs`). REASON: `send_data` takes `&Session`, owned by the read loop; the mobile side-channel send commands (`send_clipboard`/`send_chat`/`fs_list`/`fs_get`/mic `DataMsg::Audio`) run from Tauri commands that only hold a cloned `SessionSender`, and `enc()`/`Msg::Data` are private to the service module. Mirror `send_input_via` exactly. **Owner: W1 rust lane (W1-datachan).** Single small fn, no wire change.

That is the ONLY required core change. Everything else is mobile-crate wiring +
webview UI. (W5-lan-presence's `lan_devices` would need pulsar-core LAN discovery
plumbing; it is the lowest-priority item (prio 5) and is scoped as best-effort —
if `Discovery` is not trivially callable from the mobile crate, that lane ships a
stub that returns `[]` and the UI degrades gracefully. Flag, not a blocker.)

---

## 1. Architecture decisions

### 1.1 Webview module system (no bundler, `withGlobalTauri`)

- `index.html` becomes a **shell**: `<head>` token/font/component CSS links, the
  static DOM skeleton (header, nav tabs, tab `<section>`s, in-session bar
  container, overlay + sheet mount points), and **one** `<script type=module
  src="js/app.js">`. No inline logic, no inline `<style>` tokens.
- All JS is **native ES modules** loaded relatively (`./js/...`), no transpile.
  `js/tauri.js` is the single chokepoint over `window.__TAURI__` (the "TAURI yok"
  guard lives there once).

**Self-registration so feature modules are NOT shared-file collision points.**
Three registries live in `app.js` and never need editing when a feature is added:

1. **Screen registry** — `js/router.js` exposes `registerScreen({id, navIcon, navLabelKey, mount, onShow})`. Each `screens/*.js` calls `registerScreen(...)` at import time; `app.js` imports the screen modules (the import list is the ONLY thing that grows, and only in waves that add a whole new screen — see lane ownership). The router renders the bottom-nav from the registry and calls `mount()/onShow()`.
2. **Overlay-card registry** — `js/session/overlay.js` exposes `registerCard({id, modes:['remote'|'game'], section:'stream'|'display'|'audio'|'tools'|'gauges'|'controllers', order, mount})`. Each session feature panel (`quality.js`, `display.js`, `audio.js`, `sidechannels.js`, `files.js`, `hud.js`, `gamepad.js`) calls `registerCard(...)` at import time. The overlay renders only the cards whose `modes` includes the active personality — so the **remote-only vs game-only split is enforced centrally** by the overlay, not by each panel. Panels never edit `overlay.js`.
3. **Event bus** — there is no custom bus; we use the **Tauri event stream** (`listen('<name>', cb)` via `tauri.js`) for Rust→JS, and a tiny `EventTarget` singleton exported from `app.js` (`bus.emit/on`) for JS→JS (e.g. `mode-changed`, `session-started`, `session-ended`). Modules subscribe; nobody edits a central switch.

**Net effect:** after W1, every later feature is a brand-new file that
*registers itself*. The only shared files that ever get a one-line touch are
`app.js` (import the new screen module) and—rarely—`index.html` (a new DOM mount
point). Those are assigned to **exactly one owner per wave** (see §5).

### 1.2 Two personalities + remote-vs-game feature split (central enforcement)

- `body[data-mode]` is `remote` (indigo) or `game` (cyan). `router.js` is the
  **sole writer** of `data-mode` (and `body.in-session`). The `--brand*` token
  swap is purely CSS (`[data-mode='game']` block in `tokens.css`).
- The **mode is chosen at connect time** and stored on the active session
  (`session.js` registry), then pushed to `body[data-mode]` by the router. A mode
  is never toggled mid-session.
- **Remote-only features** (file transfer, clipboard, chat, mic, multi-monitor,
  reverse-direction) are gated in ONE place: `overlay.js` filters
  `registerCard({modes})` so a `modes:['remote']` card is *never mounted* in game
  mode. Game-only features (on-screen gamepad, gauges) use `modes:['game']`.
  No feature module checks the mode itself for visibility — it just declares its
  `modes`. (Mic/keyboard also self-guard their command calls as defense-in-depth,
  but the visual gate is central.)

---

## 2. Tauri command + event interface contract (THE source of truth)

All commands are `#[tauri::command]` in the **mobile crate** unless noted as a
plugin command. JSON arg field names are exactly as written (Tauri serdes camelCase
keys from JS by default; we keep snake-less single words or explicit names — match
these exactly in `invoke('cmd', { ... })`). Every command returns `Result<T,String>`
(JS `try/catch`). Events are `app.emit("<name>", payload)` consumed by
`listen("<name>", e => e.payload)`.

### 2.1 `identity.rs` (NEW) — no commands, no events

Exports `fn identity_path<R:Runtime>(app:&AppHandle<R>) -> PathBuf` (app data dir +
`identity.key`) and `fn load_identity<R:Runtime>(app) -> Identity` wrapping
`Identity::load_or_create(path)`. Consumed by `client.rs::connect_host` and
`host.rs::go_online` (replace both `Identity::generate()` calls). **W1-identity.**

### 2.2 `config.rs` (NEW)

| Command | Args (JSON) | Returns |
| --- | --- | --- |
| `get_config` | `{}` | `Config` JSON (`{relay, networkMode, deviceName, language, unattendedAccess, codecPref, qualityPref, ... }` — a minimal serde struct mirroring the fields mobile uses; serialize with the same string vocab as core: networkMode `auto`/`p2p-only`/`relay-only`, language `tr`/`en`, codec `auto`/`h265`/`h264`) |
| `set_config` | the same shape (partial allowed) | `Config` (the merged, persisted result) |

Persists to `<app_data_dir>/config.json` via `pulsar_core::Config::{load,save}` (or
a thin local serde struct if Config has desktop-only fields that don't matter on
mobile — prefer reusing `Config` for vocab parity). No events. **W1-config.**
`connect_host`/`go_online` read these when their args are omitted/empty.

### 2.3 `datachan.rs` (NEW) — side-channel decode + send

The read loop (in `client.rs`) calls `datachan::route(&app, slot, &bytes)` after
`media::parse` returns `None`; `route` runs `service::decode_data(&bytes)` and
emits the matching event. Send commands look up the per-slot `SessionSender`
(from `InputSenders` or a parallel `SessionSenders` map) and call the FLAGGED
`send_data_via`.

**Events emitted (Rust → JS):**

| Event | Payload | From |
| --- | --- | --- |
| `clipboard-in` | `{slot:u8, text:String}` | `DataMsg::Clipboard` (W4-clipboard) |
| `chat-msg` | `{slot:u8, text:String}` | `DataMsg::Chat` (W4-chat) |
| `fs-entries` | `{slot:u8, path:String, entries:[{name,dir,size}]}` | `DataMsg::FsEntries` (W4-files) |
| `file-begin` | `{slot:u8, id:u32, name:String, size:u64, chunks:u32}` | `DataMsg::FileBegin` (W4-files) |
| `file-progress` | `{slot:u8, id:u32, received:u64, total:u64}` | periodic during reassembly (W4-files) |
| `file-recv` | `{slot:u8, id:u32, name:String, savedPath:String}` | on `DataMsg::FileEnd` complete (W4-files) |
| `rumble` | `{slot:u8, large:u8, small:u8}` | `DataMsg::Rumble` (W4-gamepad-physical) |

**Commands (JS → Rust):**

| Command | Args | Returns | Item |
| --- | --- | --- | --- |
| `send_clipboard` | `{slot:u8, text:String}` | `()` | W4-clipboard |
| `send_chat` | `{slot:u8, text:String}` | `()` | W4-chat |
| `fs_list` | `{slot:u8, path:String}` | `()` (reply via `fs-entries`) | W4-files |
| `fs_get` | `{slot:u8, path:String}` | `()` (reply via `file-*`) | W4-files |
| `send_file` | `{slot:u8, name:String, bytes:base64 String}` OR a path picked via dialog | `{id:u32}` | W4-files (upload; chunks via `FileBegin/Chunk/End`, CHUNK=2048) |

W1-datachan lands only the **decode/route scaffold + `send_data_via` plumbing +
the empty event dispatch**; the individual `send_*` commands land in their W4 lanes
(which own `datachan.rs` that wave).

### 2.4 `input_cmds.rs` (NEW) — extended input (mirrors send_pointer/send_button)

| Command | Args | Returns | Item |
| --- | --- | --- | --- |
| `send_scroll` | `{slot:u8, dx:f64, dy:f64}` | `()` | W2-inputcmds → `InputEvent::Scroll` |
| `send_key` | `{slot:u8, code:u32, down:bool}` | `()` | W2-inputcmds → `InputEvent::Key` |
| `send_char` | `{slot:u8, ch:String}` (first char) | `()` | W2-inputcmds → `InputEvent::Char` |
| `send_pointer_rel` | `{slot:u8, dx:f64, dy:f64}` | `()` | W2-inputcmds → `InputEvent::PointerRelative` |
| `send_gamepad` | `{slot:u8, buttons:u16, lx:i16, ly:i16, rx:i16, ry:i16, lt:u8, rt:u8}` | `()` | W4-gamepad → `InputEvent::GamepadSlot{slot, kind:Xbox, target:Auto, state:GamepadState{...}}` |
| `send_gamepad_disconnect` | `{slot:u8}` | `()` | W4-gamepad → `InputEvent::GamepadDisconnect{slot}` |

`send_scroll/send_key/send_char/send_pointer_rel` land in W2-inputcmds; the two
`send_gamepad*` land in W4-gamepad-onscreen (which owns `input_cmds.rs` that wave).
`send_button` already accepts a `button` index (0/1/2) — reuse for right/middle.

### 2.5 `session_cmds.rs` (NEW) — teardown + live restream controls

| Command | Args | Returns | Item |
| --- | --- | --- | --- |
| `end_session` | `{slot:u8}` | `()` (cancels read loop, drops Session/Node, `stop_stream(slot)`) | W2-teardown |
| `submit_password` | `{slot:u8, password:String}` | `()` (feeds the auth race oneshot) | W3-client-authprompt |
| `set_play_codec` | `{slot:u8, codec:String}` | `()` | W3-quality (restream) |
| `set_play_bitrate` | `{slot:u8, kbps:u32}` | `()` | W3-quality |
| `set_play_fps` | `{slot:u8, fps:u32}` | `()` | W3-quality |
| `set_play_resolution` | `{slot:u8, width:u32, height:u32}` | `()` | W3-quality |
| `set_play_quality` | `{slot:u8, pref:String}` (`latency`/`balanced`/`quality`) | `()` | W3-quality |
| `set_play_encoder` | `{slot:u8, encoder:String}` | `()` | W3-quality |
| `set_play_monitor` | `{slot:u8, displayIdx:u32}` | `()` (debounced ~400ms) | W4-multimonitor |

Mechanism: a per-slot **restream `mpsc<StreamReq>`** that the `client.rs` read loop
`select!`s on; on a new `StreamReq` it re-calls `request_stream` and re-arms
`pulsar_video().start_stream(mime, slot)` for the new SPS. `end_session` holds a
per-slot cancellation `Notify`/`AbortHandle` stored beside `InputSenders`. The
restream channel + cancel registry live in `session_cmds.rs` (managed state),
read-loop integration is an edit to `client.rs` by the wave's **rust lane**.

### 2.6 `client.rs` (existing) — connect, phases, timeout, stats

`connect_host` gains params and emits phase/stat events.

**Updated `connect_host` args**: `{relay:String, target:String /*id OR ip[:port]*/, password:String, slot:u8, netmode:String, name:String, mode:String /*"remote"|"game"*/, codec:String, fps:u32, bitrateKbps:u32, width:u32, height:u32, quality:String}`. (W2-ipconnect renames `id`→`target` and branches `DeviceId::parse` vs `SocketAddr`/`connect_direct`. W2-gamemode threads `mode`+quality into `StreamReq`.) Returns `ConnectResult{ok, myId, codec, mos, transport:String /*"direct"|"relay"*/, detail}`.

**Events emitted by `client.rs`:**

| Event | Payload | Item |
| --- | --- | --- |
| `conn-phase` | `{slot:u8, phase:String /*"reaching"\|"transport"\|"auth"\|"awaiting"\|"preparing"*/, transport?:String /*"direct"\|"relay"*/}` | W2-connecting (emit after `connect()` reads `transport()`, after `authenticate`, after `request_stream`) |
| `auth-prompt` | `{slot:u8, peer:String}` | W3-client-authprompt (on `HostAuth::NeedPassword`) |
| `play-ended` | `{slot:u8, reason:String}` | W2-teardown (read loop exits: `Ok(None)`/error/cancel) |
| `play-firstframe` | `{slot:u8}` | W3-hud (first decoded AU) |
| `play-stall` | `{slot:u8, stalled:bool}` | W3-hud (no `TAG_VIDEO` ~2s; off on resume) |
| `play-stats` | `{slot:u8, fps:f32, mbps:f32, transport:String}` | W3-hud (computed ~1s over existing frame/byte counters) |
| `host-displays` | `{slot:u8, displays:[{idx,name,width,height,primary}]}` | W4-multimonitor (capture `caps.displays`, currently discarded at `client.rs:135`) |

**Timeout** (W2-timeout-errors): wrap `connect`+`authenticate`+`request_stream` in
`tokio::time::timeout` (~45s overall, +30s post-auth) returning a distinct error
string `connect-timed-out` so JS maps it to `connErr.timeout`.

`client.rs` is heavily contended across waves — it is edited by the **rust lane of
each wave** (one lane only) so its edits never split.

### 2.7 `host.rs` (existing) — lifecycle, approval, OTP, peers

| Command | Args | Returns | Item |
| --- | --- | --- | --- |
| `go_online` | `{relay:String, name:String, netmode:String}` (read Config when empty) | `OnlineResult{ok, id:u32, password:String}` (re-runnable) | W3-host-lifecycle |
| `go_offline` | `{}` | `()` (abort accept loop, drop Node, `stop_host`) | W3-host-lifecycle |
| `new_password` | `{}` | `{password:String}` (rotate OTP) | W3-host-lifecycle |
| `respond_request` | `{reqId:u32, allow:bool}` | `()` (feeds the per-incoming approval oneshot) | W3-host-lifecycle |
| `disconnect_session` | `{sid:u32}` | `()` (kick a connected peer) | W3-host-lifecycle |
| `host_codecs` | `{}` | `{codecs:[String]}` (probe MediaCodecList) | W5-host-codecprobe |

**Events emitted by `host.rs`:**

| Event | Payload | Item |
| --- | --- | --- |
| `host-password` | `{password:String}` | W3 (on rotate / per successful auth) |
| `session-request` | `{reqId:u32, peer:String, hasPassword:bool}` | W3 (replace silent auto-accept; await `respond_request` OR correct password, 30s auto-deny) |
| `host-peer-connected` | `{sid:u32, peer:String, name?:String}` | W3 (fill the no-op `on_connect` at `host.rs:158`) |
| `host-peer-disconnected` | `{sid:u32}` | W3 |

OTP moves to `Arc<Mutex<String>>`; accept-loop `JoinHandle` + `Node` stored in a
managed `HostState`. Throttle (per-peer lockout, global auto-rotate) ported from
desktop `src-tauri/src/auth.rs`. **Owner: the W3 host lane (sole writer of
`host.rs` that wave).**

### 2.8 Plugin commands (new JS-invokable — add in commands.rs + lib.rs handler + permissions/default.toml + mobile.rs + desktop.rs no-op + Kotlin @Command)

| Command (`plugin:pulsar-video|<name>`) | Args | Item |
| --- | --- | --- |
| `setAudioMuted` | `{muted:bool}` | W3-audio-mute (Kotlin `audioTrack.setVolume(0/gain)` under `audioLock`) |
| `setAspect` | `{slot:u8, mode:String /*"fit"\|"fill"\|"stretch"*/}` | W3-aspect (Kotlin branch in `applyAspect`) |
| `setOrientation` | `{landscape:bool}` | W3-aspect (`setRequestedOrientation`) |
| `micStart` / `micStop` | `{}` | W4-mic (Kotlin `AudioRecord` 48k mono s16le; returns PCM via a plugin channel the Rust `mic_start` pulls — see §2.9) |
| `decoderState` (event, not cmd) | plugin emits `play-vstats {slot, decodeMs}` / `decoder-error {slot}` | W3-hud / W5-decoder-recovery |

The default permission set in `permissions/default.toml` must be expanded to
`allow-set-audio-muted`, `allow-set-aspect`, `allow-set-orientation`,
`allow-mic-start`, `allow-mic-stop` as each lands (the owning lane edits the toml).
**`permissions/default.toml` is owned by exactly one lane per wave** (the lane that
adds a plugin command that wave).

### 2.9 `mic` — `client.rs` side (W4-mic)

| Command | Args | Returns |
| --- | --- | --- |
| `mic_start` | `{slot:u8}` | `()` (pull PCM from the plugin, send `DataMsg::Audio` ~20ms frames via `send_data_via`) |
| `mic_stop` | `{slot:u8}` | `()` (send `DataMsg::AudioEnd`) |

Requires `RECORD_AUDIO` in `AndroidManifest.xml` + runtime request.

### 2.10 `reverse_play` (W4-reverse)

| Command | Args | Returns |
| --- | --- | --- |
| `reverse_play` | `{slot:u8}` | `()` (send `DataMsg::ReverseRequest(myId)`; the local `go_online` host path receives the inbound) |

---

## 3. JS module API

Path is under `desktop-app/mobile/ui/`. "Listens" = Tauri/`bus` events; "Calls" =
Tauri commands. **SHARED files have exactly one owner per wave** (marked ★OWNER).

| Module | Exports | Listens | Calls |
| --- | --- | --- | --- |
| `index.html` ★ (shell) | — DOM skeleton + `<script type=module src=js/app.js>` | — | — |
| `css/tokens.css` | CSS vars (copy of design `tokens.css`, `--text-on-accent` restored, `[data-mode=game]` swap, `--nav-h`, 16px `--input-size`) + delta comment | — | — |
| `css/components.css` ★ | `.btn .seg .field .input .item .row-list .card .bar` + `.sheet`/`.modal`/`.fab` base | — | — |
| `js/app.js` ★ | `bus` (EventTarget singleton: `bus.emit/on`); boots i18n+config, mounts router, imports every screen/session module (the import list grows here) | — | — |
| `js/tauri.js` ★ | `invoke(cmd,args)`, `listen(name,cb)`, `hasTauri`, `clipboard`, `share` — the single `window.__TAURI__` guard | — | — |
| `js/i18n.js` | `t(key,vars)` ({var} interp, tr→en→key fallback per desktop `i18n.svelte.ts`), `setLang(l)`, `lang`; tr/en flat catalogs (subset of desktop `i18n.tr.ts`/`i18n.en.ts`); persists `pulsar.lang.v1` | — | (config) |
| `js/router.js` ★ | `registerScreen(spec)`, `show(id)`, `setMode(m)` (sole writer of `data-mode`), `enterSession()/exitSession()` (sole writer of `body.in-session`) | `bus:mode-changed` | — |
| `js/store/peers.js` | port of desktop `peers.svelte.ts` (key `pulsar.peers.v1`): `normalizeId`,`fmtPeerId`,`savedPeers`,`historyPeers`,`gameHistoryPeers`,`recordConnection`,`addPeer`,`updatePeer`,`removePeer`,`removeFromHistory`,`toggleFav`,`clearHistory` | — | — |
| `js/store/config.js` | `getConfig()`,`setConfig(patch)`,`relay()`,`netmode()`,`deviceName()`,`codec()`,`quality()` (localStorage cache over `get_config`/`set_config`) | — | `get_config`,`set_config` |
| `js/screens/connect.js` | `registerScreen` (Bağlan); `doConnect(target,slot,mode)`; ID/IP validation (port `connectTarget.ts`: `isAddr`,`fmtTarget`,`ipRe`,`canConnectTarget`); pre-connect quality presets; friendly error map | `conn-phase`,`play-ended` | `connect_host` |
| `js/screens/connecting.js` | `registerScreen`-less full-screen overlay; `start(target,mode)`,`cancel()`; phased step list + 12s slow hint | `conn-phase` | `end_session` (cancel) |
| `js/screens/devices.js` | `registerScreen` (Cihazlar/Geçmiş); list + tap-connect + long-press sheet + add sheet | `lan-presence` (W5) | (peers), `doConnect`, `lan_devices` (W5) |
| `js/screens/host.js` | `registerScreen` (Cihazım); online/offline toggle, ID/OTP copy+share+rotate, peers list+kick, approval sheet, unattended toggle, a11y entry | `host-password`,`session-request`,`host-peer-connected`,`host-peer-disconnected` | `go_online`,`go_offline`,`new_password`,`respond_request`,`disconnect_session`,`open_a11y_settings`,`a11y_enabled` |
| `js/screens/settings.js` | `registerScreen` (Ayarlar); relay (validated), netmode seg, name, codec, lang TR/EN seg, About/GPLv3 | — | `set_config`, `setLang` |
| `js/session/session.js` | `registry` (array `{slot,id,codec,mode,label}`), `startSession(...)`,`endSession(slot)`,`setActivePane(slot)`; auth-prompt sheet (W3) | `play-ended`,`play-stall`,`play-firstframe`,`auth-prompt` | `end_session`,`submit_password` |
| `js/session/input.js` | `mount(slot)`; touch→pointer engine (rAF-coalesce, tap/drag 8px, dbl-tap, long-press=right, 2-finger tap=middle, 2-finger scroll, trackpad mode) | `bus:gamepad-active` (gate off) | `send_pointer`,`send_button`,`send_scroll`,`send_pointer_rel` |
| `js/session/keyboard.js` | `registerCard({modes:['remote']})`; hidden-input soft kb, special-key/modifier bar, evdev table (port `keymap.ts`) | — | `send_char`,`send_key` |
| `js/session/overlay.js` ★ (after W3) | `registerCard(spec)`, `open()/close()`, the mode-aware dock; mounts cards filtered by `modes` | `bus:session-started` | — |
| `js/session/hud.js` | `registerCard({modes:['remote','game'],section:'gauges'})`; fps/mbps/RTT strip, stall/firstframe states | `play-stats`,`play-vstats`,`play-stall`,`play-firstframe` | — |
| `js/session/quality.js` | `registerCard({section:'stream'})` + pre-connect presets export | — | `set_play_*` |
| `js/session/display.js` | `registerCard({modes:['remote'],section:'display'})`; fit/fill/stretch + orientation + monitor picker | `host-displays` | `setAspect`,`setOrientation`(plugin),`set_play_monitor` |
| `js/session/audio.js` | `registerCard({section:'audio'})`; mute toggle (both modes) + mic toggle (remote) | `bus:session-bg` | `setAudioMuted`(plugin),`mic_start`,`mic_stop` |
| `js/session/sidechannels.js` | `registerCard({modes:['remote'],section:'tools'})`; clipboard + chat | `clipboard-in`,`chat-msg` | `send_clipboard`,`send_chat` |
| `js/session/files.js` | `registerCard({modes:['remote'],section:'tools'})`; remote browser + transfer queue | `fs-entries`,`file-begin`,`file-progress`,`file-recv` | `fs_list`,`fs_get`,`send_file` |
| `js/session/gamepad.js` | `registerCard({modes:['game'],section:'controllers'})`; on-screen pad + physical poll; emits `bus:gamepad-active` | `rumble` | `send_gamepad`,`send_gamepad_disconnect` |
| `js/session/split.js` | `registerCard({section:'tools'})`-ish layout sheet; per-pane target picker | — | `connect_host`(2nd slot),`set_play_resolution` |

---

## 4. CSS / design contract

Source of truth: `design/project/assets/tokens.css` (oklch palette, indigo accent
`oklch(0.555 0.205 272)`, cyan `oklch(0.62 0.15 215)`, radii, shadows, type scale,
`.btn`/`.btn-primary`/`.btn-ghost`/`.mono` atoms) and `design/project/Pulsar
App.html` (the 6-screen prototype) + `design/project/Pulsar - Design Direction.html`.

**`css/tokens.css` (mobile additions over the source, documented in a header
comment):**
- Restore `--text-on-accent` (mobile inline currently calls it `--on-accent` — rename back to match the source-of-truth).
- Keep the `[data-mode='game'] { --brand: var(--cyan); … }` swap (already in inline).
- Mobile-only tokens: `--nav-h: 64px`; `--input-size: 16px` (never below 16px → iOS/Android won't zoom on focus); `--touch-min: 44px` (min tap target); `--safe-top`/`--safe-bottom` via `env(safe-area-inset-*)`.
- `--ease-out: cubic-bezier(0.16,1,0.3,1)` (matches inline `--ease`).

**`css/components.css` class names** (extracted verbatim from the inline `<style>`,
then extended): `.app .top .wm .pill .mark .scroll .tab .title .sub .card .seg
.field .input .btn .btn-primary .btn-ghost .msg .row-list .item .empty .setting
.sect-label .idbig .pw-chip .statusline nav.bottom .bar .icon-btn` PLUS new touch
atoms: `.sheet` (bottom-sheet, `visualViewport`-aware), `.modal`, `.fab`,
`.overlay-dock`, `.overlay-card`, `.pill-row` (scrollable seg), `.kbd-bar`,
`.gamepad-layer`, `.hud-strip`.

**Touch sizing rules:** every interactive element ≥ `--touch-min` (44px); inputs ≥
`--input-size` (16px) font; the in-session bar + overlay respect
`env(safe-area-inset-bottom)`; the on-screen keyboard/chat composer use
`visualViewport` so they sit above the soft keyboard over the transparent surface.

**designTask → module realization** (which lane needs a design):

| designTask | Realized by module(s) | Roadmap items |
| --- | --- | --- |
| DT-connecting | `screens/connecting.js` | W2-connecting |
| DT-overlay | `session/overlay.js`, `components.css` | W3-overlay |
| DT-perf-hud | `session/hud.js` | W3-hud |
| DT-touch-input | `session/input.js` | W3-input-touch |
| DT-keyboard | `session/keyboard.js` | W3-keyboard |
| DT-quality-sheet | `session/quality.js` | W3-quality |
| DT-display | `session/display.js` | W3-aspect, W4-multimonitor |
| DT-audio | `session/audio.js` | W3-audio-mute, W4-mic |
| DT-devices | `screens/devices.js` | W4-devices-screen |
| DT-host | `screens/host.js` | W3-host-lifecycle |
| DT-client-authprompt | `session/session.js` | W3-client-authprompt |
| DT-clipboard-chat | `session/sidechannels.js` | W4-clipboard, W4-chat |
| DT-files | `session/files.js` | W4-files |
| DT-onscreen-gamepad | `session/gamepad.js` | W4-gamepad-onscreen |
| DT-mic | `session/audio.js` | W4-mic |
| DT-multisession-split | `session/session.js`, `session/split.js` | W5-multisession, W5-split |
| DT-language | `screens/settings.js` | W5-language-toggle |

---

## 5. Wave + lane execution plan

**HARD RULES.** (1) Within a wave, every file appears in **at most one lane** →
all lanes in a wave run concurrently with zero write conflicts. (2) `lib.rs` (mobile
crate) and `client.rs` are each edited by **at most one lane per wave** — the wave's
**rust lane**. (3) `app.js`, `router.js`, `tauri.js`, `i18n.js`, `index.html`,
`components.css`, `overlay.js`, `permissions/default.toml` each have **one owner per
wave**. (4) Waves are sequential; a later lane may depend on an earlier wave's
output. (5) A coder agent gets ONLY its lane's owned-file list + this contract.

> Note on `lib.rs`/`client.rs` contention: many items touch them. Within a wave we
> fold all their edits into ONE rust lane so they never split. UI lanes that "need"
> a command just consume the contract in §2; they do not edit Rust.

### WAVE 1 — Foundation (5 lanes)

| Lane | Owned files (create/edit) | Roadmap items | Design? |
| --- | --- | --- | --- |
| **W1-rust** | `src/identity.rs` (new), `src/config.rs` (new), `src/datachan.rs` (new, scaffold), `src/client.rs`, `src/host.rs`, `src/lib.rs`, `crates/pulsar-core/src/service/client.rs`, `crates/pulsar-core/src/service.rs` | W1-identity, W1-config (rust half), W1-datachan | No |
| **W1-shell** | `ui/index.html`, `ui/css/tokens.css` (new), `ui/css/components.css` (new), `ui/js/app.js` (new), `ui/js/tauri.js` (new), `ui/js/router.js` (new) | W1-cleanup, W1-modsplit | No |
| **W1-i18n** | `ui/js/i18n.js` (new) | W1-i18n | No |
| **W1-config-js** | `ui/js/store/config.js` (new) | W1-config (JS half) | No |
| **W1-peers** | `ui/js/store/peers.js` (new) | W2-peers (pulled forward — pure, no deps but W1-modsplit; safe here as it owns a unique file) | No |

Briefs:
- **W1-rust**: Add `identity.rs` (`Identity::load_or_create(app_data/identity.key)`), use it in both `connect_host` and `go_online` (delete both `Identity::generate()`). Add `config.rs` (`get_config`/`set_config` over `pulsar_core::Config`), register in `lib.rs`. Add the FLAGGED `send_data_via(&SessionSender,&DataMsg)` to `service/client.rs` + `pub use` it. Add `datachan.rs` with `route(&app,slot,&bytes)` calling `decode_data` and dispatching the event scaffold (no per-feature `send_*` yet); wire `route` into the `client.rs` read loop after `media::parse` returns `None`; `use tauri::Emitter`. Have `connect_host`/`go_online` fall back to Config when args empty. Register everything in `lib.rs`'s `generate_handler!`.
- **W1-shell**: Delete the TEMP diagnostic (`index.html:414-424`) and the auto-self-connect; set default relay to `pulsar_core::DEFAULT_RELAY`/product default (not the LAN dev IP). Split the monolith: extract `:root` → `tokens.css` (rename `--on-accent`→`--text-on-accent`, header delta comment), atoms → `components.css`, the inline `<script>` → `app.js` + `tauri.js` (invoke/listen guard) + `router.js` (bottom-nav + `data-mode`/`in-session` writer) + stub screen modules. **Move existing connect/host/settings/recents/touch logic verbatim into their modules first (no behavior change).** Export the `bus` EventTarget + the `registerScreen` registry. This unblocks every later parallel UI lane.
- **W1-i18n**: `i18n.js` per §3 (tr/en flat catalogs subset of desktop, `t()` with `{var}` interp + tr→en→key fallback, `pulsar.lang.v1`). (Consumes the shell's module skeleton; since it owns its own file it is collision-free even though it logically follows W1-modsplit — the shell lane exposes the import; if ordering is a concern, W1-i18n's file simply needs to exist for app.js to import — coordinate by having W1-shell import `./i18n.js` which W1-i18n creates.)
- **W1-config-js**: `store/config.js` per §3 over the W1-rust commands (localStorage cache fallback).
- **W1-peers**: `store/peers.js` per §3 (port desktop `peers.svelte.ts`, key `pulsar.peers.v1`).

> Intra-wave note: W1-i18n / W1-config-js / W1-peers create files that
> `app.js`/screens import. W1-shell writes the import statements as
> `import './i18n.js'` etc. (the files land in the same wave). No two lanes write
> the same file. If a coder finds a missing import target, it stubs the import,
> never edits another lane's file.

### WAVE 2 — Connect robustness + input plumbing (5 lanes)

| Lane | Owned files | Roadmap items | Design? |
| --- | --- | --- | --- |
| **W2-rust** | `src/client.rs`, `src/session_cmds.rs` (new), `src/input_cmds.rs` (new), `src/lib.rs` | W2-connecting (rust), W2-timeout-errors (rust), W2-ipconnect (rust), W2-teardown (rust), W2-inputcmds, W2-gamemode (rust) | No |
| **W2-connect** | `ui/js/screens/connect.js` | W2-timeout-errors (JS), W2-ipconnect (JS), W2-gamemode (JS) | No |
| **W2-connecting** | `ui/js/screens/connecting.js` (new) | W2-connecting (JS) | **Yes — DT-connecting** |
| **W2-session** | `ui/js/session/session.js` (new) | W2-teardown (JS) | **Yes — DT-teardown (reuses base session UX)** |
| **W2-shell-glue** | `ui/js/app.js`, `ui/js/router.js`, `ui/index.html` | wiring: register new screens, mount points for connecting overlay + session card; net-pill = real transport | No |

Briefs:
- **W2-rust**: In `connect_host` emit `conn-phase` (read `sess.transport()` after `connect()`; emit `transport` then `auth` then `preparing`). Add `tokio::time::timeout` wrappers (45s + 30s post-auth) → `connect-timed-out`. Rename `id`→`target`; branch `DeviceId::parse` vs `SocketAddr`/`connect_direct`. Thread `mode`+codec/fps/bitrate/res/quality into `StreamReq` (replace hardcoded `game_mode=false` at `:154` and `codec=caps.codecs.first()` at `:136`; honor pref when in caps; set `decode_codecs`+mime; `QualityPref::Latency` when game). Add `session_cmds.rs` (`end_session` with per-slot cancel `Notify`/`AbortHandle` + the restream `mpsc` scaffold) and `input_cmds.rs` (`send_scroll`/`send_key`/`send_char`/`send_pointer_rel`). `select!` the cancel + restream channel in the read loop; emit `play-ended{slot,reason}` on exit. Register all in `lib.rs`.
- **W2-connect**: Port `connectTarget.ts` (`isAddr`/`fmtTarget`/`ipRe`/`canConnectTarget`), stop forcing numeric-only stripping. Add the pre-connect mode toggle wiring + quality presets (Auto/Data-saver/Balanced/Performance). Port the friendly-error table (`connErr.*` from desktop i18n — relayDown/peerUnreachable/notOnline/p2pFailed/timeout/unreachable) through `t()`. Re-enable the connect button + clear in-session on reject. Read `data-mode`+config quality and pass into `connect_host`.
- **W2-connecting**: Build the full-screen phased connecting overlay (DT-connecting): target id + mode badge, pulse-ring, step list driven by `conn-phase` (reaching → P2P/relay transport → auth/awaiting → preparing), 12s slow-host hint, big Cancel → `end_session`. Indigo/cyan themed.
- **W2-session**: Build `session.js`: own `body.in-session` lifecycle via the router, the per-slot registry, listen `play-ended` → drop session, detach surface, show a "Bağlantı kesildi — Tekrar bağlan" card reusing `doConnect(lastId,lastSlot)`. Replace the detach-only `btn-end` to invoke `end_session` first.
- **W2-shell-glue**: Register `connect`/`connecting`/`session` with the router; add their DOM mount points to `index.html`; set the in-session net-pill from the real `transport` returned by `connect_host`.

### WAVE 3 — In-session overlay + live controls + host lifecycle (8 lanes)

| Lane | Owned files | Roadmap items | Design? |
| --- | --- | --- | --- |
| **W3-rust** | `src/client.rs`, `src/session_cmds.rs`, `src/lib.rs` | W3-hud (rust stats/stall/firstframe), W3-quality (rust restream cmds), W3-aspect (client.rs setAspect call), W3-client-authprompt (rust auth race + `submit_password`) | No |
| **W3-host** | `src/host.rs`, `ui/js/screens/host.js` (rewrite) | W3-host-lifecycle | **Yes — DT-host** |
| **W3-overlay** | `ui/js/session/overlay.js` (new), `ui/css/components.css` | W3-overlay | **Yes — DT-overlay** |
| **W3-input** | `ui/js/session/input.js` (new) | W3-input-touch | **Yes — DT-touch-input** |
| **W3-keyboard** | `ui/js/session/keyboard.js` (new) | W3-keyboard | **Yes — DT-keyboard** |
| **W3-hud-js** | `ui/js/session/hud.js` (new) | W3-hud (JS) | **Yes — DT-perf-hud** |
| **W3-quality-js** | `ui/js/session/quality.js` (new) | W3-quality (JS) | **Yes — DT-quality-sheet** |
| **W3-media-native** | `crates/tauri-plugin-pulsar-video/src/mobile.rs`, `.../desktop.rs`, `.../commands.rs`, `.../android/.../PulsarVideoPlugin.kt`, `.../permissions/default.toml`, `ui/js/session/audio.js` (new), `ui/js/session/display.js` (new) | W3-audio-mute, W3-aspect (native+JS) | **Yes — DT-audio, DT-display** |

> `client.rs`+`lib.rs`+`session_cmds.rs` are folded into **W3-rust** only.
> `host.rs` is **W3-host** only. `components.css` is **W3-overlay** only. The plugin
> Rust+Kotlin+toml are **W3-media-native** only (it owns the whole plugin surface
> this wave). `client.rs` calling `setAspect` is W3-rust (Rust side), while
> `display.js` invoking the plugin `setAspect` is W3-media-native — different files.

Briefs:
- **W3-rust**: read-loop fps/mbps over ~1s → `play-stats`; `play-firstframe` on first AU; `play-stall` after ~2s no `TAG_VIDEO` (off on resume). Implement `set_play_codec/bitrate/fps/resolution/quality/encoder` pushing a `StreamReq` onto the per-slot restream `mpsc`, re-`request_stream` + re-arm `start_stream` for the new SPS. Add a `client.rs` call to plugin `setAspect` on stream entry. Replace single-shot `authenticate()` with the desktop race loop (`recv_host_auth`; on `NeedPassword` emit `auth-prompt` and await a `submit_password` oneshot); add `pw_pending` map + `submit_password` command in `session_cmds.rs`.
- **W3-host**: full host lifecycle per §2.7 (go_offline, re-runnable go_online, OTP `Arc<Mutex>` + rotate + `host-password`, replace silent auto-accept with `session-request`/`respond_request` race + 30s auto-deny, `host-peer-connected/disconnected`, `disconnect_session`, unattended toggle, throttle port). Build `screens/host.js` (DT-host): online/offline toggle, ID+OTP copy/share/rotate, peers list+kick, approval sheet.
- **W3-overlay**: build the mode-aware bottom-sheet/dock + the `registerCard` registry (filters by `modes` — central remote/game enforcement), opened by status-pill/FAB; extend `components.css` with `.overlay-dock`/`.overlay-card`/`.fab`. This is the host surface all W3/W4 panels attach to.
- **W3-input**: touch→pointer engine (DT-touch-input) calling W2 input cmds; gate off when `bus:gamepad-active`.
- **W3-keyboard**: on-screen keyboard + special-key/modifier bar (remote-only card) per DT-keyboard, calling `send_char`/`send_key`; `registerCard({modes:['remote']})`.
- **W3-hud-js**: perf HUD card (DT-perf-hud) listening to `play-stats`/`play-vstats`/`play-stall`/`play-firstframe`.
- **W3-quality-js**: quality controls card + pre-connect presets export (DT-quality-sheet) calling `set_play_*`.
- **W3-media-native**: add plugin commands `setAudioMuted`/`setAspect`/`setOrientation` across `commands.rs`+`lib.rs handler`+`default.toml`+`mobile.rs`+`desktop.rs` no-op+Kotlin `@Command` (+ `applyAspect` fit/fill/stretch branch, `setRequestedOrientation`). Build `audio.js` (mute card, both modes) + `display.js` (fit/orientation card, remote-only). **This lane owns the whole plugin crate + the toml this wave** so no other lane touches it.

### WAVE 4 — Side channels, files, gamepad, mic, devices (8 lanes)

| Lane | Owned files | Roadmap items | Design? |
| --- | --- | --- | --- |
| **W4-rust-data** | `src/datachan.rs`, `src/lib.rs` | W4-clipboard (rust), W4-chat (rust), W4-files (rust: reassembler + `fs_list`/`fs_get`/`send_file`) | No |
| **W4-rust-client** | `src/client.rs`, `src/session_cmds.rs` | W4-multimonitor (rust: capture `caps.displays`→`host-displays`, `set_play_monitor`), W4-gamepad-physical (rust: `rumble` route), W4-reverse (rust: `reverse_play`) | No |
| **W4-rust-input** | `src/input_cmds.rs` | W4-gamepad-onscreen (rust: `send_gamepad`/`send_gamepad_disconnect`) | No |
| **W4-sidechannels** | `ui/js/session/sidechannels.js` (new), `ui/js/session/display.js` (extend: monitor picker) | W4-clipboard (JS), W4-chat (JS), W4-multimonitor (JS picker) | **Yes — DT-clipboard-chat, DT-display (picker)** |
| **W4-files-js** | `ui/js/session/files.js` (new), `mobile/Cargo.toml` | W4-files (JS + dialog dep) | **Yes — DT-files** |
| **W4-gamepad** | `ui/js/session/gamepad.js` (new) | W4-gamepad-onscreen (JS), W4-gamepad-physical (JS) | **Yes — DT-onscreen-gamepad** |
| **W4-devices** | `ui/js/screens/devices.js` (new), `ui/js/app.js` (register screen) | W4-devices-screen | **Yes — DT-devices** |
| **W4-mic** | `ui/js/session/audio.js`, `crates/.../src/mobile.rs`, `crates/.../src/desktop.rs`, `crates/.../src/commands.rs`, `crates/.../android/.../PulsarVideoPlugin.kt`, `crates/.../permissions/default.toml`, `gen/android/app/src/main/AndroidManifest.xml` | W4-mic (native plugin + JS + manifest; the `mic_start`/`mic_stop` Rust commands live in W4-rust-client by contract) | **Yes — DT-mic** |

> **Collision resolution for W4:** `client.rs` is touched by multimonitor, physical
> gamepad rumble, reverse, AND mic. To keep `client.rs` single-owner, ALL its W4
> edits go to **W4-rust-client** (multimonitor/rumble/reverse/mic-PCM-pull). The
> **W4-mic** lane therefore owns only the *native plugin + manifest + audio.js*
> (its `mic_start`/`mic_stop` Rust command lands in W4-rust-client, by contract).
> `datachan.rs` is **W4-rust-data** only. `input_cmds.rs` is **W4-rust-input** only.
> `app.js` is **W4-devices** only (the one screen registered this wave). `audio.js`
> is **W4-mic** only (it already exists from W3-media-native; W4-mic *edits* it to
> add the mic toggle — and since no other W4 lane touches audio.js, that's safe).
> `permissions/default.toml` + the plugin Kotlin/Rust are **W4-mic** only this wave.

Briefs:
- **W4-rust-data**: in `datachan.rs` add `send_clipboard`/`send_chat`/`fs_list`/`fs_get`/`send_file` (via `send_data_via`), port the `hold.rs` file reassembler (FileBegin/Chunk/End, per-id BTreeMap, 8-concurrent cap, idle sweep, MAX_XFER_BYTES), emit `clipboard-in`/`chat-msg`/`fs-entries`/`file-begin`/`file-progress`/`file-recv`. Save to app external files dir "Pulsar Alınanlar". Register in `lib.rs`.
- **W4-rust-client**: capture `caps.displays` (currently discarded) → `host-displays` + stash; `set_play_monitor` (restream + ~400ms debounce) in `session_cmds.rs`; route `DataMsg::Rumble` → `rumble` event; `reverse_play` (`DataMsg::ReverseRequest(myId)`); `mic_start`/`mic_stop` (pull PCM from plugin, send `DataMsg::Audio` ~20ms + `AudioEnd`).
- **W4-rust-input**: `send_gamepad`/`send_gamepad_disconnect` building `GamepadState` (axis→i16, trigger→u8, UP-positive Y) as `InputEvent::GamepadSlot{slot,kind:Xbox,target:Auto,state}` / `GamepadDisconnect{slot}`.
- **W4-sidechannels**: clipboard send/receive + chat panel (DT-clipboard-chat), remote-only card, `visualViewport`-aware composer over the surface.
- **W4-files-js**: remote file browser + per-file download + OS-picker upload + transfer-progress queue (DT-files); add `tauri-plugin-dialog` to `Cargo.toml`. Remote-only.
- **W4-gamepad**: on-screen virtual pad (DT-onscreen-gamepad, game-only) + physical `navigator.getGamepads()` poll + rumble via `vibrationActuator`; emits `bus:gamepad-active`; sends disconnect on End.
- **W4-devices**: Saved Devices/Geçmiş screen (DT-devices) over `store/peers.js`; long-press/sheet edit/forget/favorite; add-device sheet; clear-history; remote/game timeline split. Register the screen in `app.js`.
- **W4-mic**: `RECORD_AUDIO` in `AndroidManifest.xml` + runtime request; native `AudioRecord` (VOICE_COMMUNICATION, 48k mono s16le) plugin command `micStart`/`micStop` (commands.rs+lib.rs+toml+mobile.rs+desktop.rs no-op+Kotlin); add mic toggle to `audio.js` (remote-only, bg auto-mute on `visibilitychange`).

### WAVE 5 — Multi-session, split, advanced codecs, polish (8 lanes)

| Lane | Owned files | Roadmap items | Design? |
| --- | --- | --- | --- |
| **W5-rust-session** | `src/client.rs`, `src/session_cmds.rs`, `src/lib.rs` | W5-multisession (rust: per-slot active routing), W5-split (rust: per-cell res/display claim), W5-decoder-recovery (rust: keyframe nudge), W5-lan-presence (rust: `lan_devices` — best-effort/stub if Discovery not reachable) | No |
| **W5-rust-host** | `src/host.rs` | W5-host-codecprobe (rust: build `StreamCaps.codecs` from probe) | No |
| **W5-rtp** | `src/rtp.rs` | W5-av1 (rust: AV1 OBU depacketizer + `Codec::Av1`) | No |
| **W5-native** | `crates/.../PulsarVideoPlugin.kt`, `.../HostEncoder.kt`, `.../mobile.rs` | W5-multisession (slot cap bump), W5-split (positionPanes), W5-host-codecprobe (MediaCodecList), W5-av1 (av01 csd), W5-hdr (native), W5-decoder-recovery (native error emit), W5-audio-jitter (AudioTrack buffer) | No |
| **W5-session-js** | `ui/js/session/session.js` | W5-multisession (JS switcher + setActivePane) | **Yes — DT-multisession-split** |
| **W5-split-js** | `ui/js/session/split.js` (new) | W5-split (JS layout chooser) | **Yes — DT-multisession-split** |
| **W5-quality-adv** | `ui/js/session/quality.js`, `ui/js/session/hud.js` | W5-hdr (JS toggle), W5-decoder-recovery (JS resync overlay) | No |
| **W5-settings-lang** | `ui/js/screens/settings.js`, `ui/js/i18n.js` | W5-language-toggle | **Yes — DT-language** |

> `client.rs`+`session_cmds.rs`+`lib.rs` → **W5-rust-session** only. `host.rs` →
> **W5-rust-host** only. `rtp.rs` → **W5-rtp** only. The whole native plugin (Kotlin
> + `mobile.rs` + `HostEncoder.kt`) → **W5-native** only (so multisession/split/
> codecprobe/av1/hdr/jitter native edits never collide). `session.js` → **W5-session-js**;
> `split.js` (new) → **W5-split-js**; `quality.js`+`hud.js` → **W5-quality-adv**;
> `settings.js`+`i18n.js` → **W5-settings-lang**. No file is in two lanes.

Briefs:
- **W5-rust-session**: JS-mirrored multi-session active routing (`setActivePane` input+audio); per-cell reduced resolution for the 2nd slot (720p) via `StreamReq.width/height`; freeDisplayFor-style claimed-display map for same-host panes; keyframe/restream nudge for decoder recovery; `lan_devices` (best-effort — stub `[]` if `pulsar_core::Discovery` isn't trivially callable from the mobile crate, FLAG).
- **W5-rust-host**: build `StreamCaps.codecs` dynamically (prefer h265>h264 when an HEVC encoder exists), keep h264 fallback, via `host_codecs` probe.
- **W5-rtp**: AV1 RTP OBU depacketizer (RFC 9043) as `Codec::Av1`; until done, JS excludes av1 from the codec preference.
- **W5-native**: bump pane slot cap (`coerceIn(0,1)`→higher), `positionPanes` left/right + quadrant gravity, MediaCodecList enumerate, av01 csd from sequence-header OBU, HDR10/HLG `MediaFormat` + SurfaceView color mode, decoder-error event emit, AudioTrack buffer toward ~80-120ms / leaky queue.
- **W5-session-js**: touch session switcher (pill row / swipe-down sheet) + `setActivePane` routing + per-session rename (DT-multisession-split).
- **W5-split-js**: layout chooser sheet (landscape h2 default, v2 stacked, grid4 tablet) + per-pane distinct-target picker + exit-split (DT-multisession-split).
- **W5-quality-adv**: HDR toggle in quality card; "yeniden eşitleniyor" resync overlay in hud.
- **W5-settings-lang**: TR/EN seg in settings (DT-language) → `setLang()` + `set_config` language + `<html lang>` + live re-render.

### Per-wave lane counts
- Wave 1: **5 lanes** · Wave 2: **5 lanes** · Wave 3: **8 lanes** · Wave 4: **8 lanes** · Wave 5: **8 lanes**. Total: **34 lanes** across 5 waves.

### Dependency ordering sanity
Wave order follows the `dependsOn` graph: W1 foundation (modsplit, identity,
datachan scaffold, config, i18n) unblocks everything. W2 needs W1 (connecting needs
modsplit+i18n; teardown needs datachan; inputcmds/gamemode need modsplit/config).
W3 overlay needs W2-teardown; quality needs overlay+gamemode; host needs
identity+config+modsplit. W4 needs W1-datachan + W3-overlay (+ W3 gamepad/audio
deps). W5 needs W3-overlay+W2-teardown (multisession), W5-multisession+W3-quality
(split), W2-gamemode (av1), W3-quality (hdr), W3-hud (decoder-recovery),
W1-i18n+W1-config (language).

---

## 6. Verification per wave

Run from `desktop-app/mobile/`. Goal: the app stays always-runnable.

**After EVERY wave (gate before merging the wave):**
```bash
# 1. Mobile Rust compiles for the Android target (the real build target).
cd /home/kahverengi/Projects/pulsar/desktop-app/mobile
cargo check --target x86_64-linux-android -p pulsar-mobile
# 2. pulsar-core still green (esp. after the W1 send_data_via addition + the
#    DataMsg roundtrip invariant).
cargo test -p pulsar-core
# 3. Desktop sanity build of the mobile crate (the rlib/desktop no-op path).
cargo check -p pulsar-mobile
# 4. JS module syntax sanity (no bundler — each module must parse as ESM).
for f in $(find ui/js -name '*.js'); do node --check "$f" || echo "SYNTAX FAIL: $f"; done
# 5. CSS/HTML smoke: confirm index.html references only existing module/css paths.
grep -oE '(src|href)="[^"]+"' ui/index.html
```

**Full end-to-end install on the emulator (after waves that touch native/decode —
W1, W3, W4-mic, W5, and any wave changing the plugin):**
```bash
cd /home/kahverengi/Projects/pulsar/desktop-app/mobile
bun run tauri android dev --target x86_64   # builds + installs on emulator-5554
# or, for a packaged install check:
bun run tauri android build --target x86_64 --debug
adb -s emulator-5554 install -r gen/android/app/build/outputs/apk/.../app-x86_64-debug.apk
adb -s emulator-5554 logcat -s PulsarMobile:* PulsarVideoPlugin:*   # watch read-loop + decode
```

**Per-wave extra checks:**
- **W1**: confirm the TEMP diagnostic is gone (`grep -n "TestHost\|PULSARHOST" ui/index.html` → no match); confirm relay default is no longer the LAN dev IP. Launch on emulator → home screen renders, no auto MediaProjection prompt. Verify the relay-assigned ID is **stable across two launches** (logcat `id=` is identical).
- **W2**: connect to a real host → `conn-phase` events fire in order, transport pill shows real Direct/Relay; a bad ID → friendly localized error; Cancel aborts; End tears down cleanly (host stops within ~1s, not 6s).
- **W3**: overlay opens via pill/FAB and shows ONLY remote cards in remote / game cards in game; HUD shows live fps/mbps; quality change rebuilds the stream; mute toggles instantly; host go-online/offline + approval sheet + kick work.
- **W4**: clipboard/chat/files appear ONLY in remote mode (never game); a file downloads to "Pulsar Alınanlar"; on-screen gamepad appears ONLY in game; mic permission prompt + upstream; devices screen tap-to-connect.
- **W5**: two sessions hold + switch; split layout; codec probe advertises h265 when supported; language toggle re-renders live. Verify A/V drift on a real device for jitter tuning.

**Invariant guard (CI-style, run after any Rust wave):** `cargo test -p
pulsar-core data_msg_all_variants_roundtrip` — proves no wire variant was silently
dropped. (We add no new `DataMsg` variants, so this should always pass; it is the
canary if someone touches `wire.rs`.)

---

## Appendix — collision matrix (one-glance per-wave file ownership)

- **`src/lib.rs`**: W1-rust · W2-rust · W3-rust · W4-rust-data · W5-rust-session. (one per wave ✓)
- **`src/client.rs`**: W1-rust · W2-rust · W3-rust · W4-rust-client · W5-rust-session. (one per wave ✓)
- **`src/host.rs`**: W1-rust · (W2 none) · W3-host · (W4 none) · W5-rust-host. (one per wave ✓)
- **`src/session_cmds.rs`**: W2-rust · W3-rust · W4-rust-client · W5-rust-session. (one per wave ✓)
- **`src/input_cmds.rs`**: W2-rust · W4-rust-input. (one per wave ✓)
- **`src/datachan.rs`**: W1-rust · W4-rust-data. (one per wave ✓)
- **plugin crate (Kotlin+mobile.rs+desktop.rs+commands.rs+default.toml)**: W3-media-native · W4-mic · W5-native. (one per wave ✓)
- **`ui/js/app.js`**: W1-shell · W2-shell-glue · W4-devices. (one per wave ✓)
- **`ui/js/router.js`**: W1-shell · W2-shell-glue. (one per wave ✓)
- **`ui/css/components.css`**: W1-shell · W3-overlay. (one per wave ✓)
- **`ui/index.html`**: W1-shell · W2-shell-glue. (one per wave ✓)
- **`ui/js/session/overlay.js`**: W3-overlay. (one per wave ✓)
- **`ui/js/session/audio.js`**: W3-media-native · W4-mic. (one per wave ✓)
- **`ui/js/session/quality.js`**: W3-quality-js · W5-quality-adv. (one per wave ✓)
- **`ui/js/session/hud.js`**: W3-hud-js · W5-quality-adv. (one per wave ✓)
- **`ui/js/session/session.js`**: W2-session · W5-session-js. (one per wave ✓)
- **`ui/js/session/display.js`**: W3-media-native (created, fit/orientation) · W4-sidechannels (extend with the multi-monitor picker). (one per wave ✓)
- **`ui/js/session/input.js`**: W3-input. **`ui/js/session/keyboard.js`**: W3-keyboard. **`ui/js/session/sidechannels.js`**: W4-sidechannels. **`ui/js/session/files.js`**: W4-files-js. **`ui/js/session/gamepad.js`**: W4-gamepad. **`ui/js/session/split.js`**: W5-split-js. **`ui/js/store/peers.js`**: W1-peers. **`ui/js/store/config.js`**: W1-config-js. **`ui/js/i18n.js`**: W1-i18n · W5-settings-lang. **`ui/js/screens/connect.js`**: W2-connect. **`ui/js/screens/connecting.js`**: W2-connecting. **`ui/js/screens/host.js`**: W3-host. **`ui/js/screens/devices.js`**: W4-devices. **`ui/js/screens/settings.js`**: W5-settings-lang. (each one-per-wave ✓)
- **`mobile/Cargo.toml`**: W4-files-js (adds `tauri-plugin-dialog`). **`gen/android/.../AndroidManifest.xml`**: W4-mic. **`crates/pulsar-core/src/service/{client.rs,service.rs}`**: W1-rust only. (✓)
