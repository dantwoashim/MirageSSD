param(
  [string]$Version = '0.1.9',
  [string]$MsiPath,
  [string]$Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist-dev\installer'),
  [string]$Tag = 'v0.1.9-preview'
)

$ErrorActionPreference = 'Stop'
$winfspUrl = 'https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi'
$winfspSha = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'

$repo = Split-Path -Parent $PSScriptRoot
$msi = if ($MsiPath) { (Resolve-Path -LiteralPath $MsiPath).Path } else { Join-Path $Output 'MirageSSD.msi' }
if (-not (Test-Path -LiteralPath $msi -PathType Leaf)) { throw "Missing MirageSSD.msi: $msi" }

# WinFsp prerequisite: download once into a cache, verify the pinned SHA256.
$cache = Join-Path $PSScriptRoot 'cache'
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$winfsp = Join-Path $cache 'winfsp-2.1.25156.msi'
if (-not (Test-Path -LiteralPath $winfsp -PathType Leaf)) {
  Invoke-WebRequest -Uri $winfspUrl -OutFile $winfsp
}
$actual = (Get-FileHash -LiteralPath $winfsp -Algorithm SHA256).Hash
if ($actual -ne $winfspSha) { throw "WinFsp MSI hash mismatch: expected $winfspSha, got $actual" }

$wix = Get-Command wix -ErrorAction Stop
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$exe = Join-Path $Output "MirageSSD-Setup-$Tag.exe"
$arguments = @(
  'build',
  '-arch', 'x64',
  '-ext', 'WixToolset.Bal.wixext',
  '-ext', 'WixToolset.Util.wixext',
  '-d', "ProductVersion=$Version",
  '-d', "WinFspMsi=$winfsp",
  '-d', "MirageMsi=$msi",
  '-d', "BundleDir=$PSScriptRoot",
  '-o', $exe,
  (Join-Path $PSScriptRoot 'Bundle.wxs')
)
& $wix.Source @arguments
if ($LASTEXITCODE -ne 0) { throw "WiX bundle build failed with exit code $LASTEXITCODE" }

# SHA256SUMS covering the exe and the MSI it wraps.
$lines = @($exe, $msi) | ForEach-Object {
  "{0} *{1}" -f (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash, (Split-Path -Leaf $_)
}
Set-Content -LiteralPath (Join-Path $Output 'SHA256SUMS.txt') -Value $lines -Encoding ascii
Write-Host "bundle: $exe"
Write-Host ($lines -join "`n")
