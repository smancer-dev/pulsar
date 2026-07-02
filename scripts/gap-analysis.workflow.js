export const meta = {
  name: 'pulsar-mobile-gap-analysis',
  description: 'Fan out 22 opus agents to map desktop↔mobile feature gaps, then synthesize a prioritized roadmap + mobile module structure',
  phases: [
    { title: 'Find', detail: '22 opus agents, one per feature area, compare desktop vs mobile', model: 'opus' },
    { title: 'Synthesize', detail: 'opus merges findings into prioritized roadmap + proposed mobile file structure', model: 'opus' },
  ],
}

// Shared context every finder gets. Mobile is a THIN client-only Tauri shell:
// a single 430-line ui/index.html (plain HTML/JS, withGlobalTauri) + rust
// client.rs/host.rs/rtp.rs/lib.rs + the tauri-plugin-pulsar-video native crate.
// Desktop is a full SvelteKit (Svelte 5) app. UI copy is Turkish. Brand: light
// theme, electric-indigo accent, cyan for gaming. Two personalities: Remote
// Desktop (full menu) and Game Streaming (slim, low-latency).
const SHARED = `
You are auditing the Pulsar monorepo at the repo root (cwd). Pulsar is an
open-source remote-desktop + game-streaming app (Parsec/Moonlight/RustDesk-like).

DESKTOP APP (full-featured, reference): desktop-app/
  - SvelteKit UI: desktop-app/src/ (routes/+page.svelte, routes/page/*.svelte,
    lib/screens/**, lib/api*.ts, lib/settings.svelte.ts, lib/i18n.tr.ts)
  - The single best index of the desktop feature surface is the Tauri command
    bridge: desktop-app/src/lib/api.commands.ts (+ api.ts, api.dom.ts, api.events.ts).
  - Rust backend commands: desktop-app/src-tauri/src/*.rs (commands.rs, auth.rs,
    connections.rs, files.rs, fs_browse.rs, audio_io.rs, controllers.rs, viewer.rs,
    session_cmds.rs, io_cmds.rs, display_mode.rs, host/*.rs).
  - Product/architecture rules: desktop-app/CLAUDE.md (READ the relevant sections).

MOBILE APP (thin client, the TARGET we want to bring up to parity): desktop-app/mobile/
  - UI: desktop-app/mobile/ui/index.html (ONE plain HTML/JS file, ~430 lines,
    withGlobalTauri — NO SvelteKit) + ui/fonts.css.
  - Rust: desktop-app/mobile/src/{client,host,rtp,lib}.rs (only commands wired today:
    connect_host, send_pointer, send_button, go_online, open_a11y_settings, a11y_enabled).
  - Native video surface plugin: desktop-app/crates/tauri-plugin-pulsar-video/.
  - Mobile plan/decisions: read the [[pulsar-mobile-plan]] context if available;
    Path A = transparent webview over a native HW-decoded video surface; Android-first.

DESIGN references (use for any mobile UI you propose): design/project/Pulsar App.html,
design/project/app/*.jsx (6 desktop screens), design/project/assets/tokens.css
(design-token source of truth: oklch colors, type scale, radii, .btn atoms).
Mobile is touch-first + portrait/landscape — desktop layouts must be RE-DESIGNED
for mobile, not ported 1:1.

YOUR JOB: for the ASSIGNED feature area only, determine what the DESKTOP can do,
what the MOBILE app currently does (absent / partial / present), and exactly what
is MISSING on mobile. For each missing piece say whether it needs a NEW mobile-
specific UI design (touch-first), estimate complexity (S/M/L), and propose a
concrete implementation approach with the real file paths to touch. Be specific
and grounded in the actual code — cite file:line. Do NOT propose features the
product explicitly excludes (e.g. file/clipboard/mic/multi-monitor are remote-
desktop-only, NOT shown in game mode).`

const FINDING_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['area', 'mobileStatus', 'desktopCapabilities', 'missingPieces', 'notes'],
  properties: {
    area: { type: 'string' },
    mobileStatus: { type: 'string', enum: ['absent', 'partial', 'present'] },
    desktopCapabilities: { type: 'array', items: { type: 'string' } },
    missingPieces: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['feature', 'description', 'designNeeded', 'complexity', 'priority', 'approach', 'files'],
        properties: {
          feature: { type: 'string' },
          description: { type: 'string' },
          designNeeded: { type: 'boolean', description: 'true if a new touch-first mobile UI must be designed' },
          complexity: { type: 'string', enum: ['S', 'M', 'L'] },
          priority: { type: 'integer', minimum: 1, maximum: 5, description: '1=critical for usable mobile client, 5=nice-to-have' },
          approach: { type: 'string', description: 'concrete implementation approach incl. mobile + rust changes' },
          files: { type: 'array', items: { type: 'string' }, description: 'real file paths to create or edit' },
        },
      },
    },
    notes: { type: 'string' },
  },
}

