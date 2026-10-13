# Native Mirage publisher-source mount. Local config and manifests must be
# prepared and validated before changing any game's asset junctions.
param(
    [Parameter(Mandatory)][string]$HostExe,
    [Parameter(Mandatory)][string]$Config,
    [Parameter(Mandatory)][string]$Mount,
    [Parameter(Mandatory)][string]$StateRoot,
    [string]$OwnerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [string]$Label = 'Mirage Game Assets',
    [string]$BlockedProcess = 'GenshinImpact',
    [string]$ReadyDirectory = 'GenshinImpact_Data',
    [switch]$RestartMount
)
$ErrorActionPreference = 'Stop'
$resolvedHost = (Resolve-Path -LiteralPath $HostExe).Path
$resolvedConfig = (Resolve-Path -LiteralPath $Config).Path
$resolvedMount = [IO.Path]::GetFullPath($Mount).TrimEnd('\')
New-Item -ItemType Directory -Force -Path $StateRoot | Out-Null
$matchingProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'mirage-fs.exe'" | Where-Object {
    $_.ExecutablePath -eq $resolvedHost -and $_.CommandLine.Contains('"' + $resolvedMount + '"') -and
    $_.CommandLine.Contains('"--sophon"')
})
if ($RestartMount) {
    if (Get-Process -Name $BlockedProcess -ErrorAction SilentlyContinue) {
        throw "Close $BlockedProcess before restarting its Mirage asset mount."
    }
    foreach ($mountProcess in $matchingProcesses) {
        Stop-Process -Id $mountProcess.ProcessId -ErrorAction Stop
        Wait-Process -Id $mountProcess.ProcessId -Timeout 15 -ErrorAction SilentlyContinue
    }
    $deadline = (Get-Date).AddSeconds(30)
    while (Test-Path -LiteralPath (Join-Path $resolvedMount $ReadyDirectory)) {
        if ((Get-Date) -ge $deadline) { throw 'Previous Mirage mount did not release.' }
        Start-Sleep -Milliseconds 250
    }
} elseif ($matchingProcesses.Count -gt 0) {
    if (Test-Path -LiteralPath (Join-Path $resolvedMount $ReadyDirectory)) {
        Write-Output 'Mirage asset mount is already running.'
        return
    }
    throw 'A Mirage source host is running without a readable asset tree; close the game and restart the mount.'
}
if (Test-Path -LiteralPath $resolvedMount) {
    $item = Get-Item -LiteralPath $resolvedMount -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'An unrecognized mount already occupies the target.' }
    if (@(Get-ChildItem -LiteralPath $resolvedMount -Force).Count -gt 0) { throw 'Mount target must be empty.' }
}
$cacheDrive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot([IO.Path]::GetFullPath($StateRoot)))
$runtimeArguments = @($resolvedMount, $resolvedConfig, [IO.Path]::GetFullPath($StateRoot), $OwnerSid,
    '--sophon', [string]$cacheDrive.TotalSize, [string]$cacheDrive.AvailableFreeSpace, '--label', $Label)
$quotedArguments = foreach ($argument in $runtimeArguments) {
    if ($argument.Contains('"') -or $argument.EndsWith('\')) { throw 'Unsupported runtime argument quoting.' }
    '"' + $argument + '"'
}
$startup = New-CimInstance -ClassName Win32_ProcessStartup -ClientOnly -Property @{
    ShowWindow = [uint16]0
    CreateFlags = [uint32]150994944 # NO_WINDOW | BREAKAWAY_FROM_JOB
}
$result = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
    CommandLine = '"' + $resolvedHost + '" ' + ($quotedArguments -join ' ')
    CurrentDirectory = [IO.Path]::GetDirectoryName($resolvedHost)
    ProcessStartupInformation = $startup
}
if ($result.ReturnValue -ne 0) { throw "Mirage host launch failed (Windows result $($result.ReturnValue))." }
$deadline = (Get-Date).AddSeconds(45)
while (-not (Test-Path -LiteralPath (Join-Path $resolvedMount $ReadyDirectory))) {
    if (-not (Get-Process -Id $result.ProcessId -ErrorAction SilentlyContinue)) { throw 'Mirage source host exited during startup.' }
    if ((Get-Date) -ge $deadline) { throw 'Mirage source mount did not become readable.' }
    Start-Sleep -Milliseconds 250
}
[pscustomobject]@{ Pid=$result.ProcessId; Started=(Get-Date).ToString('o'); Provider='MirageSophon' } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $StateRoot 'mount-state.json')
Write-Output 'Mirage asset mount is ready.'
