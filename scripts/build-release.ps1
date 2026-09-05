[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Version,
  [string]$Output = "$PSScriptRoot\..\dist",
  [string]$WinFspSdkRoot = $env:WINFSP_SDK_ROOT
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'Release version must be an MSI-compatible major.minor.patch value.' }
if ((git -C $repo status --porcelain)) { throw 'Release builds require a clean worktree.' }
$tag = git -C $repo describe --tags --exact-match HEAD 2>$null
if ($LASTEXITCODE -ne 0 -or $tag -ne "v$Version") { throw "HEAD must be tagged v$Version" }
$sourceDateEpoch = git -C $repo show -s --format=%ct HEAD
if ($LASTEXITCODE -ne 0 -or $sourceDateEpoch -notmatch '^\d+$') { throw 'Unable to resolve the source commit timestamp.' }
$sourceDateEpoch = [long]$sourceDateEpoch
$sourceDate = [DateTimeOffset]::FromUnixTimeSeconds($sourceDateEpoch).UtcDateTime
$workspaceMetadata = cargo metadata --manifest-path (Join-Path $repo 'Cargo.toml') --locked --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Locked workspace metadata failed.' }
$workspaceIds = [Collections.Generic.HashSet[string]]::new([string[]]$workspaceMetadata.workspace_members)
$workspaceVersions = @($workspaceMetadata.packages | Where-Object { $workspaceIds.Contains([string]$_.id) } | Select-Object -ExpandProperty version -Unique)
if ($workspaceVersions.Count -ne 1 -or [string]$workspaceVersions[0] -ne $Version) {
  throw "Workspace package version must exactly match release version $Version."
}
if (Test-Path -LiteralPath $Output) {
  if (Get-ChildItem -LiteralPath $Output -Force | Select-Object -First 1) {
    throw "Release output must be empty: $Output"
  }
} else {
  New-Item -ItemType Directory -Force -Path $Output | Out-Null
}

$ui = Join-Path $repo 'apps\mirage-ui'
Push-Location $ui
try {
  npm ci
  if ($LASTEXITCODE -ne 0) { throw 'Locked UI dependency install failed.' }
  npm test
  if ($LASTEXITCODE -ne 0) { throw 'UI tests failed.' }
  npm run build
  if ($LASTEXITCODE -ne 0) { throw 'UI build failed.' }
}
finally {
  Pop-Location
}

cargo build --manifest-path (Join-Path $repo 'Cargo.toml') --workspace --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Locked release build failed.' }
if (-not $WinFspSdkRoot) { $WinFspSdkRoot = Join-Path $repo 'target\winfsp-sdk' }
if (-not (Test-Path -LiteralPath (Join-Path $WinFspSdkRoot 'inc\winfsp\winfsp.h') -PathType Leaf)) {
  & (Join-Path $PSScriptRoot 'acquire-winfsp-sdk.ps1') -Output $WinFspSdkRoot
}
if (-not (Test-Path -LiteralPath (Join-Path $WinFspSdkRoot 'inc\winfsp\winfsp.h') -PathType Leaf)) { throw 'Pinned WinFsp SDK acquisition failed.' }
if (-not (Test-Path -LiteralPath (Join-Path $WinFspSdkRoot 'License.txt') -PathType Leaf)) { throw 'Pinned WinFsp license is missing.' }
$nativeBuild = Join-Path $repo 'target\native-winfsp-release'
$cmake = Get-Command cmake -ErrorAction Stop
& $cmake.Source -S (Join-Path $repo 'native\winfsp-adapter') -B $nativeBuild -A x64 "-DWINFSP_SDK_ROOT=$WinFspSdkRoot" '-DMIRAGE_RUST_PROFILE=release'
if ($LASTEXITCODE -ne 0) { throw 'Native filesystem host configure failed.' }
& $cmake.Source --build $nativeBuild --config Release
if ($LASTEXITCODE -ne 0) { throw 'Native filesystem host build failed.' }
$artifacts = @('mirage.exe', 'mirage-service.exe', 'mirage-ui.exe')
foreach ($name in $artifacts) {
  $source = Join-Path $repo "target\release\$name"
  if (-not (Test-Path -LiteralPath $source)) { throw "Missing release artifact $name" }
  Copy-Item -LiteralPath $source -Destination $Output -Force
}
$nativeHost = Join-Path $nativeBuild 'Release\mirage-fs.exe'
if (-not (Test-Path -LiteralPath $nativeHost -PathType Leaf)) { throw 'Missing release artifact mirage-fs.exe' }
Copy-Item -LiteralPath $nativeHost -Destination $Output -Force
Copy-Item -LiteralPath (Join-Path $ui 'dist') -Destination (Join-Path $Output 'ui') -Recurse -Force
Get-ChildItem -LiteralPath $Output -Recurse -File | ForEach-Object { $_.LastWriteTimeUtc = $sourceDate }

