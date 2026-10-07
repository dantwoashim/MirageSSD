[CmdletBinding()]
param(
  # Root of the mounted volume under test, e.g. R:\
  [Parameter(Mandatory)][string]$Target,
  [int]$Files = 2000,
  [int]$Bytes = 4096,
  [string]$Output
)
# One small-file pass (SmallFiles.cs) against a mounted root, for attribution
# runs where the host is traced. Use run-managed-bench.ps1 for comparisons.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Output) {
  $Output = Join-Path $repo ('target\perf\smallfiles-' + (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$exe = Join-Path $Output 'SmallFiles.exe'
& $csc /nologo /optimize+ /platform:x64 /r:System.Web.Extensions.dll "/out:$exe" (Join-Path $PSScriptRoot 'SmallFiles.cs')
if ($LASTEXITCODE -ne 0) { throw 'Compiling SmallFiles.cs failed' }
$json = Join-Path $Output 'smallfiles.json'
$directory = Join-Path $Target ('perf-once-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
& $exe $directory $json "$Files" "$Bytes" '20' | Out-Null
$result = Get-Content -LiteralPath $json -Raw | ConvertFrom-Json
$result | Format-List | Out-String | Write-Output
if (-not $result.verified -or -not $result.cleaned) { throw 'Small-file pass failed verification or cleanup' }
Write-Output 'hash_match    : True'
