# SPDX-License-Identifier: GPL-3.0-only
# Builds the Windows installer: target\installer\Illumination_<version>_x64-setup.exe.
#
#   pwsh scripts\build-installer.ps1            release build, then the installer
#   pwsh scripts\build-installer.ps1 -NoBuild   package what target\release already holds
#
# Needs NSIS (makensis.exe): on PATH, in its usual folder, or the copy Tauri keeps.
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'No version found in Cargo.toml.' }

$makensis = @(
    (Get-Command makensis.exe -ErrorAction SilentlyContinue | ForEach-Object Source),
    "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
    "$env:ProgramFiles\NSIS\makensis.exe",
    "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe",
    "$env:LOCALAPPDATA\tauri\NSIS\Bin\makensis.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $makensis) { throw 'makensis.exe not found. Install NSIS (winget install NSIS.NSIS).' }

if (-not $NoBuild) {
    Push-Location $root
    try {
        cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'The release build failed.' }
    } finally { Pop-Location }
}

$source = Join-Path $root 'target\release'
$exe = Join-Path $source 'illum.exe'
if (-not (Test-Path $exe)) { throw "No $exe; build first." }
# The installer must carry the version it is named after.
$built = (& $exe --version) -replace '^\S+\s+', ''
if ($built -ne $version) { throw "target\release\illum.exe is $built, Cargo.toml says $version. Build first." }
if (-not (Get-ChildItem $source -Filter 'avcodec-*.dll')) { throw "No FFmpeg libraries next to $exe." }

$out = Join-Path $root 'target\installer'
New-Item -ItemType Directory -Force $out | Out-Null
$setup = Join-Path $out "Illumination_${version}_x64-setup.exe"
& $makensis /V2 "/DVERSION=$version" "/DSOURCE=$source" "/DOUTPUT=$setup" (Join-Path $root 'installer\illumination.nsi')
if ($LASTEXITCODE -ne 0) { throw 'makensis failed.' }
$size = [math]::Round((Get-Item $setup).Length / 1MB, 1)
"Built $setup ($size MB)."
