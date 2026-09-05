[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [string]$MirageExecutable,
  [Parameter(Mandatory)]
  [string]$ResticExecutable,
  [Parameter(Mandatory)]
  [string]$RcloneExecutable,
  [string[]]$Source = @('C:\', 'D:\'),
  [string[]]$ExcludePath = @('D:\MirageSSD-Cache'),
  [string]$RemoteFolder = 'MirageSSD Storage/System Backup',
  [string]$TaskName = 'MirageSSD PC Backup',
  [switch]$DeferInitialRun
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
  if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
    return $Value
  }
  $builder = [Text.StringBuilder]::new()
  [void]$builder.Append('"')
  $backslashes = 0
  foreach ($character in $Value.ToCharArray()) {
    if ($character -eq [char]92) {
      $backslashes++
      continue
    }
    if ($character -eq [char]34) {
      [void]$builder.Append([char]92, (2 * $backslashes + 1))
      [void]$builder.Append($character)
    } else {
      [void]$builder.Append([char]92, $backslashes)
      [void]$builder.Append($character)
    }
    $backslashes = 0
  }
  [void]$builder.Append([char]92, (2 * $backslashes))
  [void]$builder.Append('"')
  return $builder.ToString()
}

$mirageSource = Resolve-RequiredFile $MirageExecutable 'MirageSSD executable'
$resticSource = Resolve-RequiredFile $ResticExecutable 'Restic executable'
$rcloneSource = Resolve-RequiredFile $RcloneExecutable 'Rclone executable'
$runnerSource = Resolve-RequiredFile (Join-Path $PSScriptRoot 'run-pc-backup-task.ps1') 'Backup task runner'
$sourcePaths = foreach ($item in $Source) {
  $path = [IO.Path]::GetFullPath($item)
  if (-not (Test-Path -LiteralPath $path -PathType Container)) {
    throw "Backup source is unavailable: $path"
  }
  $path
}
$excludedPaths = foreach ($item in $ExcludePath) {
  [IO.Path]::GetFullPath($item)
}

$tokenSource = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
if (-not (Test-Path -LiteralPath $tokenSource -PathType Leaf)) {
  throw "Drive authorization is unavailable: $tokenSource"
}
$existingKeySource = Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\pc-backup-key.dpapi'
$migrationStateFile = Join-Path $env:LOCALAPPDATA 'MirageSSD\migration\status.json'

$stateRoot = Join-Path $env:ProgramData 'MirageSSD\backup\state'
$installRoot = Join-Path $env:ProgramFiles 'MirageSSD\backup'
$bin = Join-Path $installRoot 'bin'
$installedMirage = Join-Path $bin 'mirage.exe'
$installedRestic = Join-Path $bin 'restic.exe'
$installedRclone = Join-Path $bin 'rclone.exe'
$installedRunner = Join-Path $installRoot 'run-pc-backup-task.ps1'
$configurationFile = Join-Path $stateRoot 'task-config.json'
$stateFile = Join-Path $stateRoot 'state.json'
$logFile = Join-Path $stateRoot 'backup.log'
$runnerLog = Join-Path $stateRoot 'task-runner.log'
$keyStore = Join-Path $stateRoot 'pc-backup-key.dpapi'
$tokenStore = Join-Path $stateRoot 'drive-token.json'
$migrationGuardFile = Join-Path $stateRoot 'defer-while-d-migration.flag'
New-Item -ItemType Directory -Path $bin -Force | Out-Null
New-Item -ItemType Directory -Path $stateRoot -Force | Out-Null

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User.Value
foreach ($path in @($stateRoot, $installRoot, $bin)) {
  & icacls.exe $path /inheritance:r /grant:r "*$($sid):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to secure $path."
  }
}

Copy-Item -LiteralPath $mirageSource -Destination $installedMirage -Force
Copy-Item -LiteralPath $resticSource -Destination $installedRestic -Force
Copy-Item -LiteralPath $rcloneSource -Destination $installedRclone -Force
Copy-Item -LiteralPath $runnerSource -Destination $installedRunner -Force
Copy-Item -LiteralPath $tokenSource -Destination $tokenStore -Force
if (Test-Path -LiteralPath $existingKeySource -PathType Leaf) {
  Copy-Item -LiteralPath $existingKeySource -Destination $keyStore -Force
}

