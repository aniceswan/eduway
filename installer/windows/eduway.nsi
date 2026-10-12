; eduway Windows installer (NSIS). Built on CI: makensis installer/windows/eduway.nsi
!include "MUI2.nsh"

Name "eduway"
OutFile "eduway-Windows-Setup.exe"
Unicode True
InstallDir "$PROGRAMFILES\eduway"
RequestExecutionLevel admin

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "eduway" SecMain
  SetOutPath "$INSTDIR"
  File "stage\eduway.exe"
  File "stage\icon.ico"
  File "stage\README.md"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortCut "$SMPROGRAMS\eduway.lnk" "$INSTDIR\eduway.exe" "gui" "$INSTDIR\icon.ico"
  CreateShortCut "$DESKTOP\eduway.lnk" "$INSTDIR\eduway.exe" "gui" "$INSTDIR\icon.ico"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\eduway.exe"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\eduway.lnk"
  Delete "$DESKTOP\eduway.lnk"
  RMDir "$INSTDIR"
SectionEnd
