$ErrorActionPreference = 'Stop'
$root = if ($env:MIRAGE_TEST_FIXTURE_ROOT) { $env:MIRAGE_TEST_FIXTURE_ROOT } else { Join-Path ([IO.Path]::GetTempPath()) ('mirage-lifecycle-' + [Guid]::NewGuid().ToString('N')) }
New-Item -ItemType Directory -Path $root -Force | Out-Null
$originalLocal = $env:LOCALAPPDATA
$env:LOCALAPPDATA = Join-Path $root 'local'
$fixture = @{ Root = $root; Mounted = $false; Task = $null; Registry = @{}; Cases = 0 }
function Assert-True([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }
function Assert-Rejected([scriptblock]$Body) {
  $rejected = $false
  try { & $Body } catch { $rejected = $true }
  Assert-True $rejected 'Expected rejection.'
}
# Only OS integration is replaced. The real scripts, file copies, credentials
# transition, cache isolation, manifests and mutexes run against disposable data.
function Test-Path {
  param($LiteralPath, $Path, $PathType, $ErrorAction)
  $value = if ($LiteralPath) { $LiteralPath } else { $Path }
  if ($value -eq 'Z:\') { return $fixture.Mounted }
  $parameters = @{ LiteralPath = $value }
  if ($PathType) { $parameters.PathType = $PathType }
  Microsoft.PowerShell.Management\Test-Path @parameters
}
function Get-CimInstance {
  param($ClassName, $Filter, $ErrorAction)
  if ($ClassName -eq 'Win32_LogicalDisk' -and $fixture.Mounted) { [pscustomobject]@{ VolumeName = 'MirageSSD' } }
}
function Get-ScheduledTask { param($TaskName, $ErrorAction) $fixture.Task }
function Unregister-ScheduledTask { param($TaskName, [switch]$Confirm) $fixture.Task = $null }
function Remove-ItemProperty { param($LiteralPath, $Name, $ErrorAction) $fixture.Registry.Remove($Name) }
function Remove-Item {
  param($LiteralPath, [switch]$Force, $ErrorAction)
  if ([string]$LiteralPath -like 'HKCU:*') { return }
  if (-not $LiteralPath) { return }
  Microsoft.PowerShell.Management\Remove-Item -LiteralPath $LiteralPath -Force:$Force -ErrorAction Stop
}
function Disable-ScheduledTask { param($TaskName, $ErrorAction) }
function Enable-ScheduledTask { param($TaskName, $ErrorAction) }
function Stop-ScheduledTask { param($TaskName) $fixture.Mounted = $false; if ($fixture.Task) { $fixture.Task.State = 'Ready' } }
function Start-ScheduledTask { param($TaskName, $ErrorAction) $fixture.Mounted = $true; $fixture.Task.State = 'Running' }
function New-ScheduledTaskAction { param($Execute, $Argument, $WorkingDirectory) @{} }
function New-ScheduledTaskTrigger { param([switch]$AtLogOn, $User, [switch]$Once, $At, $RepetitionInterval) @{} }
function New-ScheduledTaskPrincipal { param($UserId, $LogonType, $RunLevel) @{} }
function New-ScheduledTaskSettingsSet { param($MultipleInstances, $ExecutionTimeLimit, [switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, [switch]$StartWhenAvailable) @{} }
function Register-ScheduledTask {
  param($TaskName, $Action, $Trigger, $Principal, $Settings, $Description, [switch]$Force)
  $fixture.Task = [pscustomobject]@{ TaskName = $TaskName; State = 'Ready' }
}
function New-Item {
  param($Path, $ItemType, [switch]$Force)
  if ([string]$Path -like 'HKCU:*') { return }
  Microsoft.PowerShell.Management\New-Item -Path $Path -ItemType $ItemType -Force:$Force
}
function New-ItemProperty { param($Path, $Name, $PropertyType, $Value, [switch]$Force) $fixture.Registry[$Name] = $Value; $fixture.Registry | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $fixture.Root 'registry.json') }
function New-Object {
  param($ComObject)
  if ($ComObject -ne 'WScript.Shell') { throw 'Unexpected COM access.' }
  $shell = [pscustomobject]@{}
  $shell | Add-Member ScriptMethod CreateShortcut {
    param($Path)
    $shortcut = [pscustomobject]@{ TargetPath = ''; Arguments = ''; Description = '' }
    $shortcut | Add-Member ScriptMethod Save { }
    return $shortcut
  }
  $shell
}
try {
  $source = Join-Path $root 'fake.cs'
  @'
using System;
using System.IO;
class FakeMirage {
  static int Main(string[] args) {
    string root = Path.Combine(Environment.GetEnvironmentVariable("LOCALAPPDATA"), "MirageSSD", "credentials");
    Directory.CreateDirectory(root);
    string token = Path.Combine(root, "drive-token.json");
    int index = Array.IndexOf(args, "--token-store");
    if (index >= 0) token = args[index + 1];
    if (Array.IndexOf(args, "login") >= 0) {
      string marker = Environment.GetEnvironmentVariable("MIRAGE_TEST_PAUSE_LOGIN");
      if (!String.IsNullOrEmpty(marker)) {
        File.WriteAllText(marker, System.Diagnostics.Process.GetCurrentProcess().Id.ToString());
        System.Threading.Thread.Sleep(600000);
      }
      File.WriteAllText(token, Environment.GetEnvironmentVariable("MIRAGE_TEST_ACCOUNT") ?? "account-a");
    }
    if (Array.IndexOf(args, "logout") >= 0) { File.Delete(token); Console.WriteLine("{\"ok\":true}"); return 0; }
    if (Array.IndexOf(args, "authorize-device") >= 0 && Environment.GetEnvironmentVariable("MIRAGE_TEST_AUTH_FAIL") == "1") return 1;
    bool authenticated = File.Exists(token);
    Console.WriteLine("{\"ok\":true,\"data\":{\"authenticated\":" + (authenticated ? "true" : "false") + ",\"account_id\":\"" + (authenticated ? File.ReadAllText(token) : "") + "\",\"quota_limit_bytes\":16106127360}}");
    return 0;
  }
}
'@ | Set-Content -LiteralPath $source
  $binary = Join-Path $root 'mirage.exe'
  & "$env:SystemRoot\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /target:exe "/out:$binary" $source
  Assert-True ($LASTEXITCODE -eq 0) 'Fixture compilation failed.'
  $credentials = Join-Path $root 'public-client.json'
  '{}' | Set-Content -LiteralPath $credentials
  $cache = Join-Path $root 'cache'
  $parameters = @{ MirageExecutable = $binary; RcloneExecutable = $binary; DriveClientCredentials = $credentials; CacheDirectory = $cache; DriveLetter = 'Z' }
  $env:MIRAGE_TEST_ACCOUNT = 'account-a'
  $result = & (Join-Path $PSScriptRoot 'install-device-drive.ps1') @parameters
  Assert-True (($result -join "`n" | ConvertFrom-Json).Installed) 'Fresh install failed.'
  $device = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
  $token = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
  $metadata = Join-Path $cache 'vfsMeta\miragessd\file'
  New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($metadata)) -Force | Out-Null
  '{"Dirty":false}' | Set-Content -LiteralPath $metadata
  New-Item -ItemType Directory -Path (Join-Path $cache 'vfs') -Force | Out-Null
  'keep me' | Set-Content -LiteralPath (Join-Path $cache 'vfs\file')
  $installedBinary = Join-Path $device 'mirage.exe'
  (Get-Item -LiteralPath $installedBinary).LastWriteTime = [DateTime]'2001-01-01'
  $result = & (Join-Path $PSScriptRoot 'install-device-drive.ps1') @parameters
  Assert-True ((Get-Item -LiteralPath $installedBinary).LastWriteTime.Year -gt 2001) 'Mounted reinstall skipped binary replacement.'
  Assert-True (Test-Path -LiteralPath (Join-Path $cache 'vfs\file')) 'Same-account reinstall discarded cache.'
  $fixture.Cases += 2
  . (Join-Path $PSScriptRoot 'device-lifecycle.ps1')
  foreach ($invalid in @('{"Dirty":true}', '{}', '{"Dirty":"false"}', 'broken')) {
    $invalid | Set-Content -LiteralPath $metadata
    Assert-Rejected { Test-PendingUploads $cache }
    $fixture.Cases++
  }
  '{"Dirty":false}' | Set-Content -LiteralPath $metadata
  $env:MIRAGE_TEST_ACCOUNT = 'account-b'
  $env:MIRAGE_TEST_AUTH_FAIL = '1'
  $failedSwitch = & (Join-Path $PSScriptRoot 'account-device.ps1') -Action SwitchAccount -DriveClientCredentials $credentials -Quiet -NoConfirm
  Assert-True ($LASTEXITCODE -eq 1) 'Failed authorization reported success.'
  Assert-True ((Get-Content -LiteralPath $token -Raw) -eq 'account-a') 'Failed switch changed active credentials.'
  Assert-True $fixture.Mounted 'Failed switch did not restore old mount.'
  $fixture.Cases++
  $env:MIRAGE_TEST_AUTH_FAIL = '0'
  $switched = & (Join-Path $PSScriptRoot 'account-device.ps1') -Action SwitchAccount -DriveClientCredentials $credentials -Quiet -NoConfirm
  Assert-True ((Get-Content -LiteralPath $token -Raw) -eq 'account-b') ("Successful switch did not activate new account: " + ($switched -join ' '))
  Assert-True (-not (Test-Path -LiteralPath (Join-Path $cache 'vfs\file'))) 'Old account cache exposed to new account.'
  Assert-True (@(Get-ChildItem -LiteralPath $cache -Filter 'retained-account-*' -Directory).Count -eq 1) 'Old account cache was not retained.'
  $fixture.Cases++
  & (Join-Path $PSScriptRoot 'account-device.ps1') -Action SignOut -Quiet -NoConfirm | Out-Null
  Assert-True (-not (Test-Path -LiteralPath $token)) 'Sign out retained active credentials.'
  Assert-True (-not $fixture.Mounted) 'Sign out left drive mounted.'
  $fixture.Cases++
  & (Join-Path $PSScriptRoot 'account-device.ps1') -Action SignIn -DriveClientCredentials $credentials -Quiet -NoConfirm | Out-Null
  Assert-True ($fixture.Mounted -and (Test-Path -LiteralPath $token)) 'Sign in did not reconnect.'
  $fixture.Cases++
  & (Join-Path $device 'uninstall-device-drive.ps1') -Quiet -NoConfirm | Out-Null
  Assert-True (-not (Test-Path -LiteralPath $installedBinary)) 'Uninstall retained executable.'
  Assert-True (Test-Path -LiteralPath $token) 'Uninstall removed credentials.'
  Assert-True (Test-Path -LiteralPath (Join-Path $device 'account-state.json')) 'Uninstall removed account ownership.'
  $fixture.Cases++
  $result = & (Join-Path $PSScriptRoot 'install-device-drive.ps1') @parameters
  Assert-True (($result -join "`n" | ConvertFrom-Json).Installed) 'Reinstall after uninstall failed.'
  Assert-True ((Get-Content -LiteralPath $token -Raw) -eq 'account-b') 'Reinstall changed the account.'
  $fixture.Cases++
  # A pending upload must block uninstall without deleting anything.
  New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($metadata)) -Force | Out-Null
  '{"Dirty":true}' | Set-Content -LiteralPath $metadata
  & (Join-Path $device 'uninstall-device-drive.ps1') -InstallRoot $device -Quiet -NoConfirm | Out-Null
  Assert-True ($LASTEXITCODE -eq 1) 'Pending upload did not block uninstall.'
  Assert-True (Test-Path -LiteralPath $installedBinary) 'Blocked uninstall removed the executable.'
  Assert-True (((Get-Content -LiteralPath $metadata -Raw | ConvertFrom-Json).Dirty) -eq $true) 'Blocked uninstall removed pending uploads.'
  '{"Dirty":false}' | Set-Content -LiteralPath $metadata
  $fixture.Cases++
  # An open executable must fail safely without hiding the uninstall entry.
  $locked = [IO.File]::Open($installedBinary, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
  try {
    & (Join-Path $PSScriptRoot 'uninstall-device-drive.ps1') -InstallRoot $device -Quiet -NoConfirm | Out-Null
    Assert-True ($LASTEXITCODE -eq 1) 'Locked executable did not block uninstall.'
    Assert-True ($fixture.Registry.DisplayName -eq 'MirageSSD') 'Failed uninstall removed its Settings entry.'
  } finally { $locked.Dispose() }
  $fixture.Cases++
  # The old installer did not write a manifest until mounting succeeded.
  Remove-Item -LiteralPath (Join-Path $device 'device-install.json')
  & (Join-Path $PSScriptRoot 'uninstall-device-drive.ps1') -InstallRoot $device -Quiet -NoConfirm | Out-Null
  Assert-True (-not (Test-Path -LiteralPath $installedBinary)) 'Legacy manifest-free uninstall failed.'
  Assert-True (Test-Path -LiteralPath $token) 'Legacy recovery removed credentials.'
  $fixture.Cases++
  Assert-Rejected { Clear-VfsCache 'D:\' }
  Assert-Rejected { Clear-VfsCache 'D:\fixture\..' }
  $fixture.Cases += 2
  Write-Output "PASS: $($fixture.Cases) lifecycle scenarios; actual scripts and disposable files, simulated Google and Windows task/volume integration."
} finally {
  $env:LOCALAPPDATA = $originalLocal
  Microsoft.PowerShell.Management\Remove-Item Env:MIRAGE_TEST_ACCOUNT -ErrorAction SilentlyContinue
  Microsoft.PowerShell.Management\Remove-Item Env:MIRAGE_TEST_AUTH_FAIL -ErrorAction SilentlyContinue
  # Keep the isolated fixture for diagnosis; it contains no real account data.
}
exit 0
