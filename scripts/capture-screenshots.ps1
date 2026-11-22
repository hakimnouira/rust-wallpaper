param(
    [string]$OutputDir = "docs\screenshots",
    [switch]$Restart
)

$ErrorActionPreference = "Stop"

$rootDir = Split-Path -Parent $PSScriptRoot
$distDir = Join-Path $rootDir "dist"
$shotsDir = Join-Path $rootDir $OutputDir
Remove-Item -Recurse -Force $shotsDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $shotsDir | Out-Null

Add-Type -ReferencedAssemblies System.Drawing, System.Windows.Forms -TypeDefinition @"
using System;
using System.Text;
using System.Drawing;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class ScreenCap {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc enumProc, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern IntPtr GetWindowDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("gdi32.dll")]
    public static extern bool BitBlt(IntPtr hObject, int nXDest, int nYDest, int nWidth, int nHeight, IntPtr hObjectSource, int nXSrc, int nYSrc, int dwRop);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PrintWindow(IntPtr hwnd, IntPtr hDC, uint nFlags);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);

    [DllImport("user32.dll")]
    public static extern bool ScreenToClient(IntPtr hWnd, ref POINT lpPoint);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint cButtons, uint dwExtraInfo);

    public const uint WM_MOUSEMOVE = 0x0200;
    public const uint WM_LBUTTONDOWN = 0x0201;
    public const uint WM_LBUTTONUP = 0x0202;

    public const uint MOUSEEVENTF_LEFTDOWN = 0x02;
    public const uint MOUSEEVENTF_LEFTUP = 0x04;
    public const int SRCCOPY = 0x00CC0020;

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    public static IntPtr FindMainWindow(uint targetPid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hWnd, lParam) => {
            uint pid;
            GetWindowThreadProcessId(hWnd, out pid);
            if (pid == targetPid && IsWindowVisible(hWnd)) {
                var sb = new StringBuilder(256);
                GetWindowText(hWnd, sb, 256);
                string title = sb.ToString();
                RECT r;
                GetWindowRect(hWnd, out r);
                int w = r.Right - r.Left;
                int h = r.Bottom - r.Top;
                if ((title.Contains("Rust Wallpaper") || w > 500) && w < 3000) {
                    found = hWnd;
                    return false;
                }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static Bitmap Capture(IntPtr hWnd, int w, int h) {
        Bitmap bmp = new Bitmap(w, h);
        using (Graphics g = Graphics.FromImage(bmp)) {
            IntPtr hdcDest = g.GetHdc();
            try {
                bool ok = PrintWindow(hWnd, hdcDest, 2);
                if (!ok) {
                    ok = PrintWindow(hWnd, hdcDest, 0);
                }
                if (!ok) {
                    IntPtr hdcSrc = GetWindowDC(hWnd);
                    if (hdcSrc != IntPtr.Zero) {
                        BitBlt(hdcDest, 0, 0, w, h, hdcSrc, 0, 0, SRCCOPY);
                        ReleaseDC(hWnd, hdcSrc);
                    }
                }
            } finally {
                g.ReleaseHdc(hdcDest);
            }
        }
        return bmp;
    }

    public static void ClickAt(IntPtr hWnd, int screenX, int screenY) {
        POINT pt = new POINT { X = screenX, Y = screenY };
        ScreenToClient(hWnd, ref pt);
        IntPtr lParam = (IntPtr)((pt.Y << 16) | (pt.X & 0xFFFF));
        PostMessage(hWnd, WM_MOUSEMOVE, IntPtr.Zero, lParam);
        System.Threading.Thread.Sleep(80);
        PostMessage(hWnd, WM_LBUTTONDOWN, (IntPtr)1, lParam);
        System.Threading.Thread.Sleep(80);
        PostMessage(hWnd, WM_LBUTTONUP, IntPtr.Zero, lParam);

        SetCursorPos(screenX, screenY);
        System.Threading.Thread.Sleep(150);
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        System.Threading.Thread.Sleep(80);
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        System.Threading.Thread.Sleep(200);
    }
}
"@

[ScreenCap]::SetProcessDPIAware() | Out-Null

$existingDaemon = Get-Process wallpaper-daemon -ErrorAction SilentlyContinue | Select-Object -First 1
$existingUi = Get-Process RustWallpaper, wallpaper-ui -ErrorAction SilentlyContinue | Select-Object -First 1
$startedOwnDaemon = $false
$startedOwnUi = $false

if ($Restart -or (-not $existingDaemon)) {
    Write-Host "Stopping any running instances..." -ForegroundColor Yellow
    Get-Process wallpaper-daemon, wallpaper-ui, RustWallpaper, mpv -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1

    Write-Host "Starting wallpaper daemon..." -ForegroundColor Cyan
    $daemon = Start-Process (Join-Path $distDir "wallpaper-daemon.exe") -PassThru -WindowStyle Hidden
    $startedOwnDaemon = $true
    Start-Sleep -Seconds 2

    Write-Host "Starting wallpaper UI..." -ForegroundColor Cyan
    $ui = Start-Process (Join-Path $distDir "wallpaper-ui.exe") -PassThru
    $startedOwnUi = $true
    Start-Sleep -Seconds 4
} else {
    Write-Host "Reusing running wallpaper-daemon (PID $($existingDaemon.Id))..." -ForegroundColor Cyan
    if ($existingUi) {
        $ui = $existingUi
        $hwndCheck = [ScreenCap]::FindMainWindow([uint32]$ui.Id)
        if ($hwndCheck -eq [IntPtr]::Zero) {
            # Existing tray instance may have hidden window; launching exe focuses/shows it
            $uiExe = if (Test-Path (Join-Path $distDir "RustWallpaper.exe")) { Join-Path $distDir "RustWallpaper.exe" } else { Join-Path $distDir "wallpaper-ui.exe" }
            Start-Process $uiExe | Out-Null
            Start-Sleep -Seconds 2
        }
    } else {
        $uiExe = if (Test-Path (Join-Path $distDir "RustWallpaper.exe")) { Join-Path $distDir "RustWallpaper.exe" } else { Join-Path $distDir "wallpaper-ui.exe" }
        $ui = Start-Process $uiExe -PassThru
        $startedOwnUi = $true
        Start-Sleep -Seconds 4
    }
}

# Locate egui main window
$hwnd = [IntPtr]::Zero
$retries = 0
while ($hwnd -eq [IntPtr]::Zero -and $retries -lt 30) {
    Start-Sleep -Milliseconds 500
    $hwnd = [ScreenCap]::FindMainWindow([uint32]$ui.Id)
    $retries++
}

Write-Host "Located main window HWND: $hwnd" -ForegroundColor Green

if ($hwnd -ne [IntPtr]::Zero) {
    [ScreenCap]::ShowWindow($hwnd, 9)
    [ScreenCap]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 2000

    $rect = New-Object ScreenCap+RECT
    [ScreenCap]::GetWindowRect($hwnd, [ref]$rect)
    $w = $rect.Right - $rect.Left
    $h = $rect.Bottom - $rect.Top

    Write-Host "Window Rect: Left=$($rect.Left), Top=$($rect.Top), W=$w, H=$h"

    function SaveScreenshot([string]$filename, [int]$delayMs = 1500) {
        Start-Sleep -Milliseconds $delayMs
        $bmp = [ScreenCap]::Capture($hwnd, $w, $h)
        $outPath = Join-Path $shotsDir $filename
        $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        Write-Host "Saved: $outPath ($w x $h)" -ForegroundColor Green
    }

    $clickX = $rect.Left + [int]($w * 0.08)

    # 1. Overview
    Write-Host "Capturing Overview..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.146))
    SaveScreenshot "01_overview.png" 1500

    # 2. Library
    Write-Host "Navigating to Library..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.190))
    SaveScreenshot "02_library.png" 1800

    # 3. Browse & Workshop
    Write-Host "Navigating to Browse & Workshop..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.233))
    SaveScreenshot "03_browse.png" 1800

    # 4. Monitors
    Write-Host "Navigating to Monitors..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.276))
    SaveScreenshot "04_monitors.png" 1500

    # 5. Appearance
    Write-Host "Navigating to Appearance..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.368))
    SaveScreenshot "05_appearance.png" 1500

    # 6. Settings
    Write-Host "Navigating to Settings..." -ForegroundColor Yellow
    [ScreenCap]::ClickAt($hwnd, $clickX, $rect.Top + [int]($h * 0.411))
    SaveScreenshot "06_settings.png" 1500
} else {
    Write-Error "Could not find main window for PID $($ui.Id)"
}

Start-Sleep -Seconds 1
if ($startedOwnUi -and $ui) {
    Write-Host "Stopping temporary UI process..." -ForegroundColor Yellow
    Stop-Process -Id $ui.Id -Force -ErrorAction SilentlyContinue
}
if ($startedOwnDaemon -and $daemon) {
    Write-Host "Stopping temporary daemon process..." -ForegroundColor Yellow
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
}
Write-Host "Capture completed!" -ForegroundColor Green
