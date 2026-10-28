# Downloads a pinned prebuilt FFmpeg (shared libraries + headers + import libs) into
# third_party/ffmpeg, which .cargo/config.toml points the bindings at (FFMPEG_DIR).
#
# Source: BtbN/FFmpeg-Builds, the "gpl-shared" flavour (we are GPL-3.0 ourselves). The hardware
# encoders (NVENC, AMF, QSV), libdav1d and libx265 are included.
#
# FFmpeg 8.1 rather than 9.0 on purpose: its NVENC needs NVIDIA driver 570+ (API 13.0), while
# 9.0 demands 610+ (API 13.1) and would silently fall back to software encoding on a driver
# that is a few months old.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/get-ffmpeg.ps1
param(
    [string]$Asset = "ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip"
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # the progress bar makes Invoke-WebRequest very slow
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "third_party\ffmpeg"
$zip = Join-Path $root "third_party\$Asset"
$url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$Asset"

if (Test-Path (Join-Path $dest "include\libavcodec\avcodec.h")) {
    Write-Host "FFmpeg already present in $dest"
    exit 0
}
New-Item -ItemType Directory -Force (Join-Path $root "third_party") | Out-Null
Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
Write-Host "Extracting"
$tmp = Join-Path $root "third_party\_extract"
if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
Expand-Archive -Path $zip -DestinationPath $tmp
$inner = Get-ChildItem $tmp | Select-Object -First 1
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
Move-Item $inner.FullName $dest
Remove-Item -Recurse -Force $tmp
Remove-Item $zip
Write-Host "FFmpeg ready in $dest"
Get-ChildItem (Join-Path $dest "bin") -Filter *.dll | ForEach-Object { "  bin\" + $_.Name }
