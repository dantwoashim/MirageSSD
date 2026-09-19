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
. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')

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
$installRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
$logRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\logs'
$installedMirage = Join-Path $installRoot 'mirage.exe'
$installedRclone = Join-Path $installRoot 'rclone.exe'
$launcherFile = Join-Path $installRoot 'mount-device.vbs'
$logFile = Join-Path $logRoot 'device-drive.log'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User.Value
$operation = [Threading.Mutex]::new($false, ('Local\MirageSSD-Device-' + $sid))
$operationHeld = $false
try {
try { $operationHeld = $operation.WaitOne(0) } catch [Threading.AbandonedMutexException] { $operationHeld = $true }
if (-not $operationHeld) { throw 'Another MirageSSD setup or account operation is running.' }
$previous = Get-DeviceInstallRecord $installRoot
$configurationPath = Join-Path $installRoot 'device-install.json'
if ($previous) {
  if ($previous.format -ne 'miragessd-device-install-v1' -or $previous.owner_sid -ne $sid -or $previous.task_name -ne "MirageSSD Drive $sid") { throw 'Invalid existing installation owner.' }
  $cachePath = [IO.Path]::GetFullPath([string]$previous.cache_directory)
  $mount = [string]$previous.drive_letter
  if ($mount -notmatch '^[D-Z]:\\$') { throw 'Invalid recorded drive letter.' }
  $DriveLetter = $mount.Substring(0, 1)
}
if (Test-Path -LiteralPath $mount) {
  $volume = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$($mount.TrimEnd('\'))'" -ErrorAction Stop
  if (-not $previous -or $volume.VolumeName -ne 'MirageSSD') { throw 'The drive letter is occupied by another volume.' }
}
Test-PendingUploads $cachePath
Stop-DeviceDrive "MirageSSD Drive $sid" $installRoot
Test-PendingUploads $cachePath
$deadline = [DateTime]::UtcNow.AddSeconds(15)
while ((Test-Path -LiteralPath $mount) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
if (Test-Path -LiteralPath $mount) { throw 'The previous drive has not disconnected. Close open files and retry.' }
New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
New-Item -ItemType Directory -Path $cachePath -Force | Out-Null

foreach ($path in @($installRoot, $logRoot, $cachePath)) {
  & icacls.exe $path /inheritance:r /grant:r "*$($sid):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to secure $path."
  }
}

Register-DeviceRecovery $installRoot $cachePath $mount "MirageSSD Drive $sid" $PSScriptRoot
Write-DeviceJson (Join-Path $installRoot 'setup-state.json') @{ state='installing'; version='0.1.4' }
foreach ($binary in @(@($mirageSource, $installedMirage), @($rcloneSource, $installedRclone))) {
  if ([IO.Path]::GetFullPath($binary[0]) -eq [IO.Path]::GetFullPath($binary[1])) { continue }
  $stagedBinary = $binary[1] + '.install-new'
  Copy-Item -LiteralPath $binary[0] -Destination $stagedBinary -Force
  if (Test-Path -LiteralPath $binary[1]) { [IO.File]::Replace($stagedBinary, $binary[1], [NullString]::Value) }
  else { [IO.File]::Move($stagedBinary, $binary[1]) }
}

$tokenStore = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
if (-not (Test-Path -LiteralPath $tokenStore -PathType Leaf)) {
  if ($credentialSource) {
    & $installedMirage backend login --client-credentials $credentialSource --timeout-seconds 600 | Out-Null
  } elseif ($ClientId) {
    & $installedMirage backend login --client-id $ClientId --timeout-seconds 600 | Out-Null
  } else {
    throw 'First-time setup requires a Google OAuth desktop client ID.'
  }
  if ($LASTEXITCODE -ne 0) {
    throw 'Google Drive sign-in did not complete.'
  }
}

$currentAccount = Get-SignedInAccount $installedMirage
$stateFile = Join-Path $installRoot 'account-state.json'
$recordedAccount = Read-AccountState
if (-not $currentAccount) { throw 'The signed-in Google account could not be identified.' }
if (-not $recordedAccount -or $recordedAccount.account_id -ne $currentAccount) {
  Clear-VfsCache $cachePath
}
Write-AccountState $true $currentAccount

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
$prefetchScript = Join-Path $installRoot 'prefetch-device.ps1'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'prefetch-device.ps1') -Destination $prefetchScript -Force
$accountScript = Join-Path $installRoot 'account-device.ps1'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'device-lifecycle.ps1') -Destination $installRoot -Force
$accountScriptSource = Join-Path $PSScriptRoot 'account-device.ps1'
if (Test-Path -LiteralPath $accountScriptSource -PathType Leaf) {
  Copy-Item -LiteralPath $accountScriptSource -Destination $accountScript -Force
}
$installedClientConfiguration = Join-Path $installRoot 'oauth-desktop.json'
if ($credentialSource) {
  Copy-Item -LiteralPath $credentialSource -Destination $installedClientConfiguration -Force
}
$accountStateFile = Join-Path $installRoot 'account-state.json'
$statusOutput = @(& $installedMirage --json backend status)
$statusAccount = $null
try {
  $statusEnvelope = ($statusOutput -join "`n") | ConvertFrom-Json
  if ($statusEnvelope.ok -and $statusEnvelope.data.authenticated) { $statusAccount = [string]$statusEnvelope.data.account_id }
} catch { }
$accountState = @{format='miragessd-account-state-v1';signed_in=$true;account_id=$statusAccount}
[IO.File]::WriteAllText($accountStateFile, ($accountState | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$accountFiles = @($accountScript, $accountStateFile)
if (Test-Path -LiteralPath $installedClientConfiguration -PathType Leaf) { $accountFiles += $installedClientConfiguration }
foreach ($path in $accountFiles) {
  if (Test-Path -LiteralPath $path -PathType Leaf) {
    & icacls.exe $path /inheritance:r /grant:r "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw 'Failed to secure the account-management files.'
    }
  }
}
$configuration = @{format='miragessd-device-install-v1';task_name=$taskName;cache_directory=$cachePath;cache_min_free_space=$CacheMinFreeSpace;drive_letter=$mount;owner_sid=$sid}
[IO.File]::WriteAllText((Join-Path $installRoot 'device-install.json'), ($configuration | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MirageSSDWritableDevice'
New-Item -Path $uninstallKey -Force | Out-Null
$uninstallCommand = (Quote-TaskArgument $wscript) + ' //B //NoLogo ' + (Quote-TaskArgument $hiddenRunner) + ' ' + (Quote-TaskArgument $uninstallScript)
$uninstallProperties = @{DisplayName='MirageSSD';DisplayVersion='0.1.4';Publisher='MirageSSD';InstallLocation=$installRoot;UninstallString=$uninstallCommand}
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

$programs = [Environment]::GetFolderPath('Programs')
if ($programs) {
  $shell = New-Object -ComObject WScript.Shell
  $shortcut = $shell.CreateShortcut((Join-Path $programs 'MirageSSD Prepare Files.lnk'))
  $shortcut.TargetPath = $wscript
  $shortcut.Arguments = '//B //NoLogo ' + (Quote-TaskArgument $hiddenRunner) + ' ' + (Quote-TaskArgument $prefetchScript)
  $shortcut.Description = 'Opt-in ZIP metadata preparation through the mounted MirageSSD drive'
  $shortcut.Save()
}
if ($programs -and (Test-Path -LiteralPath $accountScript -PathType Leaf)) {
  $shell = New-Object -ComObject WScript.Shell
  $shortcut = $shell.CreateShortcut((Join-Path $programs 'MirageSSD Account.lnk'))
  $shortcut.TargetPath = $wscript
  $shortcut.Arguments = '//B //NoLogo ' + (Quote-TaskArgument $hiddenRunner) + ' ' + (Quote-TaskArgument $accountScript)
  $shortcut.Description = 'Sign out of MirageSSD or switch to a different Google account'
  $shortcut.Save()
}

Start-ScheduledTask -TaskName $taskName

$deadline = [DateTime]::UtcNow.AddSeconds(55)
while (-not (Test-Path -LiteralPath $mount) -and [DateTime]::UtcNow -lt $deadline) {
  Start-Sleep -Milliseconds 250
}
if (-not (Test-Path -LiteralPath $mount)) {
  throw 'The persistent MirageSSD task was installed but the drive did not become ready.'
}

Write-DeviceJson (Join-Path $installRoot 'setup-state.json') @{ state='ready'; version='0.1.4' }
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
} finally {
  if ($operationHeld) { $operation.ReleaseMutex() }
  $operation.Dispose()
}