// 22 feature areas — one opus finder each. Non-overlapping coverage of the
// desktop surface, framed as "does mobile have this, what's missing".
const AREAS = [
  { key: 'connect-flow', title: 'Connection flow: ID/IP entry, connect button, P2P→relay, network mode, reconnect',
    hint: 'desktop: routes/page/HomeView.svelte, sessions.svelte.ts, lib/screens/Connecting.svelte, api.commands.ts connect/auto_connect. mobile: ui/index.html connect_host.' },
  { key: 'recents-devices', title: 'Saved/recent devices, device list, unattended access, forget device, naming',
    hint: 'desktop: lib/screens/Devices*, connections.rs, list_connections, forget_peer. mobile: none?' },
  { key: 'otp-auth', title: 'One-time-password auth: entering host password, host issuing/showing password, incoming-request accept/deny',
    hint: 'desktop: auth.rs, submit_password, respond_request, lib/screens password prompt UI. mobile: client.rs auth?' },
  { key: 'session-toolbar', title: 'In-session toolbar/dock (remote mode): which controls exist, what mobile shows during a session',
    hint: 'desktop: routes/page/BottomDock.svelte, Chrome.svelte, lib/screens/Session.svelte, Session/ui.svelte.ts. mobile: ui/index.html session view.' },
  { key: 'clipboard', title: 'Clipboard sync (remote-desktop only)',
    hint: 'desktop: api.dom.ts clipboard, side channels DataMsg. mobile: absent?' },
  { key: 'file-transfer', title: 'File transfer + remote file browser (remote-desktop only)',
    hint: 'desktop: files.rs, fs_browse.rs, files_window.rs. mobile: absent?' },
  { key: 'chat', title: 'In-session text chat (remote-desktop only)',
    hint: 'desktop: DataMsg chat side channel. mobile: absent?' },
  { key: 'mic-audio-in', title: 'Microphone / two-way audio (remote-desktop only)',
    hint: 'desktop: audio_io.rs mic, parecord/paplay. mobile: absent? consider Android mic perms.' },
  { key: 'multimonitor', title: 'Multi-monitor selection (remote-desktop only)',
    hint: 'desktop: display_mode.rs, monitor picker UI. mobile: absent?' },
  { key: 'gaming-overlay', title: 'Gaming personality + in-session gaming overlay (perf HUD, combo-toggled rich menu)',
    hint: 'desktop: routes/page/GamingShell.svelte, data-gaming, overlay. mobile: absent? must be touch-toggled.' },
  { key: 'stream-quality', title: 'Stream quality controls: codec, encoder/decoder, bitrate(Mbit), fps, resolution, quality/perf',
    hint: 'desktop: settings.svelte.ts, available_encoders, GamingShell menu. mobile: absent?' },
  { key: 'controllers-gamepad', title: 'Physical controller support (gamepad detection, virtual pad) on mobile',
    hint: 'desktop: controllers.rs, gilrs, vigem. mobile: Android gamepad via webview Gamepad API → send_button?' },
  { key: 'touch-onscreen-controls', title: 'On-screen TOUCH controls for gaming (virtual gamepad overlay) — mobile-specific, no desktop analog',
    hint: 'NEW for mobile. Moonlight-style on-screen buttons/sticks → controller input. design needed.' },
  { key: 'touch-pointer-kbd', title: 'Touch→mouse mapping, tap/drag/scroll, on-screen keyboard + special keys (remote desktop on a phone)',
    hint: 'desktop: Session.svelte pointer capture, keymap.ts. mobile: send_pointer/send_button exist — what gestures/keys missing?' },
  { key: 'video-decode', title: 'Video rendering/decode pipeline on mobile (native surface vs WebCodecs), HDR, scaling, aspect',
    hint: 'desktop: viewer.rs, h264.ts, native_view. mobile: tauri-plugin-pulsar-video, rtp.rs.' },
  { key: 'audio-playback', title: 'Audio playback on mobile (opus decode + output), mute, volume',
    hint: 'desktop: opus-audio.ts, audio_io.rs. mobile: any audio path?' },
  { key: 'settings-screen', title: 'Settings screen: relay endpoint, network mode, language, persisted config',
    hint: 'desktop: lib/screens/Settings*, settings.svelte.ts, config.rs. mobile: any settings UI?' },
  { key: 'host-mode', title: 'Host mode on mobile (share THIS device: go_online, show ID/password, screen-share Android)',
    hint: 'desktop: go_online, host/*.rs. mobile: host.rs go_online + a11y — how complete? Android MediaProjection?' },
  { key: 'identity-online', title: 'Device identity, stable ID display, online/offline status, relay registration UI',
    hint: 'desktop: identity persistence, go_online, ID display on home. mobile: shows own ID? online state?' },
  { key: 'session-lifecycle', title: 'Session lifecycle: connecting screen, transport/stats overlay, disconnect/end, teardown, error states',
    hint: 'desktop: Connecting.svelte, render_stats.rs, session keepalive/teardown, disconnect_peer. mobile: ui/index.html states.' },
  { key: 'i18n-branding', title: 'i18n (Turkish UI copy) + branding/design-system (PulsarMark logo, tokens, fonts) on mobile',
    hint: 'desktop: i18n.tr.ts, tokens.css, PulsarMark. mobile: ui/index.html copy + styling parity.' },
  { key: 'split-multisession', title: 'Split-screen / multiple concurrent sessions / tab switching',
    hint: 'desktop: split picker, multi-session tabs, sessions.svelte.ts. mobile: likely single-session — confirm scope.' },
]

