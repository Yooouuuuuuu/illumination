<#
.SYNOPSIS
Runs a measurement plan against a host and writes one report folder (docs/testing/measure.md).

.DESCRIPTION
On the host: `illum --service` and `illum workload` (full screen) are running.
Here: each line of the plan is one fixed-settings session of the given length; the viewer
writes a JSON file per step and `illum report` turns the folder into report.md.

.EXAMPLE
.\scripts\measure.ps1 -Target 100.100.101.1
.\scripts\measure.ps1 -Target 100.100.101.1 -Plan .\scripts\measure-plan.csv -Seconds 30
#>
param(
    [Parameter(Mandatory = $true)][string]$Target,
    [string]$Plan = (Join-Path $PSScriptRoot "measure-plan.csv"),
    [string]$Out = "",
    [string]$Illum = "",
    # Overrides the seconds column for every step (0 = use the plan).
    [int]$Seconds = 0,
    # Pause between steps, so the host finishes the previous session first.
    [int]$Pause = 3,
    # Don't ask for the host's sound.
    [switch]$Mute,
    # Which of the host's displays to stream (the one `illum workload --screen N` fills).
    [int]$Screen = 0
)

$ErrorActionPreference = "Stop"
if ($Illum -eq "") {
    $Illum = Join-Path (Split-Path $PSScriptRoot -Parent) "target\release\illum.exe"
}
if (-not (Test-Path $Illum)) { throw "illum.exe not found at $Illum (build with cargo build --release, or pass -Illum)" }
if ($Out -eq "") {
    $stamp = Get-Date -Format "yyyyMMdd-HHmm"
    $Out = Join-Path (Split-Path $PSScriptRoot -Parent) "measurements\$stamp-$env:COMPUTERNAME"
}
New-Item -ItemType Directory -Force $Out | Out-Null
$steps = Import-Csv $Plan
Write-Host "Plan: $Plan ($($steps.Count) steps) → $Out"
Write-Host "Host $Target must be running 'illum --service' and 'illum workload'."

$i = 0
foreach ($s in $steps) {
    $i++
    $secs = if ($Seconds -gt 0) { $Seconds } else { [int]$s.seconds }
    $report = Join-Path $Out "$($s.name).json"
    $args = @("view", $Target, "--fixed", "--view-only", "--screen", $Screen,
        "--duration", $secs, "--report", $report, "--label", $s.name,
        "--codec", $s.codec,
        "--max-bitrate", $s.bitrate, "--max-fps", $s.fps,
        "--fec-min", $s.parity, "--fec-max", $s.parity,
        "--scale", $s.scale)
    if ($s.decode -eq "cpu") { $args += "--software-decode" }
    if ($s.render -eq "cpu") { $args += "--software-render" }
    if ($Mute) { $args += "--mute" }
    Write-Host ("[{0}/{1}] {2}: {3} {4} Mb/s {5} fps parity {6} % scale {7} %, decode {8}, render {9}, {10} s" -f
        $i, $steps.Count, $s.name, $s.codec, $s.bitrate, $s.fps, $s.parity, $s.scale, $s.decode, $s.render, $secs)
    & $Illum @args
    if ($LASTEXITCODE -ne 0) { Write-Warning "step $($s.name) exited with $LASTEXITCODE" }
    Start-Sleep -Seconds $Pause
}

& $Illum report $Out
