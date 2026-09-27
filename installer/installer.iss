; Spreadwatch — Windows installer
; Build: iscc /DMyAppVersion=X.Y.Z installer\installer.iss
; Output: dist\spreadwatch-setup.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "Spreadwatch"
#define MyAppPublisher "Bennekrouf"
#define MyAppURL       "https://mayorana.ch/en/apps/spreadwatch"
#define MyAppExeName   "spreadwatch.exe"
; Never reuse another app's AppId: installers sharing one uninstall each other.
#define MyAppId        "{218EC912-8688-457E-9FA7-8A1F39F57D49}"

[Setup]
AppId={{#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL=https://mayorana.ch/en/contact
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=spreadwatch-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Per-user by default: no UAC prompt, and nothing here needs admin rights.
; IT can still install machine-wide with: spreadwatch-setup.exe /ALLUSERS
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName} {#MyAppVersion}
CloseApplications=yes
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\icon.ico
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
  Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\release\{#MyAppExeName}";    DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\trade.example.toml";                DestDir: "{app}"; Flags: ignoreversion
; Shortcuts point at the .ico explicitly: some Windows builds fail to extract
; the icon embedded in the .exe for shortcut display.
Source: "..\assets\icon.ico";                   DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";     Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon

[Run]
; As the original (non-elevated) user: WebView2 renders a black window when
; the host process runs as admin.
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Launch {#MyAppName}"; \
  Flags: nowait postinstall skipifsilent runascurrentuser

; The hot wallet, settings and watchlist live in %LOCALAPPDATA%\Spreadwatch.
; The uninstaller deliberately leaves that folder alone: deleting it would
; delete the only copy of the wallet key, and with it any funds.

[Code]
const
  WebView2Key = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

function RegKey(): String;
begin
  Result := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1';
end;

function GetInstalledVersion(): String;
var
  Ver: String;
begin
  if not RegQueryStringValue(HKLM, RegKey(), 'DisplayVersion', Ver) then
    if not RegQueryStringValue(HKCU, RegKey(), 'DisplayVersion', Ver) then
      Ver := '';
  Result := Ver;
end;

function GetUninstallString(): String;
var
  UninstStr: String;
begin
  if not RegQueryStringValue(HKLM, RegKey(), 'QuietUninstallString', UninstStr) then
    if not RegQueryStringValue(HKCU, RegKey(), 'QuietUninstallString', UninstStr) then
      UninstStr := '';
  Result := UninstStr;
end;

// The app renders through Microsoft Edge WebView2. It ships with Windows 11
// and most up-to-date Windows 10 machines; without it the window stays blank.
function HasWebView2(): Boolean;
var
  Ver: String;
begin
  Result :=
    (RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\' + WebView2Key, 'pv', Ver) and (Ver <> '') and (Ver <> '0.0.0.0')) or
    (RegQueryStringValue(HKLM, 'SOFTWARE\' + WebView2Key, 'pv', Ver) and (Ver <> '') and (Ver <> '0.0.0.0')) or
    (RegQueryStringValue(HKCU, 'Software\' + WebView2Key, 'pv', Ver) and (Ver <> '') and (Ver <> '0.0.0.0'));
end;

function InitializeSetup(): Boolean;
var
  InstalledVer: String;
  NewVer:       String;
  Msg:          String;
  UninstStr:    String;
  ResultCode:   Integer;
  NL:           String;
begin
  Result := True;
  NL := #13#10;

  if not HasWebView2() then
  begin
    if MsgBox('{#MyAppName} needs the Microsoft Edge WebView2 Runtime, which is not installed.' + NL + NL +
              'Open the download page now? Install the "Evergreen Bootstrapper", then run this setup again.',
              mbConfirmation, MB_YESNO) = IDYES then
      ShellExec('open', 'https://developer.microsoft.com/microsoft-edge/webview2/', '', '', SW_SHOWNORMAL, ewNoWait, ResultCode);
    Result := False;
    Exit;
  end;

  InstalledVer := GetInstalledVersion();
  if InstalledVer = '' then
    Exit;   // fresh install

  NewVer := '{#MyAppVersion}';
  if InstalledVer = NewVer then
    Msg := 'Version ' + InstalledVer + ' of {#MyAppName} is already installed.' + NL + NL +
           'Do you want to reinstall it?'
  else
    Msg := '{#MyAppName} is already installed.' + NL + NL +
           '  Installed version:  ' + InstalledVer + NL +
           '  New version:        ' + NewVer + NL + NL +
           'The old version will be removed before installing the new one.' + NL +
           'Your wallet, settings and watchlist are kept.' + NL + NL +
           'Continue?';

  if MsgBox(Msg, mbConfirmation, MB_YESNO) = IDNO then
  begin
    Result := False;
    Exit;
  end;

  UninstStr := GetUninstallString();
  if UninstStr <> '' then
  begin
    Exec('>', UninstStr, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);
  end;
end;
