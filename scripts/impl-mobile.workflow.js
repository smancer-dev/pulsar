export const meta = {
  name: 'pulsar-mobile-implement',
  description: 'Implement the mobile feature-parity roadmap wave-by-wave with sonnet lanes; verify+fix gate between waves keeps the app buildable',
  phases: [
    { title: 'Wave1', detail: 'Foundation: module split, identity, datachan, config, i18n, peers', model: 'sonnet' },
    { title: 'Wave2', detail: 'Connect robustness + input plumbing', model: 'sonnet' },
    { title: 'Wave3', detail: 'In-session overlay + live controls + host lifecycle', model: 'sonnet' },
    { title: 'Wave4', detail: 'Side channels, files, gamepad, mic, devices', model: 'sonnet' },
    { title: 'Wave5', detail: 'Multi-session, split, advanced codecs, polish', model: 'sonnet' },
  ],
}

const PLAN = 'desktop-app/mobile/IMPLEMENTATION-PLAN.md'
const REPORT = 'desktop-app/mobile/GAP-REPORT.json'

const SHARED = `You are a coder agent implementing ONE lane of the Pulsar mobile feature-parity build. cwd = repo root.

AUTHORITATIVE CONTRACT — read these FIRST, fully:
- ${PLAN}  (the plan: §1 architecture, §2 Tauri command+event contract, §3 JS module API, §4 CSS/design contract, §5 wave/lane table + per-lane briefs, §6 verification). This is the single source of truth.
- ${REPORT}  (.roadmap.designTasks[] = the touch-first design specs; .findings[] = per-area detail). Read the designTask(s) relevant to your lane.

HARD RULES (non-negotiable — the whole parallel build depends on them):
1. Edit/create ONLY the files in your lane's OWNED-FILES list below. NEVER touch any other file — other agents own them concurrently. If you think you need another file, instead expose/stub via the contract (events/commands/registry in §1) and note it in your return.
2. Build EXACTLY to the §2 command/event names + payloads and §3 module exports + §4 token/class names. Do not invent new command/event names — other lanes wire to the contract.
3. UI is touch-first (portrait + landscape phone), Turkish copy (use the i18n t() helper / keys per §3). Respect the two personalities (remote=indigo, game=cyan) and the remote-vs-game split: file/clipboard/chat/mic/multimonitor are REMOTE-ONLY, never shown in game mode (§1.2).
4. Only use pulsar-core / pulsar-proto / plugin capabilities that ACTUALLY exist (the plan verified them). If a needed core item is missing, implement the smallest additive change ONLY if your lane owns that core file; otherwise stub + note it.
5. Match surrounding code style. Rust: keep it compiling (your lane may be checked in isolation). JS: plain ES modules, no bundler, no new npm deps unless your owned files include the manifest. Keep diffs focused; no unrelated refactors.

Return a concise summary: what you implemented, any contract items you stubbed/deferred, and any cross-lane assumption you made.`

const DESIGN_NOTE = `
THIS LANE NEEDS A TOUCH-FIRST MOBILE UI DESIGN. Read the listed designTask(s) in ${REPORT} (.roadmap.designTasks) and §4 of the plan. Consult design/project/assets/tokens.css (token source of truth), design/project/Pulsar App.html and design/project/app/*.jsx (desktop screens, for visual language only — DO NOT port 1:1). Design for a phone: large tap targets (≥44px), thumb reach, safe-area insets, bottom sheets over the transparent video surface, indigo(remote)/cyan(game) theming. Produce polished, production-grade UI — not a placeholder.`

