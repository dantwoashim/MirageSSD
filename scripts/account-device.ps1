[CmdletBinding()]
param(
  [Parameter(Position = 0)]
  [ValidateSet('Menu', 'SignOut', 'SignIn', 'SwitchAccount')]
  [string]$Action = 'Menu',
  [string]$DriveClientCredentials,
  [switch]$Quiet,
  [switch]$NoConfirm
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Show-Result([string]$Message, [bool]$Failed = $false) {
  if ($Quiet) { Write-Output $Message; return }
  try {
    Add-Type -AssemblyName PresentationFramework -ErrorAction Stop
    $icon = if ($Failed) { 'Error' } else { 'Information' }
    [System.Windows.MessageBox]::Show($Message, 'MirageSSD', 'OK', $icon) | Out-Null
  } catch {
    Write-Host $Message
  }
}

function Show-Choice([string]$Message, [string]$Buttons) {
  try {
    Add-Type -AssemblyName PresentationFramework -ErrorAction Stop
    return [string]([System.Windows.MessageBox]::Show($Message, 'MirageSSD', $Buttons, 'Question'))
  } catch {
    return 'Cancel'
  }
}

function Confirm-Step([string]$Message) {
  if ($NoConfirm) { return $true }
  return (Show-Choice $Message 'YesNo') -eq 'Yes'
}

function Write-Step([string]$Message) {
  if ($Quiet) { Write-Output $Message } else { Write-Host $Message }
}

$installRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\device'
$tokenStore = Join-Path $env:LOCALAPPDATA 'MirageSSD\credentials\drive-token.json'
$stateFile = Join-Path $installRoot 'account-state.json'

. (Join-Path $PSScriptRoot 'device-lifecycle.ps1')

if ($MyInvocation.InvocationName -eq '.') { return }

$operation = $null
$operationHeld = $false
try {
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $operation = [Threading.Mutex]::new($false, ('Local\MirageSSD-Device-' + $sid))
  try { $operationHeld = $operation.WaitOne(0) } catch [Threading.AbandonedMutexException] { $operationHeld = $true }
  if (-not $operationHeld) { throw 'Another MirageSSD setup or account operation is running.' }
  $configurationPath = Join-Path $installRoot 'device-install.json'
  if (-not (Test-Path -LiteralPath $configurationPath -PathType Leaf)) {
    throw 'MirageSSD is not installed for this Windows account.'
  }
  $configuration = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  if ($configuration.format -ne 'miragessd-device-install-v1' -or $configuration.owner_sid -ne $sid -or $configuration.task_name -ne "MirageSSD Drive $sid") {
    throw 'This installation belongs to another Windows account or its configuration is invalid.'
  }
  $mirage = Join-Path $installRoot 'mirage.exe'
  if (-not (Test-Path -LiteralPath $mirage -PathType Leaf)) {
    throw 'The installed MirageSSD program is missing. Reinstall MirageSSD.'
  }
  $mount = $null
  if ($configuration.drive_letter) {
    $mount = [string]$configuration.drive_letter
    if (-not $mount.EndsWith('\')) { $mount += '\' }
  }
  $signedIn = Test-Path -LiteralPath $tokenStore -PathType Leaf

  if ($Action -eq 'Menu') {
    if ($signedIn) {
      $account = Get-SignedInAccount $mirage
      $accountText = if ($account) { " as $account" } else { '' }
      $choice = Show-Choice "MirageSSD is signed in$accountText.`n`nYes - switch to a different Google account`nNo - sign out and disconnect the drive`nCancel - keep everything as it is" 'YesNoCancel'
      if ($choice -eq 'Yes') { $Action = 'SwitchAccount' }
      elseif ($choice -eq 'No') { $Action = 'SignOut' }
      else { return }
    } else {
      if ((Show-Choice 'MirageSSD is signed out. Sign in to Google Drive and reconnect now?' 'YesNo') -ne 'Yes') { return }
      $Action = 'SignIn'
    }
  }

  if ($Action -eq 'SignOut') {
    if (-not $signedIn) {
      Show-Result 'MirageSSD is already signed out.'
      return
    }
    Test-PendingUploads ([string]$configuration.cache_directory)
    if (-not (Confirm-Step 'Close programs using MirageSSD before continuing.`n`nSign out of Google Drive and disconnect the drive? Your cloud files and local cache are kept.')) { return }
    $previousAccount = Get-SignedInAccount $mirage
    $stopped = $false
    try {
      Write-Step 'Disconnecting the MirageSSD drive...'
      Stop-DeviceDrive ([string]$configuration.task_name) $installRoot
      $stopped = $true
      Test-PendingUploads ([string]$configuration.cache_directory)
      Write-AccountState $false $previousAccount
      $logout = Invoke-MirageCommand $mirage @('--json', 'backend', 'logout')
      if ($logout.ExitCode -ne 0) { throw 'Google Drive sign-out did not complete.' }
      Disable-ScheduledTask -TaskName ([string]$configuration.task_name) -ErrorAction SilentlyContinue | Out-Null
    } catch {
      if ($stopped -and (Test-Path -LiteralPath $tokenStore -PathType Leaf)) {
        Mount-DeviceDrive ([string]$configuration.task_name) $mount | Out-Null
      }
      throw
    }
    Write-AccountState $false $previousAccount
    Show-Result 'Signed out of Google Drive. The drive is disconnected and stays off until you sign in again. Your cloud files and local cache were kept.'
    return
  }

  if ($Action -eq 'SwitchAccount' -and -not $signedIn) { $Action = 'SignIn' }
  if ($Action -eq 'SwitchAccount') {
    Test-PendingUploads ([string]$configuration.cache_directory)
    if (-not (Confirm-Step 'Close programs using MirageSSD before continuing.`n`nYou will sign in with a different Google account. The drive reconnects when sign-in finishes. Continue?')) { return }
  }
  if ($Action -ne 'SignIn' -and $Action -ne 'SwitchAccount') { return }

  $credentialFile = $DriveClientCredentials
  if (-not $credentialFile) {
    $credentialFile = Join-Path $PSScriptRoot 'oauth-desktop.json'
  }
  if (-not (Test-Path -LiteralPath $credentialFile -PathType Leaf)) {
    throw 'The Google OAuth application configuration is missing. Run the MirageSSD setup package again.'
  }

  $previousAccount = Get-SignedInAccount $mirage
  if ($Action -eq 'SignIn') {
    Test-PendingUploads ([string]$configuration.cache_directory)
  }
  Write-Step 'Disconnecting the MirageSSD drive...'
  Stop-DeviceDrive ([string]$configuration.task_name) $installRoot
  $pendingToken = Join-Path ([IO.Path]::GetDirectoryName($tokenStore)) ('pending-' + [Guid]::NewGuid().ToString('N') + '.json')
  try {
    Test-PendingUploads ([string]$configuration.cache_directory)
    Write-Step 'Opening Google sign-in in your browser. Choose the account MirageSSD should use.'
    $newAccount = Invoke-DriveLogin $mirage $credentialFile $pendingToken
    if (-not $newAccount) { throw 'Google did not identify the signed-in account.' }
    if (-not $previousAccount) {
      $recorded = Read-AccountState
      if ($recorded) { $previousAccount = $recorded.account_id }
    }
    if (-not $previousAccount -or $newAccount -ne $previousAccount) {
      Clear-VfsCache ([string]$configuration.cache_directory)
    }
    Write-AccountState $true $newAccount
    if (Test-Path -LiteralPath $tokenStore) {
      [IO.File]::Replace($pendingToken, $tokenStore, [NullString]::Value)
    } else {
      [IO.File]::Move($pendingToken, $tokenStore)
    }
  } catch {
    if ($signedIn -and (Test-Path -LiteralPath $tokenStore -PathType Leaf)) {
      Mount-DeviceDrive ([string]$configuration.task_name) $mount | Out-Null
    }
    throw
  } finally {
    if (Test-Path -LiteralPath $pendingToken -PathType Leaf) { Remove-Item -LiteralPath $pendingToken -Force }
  }
  Write-Step 'Reconnecting the MirageSSD drive...'
  $mounted = Mount-DeviceDrive ([string]$configuration.task_name) $mount
  if (-not $mounted) { throw 'Sign-in succeeded, but the drive did not reconnect. Retry setup to repair the installation.' }
  Write-AccountState $true $newAccount
  if ($mounted -and -not $Quiet -and $mount) {
    Start-Process -FilePath "$env:SystemRoot\explorer.exe" -ArgumentList $mount
  }
  $accountText = if ($newAccount) { " as $newAccount" } else { '' }
  $pendingText = if ($mounted) { '' } else { ' The drive is still reconnecting and will appear shortly.' }
  Show-Result "Signed in to Google Drive$accountText.$pendingText"
} catch {
  Show-Result $_.Exception.Message $true
  exit 1
} finally {
  if ($operationHeld) { $operation.ReleaseMutex() }
  if ($operation) { $operation.Dispose() }
}
