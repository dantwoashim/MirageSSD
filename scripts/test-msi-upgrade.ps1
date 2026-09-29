# Runs only on a disposable, elevated Windows test machine. The baseline may
# use current binaries; it exercises an installed, running service across a
# major upgrade, including rollback after removal of the previous product.
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$BaselineMsi,
  [Parameter(Mandatory = $true)][string]$UpgradeMsi,
  [Parameter(Mandatory = $true)][string]$Output
)
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]::new($identity)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'This integration test requires an elevated disposable Windows machine.'
}
$stateRoot = Join-Path $env:ProgramData 'MirageSSD'
if ((Get-Service -Name MirageSSD -ErrorAction SilentlyContinue) -or
    (Test-Path -LiteralPath (Join-Path $stateRoot 'control.db')) -or
    (Test-Path -LiteralPath (Join-Path $env:ProgramFiles 'MirageSSD'))) {
  throw 'Refusing to run against an existing MirageSSD installation or database.'
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path -LiteralPath $Output).Path
$BaselineMsi = (Resolve-Path -LiteralPath $BaselineMsi).Path
$UpgradeMsi = (Resolve-Path -LiteralPath $UpgradeMsi).Path
$installer = New-Object -ComObject WindowsInstaller.Installer

function Read-ProductCode([string]$Path) {
  $db = $installer.OpenDatabase($Path, 0)
  $view = $db.OpenView('SELECT `Value` FROM `Property` WHERE `Property` = ''ProductCode''')
  try {
    $view.Execute() | Out-Null
    $record = $view.Fetch()
    try { $record.StringData(1) }
    finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) | Out-Null }
  }
  finally {
    $view.Close() | Out-Null
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null
  }
}

function Invoke-Msi([string]$Operation, [string]$Package, [string]$Name) {
  $log = Join-Path $Output "$Name.log"
  $process = Start-Process -FilePath msiexec.exe -WindowStyle Hidden -PassThru -ArgumentList (
    "$Operation `"$Package`" /qn /norestart /l*v `"$log`"")
  if (-not $process.WaitForExit(180000)) {
    throw "MSI $Name exceeded three minutes; see $log. Discard this test machine."
  }
  $process.ExitCode
}

function Assert-Running([string]$ProductCode, [string]$Marker) {
  if ($installer.ProductState($ProductCode) -ne 5) { throw 'Expected product is not installed.' }
  if ((Get-Service -Name MirageSSD).Status -ne 'Running') { throw 'Installed service is not running.' }
  if ((Get-Content -LiteralPath $Marker -Raw).Trim() -ne 'retain across upgrade and rollback') {
    throw 'Upgrade changed retained application data.'
  }
}

$baselineCode = Read-ProductCode $BaselineMsi
$upgradeCode = Read-ProductCode $UpgradeMsi
if ($baselineCode -eq $upgradeCode) { throw 'Fixtures must have different product codes.' }
# Fault-inject only a COPY of the candidate. Its existing service executable
# returns failure for --uninstall-check without a root, without touching data.
# Schedule that deferred action after InstallServices to force real rollback.
$failureMsi = Join-Path $Output 'rollback-fixture.msi'
Copy-Item -LiteralPath $UpgradeMsi -Destination $failureMsi
$db = $installer.OpenDatabase($failureMsi, 1)
try {
  foreach ($sql in @(
    'INSERT INTO `CustomAction` (`Action`, `Type`, `Source`, `Target`) VALUES (''FailUpgradeForTest'', 3090, ''MirageService'', ''--uninstall-check'')',
    'INSERT INTO `InstallExecuteSequence` (`Action`, `Condition`, `Sequence`) VALUES (''FailUpgradeForTest'', ''NOT REMOVE'', 5850)'
  )) {
    $view = $db.OpenView($sql)
    try { $view.Execute() | Out-Null }
    finally {
      $view.Close() | Out-Null
      [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    }
  }
  $db.Commit() | Out-Null
}
finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null }
$summary = $installer.SummaryInformation($failureMsi, 1)
try {
  $summary.Property(9) = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
  $summary.Persist() | Out-Null
}
finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($summary) | Out-Null }

$marker = Join-Path $stateRoot ('upgrade-contract-' + [guid]::NewGuid().ToString('N') + '.txt')
try {
  if ((Invoke-Msi '/i' $BaselineMsi 'baseline') -ne 0) { throw 'Baseline installation failed or requires a reboot.' }
  Set-Content -LiteralPath $marker -Value 'retain across upgrade and rollback'
  Assert-Running $baselineCode $marker
  if ((Invoke-Msi '/i' $failureMsi 'rollback') -ne 1603) { throw 'Injected upgrade did not fail as expected.' }
  if (-not (Select-String -LiteralPath (Join-Path $Output 'rollback.log') -SimpleMatch 'CustomAction FailUpgradeForTest returned actual error code 1' -Quiet)) {
    throw 'Upgrade failed before reaching the injected rollback action.'
  }
  Assert-Running $baselineCode $marker
  if ($installer.ProductState($upgradeCode) -eq 5) { throw 'Failed upgrade remained installed.' }
  if ((Invoke-Msi '/i' $UpgradeMsi 'upgrade') -ne 0) { throw 'Upgrade failed or requires a reboot.' }
  Assert-Running $upgradeCode $marker
  if ($installer.ProductState($baselineCode) -eq 5) { throw 'Previous product remained installed.' }
  if ((Invoke-Msi '/x' $upgradeCode 'uninstall') -ne 0) { throw 'Fixture uninstall failed.' }
  if (Get-Service -Name MirageSSD -ErrorAction SilentlyContinue) { throw 'Fixture service was not removed.' }
  if (-not (Test-Path -LiteralPath $marker)) { throw 'Uninstall removed retained data.' }
  Write-Output 'PASS: fresh install, running-service upgrade, injected rollback, retained data, and uninstall.'
}
finally {
  # Never erase application state, even on this disposable runner. Logs and any
  # failed installation are retained for diagnosis; CI discards the whole VM.
  [Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
}
