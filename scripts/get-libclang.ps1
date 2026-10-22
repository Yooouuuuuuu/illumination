# Fetches libclang (needed once, at build time, by the FFmpeg bindings' bindgen step) into
# third_party/llvm without installing LLVM system-wide. .cargo/config.toml points LIBCLANG_PATH
# at it.
#
# Takes only bin/libclang.dll and lib/clang/<ver>/include out of the official LLVM archive.
# Needs 7-Zip (C:\Program Files\7-Zip\7z.exe) to unpack the .tar.zst; without it, install LLVM
# from https://releases.llvm.org and set LIBCLANG_PATH to its bin folder instead.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/get-libclang.ps1
param(
    [string]$Version = "23.1.4"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "third_party\llvm"
if (Test-Path (Join-Path $dest "bin\libclang.dll")) {
    Write-Host "libclang already present in $dest"
    exit 0
}
$sevenZip = "C:\Program Files\7-Zip\7z.exe"
if (-not (Test-Path $sevenZip)) {
    throw "7-Zip not found at $sevenZip. Install it, or install LLVM and set LIBCLANG_PATH."
}
$name = "clang+llvm-$Version-x86_64-pc-windows-msvc"
$asset = "$name.tar.zst"
$url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$Version/$asset"
$tp = Join-Path $root "third_party"
New-Item -ItemType Directory -Force $tp | Out-Null
$archive = Join-Path $tp $asset
Write-Host "Downloading $url (about 260 MB)"
Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
$tmp = Join-Path $tp "_llvm"
if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
& $sevenZip x -y "-o$tmp" $archive | Out-Null
$tar = Get-ChildItem $tmp -Filter *.tar | Select-Object -First 1
& $sevenZip x -y "-o$tmp" $tar.FullName "$name/bin/libclang.dll" "$name/lib/clang" | Out-Null
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
Move-Item (Join-Path $tmp $name) $dest
Remove-Item -Recurse -Force $tmp
Remove-Item $archive
Write-Host "libclang ready in $dest"
