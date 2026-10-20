[CmdletBinding()]
param([string]$MsiPath)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$product = [xml](Get-Content -LiteralPath (Join-Path $repo 'installer\Product.wxs') -Raw)
$bundle = [xml](Get-Content -LiteralPath (Join-Path $repo 'installer\Bundle.wxs') -Raw)
$launch = @($product.SelectNodes('//*[local-name()="Launch"]')) |
  Where-Object { $_.Condition -match 'WINDOWS_BUILD' }
if (@($launch).Count -ne 1 -or $launch.Condition -ne 'Installed OR (VersionNT64 AND WINDOWS_BUILD >= 19045)') {
  throw 'MSI Windows baseline or maintenance bypass changed unexpectedly.'
}
$condition = $bundle.SelectSingleNode('//*[local-name()="Condition"]')
if ($condition.Condition -ne 'WixBundleInstalled OR (VersionNT64 AND WindowsBuildNumber >= 19045)') {
  throw 'Bundle must reject unsupported fresh installs before installing prerequisites.'
}
$release = Get-Content -LiteralPath (Join-Path $repo 'scripts\build-release.ps1') -Raw
if ($release -notmatch 'windows_minimum_build\s*=\s*19045\b') { throw 'Release metadata disagrees with installer baseline.' }
$legacy = Get-Content -LiteralPath (Join-Path $repo 'scripts\setup-miragessd.ps1') -Raw
if ($legacy -notmatch 'CurrentBuildNumber' -or $legacy -notmatch '\$windowsBuild -lt 19045') {
  throw 'Legacy installer must use manifest-independent build detection and the same baseline.'
}
if ($MsiPath) {
  $installer = New-Object -ComObject WindowsInstaller.Installer
  $database = $null
  $session = $null
  try {
    $path = (Resolve-Path -LiteralPath $MsiPath).Path
    $database = $installer.OpenDatabase($path, 0)
    $view = $database.OpenView('SELECT `Condition` FROM `LaunchCondition`')
    $found = $false
    try {
      $view.Execute() | Out-Null
      while ($record = $view.Fetch()) {
        try { if ($record.StringData(1) -eq $launch.Condition) { $found = $true } }
        finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) | Out-Null }
      }
    } finally {
      $view.Close() | Out-Null
      [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    }
    if (-not $found) { throw 'Compiled MSI does not contain the intended platform condition.' }
    # OpenPackage/EvaluateCondition never runs an installation sequence.
    $session = $installer.OpenPackage($path, 1)
    foreach ($case in @(
      @(19044, '1000', '', 0), @(19045, '1000', '', 1),
      @(22000, '1000', '', 1), @(26100, '1000', '', 1),
      @(19045, '', '', 0), @(19044, '', '1', 1)
    )) {
      $session.Property('WINDOWS_BUILD') = [string]$case[0]
      $session.Property('VersionNT64') = [string]$case[1]
      $session.Property('Installed') = [string]$case[2]
      if ($session.EvaluateCondition($launch.Condition) -ne $case[3]) {
        throw "Compiled MSI platform boundary failed for build $($case[0]), x64=$($case[1]), installed=$($case[2])."
      }
    }
  } finally {
    foreach ($object in @($session, $database, $installer)) {
      if ($null -ne $object) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($object) | Out-Null }
    }
  }
}
Write-Output 'PASS: Windows 10 22H2/Windows 11 platform contract (compiled MSI boundaries when supplied).'
