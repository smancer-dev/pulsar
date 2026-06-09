; Pulsar NSIS installer hooks — auto-install the bundled Windows drivers so the
; user never installs anything by hand, and tell them a reboot is needed (kernel
; drivers activate on the next restart).
;
; To ENABLE, add to tauri.conf.json (and ensure scripts/fetch-drivers.mjs has run
; so the payloads exist under resources/):
;
;   "bundle": { "windows": { "nsis": { "installerHooks": "./installer-hooks.nsh" } } }
;
; Every step is guarded with IfFileExists, so a missing payload can never fail the
; install — it just skips that driver.

!macro NSIS_HOOK_POSTINSTALL
  ; Interception — keyboard capture below the OS hook layer (works under ASTER).
  IfFileExists "$INSTDIR\resources\interception\install-interception.exe" 0 +3
    DetailPrint "Pulsar: Interception klavye sürücüsü kuruluyor..."
    ExecWait '"$INSTDIR\resources\interception\install-interception.exe" /install'

  ; ViGEmBus — virtual Xbox gamepad on the host.
  IfFileExists "$INSTDIR\resources\ViGEmBus_Setup.exe" 0 +3
    DetailPrint "Pulsar: ViGEmBus gamepad sürücüsü kuruluyor..."
    ExecWait '"$INSTDIR\resources\ViGEmBus_Setup.exe" /quiet /norestart'

  ; The app loads interception.dll from next to its exe at runtime.
  IfFileExists "$INSTDIR\resources\interception\interception.dll" 0 +2
    CopyFiles /SILENT "$INSTDIR\resources\interception\interception.dll" "$INSTDIR\interception.dll"

  MessageBox MB_OK|MB_ICONINFORMATION "Pulsar kuruldu. Klavye/gamepad sürücülerinin etkinleşmesi için bilgisayarı bir kez yeniden başlatın."
!macroend
