$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $here 'capability-inventory.ps1')

$inventory = Get-CapabilityInventory -Path "$env:SystemDrive\"

$required = @(
  'schema_version', 'captured_at_utc', 'machine', 'user_is_admin',
  'os', 'cfapi', 'projfs', 'winfsp', 'volume', 'bypassio', 'notes'
)
foreach ($key in $required) {
  if (-not ($inventory.PSObject.Properties.Name -contains $key)) {
    throw "capability inventory is missing top-level key: $key"
  }
}
if ($inventory.schema_version -ne 1) { throw 'schema_version must be 1' }
if (@($inventory.notes).Count -ne 2) { throw 'notes must contain exactly the two fixed strings' }

$json = $inventory | ConvertTo-Json -Depth 8
$round = $json | ConvertFrom-Json
if ($round.schema_version -ne 1) { throw 'JSON round-trip lost schema_version' }
if (@($round.notes).Count -ne 2) { throw 'JSON round-trip lost notes' }

Write-Output 'capability-inventory: all assertions passed'
