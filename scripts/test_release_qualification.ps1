# Release-qualification harness: runs the fault matrix and the supporting
# durability suites. Read-only — no remote calls, no mounted volume.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $root
try {
    cargo test -p mirage-engine --test release_qualification --locked
    if ($LASTEXITCODE -ne 0) { throw "fault matrix failed" }

    cargo test -p mirage-db --test migrations --locked
    if ($LASTEXITCODE -ne 0) { throw "migration suite failed" }

    cargo test -p mirage-db --test namespace --test physical --locked
    if ($LASTEXITCODE -ne 0) { throw "durability suites failed" }

    Write-Output "release qualification: PASS"
} finally {
    Pop-Location
}