// Each lane: id, files (repo-root-relative owned list), design (designTask ids or null),
// brief (short pointer; full brief is in the plan §5 under the lane id).
const WAVES = [
  {
    phase: 'Wave1', critical: true, coreTouched: true,
    lanes: [
      { id: 'W1-rust', design: null, files: ['desktop-app/mobile/src/identity.rs', 'desktop-app/mobile/src/config.rs', 'desktop-app/mobile/src/datachan.rs', 'desktop-app/mobile/src/client.rs', 'desktop-app/mobile/src/host.rs', 'desktop-app/mobile/src/lib.rs', 'desktop-app/crates/pulsar-core/src/service/client.rs', 'desktop-app/crates/pulsar-core/src/service.rs'],
        brief: 'Foundation Rust: persistent Identity::load_or_create (kill both Identity::generate), get_config/set_config over pulsar_core::Config, the FLAGGED send_data_via(&SessionSender,&DataMsg) in service/client.rs + pub use, datachan.rs scaffold with route() wired into the read loop after media::parse returns None (event dispatch scaffold only, no per-feature send_* yet), connect_host/go_online fall back to Config when args empty. Register all commands in lib.rs generate_handler!.' },
      { id: 'W1-shell', design: null, files: ['desktop-app/mobile/ui/index.html', 'desktop-app/mobile/ui/css/tokens.css', 'desktop-app/mobile/ui/css/components.css', 'desktop-app/mobile/ui/js/app.js', 'desktop-app/mobile/ui/js/tauri.js', 'desktop-app/mobile/ui/js/router.js'],
        brief: 'Delete the TEMP auto-self-connect/auto-go-online diagnostic (index.html ~414-424) + fix default relay (not the LAN dev IP). Split the monolith into ES modules WITHOUT behavior change: :root→tokens.css (rename --on-accent→--text-on-accent), atoms→components.css, inline <script>→app.js + tauri.js (invoke/listen guard) + router.js (bottom-nav + data-mode/in-session writer) + stub screen modules (connect/host/settings/devices) you may create. Export the bus EventTarget + registerScreen registry per §1.1. Import ./i18n.js, ./store/config.js, ./store/peers.js (created by sibling lanes this wave). MOVE existing connect/host/settings/recents/touch/video/audio/split logic verbatim into modules — preserve all current behavior.' },
      { id: 'W1-i18n', design: null, files: ['desktop-app/mobile/ui/js/i18n.js'],
        brief: 'i18n.js per §3: tr/en flat catalogs (subset of desktop src/lib/i18n.tr.ts + i18n.en.ts covering all mobile copy), t(key,vars) with {var} interpolation + tr→en→key fallback, navigator.language detect, persist pulsar.lang.v1, setLang().' },
      { id: 'W1-config-js', design: null, files: ['desktop-app/mobile/ui/js/store/config.js'],
        brief: 'store/config.js per §3 over the W1-rust get_config/set_config commands, with localStorage cache fallback. Single source of truth for relay/network_mode/device_name/codec/language/quality.' },
      { id: 'W1-peers', design: null, files: ['desktop-app/mobile/ui/js/store/peers.js'],
        brief: 'store/peers.js per §3: port desktop src/lib/peers.svelte.ts to vanilla (saved-devices + recents, key pulsar.peers.v1, normalizeId/fmtPeerId, savedPeers/historyPeers/gameHistoryPeers, recordConnection/addPeer/updatePeer/removePeer/removeFromHistory/toggleFav).' },
    ],
  },
  {
    phase: 'Wave2', critical: true, coreTouched: false,
    lanes: [
      { id: 'W2-rust', design: null, files: ['desktop-app/mobile/src/client.rs', 'desktop-app/mobile/src/session_cmds.rs', 'desktop-app/mobile/src/input_cmds.rs', 'desktop-app/mobile/src/lib.rs'],
        brief: 'connect_host: emit conn-phase (transport from sess.transport() → auth → preparing), tokio::time::timeout 45s+30s → connect-timed-out, rename id→target + branch DeviceId::parse vs SocketAddr/connect_direct, thread mode+codec/fps/bitrate/res/quality into StreamReq (QualityPref::Latency for game). New session_cmds.rs (end_session w/ per-slot cancel + restream mpsc scaffold) + input_cmds.rs (send_scroll/send_key/send_char/send_pointer_rel). select! cancel+restream in read loop; emit play-ended{slot,reason}. Register in lib.rs.' },
      { id: 'W2-connect', design: null, files: ['desktop-app/mobile/ui/js/screens/connect.js'],
        brief: 'Port connectTarget.ts (isAddr/fmtTarget/ipRe/canConnectTarget), stop numeric-only stripping, pre-connect mode toggle + quality presets, friendly localized error table via t() (connErr.*), re-enable button + clear in-session on reject, read data-mode+config quality into connect_host.' },
      { id: 'W2-connecting', design: ['DT-connecting'], files: ['desktop-app/mobile/ui/js/screens/connecting.js'],
        brief: 'Full-screen phased connecting overlay: target id + mode badge, pulse-ring, step list driven by conn-phase (reaching→P2P/relay→auth/awaiting→preparing), 12s slow-host hint, big Cancel→end_session. Indigo/cyan themed.' },
      { id: 'W2-session', design: ['DT-teardown'], files: ['desktop-app/mobile/ui/js/session/session.js'],
        brief: 'session.js: own body.in-session lifecycle via router, per-slot registry, listen play-ended→drop session + detach surface + show "Bağlantı kesildi — Tekrar bağlan" card reusing doConnect(lastId,lastSlot). btn-end invokes end_session first.' },
      { id: 'W2-shell-glue', design: null, files: ['desktop-app/mobile/ui/js/app.js', 'desktop-app/mobile/ui/js/router.js', 'desktop-app/mobile/ui/index.html'],
        brief: 'Register connect/connecting/session with the router; add DOM mount points (connecting overlay + session card) to index.html; set the in-session net-pill from the real transport returned by connect_host.' },
    ],
  },
  {
    phase: 'Wave3', critical: false, coreTouched: false,
    lanes: [
      { id: 'W3-rust', design: null, files: ['desktop-app/mobile/src/client.rs', 'desktop-app/mobile/src/session_cmds.rs', 'desktop-app/mobile/src/lib.rs'],
        brief: 'read-loop fps/mbps→play-stats, play-firstframe on first AU, play-stall after ~2s no video (off on resume). set_play_codec/bitrate/fps/resolution/quality/encoder pushing StreamReq onto per-slot restream mpsc + re-request_stream. client.rs call plugin setAspect on stream entry. Replace single-shot authenticate() with desktop race loop (recv_host_auth; on NeedPassword emit auth-prompt + await submit_password oneshot) + pw_pending map + submit_password in session_cmds.rs.' },
      { id: 'W3-host', design: ['DT-host'], files: ['desktop-app/mobile/src/host.rs', 'desktop-app/mobile/ui/js/screens/host.js'],
        brief: 'Full host lifecycle §2.7: go_offline, re-runnable go_online, OTP Arc<Mutex>+rotate+host-password, replace silent auto-accept with session-request/respond_request race + 30s auto-deny, host-peer-connected/disconnected, disconnect_session, unattended toggle. host.js (DT-host): online/offline toggle, ID+OTP copy/share/rotate, peers list+kick, approval sheet.' },
      { id: 'W3-overlay', design: ['DT-overlay'], files: ['desktop-app/mobile/ui/js/session/overlay.js', 'desktop-app/mobile/ui/css/components.css'],
        brief: 'Mode-aware bottom-sheet/dock + registerCard registry (filters by modes — central remote/game enforcement), opened by status-pill/FAB. Extend components.css with .overlay-dock/.overlay-card/.fab. Host surface all W3/W4 panels attach to.' },
      { id: 'W3-input', design: ['DT-touch-input'], files: ['desktop-app/mobile/ui/js/session/input.js'],
        brief: 'Touch→pointer engine: tap=left, double-tap=double, long-press=right, two-finger tap=middle, two-finger drag=scroll, trackpad/relative mode + cursor dot. Calls W2 input cmds. Gate off when bus:gamepad-active.' },
      { id: 'W3-keyboard', design: ['DT-keyboard'], files: ['desktop-app/mobile/ui/js/session/keyboard.js'],
        brief: 'On-screen keyboard (hidden input raises soft kbd) + special-key/modifier bar (Esc/Tab/arrows/F1-12 + sticky Ctrl/Alt/Shift/Win + Ctrl+Alt+Del), visualViewport-aware. registerCard({modes:[remote]}). Calls send_char/send_key.' },
      { id: 'W3-hud-js', design: ['DT-perf-hud'], files: ['desktop-app/mobile/ui/js/session/hud.js'],
        brief: 'Perf HUD card listening play-stats/play-vstats/play-stall/play-firstframe: fps/mbps/RTT/decode strip (mono, cyan in game), first-frame loader, stall overlay. Collapsible.' },
      { id: 'W3-quality-js', design: ['DT-quality-sheet'], files: ['desktop-app/mobile/ui/js/session/quality.js'],
        brief: 'Quality controls card (segmented codec/fps/res/quality + bitrate Mbit slider) + pre-connect presets export, calling set_play_*. Switching veil during encoder rebuild. Both modes.' },
      { id: 'W3-media-native', design: ['DT-audio', 'DT-display'], files: ['desktop-app/crates/tauri-plugin-pulsar-video/src/mobile.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/src/desktop.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/src/commands.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/android/src/main/java/PulsarVideoPlugin.kt', 'desktop-app/crates/tauri-plugin-pulsar-video/permissions/default.toml', 'desktop-app/mobile/ui/js/session/audio.js', 'desktop-app/mobile/ui/js/session/display.js'],
        brief: 'Add plugin commands setAudioMuted/setAspect/setOrientation across commands.rs+lib.rs handler+default.toml+mobile.rs+desktop.rs no-op+Kotlin @Command (+ applyAspect fit/fill/stretch, setRequestedOrientation). audio.js (mute card, both modes) + display.js (fit/orientation card, remote-only). This lane owns the WHOLE plugin crate + toml this wave.' },
    ],
  },
  {
    phase: 'Wave4', critical: false, coreTouched: false,
    lanes: [
      { id: 'W4-rust-data', design: null, files: ['desktop-app/mobile/src/datachan.rs', 'desktop-app/mobile/src/lib.rs'],
        brief: 'datachan.rs: add send_clipboard/send_chat/fs_list/fs_get/send_file via send_data_via; port the hold.rs file reassembler (FileBegin/Chunk/End, per-id BTreeMap, 8-concurrent cap, idle sweep, MAX_XFER_BYTES); emit clipboard-in/chat-msg/fs-entries/file-begin/file-progress/file-recv; save to app external files dir "Pulsar Alınanlar". Register in lib.rs.' },
      { id: 'W4-rust-client', design: null, files: ['desktop-app/mobile/src/client.rs', 'desktop-app/mobile/src/session_cmds.rs'],
        brief: 'Capture caps.displays (currently discarded)→host-displays+stash; set_play_monitor (restream + ~400ms debounce) in session_cmds.rs; route DataMsg::Rumble→rumble event; reverse_play (DataMsg::ReverseRequest(myId)); mic_start/mic_stop (pull PCM from plugin, send DataMsg::Audio ~20ms + AudioEnd).' },
      { id: 'W4-rust-input', design: null, files: ['desktop-app/mobile/src/input_cmds.rs'],
        brief: 'send_gamepad/send_gamepad_disconnect building GamepadState (axis→i16, trigger→u8, UP-positive Y) as InputEvent::GamepadSlot{slot,kind:Xbox,target:Auto,state} / GamepadDisconnect{slot}.' },
      { id: 'W4-sidechannels', design: ['DT-clipboard-chat', 'DT-display'], files: ['desktop-app/mobile/ui/js/session/sidechannels.js', 'desktop-app/mobile/ui/js/session/display.js'],
        brief: 'sidechannels.js: clipboard send/receive + chat panel (visualViewport-aware composer over surface), remote-only card. EXTEND display.js with the multi-monitor picker (host-displays event + set_play_monitor), remote + displays>1.' },
      { id: 'W4-files-js', design: ['DT-files'], files: ['desktop-app/mobile/ui/js/session/files.js', 'desktop-app/mobile/Cargo.toml'],
        brief: 'files.js: remote file browser (fs_list/fs-entries) + per-file download (fs_get/file-recv) + OS-picker upload + transfer-progress queue. Remote-only. Add tauri-plugin-dialog to mobile Cargo.toml.' },
      { id: 'W4-gamepad', design: ['DT-onscreen-gamepad'], files: ['desktop-app/mobile/ui/js/session/gamepad.js'],
        brief: 'On-screen virtual pad (game-only, Moonlight-style dual sticks/D-pad/ABXY/shoulders/triggers, multi-touch) + physical navigator.getGamepads() poll + rumble via vibrationActuator on rumble event. Emits bus:gamepad-active; sends disconnect on End. Calls send_gamepad.' },
      { id: 'W4-devices', design: ['DT-devices'], files: ['desktop-app/mobile/ui/js/screens/devices.js', 'desktop-app/mobile/ui/js/app.js'],
        brief: 'Saved Devices/Geçmiş screen over store/peers.js: list (icon/name/grouped-id/online dot/last-seen), tap-to-connect, long-press/sheet edit/forget/favorite, add-device sheet, clear-history, remote/game timeline split. Register the screen in app.js.' },
      { id: 'W4-mic', design: ['DT-mic'], files: ['desktop-app/mobile/ui/js/session/audio.js', 'desktop-app/crates/tauri-plugin-pulsar-video/src/mobile.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/src/desktop.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/src/commands.rs', 'desktop-app/crates/tauri-plugin-pulsar-video/android/src/main/java/PulsarVideoPlugin.kt', 'desktop-app/crates/tauri-plugin-pulsar-video/permissions/default.toml', 'desktop-app/mobile/gen/android/app/src/main/AndroidManifest.xml'],
        brief: 'RECORD_AUDIO in AndroidManifest + runtime request; native AudioRecord (VOICE_COMMUNICATION, 48k mono s16le) plugin micStart/micStop (commands.rs+lib.rs handler is owned by W4-rust-data — only add the command fn here in commands.rs+mobile.rs+desktop.rs no-op+Kotlin+toml). Add mic toggle to audio.js (remote-only, bg auto-mute on visibilitychange). NOTE: the mic_start/mic_stop *Tauri* commands (client side) live in W4-rust-client; you add the *plugin* micStart/micStop.' },
    ],
  },
  {
    phase: 'Wave5', critical: false, coreTouched: false,
    lanes: [
      { id: 'W5-rust-session', design: null, files: ['desktop-app/mobile/src/client.rs', 'desktop-app/mobile/src/session_cmds.rs', 'desktop-app/mobile/src/lib.rs'],
        brief: 'Multi-session active routing (setActivePane input+audio); per-cell reduced resolution (720p 2nd slot) via StreamReq; claimed-display map for same-host panes; keyframe/restream nudge for decoder recovery; lan_devices (best-effort, stub [] if Discovery not reachable — FLAG).' },
      { id: 'W5-rust-host', design: null, files: ['desktop-app/mobile/src/host.rs'],
        brief: 'Build StreamCaps.codecs dynamically (prefer h265>h264 when HEVC encoder exists, keep h264 fallback) via host_codecs probe.' },
      { id: 'W5-rtp', design: null, files: ['desktop-app/mobile/src/rtp.rs'],
        brief: 'AV1 RTP OBU depacketizer (RFC 9043) as Codec::Av1.' },
      { id: 'W5-native', design: null, files: ['desktop-app/crates/tauri-plugin-pulsar-video/android/src/main/java/PulsarVideoPlugin.kt', 'desktop-app/crates/tauri-plugin-pulsar-video/android/src/main/java/HostEncoder.kt', 'desktop-app/crates/tauri-plugin-pulsar-video/src/mobile.rs'],
        brief: 'Bump pane slot cap, positionPanes left/right + quadrant gravity, MediaCodecList enumerate, av01 csd from sequence-header OBU, HDR10/HLG MediaFormat + SurfaceView color mode, decoder-error event emit, AudioTrack buffer ~80-120ms.' },
      { id: 'W5-session-js', design: ['DT-multisession-split'], files: ['desktop-app/mobile/ui/js/session/session.js'],
        brief: 'Touch session switcher (pill row / swipe-down sheet) + setActivePane routing + per-session rename.' },
      { id: 'W5-split-js', design: ['DT-multisession-split'], files: ['desktop-app/mobile/ui/js/session/split.js'],
        brief: 'Layout chooser sheet (landscape h2 default, v2 stacked, grid4 tablet) + per-pane distinct-target picker + exit-split.' },
      { id: 'W5-quality-adv', design: null, files: ['desktop-app/mobile/ui/js/session/quality.js', 'desktop-app/mobile/ui/js/session/hud.js'],
        brief: 'HDR toggle in quality card; "yeniden eşitleniyor" resync overlay in hud.' },
      { id: 'W5-settings-lang', design: ['DT-language'], files: ['desktop-app/mobile/ui/js/screens/settings.js', 'desktop-app/mobile/ui/js/i18n.js'],
        brief: 'TR/EN segmented control in settings → setLang() + set_config language + <html lang> + live re-render.' },
    ],
  },
]

