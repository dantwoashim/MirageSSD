[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$ManagedRoot,
  [string]$RcloneRoot,   # mandatory for the hot paired mode
  [switch]$ColdManaged,
  # Journal directory of the managed host; its *.payload files are deleted
  # before every cold run so each measurement starts remote-only.
  [string]$JournalDir,
  # Mount-relative paths of the three cold files (100 KiB / 5 MiB / 50 MiB).
  [string[]]$ColdFiles = @(),
  [int]$ColdRuns = 5,
  [int]$Iterations = 10,
  [int]$SmallFiles = 200,
  [int]$SmallBytes = 4096,
  [int]$MediumFiles = 20,
  [int]$MediumBytes = 1048576,
  [int]$LargeFiles = 2,
  [int]$LargeBytes = 67108864,
  [int]$RandomReads = 1000,
  [string]$Output = "$PSScriptRoot\..\dist-dev\bench\paired-mounts.json",
  [int]$Seed = 20260920
)
# Paired, interleaved benchmark of two mounted volumes on one host.
# Every iteration runs the identical workload on both mounts in a randomized
# order and verifies every byte read (BLAKE-free: SHA-256 via .NET) so the
# accuracy column is measured, not assumed. Results are medians with IQR;
# a single host with N=10 is indicative evidence only, not qualification.
$ErrorActionPreference = 'Stop'

# Cold-managed mode: measures first-open-to-first-byte and full sequential
# read for files whose journal payloads are remote-only (evicted to the
# backend). Each run deletes the journal payload files first so the host
# must fetch — and re-stage, per the Phase-2 restage — from the backend.
if ($ColdManaged) {
  if (-not $JournalDir -or -not $ColdFiles.Count) { throw '-ColdManaged needs -JournalDir and -ColdFiles' }
  $ColdFiles = @($ColdFiles | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
  $coldRows = @()
  foreach ($rel in $ColdFiles) {
    $path = Join-Path $ManagedRoot $rel
    $size = (Get-Item -LiteralPath $path).Length
    for ($run = 1; $run -le $ColdRuns; $run++) {
      Remove-Item -LiteralPath (Join-Path $JournalDir '*.payload') -Force -ErrorAction SilentlyContinue
      $firstByteMs = 0.0; $readMs = 0.0; $bytesRead = 0L
      $sw = [Diagnostics.Stopwatch]::StartNew()
      $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
      try {
        $one = $stream.ReadByte()
        $firstByteMs = $sw.Elapsed.TotalMilliseconds
        if ($one -ge 0) { $bytesRead = 1 }
        $buf = [byte[]]::new(1MB)
        while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) { $bytesRead += $n }
      } finally { $stream.Dispose() }
      $readMs = $sw.Elapsed.TotalMilliseconds
      $coldRows += [pscustomobject]@{
        file = $rel; size_bytes = $size; run = $run
        first_byte_ms = $firstByteMs; full_read_ms = $readMs
        bytes_read = $bytesRead; read_MBps = [math]::Round($bytesRead / 1MB / ($readMs / 1000), 1)
      }
      Write-Host ("cold {0} run {1}: first-byte {2:N1} ms, read {3:N1} ms" -f $rel, $run, $firstByteMs, $readMs)
    }
  }
  function Median([double[]]$Values) {
    $s = @($Values | Sort-Object)
    if (-not $s.Count) { return $null }
    return $s[[math]::Floor(($s.Count - 1) / 2)]
  }
  $coldSummary = @()
  foreach ($rel in $ColdFiles) {
    $rows = @($coldRows | Where-Object file -eq $rel)
    $coldSummary += [ordered]@{
      file = $rel; size_bytes = $rows[0].size_bytes
      first_byte_ms_median = Median ($rows | ForEach-Object { $_.first_byte_ms })
      full_read_ms_median = Median ($rows | ForEach-Object { $_.full_read_ms })
      read_MBps_median = Median ($rows | ForEach-Object { $_.read_MBps })
      runs = $rows.Count
    }
  }
  $coldReport = [ordered]@{
    report_version = 1; mode = 'cold-managed'; host = $env:COMPUTERNAME
    completed_utc = [DateTime]::UtcNow.ToString('o')
    caveats = @('Cold = journal payload files deleted before each run; host fetches+restages from the backend.',
                'First-byte includes open + backend fetch of the covering frames; full read includes restage.')
    summary = $coldSummary
    runs = $coldRows
  }
  New-Item -ItemType Directory -Force -Path (Split-Path $Output) | Out-Null
  $coldReport | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Output -Encoding UTF8
  $coldReport.summary | ConvertTo-Json -Depth 4
  exit 0
}

