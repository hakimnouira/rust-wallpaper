# Production build and packaging script for Rust Wallpaper
$ErrorActionPreference = "Stop"

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "  Building Rust Wallpaper Production Release Artifacts" -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

$RootDir = Split-Path -Parent $PSScriptRoot
Set-Location $RootDir

# 1. Compile all production binaries in release mode
Write-Host "`n[1/4] Compiling release binaries with optimizations (LTO, strip)..." -ForegroundColor Yellow
cargo build --release -p wallpaper-ui -p wallpaper-daemon -p wallpaper-installer

if ($LASTEXITCODE -ne 0) {
    Write-Error "Release build failed."
    exit 1
}

# 2. Prepare output distribution directory
Write-Host "`n[2/4] Assembling production distribution in dist/..." -ForegroundColor Yellow
$DistDir = Join-Path $RootDir "dist"
$DistBinDir = Join-Path $DistDir "bin\mpv"
$DistAssetsDir = Join-Path $DistDir "assets"

if (Test-Path $DistDir) {
    Remove-Item -Recurse -Force $DistDir -ErrorAction Stop
}
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
New-Item -ItemType Directory -Force -Path $DistBinDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $DistAssetsDir "wallpapers") | Out-Null

# 3. Copy compiled binaries and assets
Write-Host "`n[3/4] Copying executables, engine components, and assets..." -ForegroundColor Yellow
$ReleaseDir = Join-Path $RootDir "target\release"

# Unified single executable: RustWallpaper.exe
Copy-Item (Join-Path $ReleaseDir "RustWallpaper.exe") (Join-Path $DistDir "RustWallpaper.exe") -Force
# Also provide alias wallpaper-ui.exe
Copy-Item (Join-Path $ReleaseDir "RustWallpaper.exe") (Join-Path $DistDir "wallpaper-ui.exe") -Force

# Installer executable: RustWallpaper-Setup.exe
Copy-Item (Join-Path $ReleaseDir "RustWallpaper-Setup.exe") (Join-Path $DistDir "RustWallpaper-Setup.exe") -Force

# Companion daemon and uninstaller
Copy-Item (Join-Path $ReleaseDir "wallpaper-daemon.exe") (Join-Path $DistDir "wallpaper-daemon.exe") -Force
Copy-Item (Join-Path $ReleaseDir "uninstall.exe") (Join-Path $DistDir "uninstall.exe") -Force

# Assets
Copy-Item (Join-Path $RootDir "assets\icon.ico") (Join-Path $DistAssetsDir "icon.ico") -Force -ErrorAction Stop
Copy-Item (Join-Path $RootDir "assets\icon.png") (Join-Path $DistAssetsDir "icon.png") -Force -ErrorAction Stop
Copy-Item (Join-Path $RootDir "assets\app.manifest") (Join-Path $DistAssetsDir "app.manifest") -Force -ErrorAction Stop

# Copy bundled MPV & FFmpeg video engine
$UserMpvDir = Join-Path $env:USERPROFILE ".mpv"
$MpvExe = Join-Path $UserMpvDir "mpv.exe"
if (-not (Test-Path $MpvExe)) {
    throw "Bundled MPV was not found at '$MpvExe'. Install a trusted Windows MPV build before packaging."
}
foreach ($file in @("mpv.exe", "ffmpeg.exe", "d3dcompiler_43.dll")) {
    $src = Join-Path $UserMpvDir $file
    if ($file -eq "mpv.exe" -and -not (Test-Path $src)) {
        throw "Required playback engine is missing: $src"
    }
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $DistBinDir $file) -Force -ErrorAction Stop
    }
}
Copy-Item (Join-Path $RootDir "assets\wallpapers\sample-nature.mp4") (Join-Path $DistAssetsDir "wallpapers\sample-nature.mp4") -Force -ErrorAction Stop
Copy-Item (Join-Path $RootDir "assets\wallpapers\sample-abstract.webm") (Join-Path $DistAssetsDir "wallpapers\sample-abstract.webm") -Force -ErrorAction Stop

# Package Browser Extension with the locally protected signing key
python (Join-Path $PSScriptRoot "package-extension.py")
if ($LASTEXITCODE -ne 0) {
    throw "Browser extension packaging failed."
}
$DistExtDir = Join-Path $DistDir "extension"
New-Item -ItemType Directory -Force -Path $DistExtDir | Out-Null
Copy-Item (Join-Path $RootDir "extensions\chrome\*") $DistExtDir -Recurse -Force
if (Test-Path (Join-Path $RootDir "extensions\chrome.crx")) {
    Copy-Item (Join-Path $RootDir "extensions\chrome.crx") (Join-Path $DistExtDir "chrome.crx") -Force
    Copy-Item (Join-Path $RootDir "extensions\chrome.crx") (Join-Path $DistDir "chrome.crx") -Force
}
Compress-Archive -Path "$DistExtDir\*" -DestinationPath (Join-Path $DistDir "RustWallpaper-Extension.zip") -Force

# 4. Summary and validation
Write-Host "`n[4/4] Verifying production artifacts..." -ForegroundColor Yellow

$RequiredFiles = @(
    "RustWallpaper.exe",
    "RustWallpaper-Setup.exe",
    "wallpaper-daemon.exe",
    "uninstall.exe",
    "bin\mpv\mpv.exe",
    "assets\icon.ico",
    "assets\icon.png",
    "assets\wallpapers\sample-nature.mp4",
    "assets\wallpapers\sample-abstract.webm",
    "extension\chrome.crx"
)
foreach ($relativePath in $RequiredFiles) {
    $path = Join-Path $DistDir $relativePath
    if (-not (Test-Path $path -PathType Leaf)) {
        throw "Release package is missing required file '$relativePath'"
    }
}

$Version = (Select-String -Path (Join-Path $RootDir "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $Version) { throw "Could not determine release version from Cargo.toml" }
$ZipPath = Join-Path $DistDir "RustWallpaper-$Version-windows-x64.zip"
$StagingDir = Join-Path $env:TEMP ("RustWallpaper-release-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $StagingDir | Out-Null
try {
    Copy-Item (Join-Path $DistDir "*") $StagingDir -Recurse -Force
    Compress-Archive -Path (Join-Path $StagingDir "*") -DestinationPath $ZipPath -Force
} finally {
    Remove-Item -Recurse -Force $StagingDir -ErrorAction SilentlyContinue
}
$ChecksumPath = "$ZipPath.sha256"
$Checksum = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
"$Checksum  $(Split-Path -Leaf $ZipPath)" | Set-Content -Path $ChecksumPath -Encoding ascii

$Artifacts = Get-ChildItem -Path $DistDir -Filter "*.exe"
Write-Host "`nSuccessfully generated and validated release binaries in dist/:" -ForegroundColor Green
foreach ($item in $Artifacts) {
    $sizeMb = [math]::Round($item.Length / 1MB, 2)
    Write-Host "  • $($item.Name) ($sizeMb MB)" -ForegroundColor White
}

Write-Host "`nRelease package: $ZipPath" -ForegroundColor Cyan
Write-Host "SHA-256: $Checksum" -ForegroundColor Cyan
Write-Host "Run RustWallpaper-Setup.exe from the extracted ZIP; do not distribute the setup executable alone." -ForegroundColor Yellow
