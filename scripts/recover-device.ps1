[CmdletBinding()]
param([switch]$Quiet)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')
try {
  Stop-OrphanedSetup
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $mutex = [Threading.Mutex]::new($false, ('Local\MirageSSD-Device-' + $sid))
  $held = $false
  try {
    try { $held = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $held = $true }
    if (-not $held) { throw 'A setup or account operation is still active. Close its window first, then retry recovery.' }
    $root = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
    $record = Get-DeviceInstallRecord $root
    if ($record -and ((Test-Path -LiteralPath (Join-Path $root 'mirage.exe')) -or (Test-Path -LiteralPath (Join-Path $root 'mount-device.vbs')) -or (Test-Path -LiteralPath (Join-Path $root 'setup-state.json')))) {
      Register-DeviceRecovery $root ([string]$record.cache_directory) ([string]$record.drive_letter) ([string]$record.task_name) $PSScriptRoot
    }
  } finally {
    if ($held) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
  }
  Write-Output 'Interrupted setup processes are closed. You can retry installation or choose Uninstall. Cached files and credentials were kept.'
} catch {
  Write-Output $_.Exception.Message
  exit 1
}