function lanePrompt(wave, lane) {
  const design = lane.design ? `${DESIGN_NOTE}\nYour designTask(s): ${lane.design.join(', ')}.` : ''
  return `${SHARED}

=== YOUR LANE: ${lane.id}  (${wave.phase}) ===
OWNED FILES (create/edit ONLY these):
${lane.files.map((f) => '  - ' + f).join('\n')}

LANE BRIEF (full version is in ${PLAN} §5 under "${lane.id}"): ${lane.brief}
${design}

Implement your lane completely and correctly now. Keep it compiling/parsing.`
}

const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['ok', 'summary', 'errors'],
  properties: {
    ok: { type: 'boolean' },
    summary: { type: 'string' },
    errors: { type: 'array', items: { type: 'string' }, description: 'compiler/syntax error excerpts, empty if ok' },
  },
}

function verifyPrompt(wave) {
  const core = wave.coreTouched
    ? '\n4. rustup run stable cargo check -p pulsar-tauri 2>&1 | tail -40   (pulsar-core changes must not break desktop)'
    : ''
  return `You are the build-gate for ${wave.phase} of the Pulsar mobile build. cwd = repo root. Run these and report whether the app still builds. Do NOT edit any code — verify only.

Run (capture exit codes + error tails):
1. rustup run stable cargo check -p pulsar-mobile 2>&1 | tail -50
2. rustup run stable cargo check --manifest-path desktop-app/crates/tauri-plugin-pulsar-video/Cargo.toml 2>&1 | tail -30   (if it has no standalone manifest target, skip and note)
3. for f in $(find desktop-app/mobile/ui/js -name '*.js'); do node --check "$f" || echo "JS SYNTAX FAIL: $f"; done${core}
5. grep -oE '(src|href)="[^"]+"' desktop-app/mobile/ui/index.html   (sanity: referenced module/css paths exist)

Set ok=true ONLY if cargo check(s) succeed (exit 0) AND no JS syntax failures. Put the most useful error excerpts (file:line + message) in errors[]. Be precise — a fixer agent will act on exactly what you report.`
}

