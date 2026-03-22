!macro NVPN_STOP_AND_DELETE_SERVICE
  ClearErrors
  ExecWait 'sc.exe stop NvpnService' $0
  Sleep 1500
  ExecWait 'sc.exe delete NvpnService' $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NVPN_STOP_AND_DELETE_SERVICE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ClearErrors
  ReadEnvStr $1 "NVPN_WINDOWS_SERVICE_CONFIG"
  StrCmp $1 "" 0 +3
  ExecWait '"$INSTDIR\nvpn.exe" service install --force' $0
  Goto done
  ExecWait '"$INSTDIR\nvpn.exe" service install --force --config "$1"' $0
  StrCmp $0 0 done
  DetailPrint "Nostr VPN background service install failed with exit code $0"
done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NVPN_STOP_AND_DELETE_SERVICE
!macroend
