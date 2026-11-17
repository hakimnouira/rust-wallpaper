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

Stop-Process -Name "mpv", "RustWallpaper", "wallpaper-ui", "wallpaper-daemon" -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

if (Test-Path $DistDir) {
    try {
        Remove-Item -Recurse -Force $DistDir -ErrorAction Stop
    } catch {
        Write-Warning "Could not wipe $DistDir completely, continuing with overwrite..."
    }
}
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
New-Item -ItemType Directory -Force -Path $DistBinDir | Out-Null
New-Item -ItemType Directory -Force -Path $DistAssetsDir | Out-Null

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
Copy-Item (Join-Path $RootDir "assets\icon.ico") (Join-Path $DistAssetsDir "icon.ico") -Force
Copy-Item (Join-Path $RootDir "assets\icon.png") (Join-Path $DistAssetsDir "icon.png") -Force
Copy-Item (Join-Path $RootDir "assets\app.manifest") (Join-Path $DistAssetsDir "app.manifest") -Force

# Copy bundled MPV & FFmpeg video engine
$UserMpvDir = Join-Path $env:USERPROFILE ".mpv"
if (Test-Path $UserMpvDir) {
    foreach ($file in @("mpv.exe", "ffmpeg.exe", "d3dcompiler_43.dll")) {
        $src = Join-Path $UserMpvDir $file
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $DistBinDir $file) -Force
        }
    }
}

# Copy Browser Extension
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

$Artifacts = Get-ChildItem -Path $DistDir -Filter "*.exe"
Write-Host "`nSuccessfully generated production binaries in dist/:" -ForegroundColor Green
foreach ($item in $Artifacts) {
    $sizeMb = [math]::Round($item.Length / 1MB, 2)
    Write-Host "  • $($item.Name) ($sizeMb MB)" -ForegroundColor White
}

Write-Host "`nProduction Build Complete!" -ForegroundColor Green
Write-Host "• Single executable (portable): dist\RustWallpaper.exe" -ForegroundColor Cyan
Write-Host "• Application Installer:        dist\RustWallpaper-Setup.exe" -ForegroundColor Cyan
Write-Host "• Browser Extension:            dist\extension\ (or dist\RustWallpaper-Extension.zip)" -ForegroundColor Cyan