function fixPrompt(wave, errors) {
  return `${wave.phase} of the Pulsar mobile build FAILED its build gate. cwd = repo root. Fix the errors below so that 'rustup run stable cargo check -p pulsar-mobile' passes and every desktop-app/mobile/ui/js/*.js passes 'node --check', WITHOUT changing the intended behavior or the §2/§3/§4 contract in ${PLAN}. You may edit any file needed to fix compilation. Make the smallest correct changes. Re-run the checks yourself to confirm before returning.

ERRORS:
${errors.map((e) => '  - ' + e).join('\n')}

Return what you changed and the final check status.`
}

async function runWave(wave) {
  phase(wave.phase)
  log(`${wave.phase}: launching ${wave.lanes.length} lanes`)
  const results = await parallel(
    wave.lanes.map((lane) => () =>
      agent(lanePrompt(wave, lane), { label: lane.id, phase: wave.phase, model: 'sonnet', effort: 'high' })
        .then((r) => ({ lane: lane.id, ok: true, summary: r }))
        .catch((e) => ({ lane: lane.id, ok: false, summary: String(e) }))
    )
  )
  const built = results.filter(Boolean)
  log(`${wave.phase}: ${built.filter((r) => r.ok).length}/${wave.lanes.length} lanes returned`)

  // Verify → fix gate (up to 3 attempts).
  let verdict = await agent(verifyPrompt(wave), { label: `verify:${wave.phase}`, phase: wave.phase, model: 'sonnet', effort: 'low', schema: VERIFY_SCHEMA })
  let attempts = 0
  while (verdict && !verdict.ok && attempts < 3) {
    attempts++
    log(`${wave.phase}: build gate FAILED (attempt ${attempts}) — ${verdict.errors.length} errors; fixing`)
    await agent(fixPrompt(wave, verdict.errors), { label: `fix:${wave.phase}#${attempts}`, phase: wave.phase, model: 'sonnet', effort: 'high' })
    verdict = await agent(verifyPrompt(wave), { label: `reverify:${wave.phase}#${attempts}`, phase: wave.phase, model: 'sonnet', effort: 'low', schema: VERIFY_SCHEMA })
  }
  const green = !!(verdict && verdict.ok)
  log(`${wave.phase}: build gate ${green ? 'GREEN' : 'STILL RED after ' + attempts + ' fixes'}`)
  if (!green && wave.critical) {
    throw new Error(`${wave.phase} (foundation) failed its build gate after ${attempts} fix attempts — aborting so later waves don't build on broken code. Errors: ${verdict ? verdict.errors.join(' | ') : 'verify agent died'}`)
  }
  return { wave: wave.phase, lanes: built, green, attempts, errors: green ? [] : verdict ? verdict.errors : ['verify agent died'] }
}

// args (optional) = array of phase names to run this invocation (e.g. ["Wave1"]).
// Lets the orchestrator run + smoke-test waves incrementally. Default = all 5.
const todo = Array.isArray(args) && args.length ? WAVES.filter((w) => args.includes(w.phase)) : WAVES
log(`Running waves: ${todo.map((w) => w.phase).join(', ')}`)
const out = []
for (const wave of todo) {
  out.push(await runWave(wave))
}
return out
