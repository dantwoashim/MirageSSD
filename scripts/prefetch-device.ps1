[CmdletBinding()]
param(
  [string[]]$Path,
  [ValidateSet('ZipMetadata', 'Prefix', 'WholeFile')]
  [string]$Mode = 'ZipMetadata',
  [ValidateRange(1, 1073741824)]
  [long]$MaxBytes = 33554432,
  [ValidateRange(1, 600)]
  [int]$MaxSeconds = 60,
  [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'

function Get-PrefetchRanges([long]$Size, [string]$Policy) {
  if ($Size -lt 0) { throw 'Invalid file size.' }
  if ($Size -eq 0) { return }
  switch ($Policy) {
    'WholeFile' { [pscustomobject]@{ Offset = 0L; Length = $Size } }
    'Prefix' { [pscustomobject]@{ Offset = 0L; Length = [Math]::Min($Size, 1MB) } }
    'ZipMetadata' {
      $prefix = [Math]::Min($Size, 4096)
      $suffix = [Math]::Max(0L, $Size - 131072)
      if ($suffix -le $prefix) {
        [pscustomobject]@{ Offset = 0L; Length = $Size }
      } else {
        [pscustomobject]@{ Offset = 0L; Length = $prefix }
        [pscustomobject]@{ Offset = $suffix; Length = $Size - $suffix }
      }
    }
    default { throw 'Unknown prefetch mode.' }
  }
}

function ConvertFrom-PrefetchSize([string]$Value) {
  if ($Value -notmatch '^(\d+(?:\.\d+)?)(Ki|Mi|Gi|Ti)?$') {
    throw 'Unsupported cache reserve size; use Ki, Mi, Gi or Ti units.'
  }
  $multiplier = switch ($Matches[2]) { Ki { 1KB } Mi { 1MB } Gi { 1GB } Ti { 1TB } default { 1 } }
  $number = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
  if ($number * $multiplier -gt [long]::MaxValue) { throw 'Cache reserve is too large.' }
  [long][Math]::Ceiling($number * $multiplier)
}

function Assert-PrefetchPath([string]$FilePath, [string]$MountRoot) {
  # No wildcards, relative paths, alternate streams, device paths, or links.
  if ($FilePath -notmatch '^[D-Zd-z]:\\' -or $FilePath.Substring(2).Contains(':')) {
    throw 'Choose an absolute file path on the installed MirageSSD drive.'
  }
  $full = [IO.Path]::GetFullPath($FilePath)
  if (-not $full.StartsWith($MountRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The selected file is outside the installed MirageSSD drive.'
  }
  $cursor = $full
  while ($cursor.Length -gt $MountRoot.Length) {
    if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
      throw 'Prefetch does not follow symbolic links or junctions.'
    }
    $cursor = [IO.Path]::GetDirectoryName($cursor)
  }
  if (-not [IO.File]::Exists($full)) { throw 'Select a file, not a directory.' }
  $full
}

function Read-PrefetchRanges([IO.Stream]$Stream, [object[]]$Ranges, [Threading.CancellationToken]$Token, [scriptblock]$BeforeRead, $Progress) {
  $buffer = [byte[]]::new(65536)
  foreach ($range in $Ranges) {
    [void]$Stream.Seek($range.Offset, [IO.SeekOrigin]::Begin)
    $left = $range.Length
    while ($left -gt 0) {
      $Token.ThrowIfCancellationRequested()
      & $BeforeRead
      $count = [int][Math]::Min($left, $buffer.Length)
      if ($count -gt $Progress.Remaining) { throw 'Requested-read budget exhausted.' }
      $task = $Stream.ReadAsync($buffer, 0, $count, $Token)
      $got = $task.GetAwaiter().GetResult()
      if ($got -eq 0) { throw 'File ended before the selected range was read.' }
      $Progress.FileRead += $got
      $Progress.ReadBytes += $got
      $Progress.Remaining -= $got
      $left -= $got
    }
  }
}

function Invoke-DevicePrefetch {
  $installRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
  $configuration = Get-Content -LiteralPath (Join-Path $installRoot 'device-install.json') -Raw | ConvertFrom-Json
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  if ($configuration.format -ne 'miragessd-device-install-v1' -or $configuration.owner_sid -ne $sid -or $configuration.task_name -ne "MirageSSD Drive $sid") {
    throw 'A MirageSSD installation owned by this Windows user is required.'
  }
  $root = [string]$configuration.drive_letter
  if ($root -notmatch '^[D-Z]:\\$') { throw 'Invalid installed drive letter.' }
  $volume = [IO.DriveInfo]::new($root)
  if (-not $volume.IsReady -or $volume.VolumeLabel -ne 'MirageSSD') { throw 'The installed MirageSSD drive is not available.' }
  $cache = [IO.Path]::GetFullPath([string]$configuration.cache_directory)
  if (-not [IO.Directory]::Exists($cache) -or $cache.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The local cache directory is unavailable or invalid.'
  }
  # Query the actual volume containing the cache, including mounted folders.
  if (-not ('MirageSSD.PrefetchSpace' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace MirageSSD {
  public static class PrefetchSpace {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool GetDiskFreeSpaceEx(string path, out ulong available, out ulong total, out ulong free);
    public static ulong Available(string path) {
      ulong available, total, free;
      if (!GetDiskFreeSpaceEx(path, out available, out total, out free))
        throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
      return available;
    }
  }
}
'@
  }
  $reserve = if ($configuration.cache_min_free_space) { ConvertFrom-PrefetchSize $configuration.cache_min_free_space } else { 24GB }
  # Read-ahead is provider-controlled: leave extra headroom, not a hard reservation.
  $minimumFree = [ulong]($reserve + 512MB)
  $selected = @($Path)
  if (-not $Path) {
    Add-Type -AssemblyName System.Windows.Forms
    $dialog = [Windows.Forms.OpenFileDialog]::new()
    try {
      $dialog.Title = 'Prepare files: ZIP directory metadata only (not entire contents)'
      $dialog.InitialDirectory = $root
      $dialog.Multiselect = $true
      $dialog.Filter = 'ZIP archives (*.zip)|*.zip'
      if ($dialog.ShowDialog() -ne [Windows.Forms.DialogResult]::OK) { return }
      $selected = @($dialog.FileNames)
    } finally { $dialog.Dispose() }
  }
  if ($selected.Count -gt 32) { throw 'Select at most 32 files per prefetch run.' }
  $mutex = [Threading.Mutex]::new($false, "Local\MirageSSD-Prefetch-$sid")
  $ownsMutex = $false
  $cancel = $null
  try {
    try { $ownsMutex = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $ownsMutex = $true }
    if (-not $ownsMutex) { throw 'Another prefetch run is already active for this Windows user.' }
    $cancel = [Threading.CancellationTokenSource]::new()
    $cancel.CancelAfter($MaxSeconds * 1000)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $results = [Collections.Generic.List[object]]::new()
    $progress = [pscustomobject]@{ Remaining = $MaxBytes; ReadBytes = 0L; FileRead = 0L }
    foreach ($candidate in $selected) {
      if ($cancel.IsCancellationRequested) { break }
      $filePath = Assert-PrefetchPath $candidate $root
      if (-not $seen.Add($filePath)) { continue }
      if ($Mode -eq 'ZipMetadata' -and [IO.Path]::GetExtension($filePath) -ine '.zip') {
        throw 'ZipMetadata mode accepts .zip files only. Use Prefix or WholeFile explicitly for other files.'
      }
      $stream = $null
      $status = 'planned'
      $progress.FileRead = 0L
      $ranges = @()
      try {
        if ([MirageSSD.PrefetchSpace]::Available($cache) -lt $minimumFree) { throw 'Cache disk headroom is below the safety reserve. Prefetch stopped.' }
        # Share-read only: do not race local application writes, renames or deletion.
        $stream = [IO.FileStream]::new($filePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read, 1, ([IO.FileOptions]::Asynchronous -bor [IO.FileOptions]::RandomAccess))
        $size = $stream.Length
        $ranges = @(Get-PrefetchRanges $size $Mode)
        $planned = 0L
        foreach ($range in $ranges) { $planned += $range.Length }
        if ($planned -gt $progress.Remaining) { $status = 'skipped_budget' }
        elseif (-not $PlanOnly) {
          Read-PrefetchRanges $stream $ranges $cancel.Token {
            if ([MirageSSD.PrefetchSpace]::Available($cache) -lt $minimumFree) { throw 'Cache disk headroom is below the safety reserve. Prefetch stopped.' }
          } $progress
          if ($stream.Length -ne $size) { throw 'File size changed during prefetch.' }
          $status = 'read_completed'
        } else { $progress.Remaining -= $planned }
      } catch {
        if (-not $cancel.IsCancellationRequested) { throw }
        $status = 'cancelled'
      } finally {
        if ($stream) { $stream.Dispose() }
      }
      $results.Add([pscustomobject]@{ File = $filePath; Status = $status; Ranges = $ranges; ReadBytes = $progress.FileRead })
      if (-not $PlanOnly) { Write-Progress -Activity 'Preparing selected MirageSSD files' -Status $status -PercentComplete ([Math]::Min(100, 100 * $progress.ReadBytes / $MaxBytes)) }
    }
    [pscustomobject]@{
      Mode = $Mode; PlanOnly = [bool]$PlanOnly; ReadBytes = $progress.ReadBytes
      RequestedReadBudgetBytes = $MaxBytes; ElapsedSeconds = $watch.Elapsed.TotalSeconds
      Cancelled = $cancel.IsCancellationRequested; Files = @($results.ToArray())
      NetworkBytes = $null; Pinned = $false; RemoteDurabilityVerified = $false
      Note = 'Reads use the mounted provider. Network bytes/read-ahead may exceed requested bytes. Cache retention and remote upload completion are not guaranteed.'
    } | ConvertTo-Json -Depth 5
  } finally {
    Write-Progress -Activity 'Preparing selected MirageSSD files' -Completed
    if ($cancel) { $cancel.Cancel(); $cancel.Dispose() }
    if ($ownsMutex) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
  }
}

# Dot-sourcing exposes the pure helpers for focused regression checks only.
if ($MyInvocation.InvocationName -ne '.') {
  try {
    $report = Invoke-DevicePrefetch
    if ($report) { Write-Output $report }
    if (-not $Path -and $report) {
      Add-Type -AssemblyName PresentationFramework
      $summary = $report | ConvertFrom-Json
      $completed = @($summary.Files | Where-Object Status -eq 'read_completed').Count
      $skipped = @($summary.Files | Where-Object Status -eq 'skipped_budget').Count
      $message = "Files read: $completed. Skipped for budget: $skipped. Timed out: $($summary.Cancelled).`nZIP directory metadata only, not entire contents. This does not verify uploads or pin cached data."
      [Windows.MessageBox]::Show($message, 'MirageSSD', 'OK', 'Information') | Out-Null
    }
  } catch {
    if (-not $Path) {
      Add-Type -AssemblyName PresentationFramework
      [Windows.MessageBox]::Show($_.Exception.Message, 'MirageSSD preparation', 'OK', 'Error') | Out-Null
    } else { Write-Error $_ }
    exit 1
  }
}
