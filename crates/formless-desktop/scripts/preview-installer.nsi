Unicode true
Name "Formless Desktop Preview"
OutFile "${OUTPUT}"
RequestExecutionLevel user

Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Function .onInit
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\Formless Desktop Preview"
FunctionEnd

Function un.onInit
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\Formless Desktop Preview"
FunctionEnd

Section "Install"
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    File /r "${BUNDLE}\*"
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    CreateShortCut "$SMPROGRAMS\Formless Desktop Preview.lnk" "$INSTDIR\formless-desktop.exe" "--start-daemon"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FormlessDesktopPreview" "DisplayName" "Formless Desktop Preview"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FormlessDesktopPreview" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FormlessDesktopPreview" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FormlessDesktopPreview" "NoRepair" 1
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    Delete "$SMPROGRAMS\Formless Desktop Preview.lnk"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FormlessDesktopPreview"
    RMDir /r "$INSTDIR"
SectionEnd
