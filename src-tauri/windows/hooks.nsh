; NSIS installer hooks for Proxi (Tauri v2)
; Adds/removes <install-dir>\target\release to/from HKCU\Environment\Path,
; porting app/desktop/user-path.mjs semantics.

!include "WinMessages.nsh"

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec 'powershell -NoProfile -NonInteractive -WindowStyle Hidden -Command "& { param($$inst) $$dir = $$inst.Trim().TrimEnd(''\'', ''/''); $$k = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey(''Environment''); $$curr = $$k.GetValue(''Path'', '''', ''DoNotExpandEnvironmentNames''); if ($$curr -eq $$null) { $$curr = ''''; }; $$entries = @($$curr -split '';'' | Where-Object { $$_.Trim() -ne '''' }); $$norm = { param($$s) $$s.Trim().TrimEnd(''\'', ''/'').ToLowerInvariant() }; $$dirNorm = & $$norm $$dir; $$exists = $$false; foreach ($$e in $$entries) { if ((& $$norm $$e) -eq $$dirNorm) { $$exists = $$true; break; } }; if (-not $$exists) { $$newVal = ($$entries + $$dir) -join '';''; $$k.SetValue(''Path'', $$newVal, [Microsoft.Win32.RegistryValueKind]::ExpandString); [Environment]::SetEnvironmentVariable(''PROXI_PATH_REFRESH'', ''1'', ''User''); [Environment]::SetEnvironmentVariable(''PROXI_PATH_REFRESH'', $$null, ''User''); }; $$k.Close() }" -args "$INSTDIR\target\release"'
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::Exec 'powershell -NoProfile -NonInteractive -WindowStyle Hidden -Command "& { param($$inst) $$dir = $$inst.Trim().TrimEnd(''\'', ''/''); $$k = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey(''Environment''); $$curr = $$k.GetValue(''Path'', '''', ''DoNotExpandEnvironmentNames''); if ($$curr -eq $$null) { $$curr = ''''; }; $$entries = @($$curr -split '';'' | Where-Object { $$_.Trim() -ne '''' }); $$norm = { param($$s) $$s.Trim().TrimEnd(''\'', ''/'').ToLowerInvariant() }; $$dirNorm = & $$norm $$dir; $$kept = @($$entries | Where-Object { (& $$norm $$_) -ne $$dirNorm }); if ($$kept.Length -ne $$entries.Length) { $$newVal = $$kept -join '';''; $$k.SetValue(''Path'', $$newVal, [Microsoft.Win32.RegistryValueKind]::ExpandString); [Environment]::SetEnvironmentVariable(''PROXI_PATH_REFRESH'', ''1'', ''User''); [Environment]::SetEnvironmentVariable(''PROXI_PATH_REFRESH'', $$null, ''User''); }; $$k.Close() }" -args "$INSTDIR\target\release"'
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend
