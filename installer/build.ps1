[CmdletBinding()]
param(
  [string]$Configuration = 'release',
  [string]$BinDir,
  [string]$UiDir,
  [string]$Version = '0.1.0',
  [long]$SourceDateEpoch = 946684800,
  [string]$Output = "$PSScriptRoot\out"
)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'Installer version must be major.minor.patch.' }
$versionParts = @($Version.Split('.') | ForEach-Object { [uint32]$_ })
if ($versionParts[0] -gt 255 -or $versionParts[1] -gt 255 -or $versionParts[2] -gt 65535) {
  throw 'Installer version exceeds Windows Installer numeric bounds.'
}
if ($SourceDateEpoch -lt 0) { throw 'SourceDateEpoch cannot be negative.' }

function New-DeterministicGuid([string]$Seed) {
  $algorithm = [Security.Cryptography.SHA256]::Create()
  try {
    $digest = $algorithm.ComputeHash([Text.Encoding]::UTF8.GetBytes($Seed))
  }
  finally {
    $algorithm.Dispose()
  }
  $bytes = [byte[]]::new(16)
  [Array]::Copy($digest, $bytes, $bytes.Length)
  $bytes[6] = ($bytes[6] -band 0x0f) -bor 0x80
  $bytes[8] = ($bytes[8] -band 0x3f) -bor 0x80
  $hex = [Convert]::ToHexString($bytes)
  "{$($hex.Substring(0, 8))-$($hex.Substring(8, 4))-$($hex.Substring(12, 4))-$($hex.Substring(16, 4))-$($hex.Substring(20, 12))}"
}

function Set-CompoundFileRootModifiedTime([string]$Path, [DateTime]$Timestamp) {
  $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
  $reader = [IO.BinaryReader]::new($stream, [Text.Encoding]::UTF8, $true)
  $writer = [IO.BinaryWriter]::new($stream, [Text.Encoding]::UTF8, $true)
  try {
    $magic = $reader.ReadBytes(8)
    if ([Convert]::ToHexString($magic) -ne 'D0CF11E0A1B11AE1') {
      throw 'Built MSI is not a Compound File Binary container.'
    }
    $stream.Position = 30
    $sectorShift = $reader.ReadUInt16()
    if ($sectorShift -notin 9, 12) { throw "Unsupported MSI compound-file sector shift: $sectorShift" }
    $sectorSize = 1L -shl $sectorShift
    $stream.Position = 48
    $firstDirectorySector = $reader.ReadUInt32()
    if ([uint64]$firstDirectorySector -ge 4294967290) { throw 'Built MSI has no valid root directory sector.' }
    $rootOffset = ([int64]$firstDirectorySector + 1) * $sectorSize
    if ($rootOffset + 128 -gt $stream.Length) { throw 'Built MSI root directory entry is out of bounds.' }
    $stream.Position = $rootOffset + 66
    if ($reader.ReadByte() -ne 5) { throw 'Built MSI compound-file root entry is invalid.' }
    $stream.Position = $rootOffset + 108
    $writer.Write($Timestamp.ToFileTimeUtc())
    $writer.Flush()
    $stream.Flush($true)
  }
  finally {
    $writer.Dispose()
    $reader.Dispose()
    $stream.Dispose()
  }
}

$repo = Split-Path -Parent $PSScriptRoot
$bin = if ($BinDir) { (Resolve-Path -LiteralPath $BinDir).Path } else { Join-Path $repo "target\$Configuration" }
$ui = if ($UiDir) { (Resolve-Path -LiteralPath $UiDir).Path } else { Join-Path $repo 'apps\mirage-ui\dist' }
$required = @((Join-Path $bin 'mirage.exe'), (Join-Path $bin 'mirage-service.exe'), (Join-Path $bin 'mirage-fs.exe'), (Join-Path $bin 'mirage-ui.exe'), (Join-Path $ui 'index.html'), (Join-Path $ui 'assets\mirage-ui.js'), (Join-Path $ui 'assets\mirage-ui.css'))
foreach ($path in $required) { if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing release binary: $path" } }
$productCode = New-DeterministicGuid "MirageSSD/ProductCode/$Version"
$packageInputs = @(
  $required
  (Join-Path $PSScriptRoot 'Product.wxs')
  (Join-Path $PSScriptRoot 'Components.wxs')
  (Join-Path $PSScriptRoot 'Prerequisites.wxs')
)
$packageSeed = @("MirageSSD/PackageCode/$Version") + @($packageInputs | ForEach-Object {
  "$(Split-Path -Leaf $_)=$((Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash)"
})
$packageCode = New-DeterministicGuid ($packageSeed -join "`n")
$wix = Get-Command wix -ErrorAction Stop
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$arguments = @(
  'build',
  '-arch', 'x64',
  '-d', "BinDir=$bin",
  '-d', "UiDir=$ui",
  '-d', "ProductVersion=$Version",
  '-d', "ProductCode=$productCode",
  '-o', (Join-Path $Output 'MirageSSD.msi'),
  (Join-Path $PSScriptRoot 'Product.wxs'),
  (Join-Path $PSScriptRoot 'Components.wxs'),
  (Join-Path $PSScriptRoot 'Prerequisites.wxs')
)
& $wix.Source @arguments
if ($LASTEXITCODE -ne 0) { throw "WiX build failed with exit code $LASTEXITCODE" }

$msi = Join-Path $Output 'MirageSSD.msi'
$timestamp = [DateTimeOffset]::FromUnixTimeSeconds($SourceDateEpoch).UtcDateTime
$windowsInstaller = New-Object -ComObject WindowsInstaller.Installer
$summary = $windowsInstaller.SummaryInformation($msi, 3)
$summary.Property(9) = $packageCode
$summary.Property(12) = $timestamp
$summary.Property(13) = $timestamp
$summary.Persist()
[Runtime.InteropServices.Marshal]::FinalReleaseComObject($summary) | Out-Null
[Runtime.InteropServices.Marshal]::FinalReleaseComObject($windowsInstaller) | Out-Null
[GC]::Collect()
[GC]::WaitForPendingFinalizers()
Set-CompoundFileRootModifiedTime -Path $msi -Timestamp $timestamp
& $wix.Source msi validate $msi
if ($LASTEXITCODE -ne 0) { throw "MSI validation failed with exit code $LASTEXITCODE" }
