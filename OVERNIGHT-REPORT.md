# Overnight report — 2026-06-13

## Ship status
- Commit `927e337` (Windows probe crash + focus-flap click drops + fullscreen gaps) pushed to `dev`.
- CI **all green** on every platform; semantic-release cut **v0.1.0-dev.9** with full asset set
  (Windows setup.exe, Linux x64/arm64 .deb/.AppImage/.rpm, macOS dmg, relay binaries).
- Windows setup.exe (93.6 MB) downloaded + verified to `C:\Users\ahmet\Downloads\pulsar-dev9\`.
  NOT installed (the NSIS installer runs driver installers + a reboot prompt — left for you).
  The running PC client in `C:\Users\Public\Desktop\PulsarDebug\` is the same dev.9 code.
- Pi arm64 AppImage (138 MB) downloaded + verified (valid ARM aarch64 ELF) to `~/Pulsar-dev9.AppImage`.
  NOT swapped in — the working source-build host stays the active one (AppImage render/rkmpp path
  unverifiable without your eyes).

## E2E test PC ↔ Pi (passed)
Auto-connected PC client → Pi host (`--connect 303036449`, host `unattended_access: true`):
- go_online → registered → host caps (h265/h264, rkmpp/software) → native renderer spawned →
  first video RTP forwarded → renderer decoding.
- Screenshot confirmed the Pi desktop rendering on the PC with the live perf HUD:
  **58 fps · 49 ms · decode 8.9 ms · 9.3 Mbps**.
- Both PC client + Pi host left running clean for the morning.

### Note — latency suggests relay, not P2P
49 ms on a same-LAN session (both on 192.168.68.x) points to traffic going through the VPS relay
rather than a direct LAN P2P punch (which should be ~1–2 ms). Worth confirming the LAN hole-punch
actually establishes — relates to bug #5 below + the LAN beacon path.

## Bug hunt — 15 confirmed (adversarially verified, round 1 full; rate-limits cut rounds 2–3)

### CRITICAL
1. **relay/src/lib.rs:331** — Unauthenticated `Register` with a victim's (public) pubkey hijacks
   their DeviceId + token + inbound routing. X25519 pubkeys aren't secret (sent in the clear in
   handshakes). Attacker registers with the victim's pubkey → gets their 9-digit ID + a fresh valid
   token; victim is silently knocked offline and all inbound connects rendezvous to the attacker.
   Fix: prove private-key possession (sign a challenge) or refuse rebinding a live id from a new
   source without proof.

### HIGH
2. **connection/handlers.rs:303** — `Punch` handler adopts an attacker-chosen `peer_addr` with no
   source auth (the `PunchAck` arm has a candidate-source guard; `Punch` doesn't). A forged Punch
   with a known cleartext session id redirects the victim's data path / suppresses relay fallback.
3. **kbdhook/imp.rs:466** — Held remote **mouse buttons** never released on disengage/disable
   (only keyboard keys go into `g.held`/`flush_held`). Disengage mid-drag → button stuck down on
   the host. Fix: track + flush forwarded-but-unreleased mouse buttons.
4. **kbdhook/linux.rs:399** — Kiosk session force-re-engages every loop pass → 3×RightCtrl /
   Ctrl+Alt+Z release combos are undone within ~200 ms; on a Pi appliance you can never disengage.
5. **kbdhook/linux.rs:541** — Leave/overlay/fullscreen combos fire from **ungrabbed (disengaged)**
   devices (gate checks only `is_focused()`, not ENGAGED). Typing Ctrl+Shift+Q in your own
   foreground app while Pulsar is focused-but-not-engaged ends the session.

### MEDIUM
6. **kbdhook/imp.rs:476** — Absolute-coordinate mouse move is suppressed locally but never
   forwarded → dead pointer both sides for tablet/precision-touchpad/VM absolute mice.
7. **kbdhook/linux.rs:591** — xkb modifier state desyncs across overlay-suspend/focus-loss (Shift
   released during overlay is dropped) → wrong-case characters typed on the host after resume.
8. **host.rs:294** — A wrong up-front password that *crosses* the lockout threshold still pops one
   Allow/Deny window (lockout only checked once at the top). Fix: honor `record_failure()`'s bool.
9. **render.rs:559** — `apply_vidsink_rotation` spawns the new renderer without the plays lock; if
   the session tears down during spawn, the child is dropped without kill/wait → orphan pileup
   (the opi5 input-stutter class).
10. **relay/src/lib.rs:197** — `Connect` uses the target's possibly-stale cached addr (up to 30 s
    DEVICE_TTL) for Incoming + session record → blackholed punch after a NAT rebind.

### LOW
11. **kbdhook/linux.rs:620** — `char_keys` leaks across suspend → a later Ctrl+<key> mis-sent as a
    bare Char insert.
12. **host/handlers.rs:1259** — `FileEnd` with `chunks:0` saves a bogus zero-byte file + false
    "received" notification.
13. **host/handlers.rs:1255** — File-chunk accumulation isn't bounded to declared size (64 MiB cap
    only pre-allocates) → a hostile peer can grow host memory unbounded over one transfer.
14. **connection/node.rs:111** — `lan_candidate()` does a blocking std `UdpSocket` bind/connect on
    the async reactor thread on the hot accept path.
15. **relay/src/limits.rs:143** — `parse_rate`/`parse_size` truncate via `as u64`; a fat-fingered
    sub-1-byte/s or tiny value yields a 0 bucket that drops 100% of relayed traffic with no warning.

## Suggested fix order
Security first (1, 2, 10), then the input correctness HIGHs you'll actually hit (3, 4, 5),
then 6–9. 11–15 are hardening.