$configuration = [ordered]@{
  format_version = 1
  mirage_executable = $installedMirage
  restic_executable = $installedRestic
  rclone_executable = $installedRclone
  token_store = $tokenStore
  migration_state_file = $migrationStateFile
  state_root = $stateRoot
  remote_folder = $RemoteFolder
  sources = @($sourcePaths)
  exclusions = @($excludedPaths)
}
[IO.File]::WriteAllText(
  $configurationFile,
  ($configuration | ConvertTo-Json -Depth 4 -Compress),
  [Text.UTF8Encoding]::new($false)
)
foreach ($path in @($tokenStore, $configurationFile, $keyStore)) {
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { continue }
  & icacls.exe $path /inheritance:r /grant:r "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to secure $path."
  }
}
if ($DeferInitialRun) {
  [IO.File]::WriteAllText($migrationGuardFile, 'd-drive-migration', [Text.UTF8Encoding]::new($false))
  & icacls.exe $migrationGuardFile /inheritance:r /grant:r "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw 'Failed to secure the backup migration guard.'
  }
}

$powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$taskArguments = @(
  '-NoProfile',
  '-NonInteractive',
  '-ExecutionPolicy', 'Bypass',
  '-File', (Quote-TaskArgument $installedRunner),
  '-Configuration', (Quote-TaskArgument $configurationFile)
)
$action = New-ScheduledTaskAction `
  -Execute $powershell `
  -Argument ($taskArguments -join ' ') `
  -WorkingDirectory $installRoot
$daily = New-ScheduledTaskTrigger -Daily -At '2:00 AM'
$principal = New-ScheduledTaskPrincipal -UserId $identity.Name -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
  -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries `
  -StartWhenAvailable `
  -WakeToRun `
  -ExecutionTimeLimit ([TimeSpan]::Zero) `
  -MultipleInstances IgnoreNew

Register-ScheduledTask `
  -TaskName $TaskName `
  -Action $action `
  -Trigger $daily `
  -Principal $principal `
  -Settings $settings `
  -Description 'Encrypted, deduplicated C: and D: snapshots to the authenticated MirageSSD Google Drive repository.' `
  -Force `
  -ErrorAction Stop | Out-Null

$registered = Get-ScheduledTask -TaskName $TaskName -ErrorAction Stop
if ($registered.Principal.RunLevel -ne 'Highest') {
  throw 'Backup task was not registered at the required highest run level.'
}
if (-not $DeferInitialRun) {
  Start-ScheduledTask -TaskName $TaskName -ErrorAction Stop
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    Start-Sleep -Milliseconds 250
    $registered = Get-ScheduledTask -TaskName $TaskName -ErrorAction Stop
    $taskInfo = Get-ScheduledTaskInfo -TaskName $TaskName -ErrorAction Stop
  } while ($registered.State -eq 'Queued' -and [DateTime]::UtcNow -lt $deadline)
  if ($registered.State -ne 'Running' -and $taskInfo.LastTaskResult -ne 0x00041301) {
    $result = '0x{0:X8}' -f [uint32]$taskInfo.LastTaskResult
    throw "Backup task did not start successfully ($result)."
  }
}

[pscustomobject]@{
  Installed = $true
  Started = -not $DeferInitialRun
  TaskName = $TaskName
  Schedule = 'Daily at 02:00, start when available'
  RunLevel = 'Highest'
  Sources = $sourcePaths
  Exclusions = $excludedPaths
  StateFile = $stateFile
  LogFile = $logFile
  RunnerLog = $runnerLog
  TokenStore = $tokenStore
  RuntimeDirectory = $bin
  KeyProtection = 'Current-user DPAPI and restricted ACL'
  RepositoryEncryption = 'Restic authenticated encryption'
} | ConvertTo-Json -Depth 4 -Compress
