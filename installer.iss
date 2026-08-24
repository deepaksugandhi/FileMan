; Inno Setup script for FileMan
; Build:  ISCC.exe installer.iss   (output lands in .\installer\)

#define MyAppName "FileMan"
; Keep in sync with Cargo.toml (bump both on release).
#define MyAppVersion "0.1.0"
#define MyAppPublisher "FileMan"
#define MyAppExeName "fileman.exe"

[Setup]
AppId={{9F1C42A5-3E7B-4C6D-8A19-B2D05F6E7C31}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=installer
OutputBaseFilename=FileMan-{#MyAppVersion}-setup
SetupIconFile=assets\app.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
MinVersion=6.3
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; \
    Flags: nowait postinstall skipifsilent

[UninstallDelete]
; User data (settings DB) intentionally kept in %APPDATA%\FileMan.
Type: files; Name: "{app}\*.log"
