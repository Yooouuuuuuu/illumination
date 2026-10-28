# Fetches libclang (needed once, at build time, by the FFmpeg bindings' bindgen step) into
# third_party/llvm without installing LLVM system-wide. .cargo/config.toml points LIBCLANG_PATH
# at it.
#
# Takes only bin/libclang.dll and lib/clang/<ver>/include out of the official LLVM archive
# (the .tar.xz asset). Unpacks with Windows' own tar.exe where that build has xz support, and
# with 7-Zip otherwise (any 7-Zip version reads .xz; the .zst asset would need 7-Zip 23.01+).
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/get-libclang.ps1 [-Use7Zip]
param(
    [string]$Version = "23.1.4",
    [switch]$Use7Zip
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
} else {
    Write-Host "Using the already downloaded $archive"
}
$tmp = Join-Path $tp "_llvm"
if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
New-Item -ItemType Directory -Force $tmp | Out-Null
$wanted = @("$name/bin/libclang.dll", "$name/lib/clang")
$dll = Join-Path $tmp "$name\bin\libclang.dll"
$done = $false

# 1. Windows' own tar (bsdtar). Not whatever `tar` is first on PATH: Git's GNU tar reads
#    "D:\..." as a host name. Some Windows builds of tar.exe lack xz support; then 7-Zip.
$tar = Join-Path $env:SystemRoot "System32\tar.exe"
if (-not $Use7Zip -and (Test-Path $tar)) {
    Write-Host "Extracting bin\libclang.dll and lib\clang with tar.exe (this takes a minute)"
    & $tar -xf $archive -C $tmp $wanted
    $done = ($LASTEXITCODE -eq 0) -and (Test-Path $dll)
    if (-not $done) {
        Write-Host "This tar.exe cannot unpack .xz; trying 7-Zip instead."
        Remove-Item -Recurse -Force $tmp
        New-Item -ItemType Directory -Force $tmp | Out-Null
    }
}

# 2. 7-Zip, in two steps (.xz -> .tar, then the two entries). The .tar is a few GB for a moment.
if (-not $done) {
    $sevenZip = "C:\Program Files\7-Zip\7z.exe"
    if (-not (Test-Path $sevenZip)) {
        throw "7-Zip not found at $sevenZip. Install it (winget install 7zip.7zip) and run this again; the download is kept."
    }
    Write-Host "Extracting with 7-Zip (writes a few GB temporarily; takes a few minutes)"
    & $sevenZip x -y "-o$tmp" $archive | Out-Null
    $inner = Get-ChildItem $tmp -Filter *.tar | Select-Object -First 1
    if (-not $inner) { throw "7-Zip did not produce a .tar from $archive" }
    & $sevenZip x -y "-o$tmp" $inner.FullName $wanted | Out-Null
    Remove-Item $inner.FullName
    $done = Test-Path $dll
}
if (-not $done) {
    throw "libclang.dll was not extracted; the archive may be incomplete. Delete $archive and run again."
}
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
Move-Item (Join-Path $tmp $name) $dest
Remove-Item -Recurse -Force $tmp
Remove-Item $archive
Write-Host "libclang ready in $dest"
