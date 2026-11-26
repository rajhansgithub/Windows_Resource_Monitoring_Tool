; Inno Setup script. Build the exe first: cargo build --release
#define MyAppName "Resource Monitor"
#define MyAppVersion "2.18.0"
#define MyAppPublisher "Emulsion Labs"
#define MyAppURL "https://github.com/rajhansgithub/Windows_Resource_Monitoring_Tool"
#define MyAppExeName "ResourceMonitor.exe"
; Keep this GUID: upgrades find the existing install by it.
#define MyAppId "6C1F7B52-3E4D-4A8B-9F21-7D5E0A3C9B14"

[Setup]
AppId={{{#MyAppId}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
AppCopyright=Copyright (C) 2026 {#MyAppPublisher}
VersionInfoCompany={#MyAppPublisher}
VersionInfoProductName={#MyAppName}
VersionInfoVersion={#MyAppVersion}
; Per-user install: no admin prompt.
PrivilegesRequired=lowest
; Every Emulsion Labs app installs side by side under one shared folder:
;   %LOCALAPPDATA%\Programs\Emulsion Labs\<App>   and   Start Menu\Emulsion Labs\<App>
DefaultDirName={localappdata}\Programs\{#MyAppPublisher}\{#MyAppName}
DefaultGroupName={#MyAppPublisher}
DisableProgramGroupPage=yes
; Pre-2.13 installs lived in ...\Programs\Resource Monitor. Don't reuse that
; folder; [Code] below moves the install and removes the old one.
UsePreviousAppDir=no
UsePreviousGroup=no
OutputDir=dist
OutputBaseFilename=Setup_ResourceMonitor
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=res\app.ico
; Pulse Line logo: side panel and corner art (100% and 200%), from tools/icon/make.js.
WizardImageFile=res\wizard.bmp,res\wizard@2x.bmp
WizardSmallImageFile=res\wizard-small.bmp,res\wizard-small@2x.bmp
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start with Windows"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; Pre-2.13 shortcuts sat loose in the Start Menu and point at the old folder.
Type: files; Name: "{autoprograms}\{#MyAppName}.lnk"
Type: files; Name: "{autoprograms}\{#MyAppName} - Data Usage.lnk"
; Recreated below if the desktop icon task is (still) selected.
Type: files; Name: "{autodesktop}\{#MyAppName}.lnk"

[Files]
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{#MyAppName} - Data Usage"; Filename: "{app}\{#MyAppExeName}"; Parameters: "--report"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ResourceMonitor"; ValueData: """{app}\{#MyAppExeName}"""; Tasks: autostart; Flags: uninsdeletevalue
; Remove the old .NET version's autostart entry.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "WindowsNetworkMonitor"; ValueType: none; Flags: deletevalue

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\ResourceMonitor.old.exe"
Type: files; Name: "{app}\ResourceMonitor.new.exe"

[UninstallRun]
Filename: "taskkill.exe"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; RunOnceId: "KillApp"

[Code]
var
  OldDir: String;

{ Folder of an earlier install, if it is not where this one goes. }
function MovingFrom(): String;
begin
  Result := '';
  if (OldDir <> '') and DirExists(OldDir) and
     (CompareText(RemoveBackslash(OldDir), RemoveBackslash(ExpandConstant('{app}'))) <> 0) then
    Result := RemoveBackslash(OldDir);
end;

function InitializeSetup(): Boolean;
begin
  { Read before install: Setup overwrites InstallLocation with the new folder. }
  if not RegQueryStringValue(HKCU,
       'Software\Microsoft\Windows\CurrentVersion\Uninstall\{' + '{#MyAppId}' + '}_is1', 'InstallLocation', OldDir) then
    OldDir := '';
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  { The running copy lives in the old folder, which CloseApplications doesn't watch. }
  if MovingFrom() <> '' then
    Exec('taskkill.exe', '/F /IM {#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  D: String;
begin
  if CurStep <> ssPostInstall then Exit;
  D := MovingFrom();
  if D = '' then Exit;
  { Only files this app put there. Anything else keeps the folder alive. }
  DeleteFile(D + '\{#MyAppExeName}');
  DeleteFile(D + '\ResourceMonitor.old.exe');
  DeleteFile(D + '\ResourceMonitor.new.exe');
  DeleteFile(D + '\unins000.exe');
  DeleteFile(D + '\unins000.dat');
  RemoveDir(D);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Parent: String;
begin
  if CurUninstallStep <> usPostUninstall then Exit;
  { Drop the shared Emulsion Labs folder once the last app is gone. }
  Parent := ExtractFileDir(RemoveBackslash(ExpandConstant('{app}')));
  if CompareText(ExtractFileName(Parent), '{#MyAppPublisher}') = 0 then
    RemoveDir(Parent);
end;
