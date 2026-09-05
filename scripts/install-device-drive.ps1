[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [string]$MirageExecutable,
  [Parameter(Mandatory)]
  [string]$RcloneExecutable,
  [string]$ClientId,
  [string]$DriveClientCredentials,
  [ValidatePattern('^[D-Zd-z]:?$')]
  [string]$DriveLetter = 'M',
  [string]$RemoteFolder = 'MirageSSD Storage',
  [string]$CacheDirectory = "$env:LOCALAPPDATA\MirageSSD\cache",
  [string]$CacheMaxSize = '128Gi',
  [string]$CacheMinFreeSpace = '24Gi',
  [string]$CacheMaxAge = '720h',
  [string]$ReadAhead = '128Mi',
  [string]$ReadChunkSize = '16Mi',
  [ValidateRange(1, 16)]
  [int]$ReadChunkStreams = 4
)

$ErrorActionPreference = 'Stop'

function Resolve-RequiredFile([string]$Value, [string]$Label) {
  $resolved = Resolve-Path -LiteralPath $Value -ErrorAction Stop
  if (-not (Test-Path -LiteralPath $resolved.Path -PathType Leaf)) {
    throw "$Label is not a file."
  }
  return $resolved.Path
}

function Quote-TaskArgument([string]$Value) {
  if ($Value.Contains('"')) {
    throw 'Scheduled-task arguments may not contain quotation marks.'
  }
  return '"' + $Value + '"'
}

$mirageSource = Resolve-RequiredFile $MirageExecutable 'MirageSSD executable'
$rcloneSource = Resolve-RequiredFile $RcloneExecutable 'Rclone executable'
$credentialSource = if ($DriveClientCredentials) {
  Resolve-RequiredFile $DriveClientCredentials 'Drive client credential'
} else {
  $null
}
if ($ClientId) {
  $ClientId = $ClientId.Trim()
  if ($ClientId -notmatch '^[^\s\x00-\x1f]{1,480}\.apps\.googleusercontent\.com$') {
    throw 'Google OAuth desktop client ID is invalid.'
  }
}
$cachePath = [System.IO.Path]::GetFullPath($CacheDirectory)
$mount = $DriveLetter.TrimEnd(':').ToUpperInvariant() + ':\'
if (Test-Path -LiteralPath $mount) {
  $volume = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$($mount.TrimEnd('\'))'" -ErrorAction SilentlyContinue
  if ($volume.VolumeName -eq 'MirageSSD') {
    [pscustomobject]@{
      Installed = $true
      AlreadyRunning = $true
      PersistentAtLogon = $true
      MountPoint = $mount
      VolumeName = 'MirageSSD'
    } | ConvertTo-Json -Compress
    return
  }
  throw "$mount is already in use by another volume."
}

$installRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
$logRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\logs'
$installedMirage = Join-Path $installRoot 'mirage.exe'
$installedRclone = Join-Path $installRoot 'rclone.exe'
$launcherFile = Join-Path $installRoot 'mount-device.vbs'
$logFile = Join-Path $logRoot 'device-drive.log'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User.Value
$previousTask = Get-ScheduledTask -TaskName "MirageSSD Drive $sid" -ErrorAction SilentlyContinue
if ($previousTask -and $previousTask.State -eq 'Running') {
  # The mounted-volume guard above already returned for a running drive.
  # Stop only this user's failed startup before replacing its payload.
  Stop-ScheduledTask -TaskName $previousTask.TaskName
  Start-Sleep -Seconds 1
}
New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
New-Item -ItemType Directory -Path $cachePath -Force | Out-Null
Copy-Item -LiteralPath $mirageSource -Destination $installedMirage -Force
Copy-Item -LiteralPath $rcloneSource -Destination $installedRclone -Force

foreach ($path in @($installRoot, $logRoot, $cachePath)) {
  & icacls.exe $path /inheritance:r /grant:r "*$($sid):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to secure $path."
  }
}

$tokenStore = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
if (-not (Test-Path -LiteralPath $tokenStore -PathType Leaf)) {
  if ($credentialSource) {
    & $installedMirage backend login --client-credentials $credentialSource --timeout-seconds 600
  } elseif ($ClientId) {
    & $installedMirage backend login --client-id $ClientId --timeout-seconds 600
  } else {
    throw 'First-time setup requires a Google OAuth desktop client ID.'
  }
  if ($LASTEXITCODE -ne 0) {
    throw 'Google Drive sign-in did not complete.'
  }
}

if ($credentialSource) {
  & $installedMirage --json backend authorize-device --drive-client-credentials $credentialSource | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw 'Failed to protect the optional Drive client credential for persistent mounting.'
  }
}

$capacityValue = $null
if ($credentialSource) {
  $quotaOutput = @(& $installedMirage --json backend verify-live --client-credentials $credentialSource --token-store $tokenStore)
  if ($LASTEXITCODE -ne 0) { throw 'Google Drive could not refresh this login. Please run setup again and reconnect.' }
  $quotaEnvelope = ($quotaOutput -join "`n") | ConvertFrom-Json
  if (-not $quotaEnvelope.ok -or -not $quotaEnvelope.data.authenticated) { throw 'Google Drive login verification failed.' }
  if ($quotaEnvelope.data.quota_limit_bytes) {
    $capacityGiB = [double]$quotaEnvelope.data.quota_limit_bytes / 1GB
    $capacityValue = $capacityGiB.ToString('0.################', [Globalization.CultureInfo]::InvariantCulture) + 'Gi'
  }
}

