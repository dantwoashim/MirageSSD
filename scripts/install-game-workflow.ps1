[CmdletBinding()]
param(
  [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'MirageSSD\game-tools'),
  [string]$MirageExecutable
)

$ErrorActionPreference = 'Stop'

$archiveSource = Join-Path $PSScriptRoot 'archive-game.ps1'
$restoreSource = Join-Path $PSScriptRoot 'restore-game.ps1'
foreach ($path in @($archiveSource, $restoreSource)) {
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    throw "Required game tool is unavailable: $path"
  }
}
if (-not $MirageExecutable) {
  $MirageExecutable = Join-Path $PSScriptRoot '..\target\release\mirage.exe'
}
$mirageSource = Resolve-Path -LiteralPath $MirageExecutable -ErrorAction Stop
if (-not (Test-Path -LiteralPath $mirageSource.Path -PathType Leaf)) {
  throw 'MirageSSD executable is unavailable.'
}

New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
$archiveInstalled = Join-Path $InstallRoot 'archive-game.ps1'
$restoreInstalled = Join-Path $InstallRoot 'restore-game.ps1'
$mirageInstalled = Join-Path $InstallRoot 'mirage.exe'
Copy-Item -LiteralPath $archiveSource -Destination $archiveInstalled -Force
Copy-Item -LiteralPath $restoreSource -Destination $restoreInstalled -Force
Copy-Item -LiteralPath $mirageSource.Path -Destination $mirageInstalled -Force

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
& icacls.exe $InstallRoot /inheritance:r /grant:r "*$($identity.User.Value):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw 'Failed to protect the MirageSSD game-tool directory.'
}

$desktop = [Environment]::GetFolderPath('Desktop')
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$shell = New-Object -ComObject WScript.Shell

$archiveShortcut = $shell.CreateShortcut((Join-Path $desktop 'Archive Game to MirageSSD.lnk'))
$archiveShortcut.TargetPath = $powershell
$archiveShortcut.Arguments = '-NoProfile -ExecutionPolicy Bypass -NoExit -File "' + $archiveInstalled + '"'
$archiveShortcut.WorkingDirectory = $InstallRoot
$archiveShortcut.Description = 'Create an encrypted packed game archive without deleting the original'
$archiveShortcut.IconLocation = "$env:SystemRoot\System32\shell32.dll,167"
$archiveShortcut.Save()

$restoreShortcut = $shell.CreateShortcut((Join-Path $desktop 'Restore Game from MirageSSD.lnk'))
$restoreShortcut.TargetPath = $powershell
$restoreShortcut.Arguments = '-NoProfile -ExecutionPolicy Bypass -NoExit -File "' + $restoreInstalled + '" -Interactive'
$restoreShortcut.WorkingDirectory = $InstallRoot
$restoreShortcut.Description = 'Plan, restore, and verify a packed MirageSSD game archive'
$restoreShortcut.IconLocation = "$env:SystemRoot\System32\shell32.dll,168"
$restoreShortcut.Save()

$contextMenu = 'HKCU:\Software\Classes\Directory\shell\MirageSSDPackedArchive'
$contextCommand = Join-Path $contextMenu 'command'
New-Item -Path $contextCommand -Force | Out-Null
New-ItemProperty -Path $contextMenu -Name '(default)' -Value 'Archive to MirageSSD (packed)' -Force | Out-Null
New-ItemProperty -Path $contextMenu -Name 'Icon' -Value "$env:SystemRoot\System32\shell32.dll,167" -Force | Out-Null
New-ItemProperty -Path $contextMenu -Name 'MultiSelectModel' -Value 'Single' -Force | Out-Null
$contextArguments = '"' + $powershell + '" -NoProfile -ExecutionPolicy Bypass -NoExit -File "' + $archiveInstalled + '" -Source "%1"'
New-ItemProperty -Path $contextCommand -Name '(default)' -Value $contextArguments -Force | Out-Null

[pscustomobject]@{
  Installed = $true
  ArchiveShortcut = Join-Path $desktop 'Archive Game to MirageSSD.lnk'
  RestoreShortcut = Join-Path $desktop 'Restore Game from MirageSSD.lnk'
  FolderContextMenu = 'Archive to MirageSSD (packed)'
  OriginalDeletion = 'Never automatic'
} | ConvertTo-Json -Compress
