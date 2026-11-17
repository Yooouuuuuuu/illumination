; SPDX-License-Identifier: GPL-3.0-only
; The Windows installer of Illumination: Illumination_<version>_x64-setup.exe.
;
; Built by scripts/build-installer.ps1, which passes VERSION and SOURCE (the folder with
; illum.exe and the FFmpeg libraries). It installs for the whole machine and so asks for an
; administrator once: the point of installing is the service, which lets this desk be reached
; in windows that run as administrator (docs/architecture/architecture.md, section 3).
; Everything about the service itself is done by the program: `illum service install` and
; `illum service uninstall`. Keys, desks and settings live in %ProgramData%\Illumination and
; are never touched here, so an update or a reinstall keeps them.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel admin

!ifndef VERSION
  !error "VERSION is not set: build with scripts/build-installer.ps1"
!endif
!ifndef SOURCE
  !error "SOURCE is not set: build with scripts/build-installer.ps1"
!endif
!ifndef OUTPUT
  !define OUTPUT "Illumination_${VERSION}_x64-setup.exe"
!endif

!define NAME "Illumination"
!define EXE "illum.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${NAME}"

Name "${NAME}"
OutFile "${OUTPUT}"
InstallDir "$PROGRAMFILES64\${NAME}"
BrandingText "${NAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "FileDescription" "${NAME} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-only"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"

!define MUI_ICON "..\crates\illum\assets\illumination.ico"
!define MUI_UNICON "..\crates\illum\assets\illumination.ico"
!define MUI_ABORTWARNING
; Started through Explorer, so the app runs as the person and not as the administrator the
; installer is.
!define MUI_FINISHPAGE_RUN "$WINDIR\explorer.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "$\"$INSTDIR\${EXE}$\""
!define MUI_FINISHPAGE_RUN_TEXT "Open ${NAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Waits until the program's file can be written: the service has let go of it and no window
; of the app is open. Nothing is closed for the person; they are asked to.
!macro WaitUntilClosed
  StrCpy $1 0
  ${Do}
    ${IfNot} ${FileExists} "$INSTDIR\${EXE}"
      ${Break}
    ${EndIf}
    ClearErrors
    FileOpen $0 "$INSTDIR\${EXE}" a
    ${IfNot} ${Errors}
      FileClose $0
      ${Break}
    ${EndIf}
    IntOp $1 $1 + 1
    ${If} $1 < 20
      Sleep 250
    ${ElseIf} ${Silent}
      Abort "${NAME} is still open."
    ${Else}
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${NAME} is still open.$\n$\nQuit it from its icon in the tray (next to the clock), then press Retry." IDRETRY +2
      Abort "${NAME} is still open."
      StrCpy $1 0
    ${EndIf}
  ${Loop}
!macroend

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "${NAME} needs 64-bit Windows."
    Abort
  ${EndIf}
  SetRegView 64
  ; An update goes where the earlier version is.
  ReadRegStr $0 HKLM "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

Section "Install"
  SetShellVarContext all
  SetOutPath "$INSTDIR"

  ; An earlier version: stop its service so its files can be replaced.
  DetailPrint "Stopping the service of an earlier version, if there is one..."
  nsExec::Exec 'sc.exe stop ${NAME}'
  Pop $0
  !insertmacro WaitUntilClosed

  File "${SOURCE}\${EXE}"
  File "${SOURCE}\*.dll"
  File "..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\${EXE}"
  CreateShortcut "$DESKTOP\${NAME}.lnk" "$INSTDIR\${EXE}"

  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "${NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" "$0"

  DetailPrint "Installing and starting the service..."
  nsExec::ExecToLog '"$INSTDIR\${EXE}" service install'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "${NAME} is installed, but its service did not start (code $0).$\n$\nThe app still works. Until the service runs, this desk can only be reached while the app is open, and not in windows that run as administrator.$\n$\nTo try again, run in a terminal opened as administrator:$\n$\"$INSTDIR\${EXE}$\" service install" /SD IDOK
  ${EndIf}
SectionEnd

Section "Uninstall"
  SetShellVarContext all

  DetailPrint "Stopping and removing the service..."
  nsExec::ExecToLog '"$INSTDIR\${EXE}" service uninstall'
  Pop $0
  !insertmacro WaitUntilClosed

  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\*.dll"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  Delete "$DESKTOP\${NAME}.lnk"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  ; What "Start on boot" added for whoever uninstalls.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${NAME}"
  ; %ProgramData%\Illumination (this desk's key, its desks, settings, logs) stays: installing
  ; again brings the same desk back.
SectionEnd
