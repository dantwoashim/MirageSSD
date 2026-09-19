function Invoke-MirageCommand([string]$Mirage, [string[]]$Arguments) {
  # Native stderr becomes a terminating error under a strict preference, so run
  # each CLI call with an isolated, continuing preference and drop its stream.
  $preference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    $output = @(& $Mirage @Arguments 2>$null)
    $code = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $preference
  }
  return @{ ExitCode = $code; Lines = $output }
}

function Get-SignedInAccount([string]$Mirage) {
  $result = Invoke-MirageCommand $Mirage @('--json', 'backend', 'status')
  if ($result.ExitCode -ne 0) { return $null }
  try {
    $envelope = ($result.Lines -join "`n") | ConvertFrom-Json
    if ($envelope.ok -and $envelope.data.authenticated) { return [string]$envelope.data.account_id }
  } catch { }
  return $null
}

function Read-AccountState {
  if (-not (Test-Path -LiteralPath $stateFile -PathType Leaf)) { return $null }
  try {
    $recorded = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    if ($recorded.format -ne 'miragessd-account-state-v1') { return $null }
    return $recorded
  } catch {
    return $null
  }
}

function Write-AccountState([bool]$SignedIn, [string]$AccountId) {
  $record = @{ format = 'miragessd-account-state-v1'; signed_in = $SignedIn; account_id = $AccountId }
  [IO.File]::WriteAllText($stateFile, ($record | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
}

function Test-PendingUploads([string]$CacheDirectory) {
  if (-not $CacheDirectory -or $CacheDirectory -notmatch '^[A-Za-z]:\\[^\\]') { throw 'Invalid cache directory.' }
  $metadataRoot = Join-Path $CacheDirectory 'vfsMeta'
  if (Test-Path -LiteralPath $metadataRoot) {
    foreach ($file in Get-ChildItem -LiteralPath $metadataRoot -Recurse -File -ErrorAction Stop) {
      $metadata = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
      if ($metadata.Dirty -isnot [bool] -or $metadata.Dirty) {
        throw 'Uploads are still pending, or their status could not be checked. Let the drive finish uploading, then try again. Nothing was changed.'
      }
    }
  }
}

function Stop-DeviceDrive([string]$TaskName, [string]$InstallRoot) {
  $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($task) { Disable-ScheduledTask -TaskName $TaskName -ErrorAction Stop | Out-Null }
  if ($task -and $task.State -eq 'Running') {
    Stop-ScheduledTask -TaskName $TaskName
  }
  $binaries = @((Join-Path $InstallRoot 'mirage.exe'), (Join-Path $InstallRoot 'rclone.exe'))
  Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -in $binaries } | ForEach-Object {
    Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
  }
  $launcher = Join-Path $InstallRoot 'mount-device.vbs'
  Get-CimInstance Win32_Process -Filter "Name='wscript.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($launcher) } | ForEach-Object {
    Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
  }
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    $remaining = @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -in $binaries -or ($_.Name -eq 'wscript.exe' -and $_.CommandLine -and $_.CommandLine.Contains($launcher)) })
    if ($remaining.Count -eq 0) { return }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  throw 'The drive is still busy. Close programs using it and retry.'
}

function Mount-DeviceDrive([string]$TaskName, [string]$Mount) {
  Enable-ScheduledTask -TaskName $TaskName -ErrorAction Stop | Out-Null
  Start-ScheduledTask -TaskName $TaskName -ErrorAction Stop
  if (-not $Mount) { return $false }
  $deadline = [DateTime]::UtcNow.AddSeconds(90)
  while (-not (Test-Path -LiteralPath $Mount) -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 500
  }
  return (Test-Path -LiteralPath $Mount)
}

function Clear-VfsCache([string]$CacheDirectory) {
  if (-not $CacheDirectory -or $CacheDirectory -notmatch '^[A-Za-z]:\\[^\\]') {
    throw 'The recorded cache directory is unsafe to clear.'
  }
  $root = [IO.Path]::GetFullPath($CacheDirectory).TrimEnd('\')
  if ($root -eq [IO.Path]::GetPathRoot($root).TrimEnd('\')) { throw 'A volume root cannot be a cache.' }
  $current = $root
  while ($current) {
    if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Cache paths cannot traverse links.' }
    $current = [IO.Path]::GetDirectoryName($current)
  }
  Test-PendingUploads $root
  $archive = Join-Path $root ('retained-account-' + [Guid]::NewGuid().ToString('N'))
  foreach ($name in @('vfs', 'vfsTmp', 'vfsMeta')) {
    $target = [IO.Path]::GetFullPath((Join-Path $root $name))
    if (-not $target.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe cache path.' }
    if (Test-Path -LiteralPath $target) {
      if ((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cache contains a link.' }
      New-Item -ItemType Directory -Path $archive -Force | Out-Null
      Move-Item -LiteralPath $target -Destination (Join-Path $archive $name)
    }
  }
  # These records are keyed by remote path, so they must move with the old
  # account's cache rather than affecting same-named files in the new account.
  if ($installRoot -and (Test-Path -LiteralPath $installRoot)) {
    $devicePrefix = [IO.Path]::GetFullPath($installRoot).TrimEnd('\') + '\'
    foreach ($file in Get-ChildItem -LiteralPath $installRoot -Filter 'windows-attributes-*.json' -File) {
      if (-not $file.FullName.StartsWith($devicePrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe attribute path.' }
      New-Item -ItemType Directory -Path $archive -Force | Out-Null
      Move-Item -LiteralPath $file.FullName -Destination (Join-Path $archive $file.Name)
    }
  }
}

function Invoke-DriveLogin([string]$Mirage, [string]$CredentialsFile, [string]$PendingToken) {
  $login = Invoke-MirageCommand $Mirage @('--json', 'backend', 'login', '--client-credentials', $CredentialsFile, '--timeout-seconds', '600', '--token-store', $PendingToken)
  if ($login.ExitCode -ne 0) {
    throw 'Google sign-in did not complete. The browser window may have been closed or the request timed out.'
  }
  $envelope = ($login.Lines -join "`n") | ConvertFrom-Json
  if (-not $envelope.ok -or -not $envelope.data.authenticated) {
    throw 'Google sign-in did not complete.'
  }
  $authorize = Invoke-MirageCommand $Mirage @('--json', 'backend', 'authorize-device', '--drive-client-credentials', $CredentialsFile, '--token-store', $PendingToken)
  if ($authorize.ExitCode -ne 0) {
    throw 'The new sign-in could not be protected for automatic reconnect.'
  }
  return [string]$envelope.data.account_id
}
