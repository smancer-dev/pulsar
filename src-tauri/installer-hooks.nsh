; Pulsar NSIS installer hooks — auto-install the bundled Windows drivers so the
; user never installs anything by hand, and tell them a reboot is needed (kernel
; drivers activate on the next restart).
;
; Wired up in tauri.windows.conf.json:
;   "bundle": { "windows": { "nsis": { "installerHooks": "./installer-hooks.nsh" } } }
; Payloads are fetched into resources/ by scripts/fetch-drivers.mjs (run before
; `tauri build`; CI runs it automatically).
;
; Every step is guarded with IfFileExists, so a missing payload can never fail the
; install — it just skips that driver.

; LogicLib (${If}) + x64.nsh (${IsNativeARM64}) for arch-selecting the right
; nefconw. Tauri's main.nsi already includes these, but guard so this hook also
; compiles standalone.
!include "LogicLib.nsh"
!include "x64.nsh"

; Substring matching uses StrFunc's ${StrLoc}, which Tauri's generated installer.nsi
; already declares (`${StrLoc}` near the top of that file) before it !includes this
; hook — so it's in scope here with zero extra dependencies and no double-declaration.
; ${StrLoc} $out "<haystack>" "<needle>" ">"  →  $out = offset of needle, "" if absent.

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ; Interception — keyboard capture below the OS hook layer (works under ASTER).
  IfFileExists "$INSTDIR\resources\interception\install-interception.exe" 0 +3
    DetailPrint "Pulsar: Interception klavye sürücüsü kuruluyor..."
    ExecWait '"$INSTDIR\resources\interception\install-interception.exe" /install'

  ; ViGEmBus — virtual Xbox gamepad on the host.
  IfFileExists "$INSTDIR\resources\ViGEmBus_Setup.exe" 0 +3
    DetailPrint "Pulsar: ViGEmBus gamepad sürücüsü kuruluyor..."
    ExecWait '"$INSTDIR\resources\ViGEmBus_Setup.exe" /quiet /norestart'

  ; Virtual Audio Driver — a sinkless render endpoint we redirect the default
  ; output to so the host can STREAM its audio while staying silent (Sunshine
  ; model: redirect → capture that device's loopback → restore on teardown; we
  ; never mute the real endpoint). nefconw creates the ROOT\VirtualAudioDriver
  ; device node, then installs the signed INF (no [DefaultInstall] section, so
  ; --install-driver, not --inf-default-install). HWID/class come from the INF:
  ;   HWID=ROOT\VirtualAudioDriver  Class=MEDIA  GUID={4d36e96c-e325-11ce-bfc1-08002be10318}
  ; Device shows in Device Manager as "Virtual Audio Driver by MTT"; the render
  ; endpoint friendly name is likewise "Virtual Audio Driver by MTT".
  ;
  ; DORMANT until MS-attestation-signed: our bundled Virtual-Audio-Driver is only
  ; SignPath-code-signed, so on modern Windows it *installs* but the kernel will
  ; NOT *load* it (CM_PROB_UNSIGNED_DRIVER, code 52) until we get it MS-signed.
  ; The runtime therefore PREFERS any already-present *loadable* virtual sink —
  ; "Steam Streaming Speakers" (MS-signed, present whenever Steam is installed —
  ; exactly what Sunshine uses), then VB-Audio / CABLE — and only uses our node
  ; once it can load. See VIRTUAL_SINK_CANDIDATES in host/handlers.rs.
  ;
  ; CHECK-FIRST: if a usable virtual render endpoint is ALREADY present we SKIP the
  ; VAD install entirely — installing a dormant (non-loadable) node on top would be
  ; pointless clutter. We scan the MMDevices render-endpoint registry for a friendly
  ; name the runtime would prefer. The friendly name lives under each endpoint's
  ;   ...\MMDevices\Audio\Render\{epguid}\Properties
  ; in value {a45c254e-df1c-4efd-8020-67d146a850e0},2 (PKEY_Device_FriendlyName).
  StrCpy $1 0           ; HKLM\...\Render subkey index
  StrCpy $2 ""          ; set to a friendly name once a usable sink is found
  vad_scan_loop:
    ClearErrors
    EnumRegKey $3 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render" $1
    IfErrors vad_scan_done
    StrCmp $3 "" vad_scan_done
    IntOp $1 $1 + 1
    ReadRegStr $4 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\$3\Properties" "{a45c254e-df1c-4efd-8020-67d146a850e0},2"
    StrCmp $4 "" vad_scan_loop
    ; Match the runtime's preferred sinks (substring) — Steam first, then our own
    ; prior VAD node, then common virtual cables. Any hit → a usable sink exists.
    ; ${StrLoc} $5 haystack needle ">"  →  $5 = "" when the needle is NOT present.
    ${StrLoc} $5 "$4" "Steam Streaming Speakers" ">"
    StrCmp $5 "" 0 vad_found
    ${StrLoc} $5 "$4" "Virtual Audio Driver" ">"
    StrCmp $5 "" 0 vad_found
    ${StrLoc} $5 "$4" "VB-Audio" ">"
    StrCmp $5 "" 0 vad_found
    ${StrLoc} $5 "$4" "CABLE Input" ">"
    StrCmp $5 "" 0 vad_found
    Goto vad_scan_loop
  vad_found:
    StrCpy $2 $4
  vad_scan_done:
  StrCmp $2 "" 0 vad_skip
    ; No usable virtual sink present — install ours (dormant until MS-signed).
    ; Pick the nefconw matching the install arch (ARM64 vs x64).
    StrCpy $0 "$INSTDIR\resources\nefcon\x64\nefconw.exe"
    ${If} ${IsNativeARM64}
      StrCpy $0 "$INSTDIR\resources\nefcon\arm64\nefconw.exe"
    ${EndIf}
    IfFileExists "$INSTDIR\resources\virtual-audio-driver\VirtualAudioDriver.inf" 0 vad_done
    IfFileExists "$0" 0 vad_done
      DetailPrint "Pulsar: Sanal ses sürücüsü (sessiz host yayını) kuruluyor..."
      ; --no-duplicates makes the node create idempotent: an upgrade re-run won't
      ; spawn a SECOND ROOT\VirtualAudioDriver device node.
      ExecWait '"$0" --create-device-node --hardware-id "ROOT\VirtualAudioDriver" --class-name "MEDIA" --class-guid "4d36e96c-e325-11ce-bfc1-08002be10318" --no-duplicates'
      ExecWait '"$0" --install-driver --inf-path "$INSTDIR\resources\virtual-audio-driver\VirtualAudioDriver.inf"'
    Goto vad_done
  vad_skip:
    DetailPrint "Pulsar: Mevcut sanal ses aygıtı bulundu ($2) — sürücü kurulumu atlandı."
  vad_done:

  ; The app loads interception.dll from next to its exe at runtime.
  IfFileExists "$INSTDIR\resources\interception\interception.dll" 0 +2
    CopyFiles /SILENT "$INSTDIR\resources\interception\interception.dll" "$INSTDIR\interception.dll"

  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0

  MessageBox MB_OK|MB_ICONINFORMATION "Pulsar kuruldu. Klavye/gamepad ve sanal ses sürücülerinin etkinleşmesi için bilgisayarı bir kez yeniden başlatın."
!macroend