if (-not $RcloneRoot) { throw '-RcloneRoot is required for the paired hot benchmark' }
$rng = [Random]::new($Seed)
$sha = [Security.Cryptography.SHA256]::Create()

function New-Payload([int]$Bytes, [int]$Salt) {
  $buffer = [byte[]]::new($Bytes)
  $local = [Random]::new($Seed + $Salt)
  $local.NextBytes($buffer)
  Write-Output -NoEnumerate $buffer
}
function Hash([byte[]]$Bytes) { [BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '') }
function Time([scriptblock]$Block) {
  $sw = [Diagnostics.Stopwatch]::StartNew(); & $Block; $sw.Stop(); $sw.Elapsed.TotalMilliseconds
}
function Write-File([string]$Path, [byte[]]$Bytes) {
  $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try { $stream.Write($Bytes, 0, $Bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
}

$spec = @()
for ($i = 0; $i -lt $SmallFiles; $i++) { $spec += [pscustomobject]@{ Name = "s$i.bin"; Bytes = $SmallBytes; Salt = $i } }
for ($i = 0; $i -lt $MediumFiles; $i++) { $spec += [pscustomobject]@{ Name = "m$i.bin"; Bytes = $MediumBytes; Salt = 10000 + $i } }
for ($i = 0; $i -lt $LargeFiles; $i++) { $spec += [pscustomobject]@{ Name = "l$i.bin"; Bytes = $LargeBytes; Salt = 20000 + $i } }
$payloads = @{}
$expected = @{}
foreach ($file in $spec) { $payloads[$file.Name] = New-Payload $file.Bytes $file.Salt; $expected[$file.Name] = Hash $payloads[$file.Name] }
$totalBytes = ($spec | Measure-Object Bytes -Sum).Sum

function Run-Workload([string]$Root, [string]$Label, [int]$Iteration) {
  $dir = Join-Path $Root ("bench-" + $Label + "-" + $Iteration + "-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
  New-Item -ItemType Directory -Path $dir | Out-Null
  $result = [ordered]@{ mount = $Label; iteration = $Iteration; dir = $dir; incorrect_files = 0; errors = @() }
  try {
    $result.write_ms = Time { foreach ($f in $spec) { Write-File (Join-Path $dir $f.Name) $payloads[$f.Name] } }
    $result.write_MBps = [math]::Round($totalBytes / 1MB / ($result.write_ms / 1000), 1)
    $result.stat_all_ms = Time { foreach ($f in $spec) { [void](Get-Item -LiteralPath (Join-Path $dir $f.Name)).Length } }
    $result.listdir_ms = Time { [void](Get-ChildItem -LiteralPath $dir -Force) }
    $incorrect = 0
    $result.read_seq_ms = Time {
      foreach ($f in $spec) { $bytes = [IO.File]::ReadAllBytes((Join-Path $dir $f.Name)); if ([BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '') -ne $expected[$f.Name]) { $incorrect++ } }
    }
    $result.read_seq_MBps = [math]::Round($totalBytes / 1MB / ($result.read_seq_ms / 1000), 1)
    $result.incorrect_files = $incorrect
    $large = $spec | Where-Object Name -like 'l*'
    $badRandom = 0
    $result.read_random4k_ms = Time {
      foreach ($n in 1..$RandomReads) {
        $f = $large[$rng.Next($large.Count)]
        $offset = [long]($rng.Next(0, [int]($f.Bytes / 4096))) * 4096
        $stream = [IO.File]::Open((Join-Path $dir $f.Name), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
          $stream.Position = $offset; $buf = [byte[]]::new(4096); $read = $stream.Read($buf, 0, 4096)
          $exp = [byte[]]::new(4096); [Array]::Copy($payloads[$f.Name], $offset, $exp, 0, 4096)
          if ($read -ne 4096 -or -not [Linq.Enumerable]::SequenceEqual([byte[]]$buf, [byte[]]$exp)) { $badRandom++ }
        } finally { $stream.Dispose() }
      }
    }
    $result.read_random4k_us_per_op = [math]::Round($result.read_random4k_ms * 1000 / $RandomReads, 0)
    $result.incorrect_random_reads = $badRandom
    $result.rename_all_ms = Time { foreach ($f in $spec) { Rename-Item -LiteralPath (Join-Path $dir $f.Name) -NewName ($f.Name + '.r') } }
    $result.delete_all_ms = Time { foreach ($f in $spec) { Remove-Item -LiteralPath (Join-Path $dir ($f.Name + '.r')) -Force } }
    $result.rmdir_ms = Time { Remove-Item -LiteralPath $dir -Force }
  } catch {
    $result.errors += $_.Exception.Message
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
  }
  [pscustomobject]$result
}

$rows = @()
for ($it = 1; $it -le $Iterations; $it++) {
  $order = if ($rng.Next(2) -eq 0) { @('managed', 'rclone') } else { @('rclone', 'managed') }
  foreach ($label in $order) {
    $root = if ($label -eq 'managed') { $ManagedRoot } else { $RcloneRoot }
    Write-Host ("iteration {0} {1}" -f $it, $label)
    $rows += Run-Workload $root $label $it
  }
}

function Summ([object[]]$Values) {
  $sorted = @($Values | Where-Object { $_ -ne $null } | Sort-Object)
  if ($sorted.Count -eq 0) { return $null }
  $q = { param($p) $sorted[[math]::Min($sorted.Count - 1, [math]::Floor($p * $sorted.Count))] }
  [ordered]@{ median = & $q 0.5; p25 = & $q 0.25; p75 = & $q 0.75; n = $sorted.Count }
}
$metrics = 'write_ms', 'write_MBps', 'stat_all_ms', 'listdir_ms', 'read_seq_ms', 'read_seq_MBps', 'read_random4k_us_per_op', 'rename_all_ms', 'delete_all_ms'
$summary = [ordered]@{}
foreach ($label in 'managed', 'rclone') {
  $subset = $rows | Where-Object mount -eq $label
  $entry = [ordered]@{
    completed_iterations = @($subset | Where-Object { $_.errors.Count -eq 0 }).Count
    incorrect_files_total = ($subset | Measure-Object incorrect_files -Sum).Sum
    incorrect_random_reads_total = ($subset | Measure-Object incorrect_random_reads -Sum).Sum
    errors = @($subset | ForEach-Object { $_.errors })
  }
  foreach ($m in $metrics) { $entry[$m] = Summ ($subset | Where-Object { $_.errors.Count -eq 0 } | ForEach-Object { $_.$m }) }
  $summary[$label] = $entry
}
$report = [ordered]@{
  report_version = 1
  host = $env:COMPUTERNAME
  completed_utc = [DateTime]::UtcNow.ToString('o')
  workload = [ordered]@{ small = "$SmallFiles x $SmallBytes"; medium = "$MediumFiles x $MediumBytes"; large = "$LargeFiles x $LargeBytes"; random_reads = $RandomReads; total_bytes = $totalBytes }
  caveats = @(
    'Single host, interleaved randomized order, N iterations; indicative only, not A24 qualification.',
    'Both volumes serve these reads from local cache (files were just written): this is a hot-path comparison.',
    'Cold-from-cloud reads are not compared here: evicting the production rclone VFS cache was not permitted.'
  )
  summary = $summary
  iterations = $rows
}
New-Item -ItemType Directory -Force -Path (Split-Path $Output) | Out-Null
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Output -Encoding UTF8
$report.summary | ConvertTo-Json -Depth 4
