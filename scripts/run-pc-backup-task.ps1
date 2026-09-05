[CmdletBinding()]
param(
  [string]$Configuration,
  [string]$MirageExecutable = (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\bin\mirage-backup.exe'),
  [string]$ResticExecutable = (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\bin\restic.exe'),
  [string]$RcloneExecutable = (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\bin\rclone.exe'),
  [string]$TokenStore = (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup\drive-token.json'),
  [string]$MigrationStateFile = (Join-Path $env:LOCALAPPDATA 'MirageSSD\migration\status.json'),
  [string]$StateRoot = (Join-Path $env:LOCALAPPDATA 'MirageSSD\backup'),
  [string]$RemoteFolder = 'MirageSSD Storage/System Backup',
  [string[]]$Source = @('C:\', 'D:\'),
  [string[]]$ExcludePath = @('D:\MirageSSD-Cache')
)

$ErrorActionPreference = 'Stop'

if ($Configuration) {
  $configurationPath = [IO.Path]::GetFullPath($Configuration)
  if (-not (Test-Path -LiteralPath $configurationPath -PathType Leaf)) {
    throw "Backup task configuration is unavailable: $configurationPath"
  }
  $config = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
  $allowed = @(
    'format_version', 'mirage_executable', 'restic_executable', 'rclone_executable',
    'token_store', 'migration_state_file', 'state_root', 'remote_folder', 'sources', 'exclusions'
  )
  foreach ($name in $config.PSObject.Properties.Name) {
    if ($name -notin $allowed) {
      throw "Backup task configuration contains an unknown property: $name"
    }
  }
  if ($config.format_version -ne 1) {
    throw 'Backup task configuration version is unsupported.'
  }
  $MirageExecutable = [string]$config.mirage_executable
  $ResticExecutable = [string]$config.restic_executable
  $RcloneExecutable = [string]$config.rclone_executable
  $TokenStore = [string]$config.token_store
  $MigrationStateFile = [string]$config.migration_state_file
  $StateRoot = [string]$config.state_root
  $RemoteFolder = [string]$config.remote_folder
  $Source = @($config.sources | ForEach-Object { [string]$_ })
  $ExcludePath = @($config.exclusions | ForEach-Object { [string]$_ })
}

$StateRoot = [IO.Path]::GetFullPath($StateRoot)
$MirageExecutable = [IO.Path]::GetFullPath($MirageExecutable)
$ResticExecutable = [IO.Path]::GetFullPath($ResticExecutable)
$RcloneExecutable = [IO.Path]::GetFullPath($RcloneExecutable)
$TokenStore = [IO.Path]::GetFullPath($TokenStore)
$MigrationStateFile = [IO.Path]::GetFullPath($MigrationStateFile)
$Source = @($Source | ForEach-Object { [IO.Path]::GetFullPath($_) })
$ExcludePath = @($ExcludePath | ForEach-Object { [IO.Path]::GetFullPath($_) })

$diagnosticLog = Join-Path $StateRoot 'task-runner.log'
New-Item -ItemType Directory -Path $StateRoot -Force | Out-Null
function Write-TaskDiagnostic([string]$Message) {
  $line = '{0} {1}{2}' -f [DateTime]::UtcNow.ToString('O'), $Message, [Environment]::NewLine
  [IO.File]::AppendAllText($diagnosticLog, $line, [Text.UTF8Encoding]::new($false))
}

try {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = [Security.Principal.WindowsPrincipal]::new($identity)
  $elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  Write-TaskDiagnostic "runner-start user=$($identity.Name) elevated=$elevated"

  $requiredFiles = @($MirageExecutable, $ResticExecutable, $RcloneExecutable, $TokenStore)
  foreach ($path in $requiredFiles) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
      throw "Required backup component is unavailable: $path"
    }
  }

  $migrationGuardFile = Join-Path $StateRoot 'defer-while-d-migration.flag'
  if (Test-Path -LiteralPath $migrationGuardFile -PathType Leaf) {
    Write-TaskDiagnostic 'runner-deferred reason=d-drive-migration-guard'
    exit 0
  }

  $migrationProcessActive = @(
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
      Where-Object { $_.CommandLine -match 'migrate-d-drive\.ps1|backend\s+ingest-device' }
  ).Count -gt 0
  if ($migrationProcessActive) {
    Write-TaskDiagnostic 'runner-deferred reason=d-drive-migration-process'
    exit 0
  }

  $migrationStateExists = Test-Path -LiteralPath $MigrationStateFile -PathType Leaf
  Write-TaskDiagnostic "migration-gate path=$MigrationStateFile exists=$migrationStateExists"
  if ($migrationStateExists) {
    $migrationState = Get-Content -LiteralPath $MigrationStateFile -Raw | ConvertFrom-Json
    Write-TaskDiagnostic "migration-gate status=$([string]$migrationState.status)"
    if ([string]$migrationState.status -eq 'running') {
      Write-TaskDiagnostic 'runner-deferred reason=d-drive-migration-running'
      exit 0
    }
  }

  $arguments = @(
    'backend', 'backup-device',
    '--restic', $ResticExecutable,
    '--rclone', $RcloneExecutable,
    '--remote-folder', $RemoteFolder,
    '--token-store', $TokenStore,
    '--key-store', (Join-Path $StateRoot 'pc-backup-key.dpapi'),
    '--state-file', (Join-Path $StateRoot 'state.json'),
    '--log-file', (Join-Path $StateRoot 'backup.log')
  )
  foreach ($path in $Source) {
    $arguments += @('--source', $path)
  }
  foreach ($path in $ExcludePath) {
    $arguments += @('--exclude', $path)
  }

  & $MirageExecutable @arguments 2>> $diagnosticLog
  $exitCode = $LASTEXITCODE
  Write-TaskDiagnostic "runner-finish exit=$exitCode"
  exit $exitCode
} catch {
  Write-TaskDiagnostic "runner-error $($_.Exception.Message)"
  exit 1
}