$symbolStage = Join-Path $repo "target\release-symbols-$Version"
if (Test-Path -LiteralPath $symbolStage) { throw "Symbol staging path already exists: $symbolStage" }
New-Item -ItemType Directory -Path $symbolStage | Out-Null
$symbols = @(
  (Join-Path $repo 'target\release\mirage.pdb'),
  (Join-Path $repo 'target\release\mirage_service.pdb'),
  (Join-Path $repo 'target\release\mirage_ui.pdb'),
  (Join-Path $nativeBuild 'Release\mirage-fs.pdb')
)
foreach ($symbol in $symbols) {
  if (-not (Test-Path -LiteralPath $symbol -PathType Leaf)) { throw "Missing release symbol: $symbol" }
  $copied = Copy-Item -LiteralPath $symbol -Destination $symbolStage -Force -PassThru
  $copied.LastWriteTimeUtc = $sourceDate
}
Compress-Archive -Path (Join-Path $symbolStage '*') -DestinationPath (Join-Path $Output "miragessd-symbols-$Version.zip") -CompressionLevel Optimal

$installerStage = Join-Path $repo "target\installer-$Version"
if (Test-Path -LiteralPath $installerStage) { throw "Installer staging path already exists: $installerStage" }
& (Join-Path $repo 'installer\build.ps1') -Output $installerStage -Version $Version -SourceDateEpoch $sourceDateEpoch -BinDir $Output -UiDir (Join-Path $Output 'ui')
$installerVerificationStage = Join-Path $repo "target\installer-$Version-repro"
if (Test-Path -LiteralPath $installerVerificationStage) { throw "Installer reproducibility staging path already exists: $installerVerificationStage" }
& (Join-Path $repo 'installer\build.ps1') -Output $installerVerificationStage -Version $Version -SourceDateEpoch $sourceDateEpoch -BinDir $Output -UiDir (Join-Path $Output 'ui')
$installerArtifact = Join-Path $installerStage 'MirageSSD.msi'
$installerVerificationArtifact = Join-Path $installerVerificationStage 'MirageSSD.msi'
if ((Get-FileHash -LiteralPath $installerArtifact -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $installerVerificationArtifact -Algorithm SHA256).Hash) {
  throw 'MSI rebuild was not byte-for-byte reproducible.'
}
Copy-Item -LiteralPath $installerArtifact -Destination $Output -Force
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $Output 'LICENSE.txt') -Force
$licenses = Join-Path $Output 'licenses'
New-Item -ItemType Directory -Path $licenses | Out-Null
$winFspLicense = Join-Path $licenses 'WinFsp-LICENSE.txt'
Copy-Item -LiteralPath (Join-Path $WinFspSdkRoot 'License.txt') -Destination $winFspLicense -Force
& (Join-Path $PSScriptRoot 'generate-sbom.ps1') -Output (Join-Path $Output 'sbom.cdx.json') -Version $Version
& (Join-Path $PSScriptRoot 'generate-notices.ps1') -Output (Join-Path $Output 'THIRD_PARTY_NOTICES.txt') -WinFspLicensePath $winFspLicense
$commit = git -C $repo rev-parse HEAD
Set-Content -LiteralPath (Join-Path $Output 'SOURCE_COMMIT') -Value $commit -Encoding ascii
$contractFiles = @(
  Get-ChildItem -LiteralPath (Join-Path $repo 'migrations') -File
  Get-ChildItem -LiteralPath (Join-Path $repo 'schemas') -File
) | Sort-Object FullName
$contracts = @($contractFiles | ForEach-Object {
  [ordered]@{
    path = [IO.Path]::GetRelativePath($repo, $_.FullName).Replace('\', '/')
    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
  }
})
[ordered]@{
  format = 'miragessd-release-metadata-v1'
  product_version = $Version
  source_commit = $commit
  source_date_epoch = $sourceDateEpoch
  rustc = (& rustc --version).Trim()
  cargo = (& cargo --version).Trim()
  node = (& node --version).Trim()
  npm = (& npm --version).Trim()
  wix = (& wix --version).Trim()
  cmake = (& $cmake.Source --version | Select-Object -First 1).Trim()
  winfsp = '2.1.25156'
  windows_minimum_build = 22000
  signing_status = 'unsigned-development-artifact'
  persistent_contracts = $contracts
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $Output 'RELEASE_METADATA.json') -Encoding utf8NoBOM
Get-ChildItem -LiteralPath $Output -Recurse -File | ForEach-Object { $_.LastWriteTimeUtc = $sourceDate }
Get-ChildItem -LiteralPath $Output -Recurse -File | Where-Object Name -ne 'SHA256SUMS' | Get-FileHash -Algorithm SHA256 | Sort-Object Path | ForEach-Object {
  $relative = [IO.Path]::GetRelativePath($Output, $_.Path).Replace('\', '/')
  "$($_.Hash.ToLowerInvariant())  $relative"
} | Set-Content -LiteralPath (Join-Path $Output 'SHA256SUMS') -Encoding ascii
