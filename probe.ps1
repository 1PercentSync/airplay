# Windows one-shot: release build, then the four probe commands.
# Writes a timestamped log under logs\ (gitignored).
#
#   .\probe.ps1
#   .\probe.ps1 -Target 192.168.1.12

param(
    [string]$Target = "192.168.1.12"
)

$ErrorActionPreference = "Continue"
Set-Location -LiteralPath $PSScriptRoot

$logDir = Join-Path $PSScriptRoot "logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logPath = Join-Path $logDir "probe-$stamp.log"

function Write-Log {
    param([string]$Message)
    $line = "$(Get-Date -Format 'HH:mm:ss') $Message"
    Write-Host $line
    Add-Content -LiteralPath $logPath -Value $line -Encoding utf8
}

function Invoke-LoggedNative {
    param(
        [string]$Title,
        [string]$FilePath,
        [string[]]$ArgumentList
    )
    Write-Log "======== $Title ========"
    Write-Log ("command: {0} {1}" -f $FilePath, ($ArgumentList -join " "))
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $output = & $FilePath @ArgumentList 2>&1
    $code = $LASTEXITCODE
    if ($null -eq $code) {
        $code = 0
    }
    foreach ($item in @($output)) {
        Write-Log $item.ToString()
    }
    $sw.Stop()
    Write-Log ("exit={0} elapsed_ms={1}" -f $code, $sw.ElapsedMilliseconds)
    Write-Log ""
    return $code
}

Write-Log ("repo={0}" -f $PSScriptRoot)
Write-Log ("target={0}" -f $Target)
Write-Log ("log={0}" -f $logPath)
try {
    $head = & git -C $PSScriptRoot rev-parse --short HEAD 2>$null
    if ($LASTEXITCODE -eq 0 -and $head) {
        Write-Log ("git={0}" -f $head.ToString().Trim())
    }
} catch {
    Write-Log "git=unavailable"
}
Write-Log ""

$results = [ordered]@{}

$cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
if (-not $cargo) {
    Write-Log "ERROR: cargo not found on PATH"
    Write-Host "Log: $logPath"
    exit 1
}

$env:RUST_BACKTRACE = "1"
$results["cargo build --release"] = Invoke-LoggedNative `
    -Title "cargo build --release" `
    -FilePath $cargo.Source `
    -ArgumentList @("build", "--release")

if ($results["cargo build --release"] -ne 0) {
    Write-Log "build failed; skipping probe commands"
    Write-Host "Log: $logPath"
    exit $results["cargo build --release"]
}

$exe = Join-Path $PSScriptRoot "target\release\airplay.exe"
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Log ("ERROR: missing binary {0}" -f $exe)
    Write-Host "Log: $logPath"
    exit 1
}

$results["probe devices"] = Invoke-LoggedNative `
    -Title "probe devices" `
    -FilePath $exe `
    -ArgumentList @("probe", "devices")

$results["probe discover"] = Invoke-LoggedNative `
    -Title "probe discover" `
    -FilePath $exe `
    -ArgumentList @("probe", "discover")

$results["probe airplay"] = Invoke-LoggedNative `
    -Title "probe airplay" `
    -FilePath $exe `
    -ArgumentList @("probe", "airplay", $Target)

$results["probe pair"] = Invoke-LoggedNative `
    -Title "probe pair" `
    -FilePath $exe `
    -ArgumentList @("probe", "pair", $Target)

Write-Log "======== summary ========"
$failed = 0
foreach ($name in $results.Keys) {
    $code = $results[$name]
    if ($code -ne 0) {
        $failed += 1
    }
    Write-Log ("{0}: exit {1}" -f $name, $code)
}
Write-Log ("failed_steps={0}" -f $failed)
Write-Host "Log: $logPath"
if ($failed -gt 0) {
    exit 1
}
exit 0
