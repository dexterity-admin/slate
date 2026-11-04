; Slate per-user installer (no administrator rights needed).
; Built by .github/workflows/release.yml:  iscc /DAppVersion=1.2.3 installer\slate.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6E0B6A39-2E6C-4C5B-9C38-5A1E3F2B9D41}
AppName=Slate
AppVersion={#AppVersion}
AppVerName=Slate {#AppVersion}
AppPublisher=Slate contributors
AppPublisherURL=https://github.com/dexterity-admin/slate
AppSupportURL=https://github.com/dexterity-admin/slate/issues
AppUpdatesURL=https://github.com/dexterity-admin/slate/releases
DefaultDirName={localappdata}\Programs\Slate
DefaultGroupName=Slate
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=Slate-Setup-{#AppVersion}
SetupIconFile=..\res\slate.ico
UninstallDisplayIcon={app}\slate.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Close a running Slate before replacing it; it restores its session on restart.
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "openwith"; Description: "Add ""Open with Slate"" to the Explorer right-click menu"
Name: "assoc"; Description: "Open text, log and CSV files with Slate"; Flags: unchecked

[Files]
Source: "..\dist\slate.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{autoprograms}\Slate"; Filename: "{app}\slate.exe"
Name: "{autodesktop}\Slate"; Filename: "{app}\slate.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\*\shell\Slate"; ValueType: string; ValueName: ""; ValueData: "Open with Slate"; Flags: uninsdeletekey; Tasks: openwith
Root: HKCU; Subkey: "Software\Classes\*\shell\Slate"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\slate.exe"",0"; Tasks: openwith
Root: HKCU; Subkey: "Software\Classes\*\shell\Slate\command"; ValueType: string; ValueName: ""; ValueData: """{app}\slate.exe"" ""%1"""; Tasks: openwith
Root: HKCU; Subkey: "Software\Classes\Slate.Text"; ValueType: string; ValueName: ""; ValueData: "Text document"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\Slate.Text\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\slate.exe"",0"; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\Slate.Text\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\slate.exe"" ""%1"""; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: string; ValueName: "Slate.Text"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.log\OpenWithProgids"; ValueType: string; ValueName: "Slate.Text"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.csv\OpenWithProgids"; ValueType: string; ValueName: "Slate.Text"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.pcf\OpenWithProgids"; ValueType: string; ValueName: "Slate.Text"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\Applications\slate.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\slate.exe"" ""%1"""; Flags: uninsdeletekey

[Run]
Filename: "{app}\slate.exe"; Description: "Start Slate"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\slate.exe.old"
