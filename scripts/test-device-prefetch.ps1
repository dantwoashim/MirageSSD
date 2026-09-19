$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'prefetch-device.ps1')

function Assert-Equal($Actual, $Expected) {
  if ($Actual -ne $Expected) { throw "Expected $Expected; got $Actual" }
}
function Assert-Rejected([scriptblock]$Action) {
  $rejected = $false
  try { & $Action | Out-Null } catch { $rejected = $true }
  if (-not $rejected) { throw 'Invalid input was accepted.' }
}

Assert-Equal @(Get-PrefetchRanges 0 ZipMetadata).Count 0
foreach ($size in @(1L, 4096L, 131072L, 135168L, 135169L, 33554432L, [long]::MaxValue)) {
  $ranges = @(Get-PrefetchRanges $size ZipMetadata)
  $previousEnd = 0L
  $bytes = 0L
  foreach ($range in $ranges) {
    if ($range.Offset -lt $previousEnd -or $range.Length -le 0 -or $range.Length -gt $size - $range.Offset) { throw 'Invalid/overlapping planned range.' }
    $previousEnd = $range.Offset + $range.Length
    $bytes += $range.Length
  }
  Assert-Equal $bytes ([Math]::Min($size, 135168))
}
$large = @(Get-PrefetchRanges 32MB ZipMetadata)
Assert-Equal $large[0].Offset 0
Assert-Equal $large[0].Length 4096
Assert-Equal $large[1].Offset (32MB - 128KB)
Assert-Equal @(Get-PrefetchRanges 128 Prefix)[0].Length 128
Assert-Equal @(Get-PrefetchRanges 32MB Prefix)[0].Length 1MB
Assert-Equal @(Get-PrefetchRanges 32MB WholeFile)[0].Length 32MB
Assert-Equal (ConvertFrom-PrefetchSize '24Gi') 24GB
Assert-Equal (ConvertFrom-PrefetchSize '1.5Mi') 1572864
Assert-Equal (ConvertFrom-PrefetchSize '1024') 1024
Assert-Rejected { Get-PrefetchRanges -1 ZipMetadata }
Assert-Rejected { Get-PrefetchRanges 5 Unknown }
Assert-Rejected { ConvertFrom-PrefetchSize 'NaN' }
Assert-Rejected { ConvertFrom-PrefetchSize '999999999999999999Ti' }
Assert-Rejected { Assert-PrefetchPath 'C:\outside.zip' 'M:\' }
Assert-Rejected { Assert-PrefetchPath 'M:\file.zip:stream' 'M:\' }
Assert-Rejected { Assert-PrefetchPath '..\outside.zip' 'M:\' }
Assert-Rejected { Assert-PrefetchPath '\\server\file.zip' 'M:\' }
$memory = [IO.MemoryStream]::new([byte[]]::new(400000))
$cancel = [Threading.CancellationTokenSource]::new()
try {
  $progress = [pscustomobject]@{ Remaining = 135168L; ReadBytes = 0L; FileRead = 0L; Checks = 0 }
  Read-PrefetchRanges $memory @(Get-PrefetchRanges $memory.Length ZipMetadata) $cancel.Token { $progress.Checks++ } $progress
  Assert-Equal $progress.ReadBytes 135168
  Assert-Equal $progress.Remaining 0
  Assert-Equal $progress.Checks 3
  Assert-Equal $memory.Position $memory.Length
  Assert-Rejected { Read-PrefetchRanges $memory @([pscustomobject]@{Offset=0L;Length=1L}) $cancel.Token {} $progress }
  $progress.Remaining = 100
  Assert-Rejected { Read-PrefetchRanges $memory @([pscustomobject]@{Offset=0L;Length=1L}) $cancel.Token { throw 'low disk space' } $progress }
  Assert-Equal $progress.ReadBytes 135168
  Assert-Rejected { Read-PrefetchRanges $memory @([pscustomobject]@{Offset=400000L;Length=1L}) $cancel.Token {} $progress }
  $cancel.Cancel()
  Assert-Rejected { Read-PrefetchRanges $memory @([pscustomobject]@{Offset=0L;Length=1L}) $cancel.Token {} $progress }
  Assert-Equal $progress.ReadBytes 135168
} finally { $cancel.Dispose(); $memory.Dispose() }
Write-Output 'PASS: range planning, bounded reads, cancellation, reserve failure, short reads, size validation and path rejection. No mount or cloud data accessed.'
