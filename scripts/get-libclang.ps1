# Fetches libclang (needed once, at build time, by the FFmpeg bindings' bindgen step) into
# third_party/llvm without installing LLVM system-wide. .cargo/config.toml points LIBCLANG_PATH
# at it.
#
# Takes only bin/libclang.dll and lib/clang/<ver>/include out of the official LLVM archive.
# Uses the .tar.xz asset so Windows' own tar.exe (Windows 10 1803+) can unpack it; 7-Zip is the
# fallback. (The .tar.zst asset needs 7-Zip 23.01+, which many machines don't have.)
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/get-libclang.ps1
param(
    [string]$Version = "23.1.4"
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # the progress bar makes Invoke-WebRequest very slow
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "third_party\llvm"
if (Test-Path (Join-Path $dest "bin\libclang.dll")) {
    Write-Host "libclang already present in $dest"
    exit 0
}
$name = "clang+llvm-$Version-x86_64-pc-windows-msvc"
$asset = "$name.tar.xz"
$url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$Version/$asset"
$tp = Join-Path $root "third_party"
New-Item -ItemType Directory -Force $tp | Out-Null
$archive = Join-Path $tp $asset
# Leftovers from an earlier attempt with the .zst asset.
Get-ChildItem $tp -Filter "clang+llvm-*.tar.zst" -ErrorAction SilentlyContinue | Remove-Item -Force
if (-not (Test-Path $archive) -or (Get-Item $archive).Length -lt 100MB) {
    Write-Host "Downloading $url (about 900 MB; only ~100 MB of it is kept)"
    Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
}
$tmp = Join-Path $tp "_llvm"
if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
New-Item -ItemType Directory -Force $tmp | Out-Null
Write-Host "Extracting bin\libclang.dll and lib\clang (this takes a minute)"
# Windows' own tar (bsdtar). Not whatever `tar` is first on PATH: Git's GNU tar reads "D:\..."
# as a host name.
$tar = Join-Path $env:SystemRoot "System32\tar.exe"
if (Test-Path $tar) {
    & $tar -xf $archive -C $tmp "$name/bin/libclang.dll" "$name/lib/clang"
    if ($LASTEXITCODE -ne 0) { throw "tar.exe failed with exit code $LASTEXITCODE" }
} else {
    $sevenZip = "C:\Program Files\7-Zip\7z.exe"
    if (-not (Test-Path $sevenZip)) {
        throw "Neither tar.exe nor 7-Zip ($sevenZip) found. Install 7-Zip, or install LLVM and set LIBCLANG_PATH to its bin folder."
    }
    & $sevenZip x -y "-o$tmp" $archive | Out-Null
    $inner = Get-ChildItem $tmp -Filter *.tar | Select-Object -First 1
    & $sevenZip x -y "-o$tmp" $inner.FullName "$name/bin/libclang.dll" "$name/lib/clang" | Out-Null
    Remove-Item $inner.FullName
}
if (-not (Test-Path (Join-Path $tmp "$name\bin\libclang.dll"))) {
    throw "libclang.dll was not extracted; the archive may be incomplete. Delete $archive and run again."
}
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
Move-Item (Join-Path $tmp $name) $dest
Remove-Item -Recurse -Force $tmp
Remove-Item $archive
Write-Host "libclang ready in $dest"
