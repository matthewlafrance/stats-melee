; stats-melee Windows installer (NSIS, Modern UI 2).
;
; Driven entirely from CI (see .github/workflows/release.yml), which passes the
; version, output path, staged-files dir, and icon as /D defines. Produces a
; per-user installer (no admin / UAC) that drops the app under
; %LOCALAPPDATA%\Programs, creates Start Menu + Desktop shortcuts, and registers
; an uninstaller in Add/Remove Programs.

Unicode true

!define APP_NAME "stats-melee"
!define APP_EXE "stats-melee-app.exe"
!define PUBLISHER "matthewlafrance"

; Defaults so the script can be opened / linted without the CI defines.
!ifndef APP_VERSION
  !define APP_VERSION "0.0.0"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "stats-melee-Setup.exe"
!endif
!ifndef STAGE_DIR
  !define STAGE_DIR "stage"
!endif

Name "${APP_NAME} ${APP_VERSION}"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_NAME}" "InstallDir"
RequestExecutionLevel user ; per-user install — no admin prompt
ShowInstDetails show
ShowUninstDetails show

!include "MUI2.nsh"

; Custom installer / uninstaller icon (the app's gold-diamond mark). Only set
; when CI passes the path, so a bare lint run still works with the default.
!ifdef APP_ICON
  !define MUI_ICON "${APP_ICON}"
  !define MUI_UNICON "${APP_ICON}"
!endif
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

!define UNINST_KEY \
  "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"

Section "Install"
  SetOutPath "$INSTDIR"
  ; Everything CI staged (the .exe, README, LICENSE, assets/). `\*` rather than
  ; `*.*` so extensionless files like LICENSE are included too.
  File /r "${STAGE_DIR}\*"

  ; Shortcuts. The trailing `"$INSTDIR\${APP_EXE}" 0` sets the shortcut icon to
  ; the .exe's own embedded icon (index 0) — no separate .ico needed at runtime.
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" \
    "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" \
    "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0

  ; Uninstaller + Add/Remove Programs registration (per-user hive).
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\${APP_NAME}"
SectionEnd
