@echo off
setlocal
set "ROOT=%~dp0"
set "EXE=%ROOT%target\release\RustWallpaper.exe"

if not exist "%EXE%" (
    echo Built UI not found. Run: cargo build --release -p wallpaper-ui
    pause
    exit /b 1
)

echo Launching the current build (an existing instance will be focused):
echo   %EXE%
start "" "%EXE%"
endlocal
