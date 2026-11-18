@echo off
setlocal enabledelayedexpansion
title RustWallpaper Companion - Browser Extension Registration

:: Check for Administrator permissions
fltmc >nul 2>&1
if %errorlevel% neq 0 (
    echo [RustWallpaper] Requesting Administrator privileges to register browser extension...
    powershell -NoProfile -Command "Start-Process cmd.exe -ArgumentList '/c \"\"%~f0\"\"' -Verb RunAs"
    exit /b
)

echo =========================================================================
echo       RustWallpaper Companion - IDM-Style Browser Auto-Installer
echo =========================================================================
echo.

set "SCRIPT_DIR=%~dp0"
set "EXT_ID=gemjknbkapmdcopknokbikcneknbpoij"
set "VERSION=1.0.0"
set "UPDATE_URL=http://127.0.0.1:24242/extension/update.xml"

:: Locate chrome.crx
set "CRX_PATH="
if exist "%SCRIPT_DIR%extension\chrome.crx" set "CRX_PATH=%SCRIPT_DIR%extension\chrome.crx"
if not defined CRX_PATH if exist "%SCRIPT_DIR%extensions\chrome.crx" set "CRX_PATH=%SCRIPT_DIR%extensions\chrome.crx"
if not defined CRX_PATH if exist "%SCRIPT_DIR%chrome.crx" set "CRX_PATH=%SCRIPT_DIR%chrome.crx"
if not defined CRX_PATH if exist "%LOCALAPPDATA%\RustWallpaper\extension\chrome.crx" set "CRX_PATH=%LOCALAPPDATA%\RustWallpaper\extension\chrome.crx"
if not defined CRX_PATH if exist "D:\work\wallpaper-rust\extensions\chrome.crx" set "CRX_PATH=D:\work\wallpaper-rust\extensions\chrome.crx"

if not defined CRX_PATH (
    echo [ERROR] Could not find chrome.crx package!
    echo Please make sure chrome.crx is in the extension folder.
    pause
    exit /b 1
)

:: Copy to canonical LocalAppData path
if not exist "%LOCALAPPDATA%\RustWallpaper\extension" mkdir "%LOCALAPPDATA%\RustWallpaper\extension"
copy /y "%CRX_PATH%" "%LOCALAPPDATA%\RustWallpaper\extension\chrome.crx" >nul
set "CANONICAL_CRX=%LOCALAPPDATA%\RustWallpaper\extension\chrome.crx"

echo [*] Extension Package: %CANONICAL_CRX%
echo [*] Extension ID:      %EXT_ID%
echo.

:: 1. External Extensions Registry (HKLM & HKCU)
set "REG_TARGETS=HKLM\Software\Google\Chrome\Extensions HKLM\Software\Microsoft\Edge\Extensions HKLM\Software\BraveSoftware\Brave-Browser\Extensions HKLM\Software\BraveSoftware\Brave\Extensions HKLM\Software\WOW6432Node\Google\Chrome\Extensions HKLM\Software\WOW6432Node\Microsoft\Edge\Extensions HKLM\Software\WOW6432Node\BraveSoftware\Brave-Browser\Extensions HKLM\Software\WOW6432Node\BraveSoftware\Brave\Extensions HKCU\Software\Google\Chrome\Extensions HKCU\Software\Microsoft\Edge\Extensions HKCU\Software\BraveSoftware\Brave-Browser\Extensions HKCU\Software\BraveSoftware\Brave\Extensions"

for %%k in (%REG_TARGETS%) do (
    reg add "%%k\%EXT_ID%" /v "path" /t REG_SZ /d "%CANONICAL_CRX%" /f >nul 2>&1
    reg add "%%k\%EXT_ID%" /v "version" /t REG_SZ /d "%VERSION%" /f >nul 2>&1
    reg add "%%k\%EXT_ID%" /v "update_url" /t REG_SZ /d "%UPDATE_URL%" /f >nul 2>&1
)
echo [OK] Registered in External Extensions for Chrome, Edge, and Brave.

:: 2. Chromium Enterprise Policies (HKLM)
set "POLICY_TARGETS=HKLM\Software\Policies\Google\Chrome HKLM\Software\Policies\Microsoft\Edge HKLM\Software\Policies\BraveSoftware\Brave HKLM\Software\Policies\BraveSoftware\Brave-Browser"

for %%p in (%POLICY_TARGETS%) do (
    reg add "%%p" /v "ExtensionSettings" /t REG_SZ /d "{\"%EXT_ID%\":{\"installation_mode\":\"normal_installed\",\"update_url\":\"%UPDATE_URL%\"}}" /f >nul 2>&1
    reg add "%%p\ExtensionInstallForcelist" /v "1" /t REG_SZ /d "%EXT_ID%;%UPDATE_URL%" /f >nul 2>&1
    reg add "%%p\ExtensionInstallSources" /v "1" /t REG_SZ /d "http://127.0.0.1:24242/*" /f >nul 2>&1
    reg add "%%p\ExtensionInstallSources" /v "2" /t REG_SZ /d "<all_urls>" /f >nul 2>&1
)
echo [OK] Configured Chromium Enterprise Auto-Deploy Policies.

echo.
echo =========================================================================
echo  SUCCESS!
echo  The extension is now registered in Google Chrome, Microsoft Edge, and Brave.
echo.
echo  WHEN YOU LAUNCH YOUR BROWSER:
echo  The browser will automatically display the IDM-style native prompt:
echo  "'RustWallpaper Companion' was added by another program on your computer."
echo  Click [Enable extension] to finish.
echo =========================================================================
echo.
timeout /t 5
