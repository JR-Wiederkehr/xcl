; XCL installer (Inno Setup 6). Build: ISCC.exe /DDxcDir="<Windows Kits ...\x64>" installer\xcl.iss
; Per-user install, no admin rights. dxcompiler.dll + dxil.dll go next to xcl.exe, where XCL looks first.

#ifndef DxcDir
  #define DxcDir "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64"
#endif

[Setup]
AppId={{6B0E5C55-3B0C-4E0B-9C5E-58C1A1D0A7E2}
AppName=XCL
AppVersion=0.1.0-mod.16
AppVerName=XCL 0.1.0-mod.16 (modified build)
AppPublisher=XCL contributors (modified build, see CHANGES.txt)
AppPublisherURL=https://github.com/theaustinayers/xcl
ChangesEnvironment=yes
DefaultDirName={localappdata}\Programs\XCL
DefaultGroupName=XCL
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=XCL-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\xcl.exe

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "offline"; Description: "Run games with networking off (sets XCL_NETWORK=0 for your user account: a game's crash reporter cannot upload memory dumps)"

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: string; ValueName: "XCL_NETWORK"; ValueData: "0"; Flags: uninsdeletevalue; Tasks: offline

[Files]
Source: "CHANGES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\xcl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DxcDir}\dxcompiler.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DxcDir}\dxil.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\XCL"; Filename: "{app}\xcl.exe"; WorkingDir: "{app}"
Name: "{group}\Uninstall XCL"; Filename: "{uninstallexe}"
Name: "{autodesktop}\XCL"; Filename: "{app}\xcl.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\xcl.exe"; Description: "Start XCL"; Flags: nowait postinstall skipifsilent
