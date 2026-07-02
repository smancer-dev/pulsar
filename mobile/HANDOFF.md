# Pulsar Mobile — Session Handoff

Pulsar mobil app (Android, Tauri 2). Transparent webview (UI: `desktop-app/mobile/ui`, static JS)
over a native video surface (Kotlin plugin). Rust core under `desktop-app/mobile/src`.
UI dili **Türkçe**. Kullanıcı Türkçe yazar. **Caveman mode aktif** (terse cevap; ama kod/commit/PR normal).

Test cihazı: **Samsung Galaxy Z Fold4**, wireless adb. IP: **`192.168.68.58:<port>`** — **port SIK SIK döner**
(kablosuz-debug oturumu/ekran uykusu → yeni port; örn. 44927→36333→düştü). mDNS yok, auto-discover yok →
kullanıcı her düşüşte Ayarlar → Geliştirici → Kablosuz hata ayıklama → "IP & port"u verir. Ekran açık tut.

---

## Build / Deploy / Test mekanizması (KRİTİK)

- **Çalışma dizini:** `desktop-app/mobile/ui` (UI), `desktop-app/mobile` (rust), `desktop-app/crates/tauri-plugin-pulsar-video` (Kotlin plugin).
- **DEV BUILD webview'i UI'ı HOST'taki dev-server'dan yükler** (baked `devUrl`,
  son build'de `http://192.168.68.51:1430` — USB-eth IP, /22). `tauri.conf.json`'da
  devUrl YOK; `bun run tauri android dev` build sırasında host IP'sini bake eder.
  **"disk-live tauri.localhost" YANLIŞ** — yükleme HOST sunucusuna bağlı.
- **JS/CSS değişikliği = host'ta sunucu AYAKTA olmalı + relaunch** (rebuild YOK):
  - Sunucu ölmüşse webview `Failed to request http://...:1430/` hata sayfası gösterir
    (body boş, script yok). `bun run tauri android dev` öldüyse sunucu da ölür.
  - **Hızlı yol (rebuild'siz):** `ui/`'ı statik serve et — native shell zaten kurulu,
    `__TAURI__`'yi her sayfaya inject eder, sadece HTML/JS lazım:
    ```bash
    cd desktop-app/mobile/ui && setsid python3 -m http.server 1430 --bind 0.0.0.0 &
    ADB=~/Android/Sdk/platform-tools/adb; DEV=192.168.68.58:44927
    $ADB -s $DEV shell am force-stop dev.pulsar.app; sleep 1
    $ADB -s $DEV shell am start -n dev.pulsar.app/.MainActivity
    ```
    Doğrula: phone .58 erişimi `$ADB -s $DEV shell 'printf "GET / HTTP/1.0\r\n\r\n"|nc 192.168.68.51 1430|head -1'` → `200 OK`.
    Sunucu access-log'unda phone IP (.58) görmeli; sadece host IP görürsen webview hâlâ ulaşamıyor.
  - Phone hem .51 (USB-eth) hem .72 (wifi) host IP'lerine ulaşır (ping+nc OK); ama
    webview SADECE baked IP'yi (.51) ister — IP değiştiyse rebuild gerek.
  App yüklenmesi **>8s** sürer (7s'de bazen about:blank/hata sayfası görünür, normal).
- **Rust VEYA Kotlin değişikliği = full rebuild:**
  ```bash
  cd desktop-app/mobile && touch src/lib.rs   # plugin komutu eklediysen ACL re-bake
  export JAVA_HOME=/usr/lib/jvm/java-17-openjdk ANDROID_HOME=$HOME/Android/Sdk
  export NDK_HOME=$HOME/Android/Sdk/ndk/27.2.12479018 ANDROID_NDK_HOME=$NDK_HOME
  export PATH=$JAVA_HOME/bin:$ANDROID_HOME/platform-tools:$HOME/.bun/bin:$PATH
  ANDROID_SERIAL=192.168.68.58:41705 bun run tauri android dev   # setsid + log'a yönlendir
  # "Performing Streamed Install" + "Success" + "Starting: Intent" = bitti
  ```
  Rebuild ~2-4 dk. Eski dev process'i önce `kill` et.
- **Rust compile-check (hızlı):** `cd desktop-app/mobile && rustup run stable cargo check --lib`
  (plugin: `cargo check -p tauri-plugin-pulsar-video`). Distro cargo eski → **`rustup run stable cargo`**.
- **JS syntax-check:** `cd ui && bun build js/<file>.js --target=browser`.
- **Relay config** sürekli set gerekir (relaunch sonrası kalsa da bazen): PC'de 2 IP var
  (.72 wifi, **.51 USB-eth**); telefon **.51**'e ulaşır. `set_config({cfg:{relay:'192.168.68.51:21116'}})`.

### CDP ile webview debug + test (ANA test aracı)
```python
# adb forward localabstract:webview_devtools_remote_<pid> → tcp:PORT
# /json'dan type=="page" olanı seç (tauri.localhost). Raw websocket + Runtime.evaluate.
# ÖNEMLİ: awaitPromise=True olan eval, çözülmeyen promise dönerse TÜM python'u asar (timeout).
#   Riskli invoke'larda awaitPromise=False kullan VEYA .catch(()=>0) ekle.
# Sahte gamepad: navigator.getGamepads override. Sahte touch: new TouchEvent + new Touch({target,...}).
# Test sonrası relaunch et (override'ları temizlemek için).
```
Pattern repo geçmişinde (jsonl transcript) bolca var — kopyala.

- **Firewall:** dev-server/portlar için bazen `sudo firewall-cmd --add-port=...` gerekir — **kullanıcı çalıştırır** (assistant'a denied).
- **Screencap SurfaceView'i yakalayamaz** (hardware overlay) — gerçek cihazda video'yu screenshot ile göremezsin. Emülatör TextureView kullanır (yakalanır) ama -gpu host'ta yeşil.

---

## Mimari (özet)

- **Input forwarding (ÖNEMLİ):** Aktif yol **`ui/js/app.js` → `wireTouchForwarding()`** — `window`-level
  touchstart/move/end → `send_pointer`/`send_button`. `ui/js/session/input.js` ayrı bir touch-engine
  (touch-overlay z=9) ama **mount() ÇAĞRILMIYOR** → pasif. Forward bug ararken app.js'e bak, input.js'e değil.
- **Video:** Kotlin plugin (`PulsarVideoPlugin.kt`), `Pane` per slot. **Artık her yerde TextureView**
  (eskiden gerçek cihaz SurfaceView idi; pinch-zoom Matrix gerektirdi + screencap için). `Pane.surfaceView: View?`
  (isim eski, tip TextureView). `applyAspect` fit rect'i hesaplar→`applyTransform` Matrix uygular + `video-size`
  emit eder. `applyTransform` zoom/pan'i `setTransform(Matrix)` ile yapar. Bkz. pinch-zoom bölümü.
- **Plugin komut zinciri (JS→native):** build.rs COMMANDS + permissions/default.toml `allow-<cmd>` +
  Kotlin `@Command fun camelCase` + commands.rs `#[command]` + lib.rs generate_handler + mobile.rs ext
  (`run_mobile_plugin("camelCase")`) + desktop.rs noop stub. JS invoke: `plugin:pulsar-video|snake_case`.
  Args: `@InvokeArg class XArgs` (Kotlin) + `#[derive(Serialize)] struct XArgs` (mobile.rs).
- **Overlay:** `ui/js/session/overlay.js` — dock (z=15) + backdrop (z=14) + FAB (z=16). `body.overlay-open`
  + `body.fab-dragging` class'ları input'u gate'ler.
- **prefs.js** (`pulsar.prefs.v1` localStorage) client-only; config store'dan AYRI (get_config düşürür).

---

## Bu session'da YAPILAN (hepsi deploy + doğrulandı)

1. **Bağlı kontrolcüler:** Settings→Kontrolcüler + overlay kartı + connect/disconnect toast (isim+şarj).
   Kaynak **native `gamepad_battery`** (InputDevice enum), Web Gamepad API DEĞİL (gesture ister, WebView'de güvenilmez).
2. **Emülasyon hedefi:** kol başına Auto(Xbox 360)/Xbox 360/DualShock 4 seçici (prefs `gamepadTargets`).
   `send_gamepad`'e `target` param eklendi → `EmulationTarget`.
3. **Settings: overlay tuşu kapama** toggle (kapalıyken 3-parmak tap ile overlay açılır).
4. **Kol ile UI navigasyonu** (`ui/js/gamepad-nav.js`): spatial D-pad/stick, A=seç, B=geri, LB/RB=sekme,
   **sağ stick=scroll**, focus ring. İçerik nav'ı **aktif `main.scroll`'a scope'lu** (kapalı sheet'leri
   eler). Bottom nav içeriğin sonunda erişilir. `.toggle` input'u atlanır (görünür label odaklanır).
5. **B (geri) açık popup'ı kapatır** — `_closeTopPopup` tüm türleri yakalar (`.sheet-backdrop`/`.overlay-backdrop`/
   **`.sheet-overlay`** [devices add-sheet]/`.sheet`/`.overlay-dock`).
6. **Connect mode noktalar→ikon:** `index.html` m-remote/m-game `<span class="dot">` → monitor/gamepad SVG.
7. **Overlay FAB düzeltmeleri:** z 8→16 (touch-overlay z=9 üstüne), kenarlardan uzak (top safe+28, left 22),
   **sürükle-bırak + pozisyon persist** (`pulsar.fabPos.v1`), tap=touchend+preventDefault (sızan click engellendi).
8. **Input forward leak FIX (asıl bug):** `app.js wireTouchForwarding` sadece `.bar`'ı atlıyordu →
   FAB/overlay dokunuşları host'a gidiyordu. Artık `body.overlay-open||fab-dragging` + `e.target.closest(UI_SEL)`
   atlanıyor. input.js'e de `_overlayOpen()`+`_touchOnUI()` (elementFromPoint hit-test) eklendi (pasif ama dursun).

---

## Pinch-zoom + pan (AnyDesk/RustDesk tarzı) — YENİDEN YAZILDI (TextureView+Matrix)

**İstenen:** oturumda 2 parmak pinch → zoom in/out; zoom'luyken 2 parmak sürükle → pan.
Max zoom-out = fit (boş bordür yok). Max zoom-in = 4x. AnyDesk mobil / RustDesk gibi.

**3 birikmiş bug bulundu + düzeltildi:**
1. **`set_video_transform` APP komutu HİÇ register edilmemiş** (asıl sebep). JS `invoke('set_video_transform')`
   → "command not found" → `.catch(()=>{})` yutuyordu. Plugin *trait* metodu vardı ama `#[tauri::command]`
   wrapper yoktu. Eklendi: `client.rs::set_video_transform` + `lib.rs generate_handler`.
2. **SurfaceView parent-dışı layout'u izlemiyor.** Eski applyZoom view'i `W*s×H*s`'e büyütüp negatif margin
   veriyordu; SurfaceView hardware-overlay surface'i parent dışına taşınca clip/yanlış konum (bilinen kısıt).
   Split (parent içinde) çalışıyordu, zoom (parent dışı) çalışmıyordu — fark buydu.
3. **DPR uyumsuzluğu.** tx/ty CSS-px gönderiliyor, native device-px uyguluyordu (DPR ~2.6 → şift yanlış).

**Yeni mimari (RustDesk CanvasModel ile aynı: offset _x/_y + _scale, normalize edilmiş):**
- **Gerçek cihaz artık TextureView** (eskiden sadece emülatör). `setTransform(Matrix)` ile zoom/pan —
  SurfaceView'in aksine GÜVENİLİR, ayrıca **screencap ile yakalanabilir** (overlay değil). Bedeli ~1 frame
  latency; remote-desktop'ta kabul. Game-mode için ileride mode'a göre SurfaceView'a dönülebilir (gate yeri:
  `ensureSurface` içindeki `useTexture`).
- **Kontrat normalize edilmiş dest-rect:** JS `set_video_transform({slot,x,y,w,h})` gönderir — video'nun
  ekrandaki hedef dikdörtgeni, surface'e göre [0..1] (zoom'da w/h>1). Normalize olduğu için CSS-px webview ile
  device-px native DPR'den bağımsız anlaşır.
- **Aspect-doğru:** JS, decoded video boyutunu **PULL** eder: `plugin:pulsar-video|get_video_size`
  (`detail="WxH"`), `session-started`'ta poll. **ÖNEMLİ:** plugin `trigger()` event'i JS'e ULAŞMIYOR
  (ne global `listen` ne `addPluginListener` — sonuncusu "registerListener not allowed" atıyor; eski
  `decoder-error` de ölüydü). Native→JS veri için: pull command (`{ok,detail}`) VEYA Rust `app.emit`.
  vid bilinene kadar JS full-screen fallback rect kullanır (pinch yine çalışır, aspect tam değil).
- **JS** (`app.js wireTouchForwarding`): `vid{vw,vh}` + `rect{x,y,w,h}` (CSS px) state. `fitRect()`,
  `clampRect()` (eksen ekrandan küçükse ortala, büyükse kenar-içi pan), pinch focal anchor + iki-parmak pan,
  `touchTarget` rect üzerinden normalize eder (input host'a doğru gider). Badge korundu.
- **Native** (`PulsarVideoPlugin.kt`): `Pane.tnX/tnY/tnW/tnH` (normalize rect) + `applyTransform()`
  (TextureView→Matrix `setScale(w,h);postTranslate(x*W,y*H)`; SurfaceView→layout fallback). `applyAspect`
  fit'i hesaplar+uygular+`video-size` emit eder. TextureView `isOpaque=false`+siyah bg → temiz letterbox.

**DURUM:** kod yazıldı, `cargo check` (plugin+mobile) temiz, JS `bun build` temiz, rebuild+deploy edildi,
**gerçek cihazda CDP+screencap ile native yol DOĞRULANDI** (canned clip ile, host gerekmeden):
- `set_video_transform` → `OK:null` (komut artık register, eski "command not found" yok).
- `play_test` → TextureView decode + **screencap clip'i yakaladı** (overlay değil artık).
- 2× zoom screencap'i bar'ları 2× büyük+ortalı, pan (rect=0,0,2,2) görünen pencereyi sola kaydırdı,
  reset fit'e döndü. `applyTransform slot=0 rect=(...) parent=904x2316 TextureView` logları doğru.
- **KALAN (kullanıcı doğrulaması):** canlı oturumda 2-parmak PINCH jesti (JS touch→rect math). Sürdüğü
  native yol doğrulandı; jest matematiği `bun build` temiz + RustDesk CanvasModel ile aynı, ama gerçek
  parmakla test edilmedi (host'a bağlan + pinch). Aspect doğruysa input host'a doğru gider.

**CDP+screencap reçetesi (foldable!):** screencap stdout'a "[Warning] Multiple displays" basıyor → pipe'lı
PNG bozulur. `exec-out screencap -p` çıktısında `\x89PNG` magic'inden önceki baytları at. Script:
`scratchpad/cdp.py` (bu session). Test sonrası app'i force-stop+start ile relaunch et (CDP override temizliği).
```bash
ADB=~/Android/Sdk/platform-tools/adb; DEV=192.168.68.58:41705
$ADB -s $DEV logcat -d | grep -iE "applyTransform|slot . video"
```

**Aspect notu:** webview innerWidth/Height (CSS) ile native parent (device px) tam ekranı kaplar varsayımı;
status-bar inset farkı küçük bir y-offset getirebilir (input eşlemesinde), gerekirse refine. fill/stretch
aspect modları z=1'de native default; pinch sonrası JS fit-bazlı devralır (küçük tutarsızlık, çoğunluk fit kullanır).

**Değişen dosyalar:** `app.js` (wireTouchForwarding tam rewrite + video-size listener),
`PulsarVideoPlugin.kt` (TextureView default + Matrix applyTransform + applyAspect→fit+emit + VideoTransformArgs
normalize), `client.rs` (+set_video_transform app cmd), `lib.rs` (register), plugin
`commands.rs`/`mobile.rs`/`desktop.rs` (set_video_transform imza: x,y,w,h normalize).
**Referans:** RustDesk repo `/home/kahverengi/Projects/rustdesk-ref` (Flutter `CanvasModel`, model.dart:2165+).

---

## Gotchas
- **In-session input = TEK motor (`app.js wireTouchForwarding`).** `input.js` ÖLÜ KOD (kimse import etmiyor) —
  dokunma motoru orada DEĞİL. Mod (`pulsar.input.mode.v1`): **mouse** (trackpad/relative, sanal imleç
  `#pulsar-cursor`, varsayılan) | **touch** (absolute). 1-parmak: mouse→imleci delta ile oynat+tap=tık;
  touch→rect içiyse pointer. 2-parmak: pinch(Δmesafe)=zoom, paralel sürükle = (zoom>1 ise pan, değilse
  `send_scroll`). Bounds: touch modda rect dışı yok sayılır, mouse modda imleç rect'e clamp'lenir. Overlay
  Display kartında Mouse/Touch seçici → `localStorage` + bus `input-mode-changed`.
- **`body.in-session { touch-action: none }`** ŞART — yoksa webview kendi native scroll/zoom'unu yapar
  (kullanıcı "1 parmakla scroll" şikayeti buydu). `window.__TAURI__.core.invoke` **yazılamaz** (wrap edilemez)
  → CDP'de invoke intercept çalışmaz; davranışı logcat (applyTransform=zoom) + DOM (cursor/class) ile doğrula.
- **Overlay kartlarını DOM'a EKLEDİKTEN SONRA mount et.** Kartların `mount()`'u `getElementById` ile tel
  bağlar; `_renderDock` kartı önce ekleyip SONRA mount etmeli (detached mount = `getElementById` null =
  listener bağlanmaz, sessizce). `toMount` listesi → `_dockEl.appendChild(cardArea)` sonrası mount.
- **i18n: overlay'de raw key / yanlış dil.** İki sebep: (1) `index.html`'de `<html lang="tr">` sabit +
  i18n.js boot'ta `documentElement.lang`'i SET ETMİYORDU (sadece setLang ediyordu) → overlay/sidechannels/
  split'teki yerel `_t()` fallback tabloları (`document.documentElement.lang` okur) en modda Türkçe gösteriyordu.
  Fix: i18n.js boot'ta `documentElement.lang = lang`. (2) audio.*/display.*/quality.*/m.split.* anahtarları
  GLOBAL `t()` kullanıyor ama katalogda (en/tr/ru/kk) YOKTU → raw key. Fix: hepsi 4 katalog'a eklendi
  (m.sc.*/m.overlay.* de eklendi → `_t` artık katalogdan döner, tüm diller). `files.downloaded` en/ru/kk'da
  "Alınanlar" (TR) kalıntısı vardı → düzeltildi. Tarama: `ui/js` içinde `t('key')` topla, en/tr katalogla diffle.
- **Game mode = yatay:** `session.js startSession` mode==='game' ise `set_orientation{landscape:true}`;
  son oturum bitince (`_dropSlot` registry boş) `landscape:false` (portrait) ile sıfırlar (home yan kalmasın).
- **Rotate'de görüntü bozulması:** matrix eski parent W/H ile kalıyordu. Fix: native
  `onSurfaceTextureSizeChanged → applyAspect(lastFormat)` (anında refit) + JS `resize`/`orientationchange`
  → `setFit()` (input eşlemesi + gönderim). Rotate aktif zoom'u fit'e sıfırlar (AnyDesk gibi). configChanges
  zaten orientation|screenSize içeriyor → activity recreate olmuyor (JS+native state korunur).
- **Overlay kartları reopen/tab'da boş gelmesin:** `overlay.js _renderDock()` her render `_dockEl.innerHTML=''`
  yapıyor (DOM siler). Kartlar `_cardEls` map'inde CACHE'lenir, mount BİR kez olur, sonra canlı element
  yeniden append edilir. (Eski `_mounted` boolean guard'ı 2. render'dan itibaren boş kart bırakıyordu.)
- **Overlay swipe-to-dismiss + scroll** (R7, deploy+CDP doğrulandı): `overlay.js _wireDockSwipe(_dockEl)`
  — handle/header'dan VEYA card-area top'tayken aşağı sürükle → `translateY`; bırakışta `dy>90` ise `close()`,
  altında snap-back. Card-area scroll'uyla çakışmaz (atTop gate). **Seçenekler çalışmama bug'ı iki sebepti:**
  (a) kartlar DOM'a EKLENMEDEN mount ediliyordu → `getElementById` null → listener bağlanmaz (fix: mount-after-append,
  bkz. yukarı); (b) `body.in-session{touch-action:none}` overlay scroll'unu da öldürdü → fold-altı seçenek erişilemez.
  Fix: `components.css`'de `.overlay-dock/.overlay-card-area/.sheet/nav.bottom{touch-action:pan-y}` override.
  CDP doğrulama: 4 remote section (stream/display/audio/tools) kart+kontrol render; display aspect/orient/pointer
  seg tıkla→aria swap (listener canlı); card-area `touch-action:pan-y` + `overflow-y:auto`.
- **Yerel relay (Settings → Ağ → "Yerel relay")**: telefon kendi relay/rendezvous sunucusunu çalıştırır,
  LAN'daki diğer cihazlar relay olarak bu telefonu girer. `pulsar-relay` lib portable (tokio UDP) →
  Android'de derlenir; mobile Cargo'ya dep eklendi. App komutları (`relay_cmds.rs`, ACL gerekmez):
  `start_local_relay{port?}` (default 21116, idempotent, `0.0.0.0:port` bind), `stop_local_relay` (task abort),
  `local_relay_status` → `{running,ip,port}`. lib.rs'de `LocalRelay` manage + 3 komut register. UI: settings.js
  toggle + port input + tıkla-kopyala adres chip + i18n (en/tr/ru/kk `m.settings.localRelay*`). **DOĞRULANDI
  (CDP, rebuild sonrası):** status=false → start → `{running:true, ip:"192.168.68.58", port:21116}` → status=true
  idempotent. **KALAN:** UI smoke-test (toggle/adres/kopya) canlı + başka cihazdan gerçekten bağlanma (ADB düştü).
- u64-over-JSON: f64 sadece 2^53'e kadar exact. SessionId 53-bit mask'li.
- Plugin invoke casing: JS snake_case, Kotlin camelCase. lib.rs touch → ACL re-bake.
- Wayland host: gst pipeline NV12 (I420 değil — nvenc reddediyor). **Desktop host NVENC fix (gst.rs) kullanıcı rebuild etmedi.**
- Emülator: relay+desktop local ise her iki uçta `network_mode=relay-only` (auto P2P 127.0.0.1'de çöker).
