function Get-OrphanedSetupProcesses([object[]]$Processes, [string]$TempRoot) {
  $prefix = [IO.Path]::GetFullPath($TempRoot).TrimEnd('\') + '\'
  foreach ($process in $Processes) {
    if ($process.Name -ne 'powershell.exe' -or -not $process.CommandLine) { continue }
    $match = [regex]::Match($process.CommandLine, '(?i)-File\s+"([^"\r\n]+\\setup-miragessd\.ps1)"')
    if (-not $match.Success) { continue }
    $script = [IO.Path]::GetFullPath($match.Groups[1].Value)
    if (-not $script.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { continue }
    $relative = $script.Substring($prefix.Length)
    if ($relative -notmatch '^MirageSSD-Setup-[a-fA-F0-9]{32}\\MirageSSD-OneClick-\d{8}-\d{6}\\setup-miragessd\.ps1$') { continue }
    $parent = $Processes | Where-Object { $_.ProcessId -eq $process.ParentProcessId -and $_.CreationDate -le $process.CreationDate } | Select-Object -First 1
    if (-not $parent) { $process }
  }
}

function Stop-OrphanedSetup([string]$TempRoot = [IO.Path]::GetTempPath()) {
  $snapshot = @(Get-CimInstance Win32_Process)
  $orphans = @(Get-OrphanedSetupProcesses $snapshot $TempRoot)
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  foreach ($orphan in $orphans) {
    $owner = Invoke-CimMethod -InputObject $orphan -MethodName GetOwnerSid -ErrorAction Stop
    if ($owner.Sid -ne $sid) { continue }
    $children = @($snapshot | Where-Object { $_.ParentProcessId -eq $orphan.ProcessId -and $_.CreationDate -ge $orphan.CreationDate -and $_.Name -eq 'mirage.exe' })
    # Recheck creation time to avoid terminating a process that reused the PID.
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($orphan.ProcessId)"
    if (-not $current -or $current.CreationDate -ne $orphan.CreationDate) { continue }
    Stop-Process -Id $orphan.ProcessId -Force -ErrorAction Stop
    foreach ($child in $children) {
      $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($child.ProcessId)"
      if ($current -and $current.CreationDate -eq $child.CreationDate) { Stop-Process -Id $child.ProcessId -Force -ErrorAction Stop }
    }
  }
}

function Write-DeviceJson([string]$Path, $Value) {
  $temporary = $Path + '.new-' + [Guid]::NewGuid().ToString('N')
  try {
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    if (Test-Path -LiteralPath $Path) { [IO.File]::Replace($temporary, $Path, [NullString]::Value) }
    else { [IO.File]::Move($temporary, $Path) }
  } finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
  }
}

function Register-DeviceRecovery([string]$Root, [string]$Cache, [string]$Mount, [string]$Task, [string]$Source) {
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  foreach ($name in @('device-lifecycle.ps1', 'uninstall-device-drive.ps1', 'run-powershell-hidden.vbs')) {
    $from = [IO.Path]::GetFullPath((Join-Path $Source $name))
    $to = [IO.Path]::GetFullPath((Join-Path $Root $name))
    if ($from -ne $to) { Copy-Item -LiteralPath $from -Destination $to -Force }
  }
  Write-DeviceJson (Join-Path $Root 'device-install.json') @{ format='miragessd-device-install-v1'; owner_sid=$sid; task_name=$Task; cache_directory=$Cache; drive_letter=$Mount }
  $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MirageSSDWritableDevice'
  $shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
  $uninstall = Join-Path $Root 'uninstall-device-drive.ps1'
  New-Item -Path $key -Force | Out-Null
  $values = @{ DisplayName='MirageSSD'; DisplayVersion='0.1.4'; Publisher='MirageSSD'; InstallLocation=$Root; UninstallString=('"' + $shell + '" -NoProfile -ExecutionPolicy Bypass -File "' + $uninstall + '"') }
  foreach ($entry in $values.GetEnumerator()) { New-ItemProperty -Path $key -Name $entry.Key -Value $entry.Value -PropertyType String -Force | Out-Null }
}

function Get-DeviceInstallRecord([string]$Root) {
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $path = Join-Path $Root 'device-install.json'
  if (Test-Path -LiteralPath $path -PathType Leaf) {
    $record = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if ($record.format -ne 'miragessd-device-install-v1' -or $record.owner_sid -ne $sid -or $record.task_name -ne "MirageSSD Drive $sid") { throw 'Invalid installation ownership.' }
    return $record
  }
  # Older/aborted setup can have a launcher and binaries but no manifest.
  # Recover only local path settings; never execute launcher text or read tokens.
  $launcher = Join-Path $Root 'mount-device.vbs'
  if (Test-Path -LiteralPath $launcher -PathType Leaf) {
    $text = (Get-Content -LiteralPath $launcher -Raw).Replace('""', '"')
    $cacheMatch = [regex]::Match($text, '--cache-dir "([A-Za-z]:\\[^"\r\n]+)"')
    $driveMatch = [regex]::Match($text, '--drive-letter ([D-Z])\b')
    if (-not $cacheMatch.Success -or -not $driveMatch.Success) { throw 'The interrupted installation has an unrecognized launcher. No files were removed.' }
    return [pscustomobject]@{ format='miragessd-device-install-v1'; owner_sid=$sid; task_name="MirageSSD Drive $sid"; cache_directory=$cacheMatch.Groups[1].Value; drive_letter=($driveMatch.Groups[1].Value + ':\') }
  }
  return $null
}

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
  Write-DeviceJson $stateFile $record
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

function Get-DeviceDriveTasks([string]$TaskName, [string]$InstallRoot) {
  $primary = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($primary) { $primary }
  if ($TaskName -eq 'MirageSSD Drive') { return }
  $legacy = Get-ScheduledTask -TaskName 'MirageSSD Drive' -ErrorAction SilentlyContinue
  if ($legacy -and $legacy.TaskName -eq 'MirageSSD Drive') {
    $launcher = '"' + (Join-Path $InstallRoot 'mount-device.vbs') + '"'
    $matches = @($legacy.Actions | Where-Object { $_.Arguments -and $_.Arguments.IndexOf($launcher, [StringComparison]::OrdinalIgnoreCase) -ge 0 })
    if ($matches.Count) {
      $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
      $owner = [string]$legacy.Principal.UserId
      if ($owner -notmatch '^S-1-') { $owner = ([Security.Principal.NTAccount]::new($owner)).Translate([Security.Principal.SecurityIdentifier]).Value }
      if ($owner -eq $currentSid) { $legacy }
    }
  }
}

function Stop-DeviceDrive([string]$TaskName, [string]$InstallRoot) {
  foreach ($task in @(Get-DeviceDriveTasks $TaskName $InstallRoot)) {
    Disable-ScheduledTask -TaskName $task.TaskName -ErrorAction Stop | Out-Null
    if ($task.State -eq 'Running') { Stop-ScheduledTask -TaskName $task.TaskName }
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
