[CmdletBinding()]
param([switch]$Quiet, [switch]$NoConfirm)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')
$operation = $null
$operationHeld = $false
Add-Type -AssemblyName PresentationFramework
try {
  $configurationPath = Join-Path $PSScriptRoot 'device-install.json'
  $configuration = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  if ($configuration.format -ne 'miragessd-device-install-v1' -or $configuration.owner_sid -ne $sid -or $configuration.task_name -ne "MirageSSD Drive $sid") {
    throw 'This installation belongs to another Windows account or its configuration is invalid.'
  }
  $answer = if ($NoConfirm) { 'Yes' } else { [Windows.MessageBox]::Show('Close programs using MirageSSD before continuing. Remove the app and stop its drive? Your Google Drive files, local cache, sign-in record and file-attribute metadata will be kept.', 'Uninstall MirageSSD', 'YesNo', 'Question') }
  if ($answer -ne 'Yes') { return }
  $operation = [Threading.Mutex]::new($false, ('Local\MirageSSD-Device-' + $sid))
  try { $operationHeld = $operation.WaitOne(0) } catch [Threading.AbandonedMutexException] { $operationHeld = $true }
  if (-not $operationHeld) { throw 'Another MirageSSD setup or account operation is running.' }
  $metadataRoot = Join-Path $configuration.cache_directory 'vfsMeta'
  if (Test-Path -LiteralPath $metadataRoot) {
    foreach ($file in Get-ChildItem -LiteralPath $metadataRoot -Recurse -File) {
      $metadata = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
      if ($metadata.Dirty -isnot [bool] -or $metadata.Dirty) { throw 'Uploads are pending, or their status could not be checked. Let uploads finish before uninstalling. No files were removed.' }
    }
  }
  $task = Get-ScheduledTask -TaskName $configuration.task_name -ErrorAction SilentlyContinue
  Stop-DeviceDrive ([string]$configuration.task_name) $PSScriptRoot
  try { Test-PendingUploads ([string]$configuration.cache_directory) } catch {
    if ($task) { Mount-DeviceDrive ([string]$configuration.task_name) ([string]$configuration.drive_letter) | Out-Null }
    throw
  }
  if ($task) {
    Stop-ScheduledTask -TaskName $configuration.task_name
    Unregister-ScheduledTask -TaskName $configuration.task_name -Confirm:$false
  }
  $binaries = @((Join-Path $PSScriptRoot 'mirage.exe'), (Join-Path $PSScriptRoot 'rclone.exe'))
  Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -in $binaries } | ForEach-Object { Stop-Process -Id $_.ProcessId -ErrorAction Stop }
  Remove-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'MirageSSDWritableDevice' -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MirageSSDWritableDevice' -ErrorAction SilentlyContinue
  $desktop = [Environment]::GetFolderPath('DesktopDirectory')
  $shortcut = Join-Path $desktop 'MirageSSD.lnk'
  if (Test-Path -LiteralPath $shortcut) {
    $shell = New-Object -ComObject WScript.Shell
    if ($shell.CreateShortcut($shortcut).TargetPath -eq $configuration.drive_letter) { Remove-Item -LiteralPath $shortcut }
  }
  # Delete only the named application payload, never a cache/data directory.
  $programs = [Environment]::GetFolderPath('Programs')
  if ($programs) {
    $prepareShortcut = Join-Path $programs 'MirageSSD Prepare Files.lnk'
    if (Test-Path -LiteralPath $prepareShortcut -PathType Leaf) {
      $shell = New-Object -ComObject WScript.Shell
      $link = $shell.CreateShortcut($prepareShortcut)
      $expectedScript = '"' + (Join-Path $PSScriptRoot 'prefetch-device.ps1') + '"'
      if ($link.TargetPath -eq (Join-Path $env:SystemRoot 'System32\wscript.exe') -and $link.Arguments.EndsWith($expectedScript, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $prepareShortcut
      }
    }
    $accountShortcut = Join-Path $programs 'MirageSSD Account.lnk'
    if (Test-Path -LiteralPath $accountShortcut -PathType Leaf) {
      $shell = New-Object -ComObject WScript.Shell
      $link = $shell.CreateShortcut($accountShortcut)
      $expectedScript = '"' + (Join-Path $PSScriptRoot 'account-device.ps1') + '"'
      if ($link.TargetPath -eq (Join-Path $env:SystemRoot 'System32\wscript.exe') -and $link.Arguments.EndsWith($expectedScript, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $accountShortcut
      }
    }
  }
  $prefix = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\') + '\'
  # Keep device-install.json alongside retained data for recovery/reinstallation.
  foreach ($name in @('mirage.exe','rclone.exe','mount-device.vbs','run-powershell-hidden.vbs','prefetch-device.ps1','account-device.ps1','device-lifecycle.ps1','oauth-desktop.json','drive-letter.txt','uninstall-device-drive.ps1')) {
    $path = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $name))
    if (-not $path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe application path.' }
    if (Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path }
  }
  if ($Quiet) { Write-Output 'MirageSSD removed; cached data, account state and credentials retained.' }
  else { [Windows.MessageBox]::Show('MirageSSD has been removed. Cloud files, cached data, sign-in and file-attribute metadata were retained. The shared WinFsp runtime was not removed.', 'MirageSSD', 'OK', 'Information') | Out-Null }
} catch {
  if ($Quiet) { Write-Output $_.Exception.Message }
  else { [Windows.MessageBox]::Show($_.Exception.Message, 'MirageSSD uninstall', 'OK', 'Error') | Out-Null }
  exit 1
} finally {
  if ($operationHeld) { $operation.ReleaseMutex() }
  if ($operation) { $operation.Dispose() }
}
