[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$MsiPath)

$ErrorActionPreference = 'Stop'
$path = (Resolve-Path -LiteralPath $MsiPath).Path
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($path, 0)

function Read-MsiRows([string]$Query, [int]$Columns) {
  $view = $database.OpenView($Query)
  try {
    $view.Execute() | Out-Null
    while ($record = $view.Fetch()) {
      try {
        $values = @()
        for ($i = 1; $i -le $Columns; $i++) { $values += $record.StringData($i) }
        # Keep each MSI record as one pipeline item, including single-row tables.
        ,$values
      }
      finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) | Out-Null }
    }
  }
  finally {
    $view.Close() | Out-Null
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
  }
}

try {
  $sequence = @{}
  $conditions = @{}
  foreach ($row in (Read-MsiRows 'SELECT `Action`, `Condition`, `Sequence` FROM `InstallExecuteSequence`' 3)) {
    $sequence[$row[0]] = [int]$row[2]
    $conditions[$row[0]] = $row[1]
  }
  # Test the compiled MSI, not just the authoring: default MajorUpgrade removal
  # precedes InstallInitialize and cannot restore the old product on failure.
  $ordered = @('InstallValidate', 'InstallInitialize', 'RemoveExistingProducts',
    'StopServices', 'DeleteServices', 'RemoveFiles', 'InstallFiles', 'InstallServices',
    'StartServices', 'InstallFinalize')
  for ($i = 0; $i -lt $ordered.Count; $i++) {
    $action = $ordered[$i]
    if (-not $sequence.ContainsKey($action)) { throw "MSI is missing $action." }
    if ($i -gt 0 -and $sequence[$ordered[$i - 1]] -ge $sequence[$action]) {
      throw "Unsafe upgrade sequence: $($ordered[$i - 1]) must precede $action."
    }
  }
  foreach ($row in (Read-MsiRows 'SELECT `Action`, `Type`, `Target` FROM `CustomAction`' 3)) {
    if ($row[0] -in @('StopPreviousService', 'ClosePreviousApp', 'DeleteOrphanService') -or
        $row[2] -match '(?i)\b(net|taskkill|sc)\.exe\b') {
      throw "Unsafe external service/process cleanup remains: $($row[0])."
    }
    if ($row[0] -eq 'BlockUnsafeUninstall' -and (([int]$row[1] -band 3072) -ne 3072)) {
      throw 'The uninstall guard must run deferred without impersonation.'
    }
  }
  $serviceRows = @(Read-MsiRows 'SELECT `Name`, `Event`, `Wait` FROM `ServiceControl`' 3)
  $service = @($serviceRows | Where-Object { $_[0] -eq 'MirageSSD' })
  # Start on install; stop/delete on install AND uninstall (including orphans).
  if ($service.Count -ne 1 -or [int]$service[0][1] -ne 171 -or $service[0][2] -ne '1') {
    throw 'MirageSSD must use checked service start/stop/delete with bounded waiting.'
  }
  if ($conditions['BlockUnsafeUninstall'] -ne 'REMOVE~="ALL" AND NOT UPGRADINGPRODUCTCODE' -or
      $sequence['BlockUnsafeUninstall'] -le $sequence['InstallInitialize'] -or
      $sequence['BlockUnsafeUninstall'] -ge $sequence['StopServices']) {
    throw 'The ordinary uninstall guard must run before service stop, and exclude major upgrades.'
  }
  foreach ($row in (Read-MsiRows 'SELECT `Property`, `Value` FROM `Property`' 2)) {
    if ($row[0] -in @('DISABLEROLLBACK', 'MSIRESTARTMANAGERCONTROL')) {
      throw "Upgrade safety must not be disabled by $($row[0])."
    }
  }
  Write-Output 'PASS: compiled MSI upgrade ordering, transactional service lifecycle, bounded wait, and uninstall guard.'
}
finally {
  [Runtime.InteropServices.Marshal]::FinalReleaseComObject($database) | Out-Null
  [Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
}
