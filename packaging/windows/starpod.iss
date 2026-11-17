; Inno Setup script for the Starpod Windows installer.
;
; Built by .github/workflows/build.yml with:
;   iscc /DAppVersion=<version> /DStageDir=<abs stage folder> /DOutputDir=<abs dir> dist\windows\starpod.iss
;
; StageDir holds the same files as the portable zip (starpod.exe, README.md and
; the license files), so the installer ships everything the zip does.
; Per-user install by default: no administrator rights are needed.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\..\stage\starpod-windows-x86_64"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

#define AppName "Starpod"
#define AppExe "starpod.exe"

[Setup]
; Stable across versions: an upgrade installs over the same product. Do not change.
AppId={{88F1F3D5-8172-4DBF-9826-9AA5C93358E2}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=foxgirls.org
AppPublisherURL=https://github.com/foxgirlsorg/starpod
AppSupportURL=https://github.com/foxgirlsorg/starpod/issues
AppUpdatesURL=https://github.com/foxgirlsorg/starpod/releases
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\..\crates\starpod-app\assets\starpod.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
OutputDir={#OutputDir}
OutputBaseFilename=starpod-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; A running Starpod holds starpod.exe open; ask to close it rather than fail.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; IconFilename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; IconFilename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