phase('Find')
const findings = await parallel(
  AREAS.map((a) => () =>
    agent(
      `${SHARED}\n\n=== YOUR ASSIGNED AREA ===\n${a.title}\n\nWhere to look (starting hints, verify in real code): ${a.hint}\n\nRead the relevant desktop code AND the mobile app, then return the structured gap finding for THIS area only.`,
      { label: `find:${a.key}`, phase: 'Find', model: 'opus', effort: 'high', schema: FINDING_SCHEMA }
    )
  )
)

const valid = findings.filter(Boolean)
log(`Collected ${valid.length}/${AREAS.length} area findings`)

phase('Synthesize')
const ROADMAP_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['summary', 'moduleStructure', 'roadmap', 'designTasks'],
  properties: {
    summary: { type: 'string', description: 'overall state of mobile vs desktop parity' },
    moduleStructure: {
      type: 'array',
      description: 'proposed mobile UI file/module breakdown so parallel implementers own DISTINCT files and avoid collisions on one index.html',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['path', 'purpose'],
        properties: { path: { type: 'string' }, purpose: { type: 'string' } },
      },
    },
    roadmap: {
      type: 'array',
      description: 'ordered, deduped implementation items grouped into waves by dependency',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'title', 'wave', 'priority', 'designNeeded', 'complexity', 'approach', 'files', 'dependsOn'],
        properties: {
          id: { type: 'string' },
          title: { type: 'string' },
          wave: { type: 'integer', description: '1 = foundation/first, higher = later' },
          priority: { type: 'integer', minimum: 1, maximum: 5 },
          designNeeded: { type: 'boolean' },
          complexity: { type: 'string', enum: ['S', 'M', 'L'] },
          approach: { type: 'string' },
          files: { type: 'array', items: { type: 'string' } },
          dependsOn: { type: 'array', items: { type: 'string' } },
        },
      },
    },
    designTasks: {
      type: 'array',
      description: 'mobile-specific UI designs that must be produced before/with implementation',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'title', 'description'],
        properties: { id: { type: 'string' }, title: { type: 'string' }, description: { type: 'string' } },
      },
    },
  },
}

const roadmap = await agent(
  `${SHARED}\n\n=== SYNTHESIS TASK ===\nYou received ${valid.length} per-area gap findings (JSON below). Merge and DEDUPE them into a single prioritized implementation roadmap to bring the Pulsar MOBILE app toward feature parity with desktop, RESPECTING mobile constraints (touch-first, single-session, Android-first, the two personalities, and the remote-vs-game feature split).\n\nCritically: propose a MOBILE UI MODULE STRUCTURE (split the monolithic ui/index.html into distinct files / JS modules / screens) so that later parallel implementer agents can each own a DIFFERENT file and not collide. Group roadmap items into WAVES by dependency (wave 1 = foundation like the module split, routing, design tokens, connect/auth; later waves = individual features). Flag every item that needs a new touch-first design, and list those as designTasks.\n\nFINDINGS JSON:\n${JSON.stringify(valid)}`,
  { label: 'synthesize-roadmap', phase: 'Synthesize', model: 'opus', effort: 'high', schema: ROADMAP_SCHEMA }
)

return { findings: valid, roadmap }
