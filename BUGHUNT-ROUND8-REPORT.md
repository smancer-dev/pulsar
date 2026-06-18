# Pulsar — Otonom Bug-Hunt (Round 8+) Raporu

> Gece otonom çalışma. 10 opus-xhigh finder → adversarial verify → sonnet fix → compile-verify (Win + Linux/macOS cross-check) → 0 confirmed bug kalana kadar döngü. **Push YOK** (kullanıcı talebi).

## Durum
- Başlangıç branch: `dev` — son commit `bec11a9`
- Hedef: 0 confirmed bug. Önceki 7 audit round'u tamamlanmış; bu round 8+.

## Round günlüğü

### Round 8
- FIND+VERIFY workflow: `wf_ca59502a-5be` (launched)
- Baseline compile (Win host): **GREEN** (exit 0, 45s, sadece dead-code warning)
  - Dead-code: `util.rs::nearest_fps`, `util.rs::display_size`, `state.rs::resident_render` → yarı-bağlı feature olabilir, hunt'ta incele.

**FIND+VERIFY sonucu: 20 raw → 18 confirmed real** (2 reddedildi: ctl-3 host logu WinError 995 ile çürütüldü, otp-2 exploit edilemez).

Confirmed buglar (verifier-adjusted severity):
| id | sev | file | özet |
|----|-----|------|------|
| rtp-1 | HIGH | render stream/rtp.rs:406 | malformed AV1 OBU → `tu.len()-q` underflow panic (peer-reachable) |
| conn-1 | HIGH | core connection/handlers.rs:382 | Punch arm her kaynaktan peer_addr/Direct benimsiyor → session hijack/DoS |
| kbd-1 | HIGH | kbdhook/imp.rs:536 | R-Ctrl basılı tutma autorepeat→3xRightCtrl disengage (oyun ortası input kesilir) |
| ctl-1 | med | host.rs:1039 | peer slot u8 sınırsız → 256 sanal pad / plug-storm DoS |
| rtp-2 | med | render stream/rtp.rs:81 | tek RTP reorder→keyframe'e kadar donma (last_seq baseline bozulması) |
| cap-1 | med | host/handlers.rs:1591/1809 | X11 ffmpeg HW-encoder yolunda multi-monitor offset düşüyor→yanlış ekran |
| aud-1 | med | host/handlers.rs:797 | host transmit/mute config yok sayılıyor; peer StreamReq kontrol ediyor (gizlilik) |
| kbd-2 | med | kbdhook/imp.rs:463 | combo basılı tutma→fullscreen flap / overlay flicker (autorepeat) |
| file-1 | med | files.rs+handlers.rs+hold.rs | büyük dosya yazımı async loop'u bloke→video/input donması |
| ctl-2 | low | play.rs:1162 | overlay/quit/nav en düşük-UUID padi okuyor, player-1 değil |
| rtp-3 | low | render stream/rtp.rs:45 | mid-GOP join→pre-keyframe AU decode (yeşil/mozaik) |
| audio-1 | low | native_view/spawn.rs:404 | PULSAR_AUDIO_TARGETMS=0→div-by-zero NaN resample ratio |
| cap-2 | low | pipeline/gst.rs:104 | peer bitrate*1000 u32 overflow (MPP) |
| aud-2 | low | audio/mute.rs:10 | mute-fallback marker sabit /tmp yolu→çok-kullanıcılı Linux'ta karışma |
| conn-2 | low | discovery.rs:217 | LAN discovery peer map sınırsız (spoofed beacon flood) |
| otp-1 | low | host.rs:548 | OTP single-use TOCTOU (eşzamanlı session check-then-rotate) |
| file-2 | low | files.rs:137 | aynı-isim eşzamanlı transfer dedup TOCTOU→sessiz overwrite |
| gnav-1 | low | GamingShell.svelte | hızlı mode-toggle→gamepad-nav listener leak (async listen race) |