$arguments = @(
  'backend', 'mount-device',
  '--rclone', (Quote-TaskArgument $installedRclone),
  '--drive-letter', $DriveLetter.TrimEnd(':').ToUpperInvariant(),
  '--remote-folder', (Quote-TaskArgument $RemoteFolder),
  '--cache-dir', (Quote-TaskArgument $cachePath),
  '--log-file', (Quote-TaskArgument $logFile),
  '--token-store', (Quote-TaskArgument $tokenStore),
  '--cache-max-size', $CacheMaxSize,
  '--cache-min-free-space', $CacheMinFreeSpace,
  '--cache-max-age', $CacheMaxAge,
  '--read-ahead', $ReadAhead,
  '--read-chunk-size', $ReadChunkSize,
  '--read-chunk-streams', $ReadChunkStreams
) -join ' '

$commandLine = (Quote-TaskArgument $installedMirage) + ' ' + $arguments
$vbsCommand = $commandLine.Replace('"', '""')
$vbsDirectory = $installRoot.Replace('"', '""')
$capacityLine = if ($capacityValue) { 'processEnv("RCLONE_VFS_DISK_SPACE_TOTAL_SIZE") = "' + $capacityValue + '"' } else { '' }
$launcher = @"
Option Explicit
Dim shell, exitCode, files, processEnv
Set shell = CreateObject("WScript.Shell")
Set files = CreateObject("Scripting.FileSystemObject")
Set processEnv = shell.Environment("Process")
shell.CurrentDirectory = "$vbsDirectory"
$capacityLine
Do
  If Not files.DriveExists("$($mount.TrimEnd('\'))") Then
  On Error Resume Next
  exitCode = shell.Run("$vbsCommand", 0, True)
  On Error GoTo 0
  End If
  WScript.Sleep 5000
Loop
"@
[IO.File]::WriteAllText($launcherFile, $launcher, [Text.UTF8Encoding]::new($false))
& icacls.exe $launcherFile /inheritance:r /grant:r "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw 'Failed to secure the device-drive launcher.'
}

$taskName = "MirageSSD Drive $sid"
$wscript = Join-Path $env:SystemRoot 'System32\wscript.exe'
$action = New-ScheduledTaskAction -Execute $wscript -Argument ('//B //NoLogo ' + (Quote-TaskArgument $launcherFile)) -WorkingDirectory $installRoot
$triggers = @((New-ScheduledTaskTrigger -AtLogOn -User $sid), (New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)))
$principal = New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $triggers -Principal $principal -Settings $settings -Description 'Mount MirageSSD at sign-in; restart the supervisor within one minute if it stops.' -Force | Out-Null
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $runKey -Force | Out-Null
$startupCommand = (Quote-TaskArgument (Join-Path $env:SystemRoot 'System32\schtasks.exe')) + ' /Run /TN ' + (Quote-TaskArgument $taskName)
New-ItemProperty -Path $runKey -Name 'MirageSSDWritableDevice' -PropertyType String -Value $startupCommand -Force | Out-Null

Start-ScheduledTask -TaskName $taskName

$deadline = [DateTime]::UtcNow.AddSeconds(55)
while (-not (Test-Path -LiteralPath $mount) -and [DateTime]::UtcNow -lt $deadline) {
  Start-Sleep -Milliseconds 250
}
if (-not (Test-Path -LiteralPath $mount)) {
  throw 'The persistent MirageSSD task was installed but the drive did not become ready.'
}

$driveRecord = Join-Path $installRoot 'drive-letter.txt'
[IO.File]::WriteAllText($driveRecord, $mount, [Text.UTF8Encoding]::new($false))
& icacls.exe $driveRecord /inheritance:r /grant:r "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw 'Failed to secure the device-drive configuration.'
}

$uninstallScript = Join-Path $installRoot 'uninstall-device-drive.ps1'
$hiddenRunner = Join-Path $installRoot 'run-powershell-hidden.vbs'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'uninstall-device-drive.ps1') -Destination $uninstallScript -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'run-powershell-hidden.vbs') -Destination $hiddenRunner -Force
$configuration = @{format='miragessd-device-install-v1';task_name=$taskName;cache_directory=$cachePath;drive_letter=$mount;owner_sid=$sid}
[IO.File]::WriteAllText((Join-Path $installRoot 'device-install.json'), ($configuration | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MirageSSDWritableDevice'
New-Item -Path $uninstallKey -Force | Out-Null
$uninstallCommand = (Quote-TaskArgument $wscript) + ' //B //NoLogo ' + (Quote-TaskArgument $hiddenRunner) + ' ' + (Quote-TaskArgument $uninstallScript)
$uninstallProperties = @{DisplayName='MirageSSD (Friend Preview)';DisplayVersion='0.1.0';Publisher='MirageSSD';InstallLocation=$installRoot;UninstallString=$uninstallCommand}
$uninstallProperties.GetEnumerator() | ForEach-Object {
  New-ItemProperty -Path $uninstallKey -Name $_.Key -Value $_.Value -PropertyType String -Force | Out-Null
}

$desktop = [Environment]::GetFolderPath('DesktopDirectory')
if ($desktop) {
  $shell = New-Object -ComObject WScript.Shell
  $shortcut = $shell.CreateShortcut((Join-Path $desktop 'MirageSSD.lnk'))
  $shortcut.TargetPath = $mount
  $shortcut.Description = 'Open MirageSSD cloud-backed drive'
  $shortcut.Save()
}

[pscustomobject]@{
  Installed = $true
  PersistentAtLogon = $true
  StartupAgent = $taskName
  MountPoint = $mount
  VolumeName = 'MirageSSD'
  CacheMode = 'full'
  WriteBack = $true
  WriteBackDelay = '5s'
  ColdReadStreams = $ReadChunkStreams
} | ConvertTo-Json -Compress
