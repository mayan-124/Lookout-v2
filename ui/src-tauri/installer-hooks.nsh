; Remove the "start with Windows" entry on uninstall. An update keeps it.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Lookout"
  ${EndIf}
!macroend