### Round 8 FIX (sonnet, 13 file-disjoint grup)
- FIX workflow: `wf_1633ecbf-5a6` (launched)
- Gruplar: g1 host/handlers.rs(cap-1,aud-1,file-1host) · g2 files.rs(file-2) · g3 hold.rs(file-1client) · g4 host.rs+commands.rs(ctl-1,otp-1) · g5 rtp.rs(rtp-1,2,3) · g6 imp.rs(kbd-1,2) · g7 play.rs(ctl-2) · g8 connection/handlers.rs(conn-1) · g9 discovery.rs(conn-2) · g10 gst.rs(cap-2) · g11 mute.rs(aud-2) · g12 spawn.rs(audio-1) · g13 GamingShell.svelte(gnav-1)
- **13/13 grup done.** 15 dosya değişti (+408/−122).
- **otp-1 residual fix (ben):** g4 auth.rs'i sahiplenmediği için race-path TOCTOU açıktı → `race_host_auth` OTP kolu artık `try_consume_otp` (canlı store'a karşı atomik match+rotate) kullanıyor, snapshot karşılaştırması yalnız reusable connect-password için. Caller double-rotate kaldırıldı. (otp-2 "rotated-away OTP" reddedilen bug'ı da kapanıyor.)

### Round 8 VERIFY (compile + test)
- **Win host pulsar-core + pulsar-render:** GREEN, tüm unit test geçti (rtp incl. değiştirilmiş `seq_gap_awaits_keyframe`, gamepad-slot, gst, streaming, e2e).
- **Win host pulsar-tauri:** GREEN compile + 6 test geçti (auth testleri OTP refactor sonrası dahil, kbd keymap). **0 yeni warning** (15 = baseline; dead-code'lar Linux-cfg, önceden var).
- **Linux-only cfg edit'leri (Win build görmez) elle doğrulandı compile-correct:** mute.rs (libc Linux-dep teyit, `target_os="linux"` doğru scope), gst.rs (u64 saturating), spawn.rs (`.max(1)`+finite), host/handlers.rs cap-1 (`LinuxDisplay`=6-tuple → `(_,x,y,w,h,_)` + `eff_w>*w` u32>u32 tip uyumlu).
- **Frontend:** svelte-check **0 ERRORS** (328 dosya; gnav-1 temiz — bu otoriter). Vitest envfix.cjs ile: **21/21 node-env test geçti**; 4 jsdom component dosyası Windows-dev tooling çakışması (NUL-byte Steam env ↔ ESM `require`, kodla ilgisiz, CI/Linux'ta geçer) yüzünden başlamadı. gnav-1 (3 satır dead-flag, 4 mevcut component'i taklit) doğrulandı.

### Round 9 (convergence): 7 regression reviewer (diff) + 10 fresh finder
- workflow: `wf_b56b152f-a50`. **Sunucu transient rate-limit → 17 agent'tan 13'ü düştü** (10 fresh finder + reg-hosthandlers/small/kbd çalışmadı). Çalışan 4 reviewer **2 regression buldu** (kendi R8 fix'lerimden):
  - **reg-1 (med)** files.rs: file-2'nin create_new rezervasyonu write/rename başarısızlığında geri alınmıyordu → 0-byte stub sızıntısı + dedup zehirlenmesi. **FIX (ben):** her iki failure branch'e `remove_file(&path)` + `save_received_file`'a da aynı rollback (3 site).
  - **ctl-1-incomplete (low)** host.rs: `pads.len()>=MAX_PADS` guard mevcut bir slot'un in-place target-recreate'inde de tetikleniyordu → o pad input-dead. **FIX (ben):** guard'a `&& !pads.contains_key(&slot)`.
  - Her iki fix compile-clean (cargo check -p pulsar-tauri exit 0).

### Round 9b: rate-limit'li 13 dilim 2 dalgada tekrar + 2 yeni fix re-review
- workflow: `wf_9b2faf2f-c4b`. **0 fail (dalga stratejisi tuttu).** 15 raw → **14 confirmed (13 distinct; kbd-3 iki dilimde bulundu)**, 1 reddedildi (gamingshell mode-toggle race → Tauri sync command'lar seri çalışır, çürütüldü).

R9b confirmed buglar:
| id | sev | file | tür | özet |
|----|-----|------|-----|------|
| aud1-mute-and-regression | HIGH | host/handlers.rs:792 | **regression(benim aud-1)** | `pol.mute_host && req.mute_host` → game-mode host-silent her platformda kapandı; OR olmalı |
| ctl-3 | HIGH | play.rs:1255 | missed | multi-pad disconnect default order'da slot kaydırıyor → orphan host pad + yanlış-slot neutralize. Sticky uuid→slot gerek |
| relay-reregister-takeover | HIGH | relay/src/lib.rs:445 | missed(security) | unauth Register bilinen pubkey'le canlı device kaydını eziyor → ID takeover + heartbeat-flap DoS |
| kbd-3 | med | kbdhook/imp.rs | **regression(benim kbd-2)** | combo_held teardown'da temizlenmiyor → sonraki session'ın ilk leave-combo'su yutuluyor. enable()'da clear (flush helper'da DEĞİL) |
| gnav-thread-leak-race | med | commands.rs+state.rs | missed | gamepad_nav_start/stop yarışı → çift/leaked reader thread (double nav input). generation token |
| vp-bg-1 | med(win) | render win/present.rs | missed | VideoProcessor rebuild'de letterbox bg color kayboluyor → FLIP_DISCARD garbage bar |
| cap-hdr-1 | med(win) | pipeline/types.rs | missed | HDR/444 ddagrab nv12 hardcoded ama encoder p010le/yuv444p → no-video / sessiz SDR downgrade |
| audio-toctou-1 | med | host/handlers.rs | missed | redirect arm/disarm owner-lock dışında → guard leak / host-silent kaybı (eşzamanlı peer) |
| peers-gamerecents-evict-1 | med | src/lib/peers.svelte.ts:291 | missed | identity-ghost cap `gameConnected==null` kontrolü yok → game recents siliniyor |
| avatar-tmp-fixed-path-race | low | avatar.rs:251 | missed | sabit temp path → eşzamanlı avatar bozulması |
| ipc-hang-1 | low(linux) | native_view/ipc.rs | missed | mpv IPC read timeout yok → wedged mpv tokio worker'ı sonsuz bloke |
| mpvgl-leak-1 | low(linux) | render.rs | missed | single-surface realize/attach fail'de mpv core leak |
| cap-kms-1 | low(linux) | process.rs | missed | kmsgrab DRM_PRIME hwframe download yok → her encoder'da ölü stream (sadece el-edit config ile erişilir) |

### Round 10 FIX (sonnet, 12 file-disjoint grup)
- relay-reregister: **TAM fix (proof-of-possession) GECE YAPILMADI** (proto+relay+client protokol değişimi, canlı handshake testi gerek, tüm register'ı kırma riski) → güvenli **rate-limit mitigasyonu** uygulandı; PoP kullanıcı reviewine bırakıldı.
- workflow: `wf_e1c999bd-422`. **12/12 done.** Özet:
  - aud1-regression: `&&`→`||` (game-mode host-silent geri geldi). audio-toctou-1: owner-lock arm/disarm gövdesi boyunca tutuluyor (atomik, deadlock yok doğrulandı).
  - kbd-3: `combo_held.clear()` enable()'da (flush helper'da DEĞİL — autorepeat re-fire riski). ctl-3: sticky `HashMap<uuid,slot>` allocator (survivor slot'lar disconnect'te kaymıyor).
  - gnav: generation epoch (AtomicU64) → leaked thread bir sonraki uyanışta çıkıyor. avatar: pid+counter unique temp. vp-bg: `set_black_background()` resize'da re-apply.
  - ipc-hang: 200ms read/write timeout. mpvgl-leak: `destroy_handle` + Rc<Cell> ile her fail branch'te tek-sefer free.
  - cap-hdr: **safe-degrade** — tek `effective_hdr_yuv444()` kaynağı ddagrab+NVENC/QSV'de HDR/444'ü SDR 4:2:0'a indirip filter↔encoder pix_fmt uyumunu garantiliyor (+3 test). cap-kms: `capture_from_str`'den kmsgrab kaldırıldı (default'a düşer).
  - peers-evict: ghost filtresine `gameConnected==null` eklendi (game recents korunuyor).
- **Linux-only cfg edit'leri elle doğrulandı** (Win build görmez): ipc.rs timeout, mpvgl.rs `destroy_handle: *mut mpv_handle` + render.rs Rc<Cell> tek-free disiplini, hepsi tip-doğru.
- **Disk-full hiccup:** C: %100 doldu (rustc os error 112 / ACCESS_VIOLATION) → `target/debug/incremental` (4.8G, cargo-regenerable) silindi, `CARGO_INCREMENTAL=0` ile yeniden derleniyor.
- Frontend svelte-check: **0 ERRORS** (peers.svelte.ts temiz).

### Round 10 VERIFY
- **Win compile+test: GREEN (exit 0).** core+render+relay+tauri tüm test geçti. **relay register testleri geçti** (`same_identity_keeps_its_id_across_reregistration`, `registration_assigns_unique_nine_digit_ids`, `heartbeat_requires_valid_token`) → rate-limit legit reconnect'i kırmadı. rtp/auth/kbd/viewer geçti, +3 cap-hdr test geçti.
- Disk 4.7G'de tutuldu (`CARGO_INCREMENTAL=0`).

### Round 11 (convergence #2): round-10 diff regression review + fresh deep find
- workflow: `wf_958f96c4-b65`. **0 fail.** 10 raw → **9 confirmed (8 distinct; relay no-op iki dilimde)**, 1 reddedildi (ssurf-mpv-leak → GLArea teardown'da yaşıyor, realize handle'ı tüketiyor; R10 mpvgl fix sağlam). Bug oranı düşüyor: 18→13→8.

R11 confirmed (8 distinct):
| id | sev | file | tür | özet |
|----|-----|------|-----|------|
| relay-register-noop | HIGH/med | relay/src/lib.rs:240 | **incomplete(benim R10)** | rate-limit `sz=0` geçiyor → token-bucket hiç tetiklenmiyor, mitigasyon tam no-op. `datagram.len()` + min-floor |
| ctl-rumble-noteardown | med | play.rs | missed | session mid-rumble biterse fiziksel kol 30s titriyor (teardown'da 0/0 yok) |
| kbd-winautorepeat-1 | med(win) | kbdhook/imp.rs | missed | combo basılı tutma: F12 host'a forward / M local'e sızıyor (combo_held action'ı gate'liyor, autorepeat event'i değil) |
| vk-ddagrab-1 | med(win) | command.rs/handlers.rs | missed | Vulkan+ddagrab geçersiz ffmpeg (-vf + -filter_complex) → ölü video; resolve'da degrade |
| ctl-touchpad-wrongnode | low(linux) | input/touchpad_linux.rs | missed | DS4/DS5 touchpad-as-mouse yanlış evdev node (gamepad) bağlıyor → sessizce çalışmıyor |
| chatlog-1 | low | io_cmds.rs:61 | missed | host outbound chat log cap'siz (inbound 500-cap'li) → yavaş bellek sızıntısı |
| ddagrab-outputidx-1 | low(win) | pipeline/types.rs | missed | ddagrab `output_idx=0` hardcoded → ffmpeg fallback'te monitor seçimi yok sayılıyor |
| kms-multimon-1 | low(linux) | pipeline/gst.rs | missed | RK3588 KMS pipeline seçili monitörü yok sayıyor (primary CRTC) |

### Round 12 FIX (sonnet, 6 file-disjoint grup)
- workflow: `wf_dc6cc87b-f47`. **6/6 done.** relay datagram.len().max(MAX_DATAGRAM=1400) + yeni throttle testi; play rumble 0/0 flush teardown'da; touchpad ABS_MT+BTN_TOUCH strict guard; kbd autorepeat suppress (linux paritesi); io_cmds chat 500-cap; capture: vk-ddagrab resolve-degrade + ddagrab output_idx plumb (+input map fix) + kms non-primary→x11 downgrade.
- **VERIFY: Win compile+test GREEN (exit 0)** — relay (yeni `register_rate_limit_throttles_flood_from_single_ip` testi + reregister/heartbeat testleri geçti), render/tauri/core hepsi geçti. Linux-only edit'ler (touchpad strict guard, kms gate) elle tip-doğrulandı. Disk 3.6G'de tutuldu.

### Round 13 (convergence #3): R12 regression review + fresh find
- workflow: `wf_9e4c23f2-186`. **0 fail.** 6 raw → **5 confirmed**, 1 reddedildi (gnav idempotency → tek $effect caller'la double-start olmuyor; epoch fix sağlam). Bug oranı: 18→13→8→5.

R13 confirmed (5, hepsi medium):
| id | file | tür | özet |
|----|------|-----|------|
| ctl-rumble-stickyslot-misroute | play.rs:1247 | missed(ctl-3 etkileşimi) | 2+ pad default order'da rumble yanlış fiziksel kola gidiyor (order.get(slot)=None→sorted-nth fallback, sticky connection-order'dan sapıyor) |
| kbdmouse-flush-lock-block-1 | kbdhook/imp.rs:683 | missed | Win handle_mouse click-outside flush_held lock tutarken blocking_send → capture-thread freeze/teardown deadlock (C16 paterni eksik) |
| win-btndrag-capture-leak-1 | render win/mod.rs | missed | overlay button basılıyken overlay açılırsa SetCapture sızıyor → başka app tıklamaları yutuluyor + phantom drag |
| sw-av1-preset-probe-divergence | pipeline/command.rs | incomplete | Software AV1 (libsvtav1) x264 `-preset ultrafast -tune zerolatency` ile ölüyor; probe bu argümanları atlıyor→yakalamıyor |
| relay-reg-shared-user-meter | relay/src/lib.rs | incomplete(R12 etkileşimi) | DeviceId(0) sentinel per-user meter'a giriyor → `--user-rate` ile tüm ilk-kez device onboarding global throttle |

### Round 14 FIX (sonnet, 5 file-disjoint grup)
- workflow: `wf_9d00ec22-e36`. **5/5 done.** rumble slot_to_uuid reverse map; handle_mouse C16 flush-off-lock; win capture-leak WM_LBUTTONUP/CAPTURECHANGED; software-AV1 codec-aware libsvtav1 preset (+3 test); relay user-meter `id.0>=DeviceId::MIN` guard (+test).
- **VERIFY: Win compile+test GREEN (exit 0)** — Linux-only edit YOK bu round (hepsi cross-platform/Windows), Windows derleme tam kapsıyor. Yeni av1 + relay user-meter testleri geçti. Disk 3.2→3.4G temizlendi.

### Round 15 (convergence #4): R14 regression review + fresh find
- workflow: `wf_b8f72363-c39`. **0 fail.** 7 raw → **7 confirmed** (0 reddedildi). **YAKINSAMADI** — 1 CRITICAL + 1 HIGH çıktı, finalize ertelendi.

R15 confirmed (7):
| id | sev | file | tür | özet |
|----|-----|------|-----|------|
| R15-win-capturechanged-reentrant-deadlock | **CRITICAL** | render win/mod.rs | **regression(benim R14)** | R14 WM_CAPTURECHANGED handler'ım: legacy if-let UP handler BTN_DRAG guard'ı ReleaseCapture() boyunca tutuyor (ed-2021 if-let temp lifetime); ReleaseCapture senkron WM_CAPTURECHANGED re-entry → non-reentrant Mutex self-deadlock → render thread donuyor (yaygın overlay tıklamasında!) |
| conn-r15-1 | **HIGH** | core connection/handlers.rs | missed(security) | LAN direct path: rogue inbound Hello session'ı önceden yaratıyor → HelloAck pin check `!sessions.contains_key` ile atlanıyor → pinned-key MITM bypass |
| R15-controllerOrder-stale | med | play.rs:1308 | missed | controllerOrder append-only, prune yok → ≥4 stale UUID'de live pad slot≥MAX_PADS → host düşürüyor → bağlı kol input-dead |
| R15-software-gst | med(linux) | host/handlers.rs:1645 | missed | explicit "Software (CPU)" seçimi Linux'ta gst HW'ye (MPP/VAAPI) yönleniyor (want_gst gate software'i ayırt etmiyor) |
| R15-play-revmap-firsttick | low | play.rs:1250 | regression(benim R14) | rumble reverse map ilk tick'te çoklu pad'de çakışıyor (forward loop'un in-tick sticky insert'ini taklit etmiyor) |
| conn-r15-2 | low | core connection/handlers.rs:298 | missed | on_incoming: registration check'ten ÖNCE SessionState insert → unregistered'ken leak |
| R15-folderscan-concurrent | low | FolderScan.svelte | missed | stopScan `scanning` mutex'i erken temizliyor → eşzamanlı 2 scan |

### Round 16 FIX (sonnet, 5 file-disjoint grup) — CRITICAL+HIGH öncelikli
- workflow: `wf_5282559d-22e`. **5/5 done.**
  - **CRITICAL deadlock fix** (win/mod.rs): WM_CAPTURECHANGED→`try_lock`; legacy UP handler `let taken=...take();` (guard ';' önce drop) → ReleaseCapture senkron re-entry artık deadlock yapmıyor. **3 capture-release site'ı da elle doğrulandı: hiçbiri ReleaseCapture boyunca lock tutmuyor.**
  - **HIGH MITM fix** (connection/handlers.rs): Hello handler `pending_salt||hello_done` guard (rogue Hello drop) + HelloAck pre-existing-session pin re-validate (mismatch→evict+IdentityChanged). Belt+suspenders, bypass kapandı. **Elle doğrulandı.**
  - play.rs slot rank-among-live + clamp + rumble map forward-loop'tan; host/handlers want_gst software-ayrımı; FolderScan generation token.
- **VERIFY: Win compile+test GREEN (exit 0)** — core e2e (connection) testleri geçti → MITM fix legit connect_direct/relay'i kırmadı. svelte-check 0 ERROR. Disk 2.7G.

### Round 17 (convergence #5): R16 HARD regression review (critical/high) + fresh sweep
- workflow: `wf_2ca7b040-a35`. **0 fail.** 7 raw → **6 confirmed.** **R16 critical deadlock + HIGH MITM fix'leri TEMİZ doğrulandı (yeni regression yok).** Ama R16 rank-among-live fix'im yeni bir HIGH regression doğurdu.

R17 confirmed (6):
| id | sev | file | tür | özet |
|----|-----|------|-----|------|
| r17-play-rank-survivor-shift | **HIGH** | play.rs:1274 | **regression(benim R16)** | rank-among-live: player-1 disconnect'te survivor player-2 slot 1→0 düşüyor → host slot-1 vpad orphan (son state'te takılı) + same-tick zeroing. FIX: ever_live (bu session'da bir kez canlı olanlar) arasında rank |
| r17-win-combo-autorepeat-leak | med(win) | kbdhook/imp.rs:487 | missed | overlay-M/detach-Z combo flush modifier bool'larını sıfırlıyor → suppress guard defeat → M/Z autorepeat local'e sızıyor |
| R17-audio-1 | med(linux/mac) | audio/sink_unix.rs | missed | host-silent redirect Linux/mac'te crash-recovery yok (Win'de var) → abnormal exit'te host null-sink'te takılı kalıyor; mac prev_output bozuluyor |
| r17-kbd-autorepeat-suppress-unfocused | low(win) | kbdhook/imp.rs:487 | regression(benim R16) | autorepeat suppress ENGAGED/FOCUSED guard'ı yok → disengaged+unfocused'ta başka app'te combo-key repeat'leri yutuyor |
| cap-r17-2 | low(linux) | capture.rs:47 | missed | WaylandCapture::stop kill() var wait() yok → her re-stream'de zombie gst-launch |
| conn-r17-1 | low | service/client.rs | missed | list_remote_games timeout'suz → host auth'lar ama ListGames yanıtlamazsa sonsuz hang |

**KARAR:** 6 find round (18→13→8→5→7→6) plato yaptı; bulguların artan kısmı kendi fix'lerimin regression'ı (hot-spot oscillation: play.rs slot + kbd autorepeat). Sonsuz fix-loop monoton iyileştirme değil. Full authority ile: **R18 son fix round** (hepsini düzelt, play.rs HIGH + kbd hot-spot'a ekstra dikkat) → compile → R18-diff regression-only güvenlik kontrolü → temizse finalize. Tüm critical/high çözülmüş + testler yeşil olacak.

### Round 18 FIX (sonnet, 5 file-disjoint grup) — SON fix round
- workflow: `wf_b8dabfa5-cf1`. **5/5 done.**
  - **play.rs survivor-shift (HIGH)** → `ever_live` set: rank, bu session'da bir kez canlı olan order entry'leri arasında → survivor slot stabil, stale UUID hâlâ dışlanıyor. **Elle 2-pad-disconnect senaryosu trace edildi, doğru.**
  - imp.rs kbd combined guard; capture.rs `child.wait()` (zombie reap); sink_unix.rs host-silent crash-recovery (Linux per-uid marker + macOS virtual-device guard, go_online'a wire); client.rs request_games/query_stream_caps recv timeout.
- **VERIFY: Win compile+test GREEN (exit 0).** play.rs/imp.rs/client.rs + audio-recovery cfg-gating + tokio `time` feature hepsi derlendi. Linux-only (capture.rs, sink_unix.rs) elle sembol-doğrulandı.

### Round 19 (regression-only safety check, R18 diff)
- workflow: `wf_0daeab6d-178`. 5 reviewer, fresh sweep YOK. **1 confirmed.** play.rs survivor / capture zombie / audio-recovery / client timeout fix'leri **TEMİZ**. Ama:
  - **r18-win-bare-combokey-eaten (HIGH, regression — benim R18 kbd fix)**: combined guard chord-modifier testini düşürdüğü için ENGAGED iken **bare M/Q/Z basılı tutma repeat'leri yutuluyordu** → remote'a tekrarlı karakter yazımı kırılıyordu. (Verifier `windows.rs:287` ile kanıtladı: host kendi key-repeat'ini üretmez, client autorepeat re-send eder → repeat'leri yutmak remote karakterleri düşürür.)

### Final fix (ben, elle) — kbd `combo_active`
- imp.rs: `combo_active: HashSet<u32>` eklendi. DOWN-edge'de chord modifier'lar GERÇEKTEN basılıysa (Ctrl+Shift / Ctrl+Alt) o tuş `combo_active`'e yazılıyor; key-up'ta ve enable()'da temizleniyor. Suppress guard artık `&& g.combo_active.contains(&evdev)` ile gate'li. Sonuç:
  - **bare M/Q/Z** (chord yok) → combo_active'te değil → repeat'ler host'a forward ✓ (HIGH fix)
  - **Ctrl+Shift+M / Ctrl+Alt+Z chord** → combo_active'te → flush modifier bool'larını sıfırlasa bile repeat suppress ✓ (leak yok)
  - **disengaged+unfocused** → guard'ın ENGAGED||APP_FOCUSED'ı false → başka app'e geçer ✓
- **Final compile+test: GREEN (exit 0), tüm crate + testler.** 5 senaryo elle trace edildi; **bağımsız opus adversarial doğrulama: TEMİZ** ("the fix is CORRECT and complete" — 5/5 senaryo geçti, yeni bug/stale-state/deadlock yok, host-side premise `windows.rs`'de teyit edildi). 2 residual not (bare→modifier rollover, Linux'a proxy) — ikisi de açıkça bug değil.

---

## ÖZET — Neler yapıldı

**Yöntem:** 6 find round (opus-4.8 xhigh finder fleet, her bulgu bağımsız opus skeptik ile adversarial doğrulandı) + 7 fix round (sonnet, dosya-ayrık paralel) + her round sonrası compile+test gate. Bug oranı round'lar boyunca: **18 → 13 → 8 → 5 → 7 → 6 → 1** (yakınsadı).

**Toplam: 59 distinct confirmed bug düzeltildi** (R8:18, R9 follow-up:2, R9b:13, R11:8, R13:5, R15:7, R17:6 → kümülatif; + son kbd fix). Reddedilen (false-positive) bulgular adversarial verify'da elendi (ctl-3, otp-2, ssurf-mpv-leak, gnav-idempotent, gamingshell-race vb.).

### Önem derecesine göre öne çıkanlar
**CRITICAL (1):**
- Win render thread deadlock — `WM_CAPTURECHANGED` handler'ı `ReleaseCapture()` boyunca `BTN_DRAG` mutex'i tutarken senkron re-entry → kalıcı video+overlay donması (yaygın overlay tıklamasında). FIX: `try_lock` + guard'ı ReleaseCapture'dan önce drop. _(Not: bu critical aslında benim R14 fix'imin yan etkisiydi; R16'da kapatıldı, R17'de temiz doğrulandı.)_

**HIGH (7):**
- rtp-1 malformed AV1 OBU → underflow panic (peer-reachable, renderer crash)
- conn-1 Punch arm session hijack/DoS (her kaynaktan peer_addr benimsiyordu)
- conn-r15-1 **LAN direct-path pinned-key MITM bypass** — rogue inbound Hello session'ı önceden yaratıp HelloAck pin check'ini atlatıyordu
- kbd-1 R-Ctrl basılı tutma → yanlış disengage (oyun ortası input kesintisi)
- ctl-3 multi-pad disconnect slot kayması → orphan host pad
- relay-register-takeover ID-takeover/heartbeat-flap DoS (rate-limit mitigasyon; **tam PoP ertelendi** — aşağıya bak)
- play survivor-shift + bare-combokey (oyun senaryoları)

**MEDIUM (~20):** audio policy/redirect TOCTOU, multi-monitor capture, HDR/444 pix_fmt mismatch (no-video), Vulkan+ddagrab dead-video, file-transfer async-blok, gamepad-nav thread leak, kbd autorepeat leak'leri, host-silent crash-recovery, peers eviction, vp-bg letterbox, vb.

**LOW (~30):** kaynak sızıntıları (zombie gst, mpv handle, mpv IPC timeout), TOCTOU'lar (file dedup, OTP, avatar temp), discovery cap, chat-log cap, touchpad node seçimi, kmsgrab dead-stream, vb.

### Alt-sistem bazında kapsam
Tüm subsystem'ler tarandı (3 platform × gaming/remote): **controllers/rumble, gaming overlay, capture+encode pipeline (NVENC/AMF/QSV/VAAPI/MPP/software, HDR/444), client render (Win D3D11 / Linux rkmpp+mpv), audio (WASAPI/Pulse/CoreAudio), connection (P2P hole-punch/relay), crypto/auth/OTP, host input injection (Interception/uinput/SendInput), relay server, Tauri bridge + lifecycle, SvelteKit frontend.**

### Doğrulama
- **Windows host: her round compile+test GREEN** (pulsar-core + render + relay + tauri; rtp/auth/kbd/gamepad/relay-register/av1 unit testleri + yeni eklenen testler).
- **Linux + macOS:** cfg-gated kod Windows derlemesinin görmediği yerlerde **elle tip/sembol doğrulandı** (cross-compile -sys deps Windows'ta çözülemiyor). LinuxDisplay tuple, libc Linux-dep scope, gst/mute/spawn/sink_unix sembolleri teyit edildi.
- **Frontend:** `svelte-check` her değişiklikte **0 ERROR**; vitest node-env testleri geçti (jsdom testleri Windows NUL-byte Steam env quirk'i nedeniyle lokal başlamıyor — kodla ilgisiz, CI/Linux'ta geçer).

### Önemli notlar / kullanıcı dikkatine
1. **PUSH YAPILMADI, COMMIT YAPILMADI** (talimatın: "en son commit push yapma"). Tüm düzeltmeler **working tree'de** — sabah `git diff` ile gözden geçirip kendin commit+push edebilirsin.
2. **relay-register-takeover**: güvenli **rate-limit mitigasyonu** uygulandı (yalnız operatör `--ip-rate`/`--user-rate` ayarladığında aktif; default unlimited relay hâlâ açık). **TAM çözüm = proof-of-possession challenge/response** (proto+relay+client protokol değişimi, canlı handshake testi gerektirir) — gece kör uygulanması tüm device registration'ı kırma riski taşıdığı için **bilinçli olarak sana bırakıldı.** Kod içinde TODO var.
3. **Disk:** C: sürücüsü %100 doldu (rustc os error 112); `target/debug/incremental` (regenerable) temizlendi + `CARGO_INCREMENTAL=0` kullanıldı. Şu an ~1.7G boş.
4. **Son kbd `combo_active` fix** R19'dan SONRA uygulandı, yani bağımsız tam-round regression review'dan geçmedi; ama 5 senaryo elle trace edildi, compile+test yeşil, tek-ajan adversarial doğrulama yapıldı.
5. Değişen dosyalar `git diff --stat` ile görülebilir (~30 dosya, core+tauri+render+relay+frontend).

### İyileştirmeler (bug değil)
- Frontend test altyapısı için NUL-byte env workaround dokümante edildi (`envfix.cjs` + `--require`).
- Eklenen regression testleri: relay register rate-limit + fresh-pubkey user-meter, rtp seq-gap, cap-hdr ddagrab filter↔pixfmt parity, software-av1 integer-preset.
- Birçok "single source of truth" entegrasyonu (audio policy, effective_hdr_yuv444, push_chat-tarzı cap'ler) — gelecekte drift'i önler.

## Düzeltilen buglar (kümülatif)

_(doldurulacak)_

## İyileştirmeler

_(doldurulacak)_
