; Installer for Остров. Per-user, no admin rights: the app updates itself in place,
; so it has to live in a folder the user can write to.
; Build: makensis -DVERSION=0.12.0 -DSRC=<folder with Island.exe> -DOUT=<setup.exe> island.nsi

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define NAME "Остров"
!define EXE "Island.exe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Island"

Name "${NAME}"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\Island"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"
BrandingText "${NAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "FileDescription" "Установка программы ${NAME}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "stariyyded-ai"

!define MUI_ICON "../src-tauri/icons/icon.ico"
!define MUI_UNICON "../src-tauri/icons/icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Запустить ${NAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"

Function StopIsland
  ; a running copy holds its files open
  nsExec::Exec 'taskkill /F /IM ${EXE}'
  Sleep 600
FunctionEnd

Function un.StopIsland
  nsExec::Exec 'taskkill /F /IM ${EXE}'
  Sleep 600
FunctionEnd

Section "Install"
  Call StopIsland
  SetOutPath "$INSTDIR"
  File "${SRC}/${EXE}"
  File "${SRC}/WebView2Loader.dll"
  File /nonfatal "${SRC}/README.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortCut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0
  ; start with Windows from the installed copy (can be turned off in the island's settings)
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Island" '"$INSTDIR\${EXE}"'

  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "stariyyded-ai"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://github.com/stariyyded-ai/DynamicIsland"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  Call un.StopIsland
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\WebView2Loader.dll"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Island"
  DeleteRegKey HKCU "${UNINST_KEY}"
  MessageBox MB_YESNO|MB_ICONQUESTION "Удалить также стикеры, полку и настройки острова?" /SD IDNO IDNO keep
    RMDir /r "$APPDATA\Island"
  keep:
SectionEnd
