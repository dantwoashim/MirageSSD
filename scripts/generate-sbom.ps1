[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Output,
  [Parameter(Mandatory)][string]$Version
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$cargoJson = cargo metadata --manifest-path (Join-Path $repo 'Cargo.toml') --locked --format-version 1
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
$cargo = $cargoJson | ConvertFrom-Json
$components = [Collections.Generic.List[object]]::new()

foreach ($package in $cargo.packages) {
  if (-not $package.license) { throw "Cargo package has no declared license: $($package.name) $($package.version)" }
  $purl = "pkg:cargo/$([Uri]::EscapeDataString($package.name))@$($package.version)"
  $properties = @([ordered]@{ name = 'miragessd:ecosystem'; value = 'cargo' })
  if ($package.source) {
    $properties += [ordered]@{ name = 'miragessd:source'; value = [string]$package.source }
  }
  $components.Add([ordered]@{
    type = 'library'
    'bom-ref' = $purl
    name = $package.name
    version = [string]$package.version
    licenses = @([ordered]@{ expression = [string]$package.license })
    purl = $purl
    properties = $properties
  })
}

$npmLockPath = Join-Path $repo 'apps\mirage-ui\package-lock.json'
$npmLock = Get-Content -Raw -LiteralPath $npmLockPath | ConvertFrom-Json -AsHashtable
foreach ($entry in $npmLock.packages.GetEnumerator()) {
  if ($entry.Key -notlike 'node_modules/*') { continue }
  $name = $entry.Key -replace '^.*node_modules/', ''
  $package = $entry.Value
  if (-not $package.version) { throw "Locked npm package has no version: $name" }
  if (-not $package.license) { throw "Locked npm package has no declared license: $name $($package.version)" }
  $purlName = if ($name.StartsWith('@')) { '%40' + $name.Substring(1) } else { $name }
  $purl = "pkg:npm/$purlName@$($package.version)"
  $properties = @([ordered]@{ name = 'miragessd:ecosystem'; value = 'npm' })
  if ($package.resolved) {
    $properties += [ordered]@{ name = 'miragessd:resolved'; value = [string]$package.resolved }
  }
  $components.Add([ordered]@{
    type = 'library'
    'bom-ref' = $purl
    name = $name
    version = [string]$package.version
    licenses = @([ordered]@{ expression = [string]$package.license })
    purl = $purl
    properties = $properties
  })
}

$orderedComponents = @($components | Sort-Object { $_.'bom-ref' })
$document = [ordered]@{
  bomFormat = 'CycloneDX'
  specVersion = '1.6'
  version = 1
  metadata = [ordered]@{
    component = [ordered]@{
      type = 'application'
      'bom-ref' = "pkg:generic/miragessd@$Version"
      name = 'MirageSSD'
      version = $Version
      licenses = @([ordered]@{ expression = 'Apache-2.0' })
      purl = "pkg:generic/miragessd@$Version"
    }
  }
  components = $orderedComponents
}

$document | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $Output -Encoding utf8NoBOM
