[CmdletBinding()]
param(
  [string]$Name,
  [string]$Target,
  [string]$MirageExecutable,
  [string]$ResticExecutable,
  [string]$RcloneExecutable,
  [string]$TokenStore,
  [switch]$Apply,
  [switch]$AllowMerge,
  [switch]$AllowConcurrentBackup,
  [switch]$Interactive
)

$ErrorActionPreference = 'Stop'

function Resolve-RequiredFile([string]$Value, [string[]]$Candidates, [string]$Label) {
  $items = @()
  if ($Value) {
    $items += $Value
  }
  $items += $Candidates
  foreach ($item in $items) {
    if (-not $item) {
      continue
    }
    $expanded = [Environment]::ExpandEnvironmentVariables($item)
    if (Test-Path -LiteralPath $expanded -PathType Leaf) {
      return (Resolve-Path -LiteralPath $expanded).Path
    }
  }
  throw "$Label was not found."
}

$gamesRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\games'
$manifests = @(Get-ChildItem -LiteralPath $gamesRoot -Filter manifest.json -File -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
  try { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json } catch { }
} | Where-Object { $_.format_version -eq 1 })
if ($manifests.Count -eq 0) {
  throw 'No completed MirageSSD game archive is registered on this PC.'
}

if (-not $Name) {
  if (-not $Interactive) {
    throw 'Pass -Name. Available archive IDs: ' + (($manifests | ForEach-Object { $_.archive_id }) -join ', ')
  }
  Write-Host 'Available MirageSSD game archives:'
  for ($index = 0; $index -lt $manifests.Count; $index++) {
    Write-Host ("  [{0}] {1} ({2:N2} GiB)" -f ($index + 1), $manifests[$index].game_name, ([int64]$manifests[$index].total_bytes / 1GB))
  }
  $selection = Read-Host 'Choose a number'
  $selectedIndex = 0
  if (-not [int]::TryParse($selection, [ref]$selectedIndex) -or $selectedIndex -lt 1 -or $selectedIndex -gt $manifests.Count) {
    throw 'Invalid archive selection.'
  }
  $manifest = $manifests[$selectedIndex - 1]
} else {
  $manifest = $manifests | Where-Object {
    ([string]$_.archive_id).Equals($Name, [StringComparison]::OrdinalIgnoreCase) -or
    ([string]$_.game_name).Equals($Name, [StringComparison]::OrdinalIgnoreCase)
  } | Select-Object -First 1
  if (-not $manifest) {
    throw "Game archive was not found: $Name"
  }
}

if (-not $AllowConcurrentBackup) {
  $task = Get-ScheduledTask -TaskName 'MirageSSD PC Backup' -ErrorAction SilentlyContinue
  if ($task -and $task.State -eq 'Running') {
    throw 'The whole-PC backup is running. Let it finish before restoring a game so both jobs do not split network and disk bandwidth.'
  }
}

$mirage = Resolve-RequiredFile $MirageExecutable @(
  (Join-Path $PSScriptRoot 'mirage.exe'),
  (Join-Path $env:ProgramFiles 'MirageSSD\backup\bin\mirage.exe'),
  (Join-Path $PSScriptRoot '..\target\release\mirage.exe'),
  (Join-Path $env:LOCALAPPDATA 'MirageSSD\device\mirage.exe')
) 'MirageSSD executable'
$restic = Resolve-RequiredFile $ResticExecutable @(
  (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\bin\restic.exe'),
  (Join-Path $env:ProgramData 'MirageSSD\backup\bin\restic.exe')
) 'Restic executable'
$rclone = Resolve-RequiredFile $RcloneExecutable @(
  (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\bin\rclone.exe'),
  (Join-Path $env:ProgramData 'MirageSSD\backup\bin\rclone.exe'),
  (Join-Path $env:LOCALAPPDATA 'MirageSSD\device\rclone.exe')
) 'Rclone executable'
if (-not $TokenStore) {
  $TokenStore = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
}
$tokenPath = Resolve-RequiredFile $TokenStore @() 'Protected Drive authorization'

if (-not $Target) {
  $Target = [string]$manifest.source
}
$targetPath = [IO.Path]::GetFullPath($Target)
if ([IO.Path]::GetPathRoot($targetPath).TrimEnd('\').Equals('M:', [StringComparison]::OrdinalIgnoreCase)) {
  throw 'Restore to a native SSD path such as D:, not back into the cloud mount.'
}

$restoreLog = Join-Path (Split-Path -Parent ([string]$manifest.key_store)) 'restore.log'
$arguments = @(
  'backend', 'restore-device',
  '--restic', $restic,
  '--rclone', $rclone,
  '--snapshot', ([string]$manifest.snapshot_id),
  '--include', ([string]$manifest.source),
  '--target', $targetPath,
  '--remote-folder', ([string]$manifest.remote_folder),
  '--token-store', $tokenPath,
  '--key-store', ([string]$manifest.key_store),
  '--log-file', $restoreLog
)

Write-Host "Checking restore plan for '$($manifest.game_name)'..."
& $mirage @arguments
if ($LASTEXITCODE -ne 0) {
  throw "Restore plan failed. Inspect $restoreLog"
}

if (-not $Apply -and $Interactive) {
  $answer = Read-Host "Restore and verify now to '$targetPath'? Type YES to continue"
  $Apply = $answer -ceq 'YES'
}
if (-not $Apply) {
  [pscustomobject]@{
    PlanPassed = $true
    Game = $manifest.game_name
    Target = $targetPath
    SizeGiB = [math]::Round(([int64]$manifest.total_bytes / 1GB), 2)
    ApplyCommand = ".\restore-game.ps1 -Name '$($manifest.archive_id)' -Target '$targetPath' -Apply"
  } | Format-List
  return
}

if (Test-Path -LiteralPath $targetPath -PathType Container) {
  $hasContent = Get-ChildItem -LiteralPath $targetPath -Force -ErrorAction Stop | Select-Object -First 1
  if ($hasContent -and -not $AllowMerge) {
    throw "Restore target is not empty: $targetPath. Use -AllowMerge only if overwriting changed files is intentional."
  }
}
$driveRoot = [IO.Path]::GetPathRoot($targetPath)
$driveName = $driveRoot.TrimEnd('\').TrimEnd(':')
$drive = Get-PSDrive -Name $driveName -PSProvider FileSystem -ErrorAction Stop
if ([int64]$drive.Free -lt ([int64]$manifest.total_bytes + 2GB)) {
  throw ("Not enough free space on {0}. Need approximately {1:N2} GiB plus a 2 GiB reserve; available {2:N2} GiB." -f $driveRoot, ([int64]$manifest.total_bytes / 1GB), ($drive.Free / 1GB))
}

Write-Host "Restoring and verifying '$($manifest.game_name)' to '$targetPath'..."
& $mirage @arguments --apply
if ($LASTEXITCODE -ne 0) {
  throw "Verified restore failed. Inspect $restoreLog"
}

[pscustomobject]@{
  Restored = $true
  Verified = $true
  Game = $manifest.game_name
  Target = $targetPath
  Snapshot = $manifest.snapshot_id
} | Format-List
