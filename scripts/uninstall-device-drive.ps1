[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationFramework
try {
  $configurationPath = Join-Path $PSScriptRoot 'device-install.json'
  $configuration = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  if ($configuration.format -ne 'miragessd-device-install-v1' -or $configuration.owner_sid -ne $sid -or $configuration.task_name -ne "MirageSSD Drive $sid") {
    throw 'This installation belongs to another Windows account or its configuration is invalid.'
  }
  $answer = [Windows.MessageBox]::Show('Close programs using MirageSSD before continuing. Remove the app and stop its drive? Your Google Drive files, local cache, sign-in record and file-attribute metadata will be kept.', 'Uninstall MirageSSD', 'YesNo', 'Question')
  if ($answer -ne 'Yes') { return }
  $metadataRoot = Join-Path $configuration.cache_directory 'vfsMeta'
  if (Test-Path -LiteralPath $metadataRoot) {
    foreach ($file in Get-ChildItem -LiteralPath $metadataRoot -Recurse -File) {
      $metadata = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
      if ($null -eq $metadata.Dirty -or $metadata.Dirty) { throw 'Uploads are pending, or their status could not be checked. Let uploads finish before uninstalling. No files were removed.' }
    }
  }
  $task = Get-ScheduledTask -TaskName $configuration.task_name -ErrorAction SilentlyContinue
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
  $prefix = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\') + '\'
  # Keep device-install.json alongside retained data for recovery/reinstallation.
  foreach ($name in @('mirage.exe','rclone.exe','mount-device.vbs','run-powershell-hidden.vbs','drive-letter.txt','uninstall-device-drive.ps1')) {
    $path = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $name))
    if (-not $path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe application path.' }
    if (Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path }
  }
  [Windows.MessageBox]::Show('MirageSSD has been removed. Cloud files, cached data, sign-in and file-attribute metadata were retained. The shared WinFsp runtime was not removed.', 'MirageSSD', 'OK', 'Information') | Out-Null
} catch {
  [Windows.MessageBox]::Show($_.Exception.Message, 'MirageSSD uninstall', 'OK', 'Error') | Out-Null
  exit 1
}
