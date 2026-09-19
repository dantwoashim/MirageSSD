$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')
$root = Join-Path ([IO.Path]::GetTempPath()) ('mirage-recovery-' + [Guid]::NewGuid().ToString('N'))
$stage = Join-Path $root ('MirageSSD-Setup-' + [Guid]::NewGuid().ToString('N') + '\MirageSSD-OneClick-20260101-000000')
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$childScript = Join-Path $stage 'setup-miragessd.ps1'
$marker = Join-Path $root 'child.txt'
$escapedMarker = $marker.Replace("'", "''")
@"
`$mutex = [Threading.Mutex]::new(`$false, ('Local\MirageSSD-Device-' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value))
try { `$null = `$mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { }
[IO.File]::WriteAllText('$escapedMarker', [string]`$PID)
Start-Sleep -Seconds 600
"@ | Set-Content -LiteralPath $childScript
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$parentScript = Join-Path $root 'parent.ps1'
"Start-Process -FilePath '$($powershell.Replace("'", "''"))' -ArgumentList '-NoProfile -File `"$($childScript.Replace("'", "''"))`"' -WindowStyle Hidden" | Set-Content -LiteralPath $parentScript
$child = $null
$oldTemp = $env:TEMP
try {
  $parent = Start-Process -FilePath $powershell -ArgumentList ('-NoProfile -File "' + $parentScript + '"') -WindowStyle Hidden -PassThru
  if (-not $parent.WaitForExit(5000)) { throw 'Fixture parent did not exit.' }
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  while (-not (Test-Path -LiteralPath $marker) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
  if (-not (Test-Path -LiteralPath $marker)) { throw 'Legacy orphan fixture did not start.' }
  $child = Get-Process -Id ([int](Get-Content -LiteralPath $marker -Raw))
  $env:TEMP = $root
  $snapshot = @(Get-CimInstance Win32_Process)
  $orphans = @(Get-OrphanedSetupProcesses $snapshot $root)
  if ($child.Id -notin $orphans.ProcessId) { throw 'Legacy orphan was not identified.' }
  # An otherwise matching process with a live parent must never be reclaimed.
  $fakeChild = [pscustomobject]@{ Name='powershell.exe'; CommandLine=('-File "' + $childScript + '"'); ProcessId=123; ParentProcessId=$PID; CreationDate=[DateTime]::Now }
  $liveParent = [pscustomobject]@{ ProcessId=$PID; CreationDate=[DateTime]::Now.AddMinutes(-1) }
  if (@(Get-OrphanedSetupProcesses @($fakeChild,$liveParent) $root).Count) { throw 'An active installer would be terminated.' }
  Stop-OrphanedSetup $root
  if (-not $child.WaitForExit(5000)) { throw 'Legacy orphan survived recovery.' }
  $mutex=[Threading.Mutex]::new($false,('Local\MirageSSD-Device-'+[Security.Principal.WindowsIdentity]::GetCurrent().User.Value))
  try {
    $held=$false
    try { $held=$mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $held=$true }
    if (-not $held) { throw 'Recovered orphan still holds the device mutex.' }
    $mutex.ReleaseMutex()
  } finally { $mutex.Dispose() }
  Write-Output 'PASS: legacy orphan identified and stopped, active-parent process excluded, abandoned mutex recovered. No real installation or account touched.'
} finally {
  $env:TEMP=$oldTemp
  if ($child -and -not $child.HasExited) { Stop-Process -Id $child.Id -Force }
}
