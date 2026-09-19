[CmdletBinding()]
param([switch]$Quiet, [switch]$NoConfirm, [string]$InstallRoot = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')
$operation = $null
$operationHeld = $false
Add-Type -AssemblyName PresentationFramework
try {
  $InstallRoot = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\')
  if ([IO.Path]::GetFileName($InstallRoot) -ne 'device' -or [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($InstallRoot)) -ne 'MirageSSD') { throw 'Invalid MirageSSD installation directory.' }
  $configuration = Get-DeviceInstallRecord $InstallRoot
  if (-not $configuration) {
    # A cancelled first install may have copied binaries but never made a
    # launcher. No cache is removed, even when its location is unknown.
    $configuration = [pscustomobject]@{ format='miragessd-device-install-v1'; owner_sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; task_name=('MirageSSD Drive ' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value); cache_directory=(Join-Path $env:LOCALAPPDATA 'MirageSSD\cache'); drive_letter=$null }
  }
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
  $tasks = @(Get-DeviceDriveTasks ([string]$configuration.task_name) $InstallRoot)
  Stop-DeviceDrive ([string]$configuration.task_name) $InstallRoot
  try { Test-PendingUploads ([string]$configuration.cache_directory) } catch {
    foreach ($task in $tasks) { Mount-DeviceDrive ([string]$task.TaskName) ([string]$configuration.drive_letter) | Out-Null }
    throw
  }
  foreach ($task in $tasks) {
    Unregister-ScheduledTask -TaskName $task.TaskName -Confirm:$false
  }
  $binaries = @((Join-Path $InstallRoot 'mirage.exe'), (Join-Path $InstallRoot 'rclone.exe'))
  Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -in $binaries } | ForEach-Object { Stop-Process -Id $_.ProcessId -ErrorAction Stop }
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
      $expectedScript = '"' + (Join-Path $InstallRoot 'prefetch-device.ps1') + '"'
      if ($link.TargetPath -eq (Join-Path $env:SystemRoot 'System32\wscript.exe') -and $link.Arguments.EndsWith($expectedScript, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $prepareShortcut
      }
    }
    $accountShortcut = Join-Path $programs 'MirageSSD Account.lnk'
    if (Test-Path -LiteralPath $accountShortcut -PathType Leaf) {
      $shell = New-Object -ComObject WScript.Shell
      $link = $shell.CreateShortcut($accountShortcut)
      $expectedScript = '"' + (Join-Path $InstallRoot 'account-device.ps1') + '"'
      if ($link.TargetPath -eq (Join-Path $env:SystemRoot 'System32\wscript.exe') -and $link.Arguments.EndsWith($expectedScript, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $accountShortcut
      }
    }
  }
  $prefix = $InstallRoot + '\'
  # Keep device-install.json alongside retained data for recovery/reinstallation.
  foreach ($name in @('mirage.exe','rclone.exe','mirage.exe.install-new','rclone.exe.install-new','mount-device.vbs','run-powershell-hidden.vbs','prefetch-device.ps1','account-device.ps1','device-lifecycle.ps1','oauth-desktop.json','drive-letter.txt','setup-state.json','uninstall-device-drive.ps1')) {
    $path = [IO.Path]::GetFullPath((Join-Path $InstallRoot $name))
    if (-not $path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe application path.' }
    if (Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path }
  }
  Remove-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'MirageSSDWritableDevice' -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MirageSSDWritableDevice' -ErrorAction SilentlyContinue
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
