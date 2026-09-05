[CmdletBinding()]
param(
  [string]$ClientId,
  [Parameter(Mandatory)]
  [string]$DriveClientCredentials,
  [Parameter(Mandatory)]
  [string]$MirageExecutable,
  [Parameter(Mandatory)]
  [string]$RcloneExecutable,
  [string]$OutputRoot = (Join-Path $env:USERPROFILE 'Downloads\MirageSSD-Packages')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$pinnedWinFspVersion = '2.1.25156'
$pinnedWinFspHash = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'
$pinnedWinFspUri = "https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-$pinnedWinFspVersion.msi"

function Resolve-RequiredFile([string]$Path, [string]$Label) {
  $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
  if (-not (Test-Path -LiteralPath $resolved.Path -PathType Leaf)) { throw "$Label is not a file." }
  $resolved.Path
}

$clientSource = Resolve-RequiredFile $DriveClientCredentials 'Desktop OAuth application configuration'
$clientDocument = Get-Content -LiteralPath $clientSource -Raw | ConvertFrom-Json
$desktopClient = $clientDocument.installed
if (-not $desktopClient -or -not $desktopClient.client_secret -or $desktopClient.client_secret.Length -gt 512) {
  throw 'A valid installed/Desktop OAuth application configuration is required.'
}
if (-not $ClientId) { $ClientId = $desktopClient.client_id }
$ClientId = $ClientId.Trim()
if ($ClientId -notmatch '^[^\s\x00-\x1f]{1,480}\.apps\.googleusercontent\.com$') {
  throw 'Google OAuth desktop client ID is invalid.'
}
if ($ClientId -ne $desktopClient.client_id) { throw 'The Desktop OAuth configuration and client ID do not match.' }
$mirage = Resolve-RequiredFile $MirageExecutable 'MirageSSD executable'
$rclone = Resolve-RequiredFile $RcloneExecutable 'Rclone executable'
$providerOutput = @(& $rclone version 2>&1)
$providerExitCode = $LASTEXITCODE
$providerVersion = [string]$providerOutput[0]
if ($providerExitCode -ne 0 -or $providerVersion -notmatch 'miragessd') {
  throw 'Rclone is not the required MirageSSD-patched provider.'
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$outputBase = [IO.Path]::GetFullPath($OutputRoot)
$repoPrefix = $repo.TrimEnd('\') + '\'
if ($outputBase.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw 'Personalized setup bundles must be written outside the source repository.'
}
$stamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
$bundle = Join-Path $outputBase "MirageSSD-OneClick-$stamp"
if (Test-Path -LiteralPath $bundle) { throw "Output already exists: $bundle" }
$payload = Join-Path $bundle 'payload'
$prerequisites = Join-Path $bundle 'prerequisites'
New-Item -ItemType Directory -Path $payload, $prerequisites -Force | Out-Null

Copy-Item -LiteralPath $mirage -Destination (Join-Path $payload 'mirage.exe')
Copy-Item -LiteralPath $rclone -Destination (Join-Path $payload 'rclone.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'setup-miragessd.ps1') -Destination $bundle
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-device-drive.ps1') -Destination $bundle
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'uninstall-device-drive.ps1') -Destination $bundle
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'run-powershell-hidden.vbs') -Destination $bundle
[IO.File]::WriteAllText((Join-Path $bundle 'client-id.txt'), $ClientId, [Text.UTF8Encoding]::new($false))
# Installed desktop apps are public OAuth clients. Package only their application
# registration, never the user's access/refresh tokens or DPAPI account record.
$applicationConfiguration = @{installed=@{
  client_id=$ClientId;client_secret=$desktopClient.client_secret;project_id=$desktopClient.project_id
  auth_uri='https://accounts.google.com/o/oauth2/auth';token_uri='https://oauth2.googleapis.com/token'
  auth_provider_x509_cert_url='https://www.googleapis.com/oauth2/v1/certs';redirect_uris=@('http://localhost')
}}
[IO.File]::WriteAllText((Join-Path $bundle 'oauth-desktop.json'), ($applicationConfiguration | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))

$downloadRoot = Join-Path $env:LOCALAPPDATA 'MirageSSD\downloads'
$cachedWinFsp = Join-Path $downloadRoot "winfsp-$pinnedWinFspVersion.msi"
New-Item -ItemType Directory -Path $downloadRoot -Force | Out-Null
if (-not (Test-Path -LiteralPath $cachedWinFsp -PathType Leaf) -or (Get-FileHash -LiteralPath $cachedWinFsp -Algorithm SHA256).Hash -ne $pinnedWinFspHash) {
  Invoke-WebRequest -Uri $pinnedWinFspUri -OutFile $cachedWinFsp
}
if ((Get-FileHash -LiteralPath $cachedWinFsp -Algorithm SHA256).Hash -ne $pinnedWinFspHash) {
  throw 'Pinned WinFsp download failed its SHA-256 check.'
}
Copy-Item -LiteralPath $cachedWinFsp -Destination (Join-Path $prerequisites "winfsp-$pinnedWinFspVersion.msi")

$licenses = Join-Path $bundle 'licenses'
New-Item -ItemType Directory -Path $licenses | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $licenses 'MirageSSD-LICENSE.txt')
$winFspLicense = Join-Path ${env:ProgramFiles(x86)} 'WinFsp\License.txt'
if (-not (Test-Path -LiteralPath $winFspLicense)) { throw 'The pinned WinFsp license must be present before packaging.' }
Copy-Item -LiteralPath $winFspLicense -Destination (Join-Path $licenses 'WinFsp-LICENSE.txt')
Invoke-WebRequest -Uri 'https://raw.githubusercontent.com/rclone/rclone/9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048/COPYING' -OutFile (Join-Path $licenses 'rclone-LICENSE.txt')
$providerSource = Join-Path $bundle 'provider-source'
$providerPatch = Join-Path $providerSource 'third_party\rclone-miragessd'
$providerScripts = Join-Path $providerSource 'scripts'
New-Item -ItemType Directory -Path $providerPatch, $providerScripts -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'third_party\rclone-miragessd\rclone-v1.75.0.patch') -Destination $providerPatch
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'build-rclone-miragessd.ps1') -Destination $providerScripts
$cachedProviderSource = Join-Path $downloadRoot 'rclone-source-9ee9d0a0.tar.gz'
if (-not (Test-Path -LiteralPath $cachedProviderSource)) {
  Invoke-WebRequest -Uri 'https://codeload.github.com/rclone/rclone/tar.gz/9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048' -OutFile $cachedProviderSource
}
Copy-Item -LiteralPath $cachedProviderSource -Destination (Join-Path $providerSource 'rclone-upstream-source.tar.gz')

