[CmdletBinding()]
param(
  [string]$ClientId,
  [ValidatePattern('^[D-Zd-z]:?$')]
  [string]$DriveLetter = 'M',
  [switch]$VerifyOnly,
  [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Show-Result([string]$Message, [bool]$Failed = $false) {
  if ($Quiet) { Write-Output $Message; return }
  try {
    Add-Type -AssemblyName PresentationFramework -ErrorAction Stop
    $icon = if ($Failed) { 'Error' } else { 'Information' }
    [System.Windows.MessageBox]::Show($Message, 'MirageSSD', 'OK', $icon) | Out-Null
  } catch {
    Write-Host $Message
  }
}

function Test-PinnedWinFsp {
  $installDirectory = (Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\WinFsp' -ErrorAction SilentlyContinue).InstallDir
  if (-not $installDirectory) { return $false }
  $candidates = @(
    (Join-Path $installDirectory 'bin\winfsp-x64.dll')
    (Join-Path $installDirectory 'SxS\*\bin\winfsp-x64.dll')
  )
  foreach ($candidate in $candidates) {
    foreach ($file in @(Get-Item -Path $candidate -ErrorAction SilentlyContinue)) {
      if ($file.VersionInfo.FileVersion -like '2.1.25156*') { return $true }
    }
  }
  return $false
}

function Select-CacheProfile([string]$ReservedDriveLetter) {
  $reservedDevice = $ReservedDriveLetter.TrimEnd(':').ToUpperInvariant() + ':'
  $candidate = Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' |
    Where-Object {
      $_.DeviceID -ne $reservedDevice -and
      $_.FileSystem -eq 'NTFS' -and
      $_.VolumeName -ne 'MirageSSD' -and
      $_.FreeSpace
    } |
    Sort-Object FreeSpace -Descending |
    Select-Object -First 1
  if (-not $candidate) { throw 'No writable local NTFS disk is available for the MirageSSD cache.' }

  $gib = [math]::Floor($candidate.FreeSpace / 1GB)
  if ($gib -ge 160) {
    $maximum = '128Gi'; $minimumFree = '24Gi'
  } elseif ($gib -ge 96) {
    $maximum = '64Gi'; $minimumFree = '16Gi'
  } elseif ($gib -ge 48) {
    $maximum = '32Gi'; $minimumFree = '12Gi'
  } elseif ($gib -ge 24) {
    $maximum = '16Gi'; $minimumFree = '8Gi'
  } elseif ($gib -ge 12) {
    $maximum = '8Gi'; $minimumFree = '4Gi'
  } else {
    throw 'MirageSSD needs at least 12 GiB free on a local NTFS disk for a safe cache.'
  }

  [pscustomobject]@{
    Directory = Join-Path ($candidate.DeviceID + '\') 'MirageSSD-Cache'
    Maximum = $maximum
    MinimumFree = $minimumFree
  }
}

function Resolve-DriveLetter([string]$RequestedDriveLetter) {
  $requested = $RequestedDriveLetter.TrimEnd(':').ToUpperInvariant()
  $existing = @(Get-CimInstance Win32_LogicalDisk | Where-Object { $_.VolumeName -eq 'MirageSSD' })
  if ($existing.Count -gt 0) { return $existing[0].DeviceID.TrimEnd(':') }
  $ordered = @($requested) + @(68..90 | ForEach-Object { [char]$_ } | Where-Object { $_ -ne $requested })
  $occupied = @(Get-PSDrive -PSProvider FileSystem -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
  foreach ($candidate in $ordered) {
    if ($candidate -notin $occupied) { return [string]$candidate }
  }
  throw 'No free Windows drive letter is available for MirageSSD.'
}

function Test-BundleManifest {
  $manifest = Join-Path $PSScriptRoot 'SHA256SUMS'
  if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw 'The package integrity manifest is missing.' }
  $prefix = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\') + '\'
  $covered = @{}
  foreach ($line in Get-Content -LiteralPath $manifest) {
    if (-not $line.Trim()) { continue }
    if ($line -notmatch '^([a-fA-F0-9]{64}) \*(.+)$') { throw 'The package integrity manifest is invalid.' }
    $expected = $Matches[1]
    $relative = $Matches[2].Replace('/', '\')
    if ([IO.Path]::IsPathRooted($relative)) { throw 'The package contains an unsafe path.' }
    $full = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $relative))
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -or $covered.ContainsKey($full)) { throw 'The package contains an unsafe or duplicate path.' }
    if (-not (Test-Path -LiteralPath $full -PathType Leaf) -or (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash -ne $expected) {
      throw "The package is damaged: $relative. Download the installer again."
    }
    $covered[$full] = $true
  }
  foreach ($file in Get-ChildItem -LiteralPath $PSScriptRoot -Recurse -File) {
    if ($file.FullName -ne $manifest -and -not $covered.ContainsKey($file.FullName)) { throw 'The package contains an unexpected file.' }
  }
}

try {
  Test-BundleManifest
  if (-not [Environment]::Is64BitOperatingSystem -or [Environment]::OSVersion.Version.Build -lt 22000) {
    throw 'This MirageSSD build requires 64-bit Windows 11.'
  }

  $payload = Join-Path $PSScriptRoot 'payload'
  $mirage = Join-Path $payload 'mirage.exe'
  $rclone = Join-Path $payload 'rclone.exe'
  $installer = Join-Path $PSScriptRoot 'install-device-drive.ps1'
  $clientConfiguration = Join-Path $PSScriptRoot 'oauth-desktop.json'
  $winFspMsi = Join-Path $PSScriptRoot 'prerequisites\winfsp-2.1.25156.msi'
  foreach ($required in @($mirage, $rclone, $installer, $winFspMsi, $clientConfiguration)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
      throw "The setup package is incomplete: $([IO.Path]::GetFileName($required)) is missing."
    }
  }

  if (-not $ClientId) {
    $clientIdFile = Join-Path $PSScriptRoot 'client-id.txt'
    if (Test-Path -LiteralPath $clientIdFile -PathType Leaf) {
      $ClientId = (Get-Content -LiteralPath $clientIdFile -Raw).Trim()
    }
  }
  if ($ClientId -notmatch '^[^\s\x00-\x1f]{1,480}\.apps\.googleusercontent\.com$') {
    throw 'This package has no valid Google OAuth desktop client ID.'
  }

  $desktopClient = (Get-Content -LiteralPath $clientConfiguration -Raw | ConvertFrom-Json).installed
  if (-not $desktopClient -or $desktopClient.client_id -ne $ClientId -or -not $desktopClient.client_secret) {
    throw 'The package has an invalid Desktop OAuth application configuration.'
  }
  $expectedWinFsp = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'
  if ((Get-FileHash -LiteralPath $winFspMsi -Algorithm SHA256).Hash -ne $expectedWinFsp) { throw 'The filesystem prerequisite failed its integrity check.' }
  $rcloneOutput = @(& $rclone version 2>&1)
  if ($LASTEXITCODE -ne 0 -or [string]$rcloneOutput[0] -notmatch 'miragessd') { throw 'The bundled filesystem provider is invalid.' }
  & $mirage --version | Out-Null
  if ($LASTEXITCODE -ne 0) { throw 'The MirageSSD application could not start.' }
  if ($VerifyOnly) { Write-Output 'VERIFIED: package hashes, application configuration, binaries and filesystem prerequisite. No installation or sign-in performed.'; return }

  $DriveLetter = Resolve-DriveLetter $DriveLetter

  if (-not (Test-PinnedWinFsp)) {
    $expectedWinFsp = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'
    $actualWinFsp = (Get-FileHash -LiteralPath $winFspMsi -Algorithm SHA256).Hash
    if ($actualWinFsp -ne $expectedWinFsp) { throw 'The bundled WinFsp installer failed its integrity check.' }
    $installation = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList @('/i', "`"$winFspMsi`"", '/qn', '/norestart') -Verb RunAs -WindowStyle Hidden -Wait -PassThru
    if ($installation.ExitCode -notin @(0, 1641, 3010) -or -not (Test-PinnedWinFsp)) {
      throw 'WinFsp installation did not complete. Accept the Windows administrator prompt and run setup again.'
    }
  }

  $rcloneOutput = @(& $rclone version 2>&1)
  $rcloneExitCode = $LASTEXITCODE
  $rcloneVersion = [string]$rcloneOutput[0]
  if ($rcloneExitCode -ne 0 -or $rcloneVersion -notmatch 'miragessd') {
    throw 'The bundled MirageSSD filesystem provider is invalid.'
  }

  $profile = Select-CacheProfile $DriveLetter
  Write-Output 'Installing MirageSSD. Complete Google sign-in in your browser when it opens.'
  $parameters = @{
    MirageExecutable = $mirage
    RcloneExecutable = $rclone
    ClientId = $ClientId
    DriveClientCredentials = $clientConfiguration
    DriveLetter = $DriveLetter
    CacheDirectory = $profile.Directory
    CacheMaxSize = $profile.Maximum
    CacheMinFreeSpace = $profile.MinimumFree
  }
  $result = & $installer @parameters
  if ($LASTEXITCODE -ne 0) { throw 'MirageSSD installation failed.' }

  $mount = $DriveLetter.TrimEnd(':').ToUpperInvariant() + ':\'
  Start-Process -FilePath "$env:SystemRoot\explorer.exe" -ArgumentList $mount
  Show-Result "MirageSSD is ready at $mount`n`nIt will reconnect automatically whenever you sign in to Windows."
  $result
} catch {
  $message = $_.Exception.Message
  Show-Result "Setup could not finish:`n`n$message" $true
  Write-Error $message
  exit 1
}
