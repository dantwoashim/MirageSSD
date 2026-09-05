[CmdletBinding()]
param(
  [string]$Output = "$PSScriptRoot\..\target\winfsp-sdk",
  [string]$WixExecutable = 'wix'
)

$ErrorActionPreference = 'Stop'
$version = '2.1.25156'
$expectedSha256 = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'
$uri = "https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-$version.msi"
$wix = Get-Command $WixExecutable -ErrorAction Stop
$scratch = Join-Path ([IO.Path]::GetTempPath()) ("mirage-winfsp-sdk-" + [Guid]::NewGuid().ToString('N'))
$package = Join-Path $scratch "winfsp-$version.msi"
$extracted = Join-Path $scratch 'extracted'
$decompiled = Join-Path $scratch 'winfsp.wxs'

New-Item -ItemType Directory -Force -Path $scratch | Out-Null
try {
  Invoke-WebRequest -Uri $uri -OutFile $package
  $actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $package).Hash
  if ($actualSha256 -ne $expectedSha256) {
    throw "WinFsp SDK package checksum mismatch: expected $expectedSha256, got $actualSha256"
  }

  & $wix.Source msi decompile -x $extracted -o $decompiled $package
  if ($LASTEXITCODE -ne 0) { throw "WinFsp MSI extraction failed with exit code $LASTEXITCODE" }

  $payload = Join-Path $extracted 'File'
  $required = @('winfsp.h', 'fsctl.h', 'launch.h', 'winfsp_x64.lib', 'License.txt')
  foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $payload $name) -PathType Leaf)) {
      throw "Pinned WinFsp package is missing SDK payload: $name"
    }
  }

  $include = Join-Path $Output 'inc\winfsp'
  $library = Join-Path $Output 'lib'
  New-Item -ItemType Directory -Force -Path $include, $library | Out-Null
  Copy-Item -LiteralPath (Join-Path $payload 'winfsp.h') -Destination $include -Force
  Copy-Item -LiteralPath (Join-Path $payload 'fsctl.h') -Destination $include -Force
  Copy-Item -LiteralPath (Join-Path $payload 'launch.h') -Destination $include -Force
  Copy-Item -LiteralPath (Join-Path $payload 'winfsp_x64.lib') -Destination (Join-Path $library 'winfsp-x64.lib') -Force
  Copy-Item -LiteralPath (Join-Path $payload 'License.txt') -Destination (Join-Path $Output 'License.txt') -Force
  Set-Content -LiteralPath (Join-Path $Output 'VERSION') -Value $version -Encoding ascii
  Set-Content -LiteralPath (Join-Path $Output 'SOURCE_SHA256') -Value $expectedSha256.ToLowerInvariant() -Encoding ascii
}
finally {
  if (Test-Path -LiteralPath $scratch) {
    $resolvedScratch = (Resolve-Path -LiteralPath $scratch).Path
    $resolvedTemp = (Resolve-Path -LiteralPath ([IO.Path]::GetTempPath())).Path
    if (-not $resolvedScratch.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase)) {
      throw "Refusing to remove unexpected SDK scratch path: $resolvedScratch"
    }
    Remove-Item -LiteralPath $resolvedScratch -Recurse -Force
  }
}