$launcher = @'
@echo off
setlocal
set "PSModulePath=%SystemRoot%\System32\WindowsPowerShell\v1.0\Modules"
title MirageSSD Setup
echo MirageSSD will open Google sign-in once, then mount itself automatically.
echo.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup-miragessd.ps1"
set "MIRAGE_EXIT=%ERRORLEVEL%"
if not "%MIRAGE_EXIT%"=="0" (
  echo.
  echo Setup did not finish. The error above explains what needs attention.
  pause
)
exit /b %MIRAGE_EXIT%
'@
[IO.File]::WriteAllText((Join-Path $bundle 'START MIRAGESSD.cmd'), $launcher, [Text.ASCIIEncoding]::new())

$readme = @'
MIRAGESSD - WINDOWS 11 X64 FRIEND PREVIEW

1. Extract this entire folder.
2. Double-click START MIRAGESSD.cmd.
3. Accept the Windows administrator prompt if WinFsp needs installation.
4. Sign in to Google and approve the requested Drive file access.

MirageSSD then appears in This PC and reconnects automatically at Windows sign-in.
If M: is occupied, setup chooses another free drive letter. The supervisor retries
within one minute if it stops. Setup detects your own Google Drive capacity.

Requirements: Windows 11 x64, a local NTFS disk with at least 12 GiB free,
internet, and administrator approval for the signed WinFsp prerequisite.
Google sign-in runs as your ordinary Windows user, not as administrator.

This preview is unsigned. Windows may warn about or block it. Do not disable
Windows security. Only use the installer supplied by the developer.
If Google reports "access_denied" while the app is in Testing, its developer
must add your Google email under Google Auth Platform > Audience > Test users.

The included OAuth Desktop configuration identifies the app; it does not contain
the developer's Google login, personal access/refresh tokens, or Drive files.
Your files transfer directly to your Google Drive. Your quota remains your own.

Cached writes finish locally before their cloud upload completes. Do not remove
originals until cloud verification is complete. Uncached reads use the internet.
Do not use this preview as the only copy of irreplaceable data or run games on it.
Test with a disposable folder: copy, wait for upload, reopen, sign out of Windows
and sign back in, then confirm the drive returns and files remain readable.

Remove it through Windows Settings > Apps > Installed apps > MirageSSD.
Uninstall refuses known pending uploads and retains cloud files, cached data,
credentials and Windows-attribute metadata. It does not remove shared WinFsp.
The package includes upstream licenses and the patched provider's source patch.
'@
[IO.File]::WriteAllText((Join-Path $bundle 'README FIRST.txt'), $readme, [Text.UTF8Encoding]::new($false))

$manifest = Get-ChildItem -LiteralPath $bundle -Recurse -File | Sort-Object FullName | ForEach-Object {
  $relative = $_.FullName.Substring($bundle.Length + 1).Replace('\', '/')
  "{0} *{1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
}
[IO.File]::WriteAllLines((Join-Path $bundle 'SHA256SUMS'), $manifest, [Text.UTF8Encoding]::new($false))

$zip = "$bundle.zip"
Compress-Archive -LiteralPath $bundle -DestinationPath $zip -CompressionLevel Optimal
$setupExecutable = Join-Path $outputBase "MirageSSD-Setup-$stamp.exe"
$compiler = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path -LiteralPath $compiler)) { throw 'The Windows .NET Framework compiler is required for the single-file setup wrapper.' }
& $compiler /nologo /target:winexe /platform:x64 /optimize+ /reference:System.dll /reference:System.Core.dll /reference:System.Windows.Forms.dll /reference:System.Drawing.dll /reference:System.IO.Compression.dll /reference:System.IO.Compression.FileSystem.dll "/win32manifest:$(Join-Path $PSScriptRoot 'friend-setup.manifest')" "/resource:$zip,MirageSSD.Payload.zip" "/out:$setupExecutable" (Join-Path $PSScriptRoot 'friend-setup.cs')
if ($LASTEXITCODE -ne 0) { throw 'The single-file Windows installer could not be built.' }
[pscustomobject]@{
  Bundle = $bundle
  Zip = $zip
  Installer = $setupExecutable
  InstallerSha256 = (Get-FileHash -LiteralPath $setupExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
  Provider = $providerVersion
  Files = (Get-ChildItem -LiteralPath $bundle -Recurse -File).Count
  Sha256 = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
} | ConvertTo-Json -Compress
