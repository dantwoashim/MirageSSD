[CmdletBinding()]
param(
  # Root of the mounted volume under test, e.g. R:\
  [Parameter(Mandatory)][string]$Target,
  # Native NTFS directory used as the reference. Default: a new folder under
  # $env:TEMP (the same disk as the temporary managed volume's state root).
  [string]$NativeRoot,
  [int]$Rounds = 5,
  [int]$SmallFileCount = 2000,
  [int]$SmallFileBytes = 4096,
  [string]$Output
)
# Paired benchmark of a mounted MirageSSD volume against native NTFS.
# Each round runs the identical workloads on both targets, alternating which
# goes first. Every byte read is verified by the workload tools; a run with
# any verification failure is reported and fails the script. Results are
# medians with min/max over $Rounds rounds on one host: indicative evidence,
# not a qualification.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Output) {
  $Output = Join-Path $repo ('target\perf\' + (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null

$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$tools = @{}
foreach ($name in @('Bench', 'SmallFiles')) {
  $exe = Join-Path $Output "$name.exe"
  & $csc /nologo /optimize+ /platform:x64 /r:System.Web.Extensions.dll "/out:$exe" (Join-Path $PSScriptRoot "$name.cs")
  if ($LASTEXITCODE -ne 0) { throw "Compiling $name.cs failed" }
  $tools[$name] = $exe
}

$nativeCreated = $false
if (-not $NativeRoot) {
  $NativeRoot = Join-Path $env:TEMP ('mirage-perf-native-' + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $NativeRoot | Out-Null
  $nativeCreated = $true
}
$targets = [ordered]@{ mounted = $Target; native = $NativeRoot }
$batch = [Guid]::NewGuid().ToString('N').Substring(0, 8)

function Invoke-Workload([string]$Tool, [string[]]$Arguments, [string]$Json) {
  $quoted = ($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
  $process = Start-Process -FilePath $Tool -ArgumentList $quoted -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput "$Json.stdout.txt" -RedirectStandardError "$Json.stderr.txt"
  if (-not $process.WaitForExit(600000)) {
    Stop-Process -Id $process.Id -Force
    throw "Timed out: $Tool $quoted"
  }
  if (-not (Test-Path -LiteralPath $Json)) { throw "No result file from $Tool $quoted" }
  return Get-Content -LiteralPath $Json -Raw | ConvertFrom-Json
}

function Get-Median([double[]]$Values) {
  $sorted = @($Values | Sort-Object)
  $middle = [int][math]::Floor($sorted.Count / 2)
  if ($sorted.Count % 2) { return $sorted[$middle] }
  return ($sorted[$middle - 1] + $sorted[$middle]) / 2
}

$rows = @()
$failures = @()
try {
  for ($round = 1; $round -le $Rounds; $round++) {
    $order = if ($round % 2) { @('mounted', 'native') } else { @('native', 'mounted') }
    foreach ($kind in $order) {
      $root = $targets[$kind]
      $largeJson = Join-Path $Output "$kind-$round-large.json"
      $smallJson = Join-Path $Output "$kind-$round-small.json"
      $large = Invoke-Workload $tools.Bench @((Join-Path $root "perf-$batch-$round-large"), $largeJson) $largeJson
      $small = Invoke-Workload $tools.SmallFiles @((Join-Path $root "perf-$batch-$round-small"), $smallJson, "$SmallFileCount", "$SmallFileBytes", '20') $smallJson
      foreach ($result in @($large, $small)) {
        if (-not $result.verified -or -not $result.cleaned) {
          $failures += [pscustomobject]@{ kind = $kind; round = $round; directory = $result.directory; error = $result.error; cleanup_error = $result.cleanup_error }
        }
      }
      if (-not $large.verified -or -not $small.verified) { continue }
      $mib = [double]$large.bytes / 1MB
      $rows += [pscustomobject]@{
        kind = $kind
        round = $round
        write_MiBps = $mib / ($large.write_flush_ms / 1000)
        buffered_read_MiBps = $mib / ($large.buffered_read_hash_ms / 1000)
        unbuffered_read_MiBps = $mib / ($large.unbuffered_read_hash_ms / 1000)
        random_4k_us = [double]$large.random_4k_verified_us
        small_create_files_per_s = $small.files / ($small.create_ms / 1000)
        small_enumerate_ms = [double]$small.enumerate_ms
        small_stat_us_per_file = $small.stat_ms * 1000 / $small.files
        small_read_files_per_s = $small.files / ($small.read_verify_ms / 1000)
        small_rename_ms_per_op = $small.rename_ms / $small.renamed
        small_delete_files_per_s = $small.files / ($small.delete_ms / 1000)
      }
      Write-Host ("round {0} {1,-7} write {2,7:N1} MiB/s  read {3,7:N1} MiB/s  rand4k {4,7:N1} us  create {5,7:N0} f/s  stat {6,6:N1} us" -f `
        $round, $kind, $rows[-1].write_MiBps, $rows[-1].buffered_read_MiBps, $rows[-1].random_4k_us, $rows[-1].small_create_files_per_s, $rows[-1].small_stat_us_per_file)
    }
  }
} finally {
  if ($nativeCreated -and (Test-Path -LiteralPath $NativeRoot)) {
    # Only the empty folder this script created; the workload tools remove their own files.
    Remove-Item -LiteralPath $NativeRoot -ErrorAction SilentlyContinue
  }
  $rows | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $Output 'results.json') -Encoding utf8
}

# Direction: true = higher is better.
$metrics = [ordered]@{
  write_MiBps = $true; buffered_read_MiBps = $true; unbuffered_read_MiBps = $true; random_4k_us = $false
  small_create_files_per_s = $true; small_enumerate_ms = $false; small_stat_us_per_file = $false
  small_read_files_per_s = $true; small_rename_ms_per_op = $false; small_delete_files_per_s = $true
}
$summary = @()
foreach ($metric in $metrics.Keys) {
  $entry = [ordered]@{ metric = $metric; higher_is_better = $metrics[$metric] }
  foreach ($kind in @('mounted', 'native')) {
    $values = @($rows | Where-Object { $_.kind -eq $kind } | ForEach-Object { [double]$_.$metric })
    if ($values.Count -eq 0) { continue }
    $entry["${kind}_median"] = [math]::Round((Get-Median $values), 2)
    $entry["${kind}_min"] = [math]::Round(($values | Measure-Object -Minimum).Minimum, 2)
    $entry["${kind}_max"] = [math]::Round(($values | Measure-Object -Maximum).Maximum, 2)
    $entry["${kind}_n"] = $values.Count
  }
  if ($entry.Contains('mounted_median') -and $entry.Contains('native_median') -and $entry.native_median -ne 0) {
    $entry['mounted_over_native'] = [math]::Round($entry.mounted_median / $entry.native_median, 3)
  }
  $summary += [pscustomobject]$entry
}
$report = [ordered]@{
  report_version = 1
  host = $env:COMPUTERNAME
  completed_utc = (Get-Date).ToUniversalTime().ToString('o')
  target = $Target
  native_root = $NativeRoot
  rounds = $Rounds
  small_files = "$SmallFileCount x $SmallFileBytes bytes in 20 directories"
  large_file = '128 MiB, 1 MiB requests, 1,000 random 4 KiB reads'
  failures = $failures
  summary = $summary
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output 'summary.json') -Encoding utf8
$summary | Format-Table metric, mounted_median, native_median, mounted_over_native, mounted_min, mounted_max -AutoSize | Out-String | Write-Host
Write-Host "results: $Output"
if ($failures.Count -gt 0) {
  $failures | Format-List | Out-String | Write-Host
  throw "$($failures.Count) workload run(s) failed verification or cleanup"
}
# bench_mounted.rs asserts this line on stdout.
Write-Output 'hash_match    : True'
