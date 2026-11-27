; Remove Lookout's registry entries on uninstall. An update keeps them.
; Screenshots and other files of the user's are left alone.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    ; start with Windows
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Lookout"
    ; the name Windows shows on notifications (core/src/notify.rs)
    DeleteRegKey HKCU "Software\Classes\AppUserModelId\dev.lookout.app"
  ${EndIf}
!macroend
