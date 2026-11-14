; Per-user Windows installer. User data in %LOCALAPPDATA%\Linote is never removed.
Unicode true
Target amd64-unicode
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef VERSION
  !error "VERSION is required"
!endif
!ifndef STAGE
  !error "STAGE is required"
!endif
!ifndef OUTPUT
  !error "OUTPUT is required"
!endif

Name "Linote"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Linote"
InstallDirRegKey HKCU "Software\Linote" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Linote"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "Linote Setup"
VIAddVersionKey "FileVersion" "${VERSION}.0"
VIAddVersionKey "LegalCopyright" ""

!define MUI_ABORTWARNING
!define MUI_ICON "../res/linote.ico"
!define MUI_UNICON "../res/linote.ico"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Japanese"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "Linote は 64 ビット版 Windows 用です。"
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext current
  ReadRegStr $0 HKCU "Software\Linote" "InstallDir"
  ${If} $0 != ""
  ${AndIf} $INSTDIR == "$LOCALAPPDATA\Programs\Linote"
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

Function un.onInit
  SetRegView 64
  SetShellVarContext current
FunctionEnd

Section "Linote" Main
  SetOutPath "$INSTDIR"
  ; Abort on a locked executable rather than presenting a successful partial upgrade.
  ClearErrors
  File "${STAGE}/linote.exe"
  IfErrors install_failed
  File "${STAGE}/THIRD_PARTY_LICENSES.txt"
  IfErrors install_failed
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  IfErrors install_failed
  CreateShortcut "$SMPROGRAMS\Linote.lnk" "$INSTDIR\linote.exe"
  WriteRegStr HKCU "Software\Linote" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "DisplayName" "Linote"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "DisplayIcon" "$INSTDIR\linote.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote" "NoRepair" 1
  Goto done
install_failed:
  SetErrorLevel 1
  Abort
done:
SectionEnd

Section "Uninstall"
  ClearErrors
  Delete "$INSTDIR\linote.exe"
  IfErrors uninstall_failed
  Delete "$SMPROGRAMS\Linote.lnk"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  Delete "$INSTDIR\Uninstall.exe"
  ; No recursive removal: preserve extra files and all settings/memos/recovery data.
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Linote"
  DeleteRegKey HKCU "Software\Linote"
  Goto uninstall_done
uninstall_failed:
  SetErrorLevel 1
  Abort
uninstall_done:
SectionEnd
