[CmdletBinding()]
param(
  [string]$Source,
  [string]$Name,
  [string]$MirageExecutable,
  [string]$ResticExecutable,
  [string]$RcloneExecutable,
  [string]$TokenStore,
  [switch]$AllowConcurrentBackup
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

function Protect-Directory([string]$Path) {
  New-Item -ItemType Directory -Path $Path -Force | Out-Null
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  & icacls.exe $Path /inheritance:r /grant:r "*$($identity.User.Value):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to protect $Path."
  }
}

function Get-GameSlug([string]$Value) {
  $slug = [regex]::Replace($Value.Trim(), '[^A-Za-z0-9._-]+', '-')
  $slug = $slug.Trim('-', '.')
  if (-not $slug -or $slug.Length -gt 80) {
    throw 'Game name must produce a non-empty identifier of at most 80 characters.'
  }
  return $slug
}

if (-not $Source) {
  Add-Type -AssemblyName System.Windows.Forms
  $picker = [Windows.Forms.FolderBrowserDialog]::new()
  $picker.Description = 'Choose the complete game folder to archive'
  $picker.ShowNewFolderButton = $false
  if ($picker.ShowDialog() -ne [Windows.Forms.DialogResult]::OK) {
    return
  }
  $Source = $picker.SelectedPath
}

$sourcePath = [IO.Path]::GetFullPath($Source)
if (-not (Test-Path -LiteralPath $sourcePath -PathType Container)) {
  throw "Game folder is unavailable: $sourcePath"
}
if ([IO.Path]::GetPathRoot($sourcePath).TrimEnd('\').Equals('M:', [StringComparison]::OrdinalIgnoreCase)) {
  throw 'Archive from the native game folder, not M:. Reading and rewriting the cloud mount would be needlessly slow.'
}
if (-not $Name) {
  $Name = Split-Path -Leaf $sourcePath.TrimEnd('\')
}
$slug = Get-GameSlug $Name

if (-not $AllowConcurrentBackup) {
  $task = Get-ScheduledTask -TaskName 'MirageSSD PC Backup' -ErrorAction SilentlyContinue
  if ($task -and $task.State -eq 'Running') {
    throw 'The whole-PC backup is running. Let it finish before starting a game archive so both jobs do not split upload bandwidth.'
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

$stateRoot = Join-Path $env:LOCALAPPDATA "MirageSSD\games\$slug"
Protect-Directory $stateRoot
$manifestPath = Join-Path $stateRoot 'manifest.json'
$keyPath = Join-Path $stateRoot 'archive-key.dpapi'
$statePath = Join-Path $stateRoot 'archive-state.json'
$logPath = Join-Path $stateRoot 'archive.log'
$remoteFolder = "MirageSSD Storage/Game Archives/$slug"

if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
  $existing = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
  if (-not ([string]$existing.source).Equals($sourcePath, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Archive name '$Name' already belongs to $($existing.source). Choose a different name."
  }
}

Write-Host "Archiving '$Name' as encrypted, deduplicated packs. The original will not be removed."
$arguments = @(
  'backend', 'backup-device',
  '--restic', $restic,
  '--rclone', $rclone,
  '--source', $sourcePath,
  '--remote-folder', $remoteFolder,
  '--token-store', $tokenPath,
  '--key-store', $keyPath,
  '--state-file', $statePath,
  '--log-file', $logPath
)
& $mirage @arguments
if ($LASTEXITCODE -ne 0) {
  throw "Game archive failed. Inspect $logPath"
}

$summary = Get-Content -LiteralPath $logPath -ErrorAction Stop | ForEach-Object {
  if ($_ -like '{*') {
    try { $_ | ConvertFrom-Json } catch { }
  }
} | Where-Object { $_.message_type -eq 'summary' } | Select-Object -Last 1
if (-not $summary -or -not $summary.snapshot_id) {
  throw 'The archive completed without a readable Restic snapshot receipt.'
}

$manifest = [ordered]@{
  format_version = 1
  game_name = $Name
  archive_id = $slug
  source = $sourcePath
  remote_folder = $remoteFolder
  snapshot_id = [string]$summary.snapshot_id
  total_bytes = [int64]$summary.total_bytes_processed
  total_files = [int64]$summary.total_files_processed
  archived_utc = [DateTime]::UtcNow.ToString('O')
  key_store = $keyPath
  log_file = $logPath
}
$temporaryManifest = "$manifestPath.tmp"
[IO.File]::WriteAllText($temporaryManifest, ($manifest | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporaryManifest -Destination $manifestPath -Force
& icacls.exe $manifestPath /inheritance:r /grant:r "*$([Security.Principal.WindowsIdentity]::GetCurrent().User.Value):F" '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw 'The archive succeeded, but its local restore manifest could not be protected.'
}

[pscustomobject]@{
  Archived = $true
  Game = $Name
  Snapshot = $manifest.snapshot_id
  SizeGiB = [math]::Round($manifest.total_bytes / 1GB, 2)
  OriginalPreserved = $true
  RestoreCommand = ".\restore-game.ps1 -Name '$slug' -Apply"
} | Format-List
