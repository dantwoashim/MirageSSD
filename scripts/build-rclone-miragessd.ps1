[CmdletBinding()]
param(
  # miragessd3 is the current provider: rclone v1.75.1 with the bound,
  # roll-back-safe attribute journal. miragessd2 is the preserved shipping
  # baseline on rclone v1.75.0; keep it reproducible for comparison runs.
  [ValidateSet('miragessd2', 'miragessd3')]
  [string]$Variant = 'miragessd3',
  [string]$SourceDirectory = '',
  [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
$variants = @{
  miragessd2 = @{
    Tag    = 'v1.75.0'
    Commit = '9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048'
    Patch  = 'rclone-v1.75.0.patch'
    Suffix = 'miragessd2'
  }
  miragessd3 = @{
    Tag    = 'v1.75.1'
    Commit = '687d264b689b8c49a67e2e52a8a5e0caa01c04ce'
    Patch  = 'rclone-v1.75.1.patch'
    Suffix = 'miragessd3'
  }
}
$selected = $variants[$Variant]
$upstream = 'https://github.com/rclone/rclone.git'
$tag = $selected.Tag
$commit = $selected.Commit
$patch = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot ('..\third_party\rclone-miragessd\' + $selected.Patch))).Path
if (-not $SourceDirectory) {
  $SourceDirectory = Join-Path $PSScriptRoot ('..\target\rclone-miragessd-source-' + $tag)
}
if (-not $OutputDirectory) {
  $OutputDirectory = Join-Path $PSScriptRoot '..\target\rclone-miragessd'
}
$source = [IO.Path]::GetFullPath($SourceDirectory)
$output = [IO.Path]::GetFullPath($OutputDirectory)

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
  throw 'Git is required to build the pinned MirageSSD rclone provider.'
}
if (-not (Get-Command go -ErrorAction SilentlyContinue)) {
  throw 'Go is required to build the pinned MirageSSD rclone provider.'
}
if (-not (Test-Path -LiteralPath (Join-Path $source '.git'))) {
  git clone --filter=blob:none --branch $tag $upstream $source
  if ($LASTEXITCODE -ne 0) {
    throw 'Failed to clone the pinned rclone source.'
  }
}

$actualCommit = (git -C $source rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $commit) {
  throw "Rclone source must be the pinned $tag commit $commit; found $actualCommit."
}

$dirty = @(git -C $source status --porcelain)
if ($dirty.Count -eq 0) {
  git -C $source apply --check $patch
  if ($LASTEXITCODE -ne 0) {
    throw 'MirageSSD rclone patch does not apply to the pinned source.'
  }
  git -C $source apply $patch
  if ($LASTEXITCODE -ne 0) {
    throw 'Failed to apply the MirageSSD rclone patch.'
  }
} else {
  git -C $source apply --reverse --check $patch 2>$null
  if ($LASTEXITCODE -ne 0) {
    throw 'Rclone source contains changes other than the exact MirageSSD patch.'
  }
}

New-Item -ItemType Directory -Path $output -Force | Out-Null
$binary = Join-Path $output 'rclone.exe'
$previousCgo = $env:CGO_ENABLED
try {
  $env:CGO_ENABLED = '0'
  go -C $source test -tags cmount ./cmd/cmount
  if ($LASTEXITCODE -ne 0) {
    throw 'Patched rclone cmount tests failed.'
  }
  go -C $source build `
    -trimpath `
    -ldflags ('-s -w -X github.com/rclone/rclone/fs.VersionSuffix=' + $selected.Suffix) `
    -tags cmount `
    -o $binary `
    .
  if ($LASTEXITCODE -ne 0) {
    throw 'Patched rclone build failed.'
  }
} finally {
  $env:CGO_ENABLED = $previousCgo
}

[pscustomobject]@{
  Built = $true
  Variant = $Variant
  UpstreamTag = $tag
  UpstreamCommit = $commit
  PatchSha256 = (Get-FileHash -LiteralPath $patch -Algorithm SHA256).Hash.ToLowerInvariant()
  Binary = $binary
  Sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
  Version = (& $binary version)[0]
  Linkage = 'static'
  BuildTag = 'cmount'
  BuildFlags = '-trimpath -tags cmount CGO_ENABLED=0'
} | ConvertTo-Json -Compress
