[CmdletBinding()]
param(
  [string]$Path = "$env:SystemDrive\",
  [string]$OutputPath,
  [switch]$AsObject
)

$ErrorActionPreference = 'Stop'

# E0 capability inventory: records platform facts and never changes them.
# No feature enabling, no sync-root registration, no placeholder creation, no
# policy changes, no elevation. Every probe that fails records
# status='unavailable' with the error text instead of failing the run.

function Get-InventoryAdmin {
  $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-InventoryOs {
  $os = Get-CimInstance Win32_OperatingSystem
  $cv = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
  [pscustomobject][ordered]@{
    status           = 'recorded'
    caption          = [string]$os.Caption
    version          = [string]$os.Version
    build_number     = [string]$os.BuildNumber
    ubr              = [int]$cv.UBR
    display_version  = [string]$cv.DisplayVersion
  }
}

function Get-InventoryCfapi {
  $dll = Join-Path $env:SystemRoot 'System32\cldapi.dll'
  $present = [IO.File]::Exists($dll)
  if (-not $present) {
    return [pscustomobject][ordered]@{ status = 'unavailable'; error = 'cldapi.dll is not present in System32'; dll_present = $false }
  }
  try {
    if (-not ('MirageSSD.CfApi' -as [type])) {
      Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace MirageSSD {
  public static class CfApi {
    [StructLayout(LayoutKind.Sequential)]
    public struct CF_PLATFORM_INFO {
      public int BuildNumber;
      public int RevisionNumber;
      public int IntegrationNumber;
    }
    [DllImport("cldapi.dll")]
    public static extern int CfGetPlatformInfo(out CF_PLATFORM_INFO info);
  }
}
'@
    }
    $info = [MirageSSD.CfApi+CF_PLATFORM_INFO]::new()
    $hr = [MirageSSD.CfApi]::CfGetPlatformInfo([ref]$info)
    [pscustomobject][ordered]@{
      status                  = 'recorded'
      dll_present             = $true
      hresult                 = $hr
      build_number            = $info.BuildNumber
      revision_number         = $info.RevisionNumber
      integration_number      = $info.IntegrationNumber
      integration_number_hex  = ('0x{0:x}' -f $info.IntegrationNumber)
    }
  } catch {
    [pscustomobject][ordered]@{
      status      = 'unavailable'
      error       = $_.Exception.Message
      dll_present = $true
    }
  }
}

function Get-InventoryProjfs {
  $result = [ordered]@{ status = 'recorded' }
  try {
    $feature = Get-CimInstance Win32_OptionalFeature -Filter "Name='Client-ProjFS'"
    if ($null -eq $feature) {
      $result.install_state = $null
      $result.meaning = 'not_listed'
    } else {
      $result.install_state = [int]$feature.InstallState
      $result.meaning = switch ([int]$feature.InstallState) {
        1 { 'enabled' }
        2 { 'disabled' }
        3 { 'absent' }
        default { "unknown_$($feature.InstallState)" }
      }
    }
  } catch {
    $result.status = 'unavailable'
    $result.error = $_.Exception.Message
  }
  try {
    $service = Get-Service -Name PrjFlt -ErrorAction SilentlyContinue
    $result.prjflt_service_present = [bool]($null -ne $service)
    if ($service) { $result.prjflt_service_status = [string]$service.Status }
  } catch {
    $result.prjflt_service_present = $false
    $result.prjflt_service_error = $_.Exception.Message
  }
  [pscustomobject]$result
}

function Get-InventoryWinfsp {
  $result = [ordered]@{ status = 'recorded' }
  try {
    $key = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\WOW6432Node\WinFsp' -ErrorAction SilentlyContinue
    if ($null -eq $key) {
      $key = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\WinFsp' -ErrorAction SilentlyContinue
    }
    if ($null -eq $key) {
      $result.installed = $false
      $result.install_dir = $null
      $result.version = $null
    } else {
      $result.installed = $true
      $result.install_dir = [string]$key.InstallDir
      $result.version = [string]$key.Version
    }
  } catch {
    $result.status = 'unavailable'
    $result.error = $_.Exception.Message
  }
  try {
    $launcher = Get-Service -Name 'WinFsp.Launcher' -ErrorAction SilentlyContinue
    $result.launcher_present = [bool]($null -ne $launcher)
    if ($launcher) { $result.launcher_status = [string]$launcher.Status }
  } catch {
    $result.launcher_present = $false
    $result.launcher_error = $_.Exception.Message
  }
  [pscustomobject]$result
}

function Get-InventoryVolume([string]$TargetPath) {
  $result = [ordered]@{ status = 'recorded'; path = $TargetPath }
  try {
    $volume = $null
    try {
      $volume = Get-Volume -FilePath $TargetPath -ErrorAction Stop
    } catch {
      $root = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($TargetPath))
      $letter = $root.TrimEnd('\').TrimEnd(':')
      $volume = Get-Volume -DriveLetter $letter -ErrorAction Stop
    }
    $result.file_system_type = [string]$volume.FileSystemType
    $result.drive_letter = [string]$volume.DriveLetter
    $result.size = [long]$volume.Size
    $result.size_remaining = [long]$volume.SizeRemaining
    $result.allocation_unit_size = if ($volume.AllocationUnitSize) { [long]$volume.AllocationUnitSize } else { $null }
  } catch {
    $result.status = 'unavailable'
    $result.volume_error = $_.Exception.Message
    return [pscustomobject]$result
  }
  try {
    $letter = [string]$volume.DriveLetter
    if ($letter) {
      $partition = Get-Partition -DriveLetter $letter -ErrorAction Stop
      $disk = $partition | Get-Disk -ErrorAction Stop
      $result.disk_bus_type = [string]$disk.BusType
      $result.disk_model = [string]$disk.Model
      try {
        $physical = Get-PhysicalDisk -ErrorAction Stop | Where-Object { [string]$_.DeviceId -eq [string]$disk.Number } | Select-Object -First 1
        $result.media_type = if ($physical) { [string]$physical.MediaType } else { $null }
      } catch {
        $result.media_type = $null
        $result.media_type_error = $_.Exception.Message
      }
    }
  } catch {
    $result.disk_error = $_.Exception.Message
  }
  [pscustomobject]$result
}

function Get-InventoryBypassIo([string]$TargetPath) {
  try {
    $output = & fsutil bypassIo state $TargetPath 2>&1 | Out-String
    $code = $LASTEXITCODE
    [pscustomobject][ordered]@{
      status    = if ($code -eq 0) { 'recorded' } else { 'unavailable' }
      exit_code = $code
      output    = $output.TrimEnd()
    }
  } catch {
    [pscustomobject][ordered]@{
      status = 'unavailable'
      error  = $_.Exception.Message
    }
  }
}

function Get-CapabilityInventory {
  [CmdletBinding()]
  param(
    [string]$Path = "$env:SystemDrive\"
  )
  [pscustomobject][ordered]@{
    schema_version  = 1
    captured_at_utc = (Get-Date).ToUniversalTime().ToString('o')
    machine         = [string]$env:COMPUTERNAME
    user_is_admin   = Get-InventoryAdmin
    os              = Get-InventoryOs
    cfapi           = Get-InventoryCfapi
    projfs          = Get-InventoryProjfs
    winfsp          = Get-InventoryWinfsp
    volume          = Get-InventoryVolume $Path
    bypassio        = Get-InventoryBypassIo $Path
    notes           = @(
      'API presence is recorded separately from behavior; it is not evidence of partial-hydration performance, compatibility, or support for a title.',
      'No Windows feature, sync root, placeholder, filter, or security policy was changed by this inventory.'
    )
  }
}

# Dot-sourcing exposes the helpers for focused regression checks only.
if ($MyInvocation.InvocationName -ne '.') {
  try {
    $inventory = Get-CapabilityInventory -Path $Path
    if ($AsObject) {
      Write-Output $inventory
    } else {
      $json = $inventory | ConvertTo-Json -Depth 8
      if ($OutputPath) {
        Set-Content -LiteralPath $OutputPath -Value $json -Encoding UTF8
      } else {
        Write-Output $json
      }
    }
  } catch {
    Write-Error $_
    exit 1
  }
}
