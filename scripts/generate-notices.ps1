[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Output,
  [Parameter(Mandatory)][string]$WinFspLicensePath
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not (Test-Path -LiteralPath $WinFspLicensePath -PathType Leaf)) {
  throw "WinFsp license file is missing: $WinFspLicensePath"
}

$cargoJson = cargo metadata --manifest-path (Join-Path $repo 'Cargo.toml') --locked --format-version 1
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed while generating notices.' }
$cargo = $cargoJson | ConvertFrom-Json
$lines = [Collections.Generic.List[string]]::new()
$lines.Add('MirageSSD third-party notices')
$lines.Add('')
$lines.Add('This inventory is generated from Cargo.lock and apps/mirage-ui/package-lock.json.')
$lines.Add('License expressions identify governing upstream terms; consult each upstream package for the complete text.')
$lines.Add('The exact WinFsp license text is distributed as licenses/WinFsp-LICENSE.txt.')
$lines.Add('')
$lines.Add('WinFsp runtime and SDK')
$lines.Add('  Version: 2.1.25156')
$lines.Add('  Source: https://github.com/winfsp/winfsp/releases/tag/v2.1')
$lines.Add('  Package SHA-256: 073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a')
$lines.Add('')
$lines.Add('Cargo dependencies')
foreach ($package in @($cargo.packages | Where-Object { $_.source } | Sort-Object name, version)) {
  if (-not $package.license) { throw "Cargo package has no declared license: $($package.name) $($package.version)" }
  $lines.Add("  $($package.name) $($package.version) - $($package.license)")
}

$npmLock = Get-Content -Raw -LiteralPath (Join-Path $repo 'apps\mirage-ui\package-lock.json') | ConvertFrom-Json -AsHashtable
$lines.Add('')
$lines.Add('npm dependencies')
$npmPackages = foreach ($entry in $npmLock.packages.GetEnumerator()) {
  if ($entry.Key -notlike 'node_modules/*') { continue }
  [PSCustomObject]@{
    Name = $entry.Key -replace '^.*node_modules/', ''
    Version = $entry.Value.version
    License = $entry.Value.license
  }
}
foreach ($package in @($npmPackages | Sort-Object Name, Version)) {
  if (-not $package.License) { throw "Locked npm package has no declared license: $($package.Name) $($package.Version)" }
  $lines.Add("  $($package.Name) $($package.Version) - $($package.License)")
}

$lines | Set-Content -LiteralPath $Output -Encoding utf8NoBOM
