$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ('mirage-interruption-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$wrapper = Join-Path $root 'setup-test.exe'
$source = Join-Path $PSScriptRoot 'friend-setup.cs'
$compiler = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
& $compiler /nologo /define:LIFECYCLE_TEST /target:winexe /platform:x64 /reference:System.dll /reference:System.Core.dll /reference:System.Windows.Forms.dll /reference:System.Drawing.dll /reference:System.IO.Compression.dll /reference:System.IO.Compression.FileSystem.dll "/out:$wrapper" $source
if ($LASTEXITCODE -ne 0) { throw 'Wrapper compilation failed.' }
$fixture = Join-Path $root 'fixture'
$marker = Join-Path $root 'login-paused.txt'
$env:MIRAGE_TEST_FIXTURE_ROOT = $fixture
$env:MIRAGE_TEST_PAUSE_LOGIN = $marker
$process = $null
$native = $null
try {
  $script = Join-Path $PSScriptRoot 'test-device-lifecycle.ps1'
  $process = Start-Process -FilePath $wrapper -ArgumentList ('--test-child "' + $script + '"') -WindowStyle Hidden -PassThru
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  while (-not (Test-Path -LiteralPath $marker) -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
  if (-not (Test-Path -LiteralPath $marker)) { throw 'The real installer did not reach the interrupted login fixture.' }
  $native = Get-Process -Id ([int](Get-Content -LiteralPath $marker -Raw))
  $device = Join-Path $fixture 'local\MirageSSD\device'
  foreach ($file in @('device-install.json','uninstall-device-drive.ps1','device-lifecycle.ps1','setup-state.json')) {
    if (-not (Test-Path -LiteralPath (Join-Path $device $file))) { throw "Recovery file missing before login: $file" }
  }
  $registry = Get-Content -LiteralPath (Join-Path $fixture 'registry.json') -Raw | ConvertFrom-Json
  if ($registry.DisplayName -ne 'MirageSSD' -or -not $registry.UninstallString) { throw 'Uninstall registration was not prepared before login.' }
  Stop-Process -Id $process.Id -Force
  if (-not $process.WaitForExit(5000) -or -not $native.WaitForExit(5000)) { throw 'An installer child survived termination.' }
  # A surviving CLI would lock its executable and prevent this rename.
  $binary = Join-Path $device 'mirage.exe'
  Move-Item -LiteralPath $binary -Destination ($binary + '.probe')
  Move-Item -LiteralPath ($binary + '.probe') -Destination $binary
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $mutex = [Threading.Mutex]::new($false, ('Local\MirageSSD-Device-' + $sid))
  try {
    $held = $false
    try { $held = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $held = $true }
    if (-not $held) { throw 'Interrupted setup retained the operation mutex.' }
    $mutex.ReleaseMutex()
  } finally { $mutex.Dispose() }
  Remove-Item Env:MIRAGE_TEST_PAUSE_LOGIN
  $retry = Start-Process -FilePath $wrapper -ArgumentList ('--test-child "' + $script + '"') -WindowStyle Hidden -Wait -PassThru
  if ($retry.ExitCode -ne 0) { throw 'Retry after force-killing setup failed.' }
  $env:MIRAGE_TEST_FIXTURE_ROOT = Join-Path $root 'cancel-fixture'
  $cancelMarker = Join-Path $root 'cancel-login.txt'
  $env:MIRAGE_TEST_PAUSE_LOGIN = $cancelMarker
  $cancelled = Start-Process -FilePath $wrapper -ArgumentList ('--test-cancel "' + $script + '" "' + $cancelMarker + '"') -WindowStyle Hidden -Wait -PassThru
  if ($cancelled.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $cancelMarker)) { throw 'Explicit cancellation did not stop the actual installer.' }
  $cancelChild = Get-Process -Id ([int](Get-Content -LiteralPath $cancelMarker -Raw)) -ErrorAction SilentlyContinue
  if ($cancelChild) { throw 'Explicit cancellation left a child process alive.' }
  Write-Output 'PASS: real setup wrapper terminated during actual installer login; child exited, executable unlocked, mutex released, uninstall registered, retry and lifecycle suite passed. Authentication/tasks were simulated.'
  Write-Output 'PASS: explicit Cancel operation stops the installer and its native login child.'
} finally {
  if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force }
  if ($native -and -not $native.HasExited) { Stop-Process -Id $native.Id -Force }
  Remove-Item Env:MIRAGE_TEST_FIXTURE_ROOT -ErrorAction SilentlyContinue
  Remove-Item Env:MIRAGE_TEST_PAUSE_LOGIN -ErrorAction SilentlyContinue
}
