[CmdletBinding()]
param(
  [string]$Configuration = 'release',
  [string]$BinDir,
  [string]$UiDir,
  [string]$Version = '0.1.15',
  [Parameter(Mandatory = $true)]
  [string]$DriveClientCredentials,
  [string]$VCRuntimeDir,
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
  # Windows PowerShell 5.1 lacks [Convert]::ToHexString; produce the same uppercase hex.
  $hex = ($bytes | ForEach-Object { $_.ToString('X2') }) -join ''
  "{$($hex.Substring(0, 8))-$($hex.Substring(8, 4))-$($hex.Substring(12, 4))-$($hex.Substring(16, 4))-$($hex.Substring(20, 12))}"
}

function Set-CompoundFileRootModifiedTime([string]$Path, [DateTime]$Timestamp) {
  $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
  $reader = [IO.BinaryReader]::new($stream, [Text.Encoding]::UTF8, $true)
  $writer = [IO.BinaryWriter]::new($stream, [Text.Encoding]::UTF8, $true)
  try {
    $magic = $reader.ReadBytes(8)
    $magicHex = ($magic | ForEach-Object { $_.ToString('X2') }) -join ''
    if ($magicHex -ne 'D0CF11E0A1B11AE1') {
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

# The shipped OAuth Desktop client JSON is a public client; validate its
# shape and stage it under its installed name so the MSI can embed it.
$credentials = (Resolve-Path -LiteralPath $DriveClientCredentials).Path
$parsed = Get-Content -LiteralPath $credentials -Raw | ConvertFrom-Json
if (-not $parsed.installed -or -not $parsed.installed.client_id -or -not $parsed.installed.client_secret) {
  throw 'DriveClientCredentials is not a Google "installed" (Desktop) OAuth client JSON.'
}
if (-not ($parsed.installed.client_id -match '\.apps\.googleusercontent\.com$')) {
  throw 'DriveClientCredentials client_id is not an apps.googleusercontent.com client.'
}
$oauth = Join-Path $Output 'oauth-desktop.json'
New-Item -ItemType Directory -Force -Path $Output | Out-Null
Copy-Item -LiteralPath $credentials -Destination $oauth -Force
$required = @((Join-Path $bin 'mirage.exe'), (Join-Path $bin 'mirage-service.exe'), (Join-Path $bin 'mirage-fs.exe'), (Join-Path $bin 'mirage-ui.exe'), (Join-Path $ui 'index.html'), (Join-Path $ui 'assets\mirage-ui.js'), (Join-Path $ui 'assets\mirage-ui.css'))
foreach ($path in $required) { if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing release binary: $path" } }
# App-local deployment keeps the native host runnable on PCs without Visual
# Studio or a separately installed VC++ redistributable. Use the toolchain's
# redistributable directory, never DLLs copied from Windows/System32.
if (-not $VCRuntimeDir) {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  if ($LASTEXITCODE -ne 0 -or -not $vs) { throw 'Cannot locate the MSVC redistributable; supply -VCRuntimeDir.' }
  $redist = Get-ChildItem -LiteralPath (Join-Path $vs 'VC\Redist\MSVC') -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
    Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
  if ($redist) { $VCRuntimeDir = Join-Path $redist.FullName 'x64\Microsoft.VC143.CRT' }
}
foreach ($name in @('msvcp140.dll', 'msvcp140_1.dll', 'msvcp140_2.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
  if (-not $VCRuntimeDir -or -not (Test-Path -LiteralPath (Join-Path $VCRuntimeDir $name))) {
    throw "Missing redistributable runtime: $name. Supply the matching x64 CRT directory with -VCRuntimeDir."
  }
}
$runtimeFiles = @(Get-ChildItem -LiteralPath $VCRuntimeDir -Filter '*.dll' -File | Sort-Object Name)
$runtimeComponents = foreach ($file in $runtimeFiles) {
  $id = 'Runtime_' + ($file.Name -replace '[^A-Za-z0-9_]', '_')
  $source = [Security.SecurityElement]::Escape($file.FullName)
  "<Component Id=`"$id`" Guid=`"*`" Bitness=`"always64`"><File Id=`"${id}_File`" Source=`"$source`" KeyPath=`"yes`" /></Component>"
}
$runtimeWxs = Join-Path $Output 'Runtime.wxs'
Set-Content -LiteralPath $runtimeWxs -Encoding utf8 -Value (
  '<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs"><Fragment><ComponentGroup Id="RuntimeComponents" Directory="INSTALLFOLDER">' +
  ($runtimeComponents -join '') + '</ComponentGroup></Fragment></Wix>')
$productCode = New-DeterministicGuid "MirageSSD/ProductCode/$Version"
$packageInputs = @(
  $required
  $runtimeFiles.FullName
  $credentials
  (Join-Path $PSScriptRoot 'Product.wxs')
  (Join-Path $PSScriptRoot 'Components.wxs')
  (Join-Path $PSScriptRoot 'Prerequisites.wxs')
  $runtimeWxs
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
  '-d', "IconFile=$(Join-Path $PSScriptRoot 'assets\mirage-drive.ico')",
  '-d', "UiDir=$ui",
  '-d', "OAuthDesktop=$oauth",
  '-d', "ProductVersion=$Version",
  '-d', "ProductCode=$productCode",
  '-o', (Join-Path $Output 'MirageSSD.msi'),
  $runtimeWxs,
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
