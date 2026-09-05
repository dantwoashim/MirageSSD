[CmdletBinding()]
param([Parameter(Mandatory)][string[]]$Paths, [Parameter(Mandatory)][string]$CertificateThumbprint, [Parameter(Mandatory)][string]$TimestampUrl)
$ErrorActionPreference = 'Stop'
$signtool = Get-Command signtool.exe -ErrorAction Stop
foreach ($path in $Paths) {
  $resolved = (Resolve-Path -LiteralPath $path).Path
  & $signtool.Source sign /sha1 $CertificateThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $resolved
  if ($LASTEXITCODE -ne 0) { throw "Signing failed: $resolved" }
  & $signtool.Source verify /pa /all $resolved
  if ($LASTEXITCODE -ne 0) { throw "Signature verification failed: $resolved" }
}
