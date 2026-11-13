//! Fullscreen application and game detection.
//!
//! Evaluates the foreground top-level window to determine if it covers the entire
//! monitor or virtual screen, and whether it is a high-performance fullscreen
//! game vs a normal maximized application or desktop shell window.

use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullscreenStatus {
    pub is_fullscreen: bool,
    pub is_game: bool,
    pub window_title: String,
    pub class_name: String,
    pub monitor_rect: (i32, i32, i32, i32),
    pub window_rect: (i32, i32, i32, i32),
}

impl Default for FullscreenStatus {
    fn default() -> Self {
        Self {
            is_fullscreen: false,
            is_game: false,
            window_title: String::new(),
            class_name: String::new(),
            monitor_rect: (0, 0, 0, 0),
            window_rect: (0, 0, 0, 0),
        }
    }
}

/// Evaluates geometric coverage between window rect and monitor rect.
///
/// Returns true if the window rect completely covers or extends beyond the monitor rect.
pub fn is_rect_fullscreen(
    win_left: i32,
    win_top: i32,
    win_right: i32,
    win_bottom: i32,
    mon_left: i32,
    mon_top: i32,
    mon_right: i32,
    mon_bottom: i32,
) -> bool {
    // Tolerances: fullscreen windows may have small border offsets (-1 to -8 px on Windows DWM)
    // or exact coverage.
    win_left <= mon_left && win_top <= mon_top && win_right >= mon_right && win_bottom >= mon_bottom
}

/// Heuristically evaluates whether a window is likely a game/exclusive media app
/// rather than a standard maximized productivity application or desktop shell.
pub fn is_game_heuristic(
    class_name: &str,
    window_title: &str,
    has_caption: bool,
    has_thick_frame: bool,
) -> bool {
    let lower_class = class_name.to_lowercase();
    let lower_title = window_title.to_lowercase();

    // Ignore Windows Shell, Taskbar, Desktop and Start Menu
    if lower_class == "progman"
        || lower_class == "workergw"
        || lower_class == "workerw"
        || lower_class == "shell_traywnd"
        || lower_class == "shell_secondarytraywnd"
        || lower_class == "windows.ui.core.corewindow"
        || lower_class == "applicationframewindow" && lower_title.contains("search")
    {
        return false;
    }

    // Common game engine class names and window patterns
    if lower_class.contains("unrealwindow")
        || lower_class.contains("unitywndclass")
        || lower_class.contains("directx")
        || lower_class.contains("opengl")
        || lower_class.contains("valve001")
        || lower_class.contains("source")
        || lower_class.contains("sdl_app")
        || lower_class.contains("glfw")
        || lower_class.contains("raylib")
    {
        return true;
    }

    // Borderless / exclusive fullscreen games almost always strip WS_CAPTION and WS_THICKFRAME
    if !has_caption && !has_thick_frame {
        return true;
    }

    false
}

#[cfg(windows)]
pub use platform::check_foreground_fullscreen;

#[cfg(not(windows))]
pub fn check_foreground_fullscreen() -> FullscreenStatus {
    FullscreenStatus::default()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        IsIconic, IsWindow, IsWindowVisible, GWL_STYLE, WS_CAPTION, WS_THICKFRAME,
    };

    /// Probes the current Windows foreground window for fullscreen status.
    pub fn check_foreground_fullscreen() -> FullscreenStatus {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd == HWND::default()
                || !IsWindow(hwnd).as_bool()
                || !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
            {
                return FullscreenStatus::default();
            }

            // Retrieve class name
            let mut class_buf = [0u16; 256];
            let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
            let class_name = String::from_utf16_lossy(&class_buf[..class_len]);

            // Retrieve window title
            let mut title_buf = [0u16; 256];
            let title_len = GetWindowTextW(hwnd, &mut title_buf) as usize;
            let window_title = String::from_utf16_lossy(&title_buf[..title_len]);

            // Ignore our own wallpaper windows, Progman, and WorkerW
            if class_name == "RustWallpaperWindow"
                || class_name.eq_ignore_ascii_case("Progman")
                || class_name.eq_ignore_ascii_case("WorkerW")
                || class_name.eq_ignore_ascii_case("Shell_TrayWnd")
                || class_name.eq_ignore_ascii_case("Shell_SecondaryTrayWnd")
            {
                return FullscreenStatus::default();
            }

            let mut win_rect = RECT::default();
            if GetWindowRect(hwnd, &mut win_rect).is_err() {
                return FullscreenStatus::default();
            }

            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
            if hmon.is_invalid() {
                return FullscreenStatus::default();
            }

            let mut mi = MONITORINFO::default();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
                return FullscreenStatus::default();
            }

            let mon_rect = mi.rcMonitor;

            let is_fullscreen = is_rect_fullscreen(
                win_rect.left,
                win_rect.top,
                win_rect.right,
                win_rect.bottom,
                mon_rect.left,
                mon_rect.top,
                mon_rect.right,
                mon_rect.bottom,
            );

            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let has_caption = (style & WS_CAPTION.0) == WS_CAPTION.0;
            let has_thick_frame = (style & WS_THICKFRAME.0) == WS_THICKFRAME.0;

            let is_game = is_fullscreen
                && is_game_heuristic(&class_name, &window_title, has_caption, has_thick_frame);

            debug!(
                is_fullscreen,
                is_game,
                title = %window_title,
                class = %class_name,
                "Foreground window fullscreen evaluation"
            );

            FullscreenStatus {
                is_fullscreen,
                is_game,
                window_title,
                class_name,
                monitor_rect: (mon_rect.left, mon_rect.top, mon_rect.right, mon_rect.bottom),
                window_rect: (win_rect.left, win_rect.top, win_rect.right, win_rect.bottom),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_fullscreen_exact_and_oversized() {
        // Exact match
        assert!(is_rect_fullscreen(0, 0, 1920, 1080, 0, 0, 1920, 1080));
        // Oversized borderless game (common with DWM negative offsets)
        assert!(is_rect_fullscreen(-2, -2, 1922, 1082, 0, 0, 1920, 1080));
        // Normal window inside monitor
        assert!(!is_rect_fullscreen(100, 100, 1200, 800, 0, 0, 1920, 1080));
        // Maximized window with taskbar remaining (bottom is 1040 instead of 1080)
        assert!(!is_rect_fullscreen(0, 0, 1920, 1040, 0, 0, 1920, 1080));
    }

    #[test]
    fn test_game_heuristics() {
        // Unreal Engine window
        assert!(is_game_heuristic(
            "UnrealWindow",
            "Game Client",
            false,
            false
        ));
        // Unity window
        assert!(is_game_heuristic("UnityWndClass", "Unity Game", true, true));
        // Borderless game without caption or resizable frame
        assert!(is_game_heuristic(
            "CustomWindowClass",
            "Indie Game",
            false,
            false
        ));
        // Standard maximized notepad / browser with caption and thick frame
        assert!(!is_game_heuristic(
            "Notepad",
            "Untitled - Notepad",
            true,
            true
        ));
        // Shell tray taskbar
        assert!(!is_game_heuristic("Shell_TrayWnd", "", false, false));
    }

    #[test]
    fn test_maximized_vs_borderless_fullscreen_distinction() {
        // 1. Standard productivity apps (Chrome, VSCode, Explorer, Notepad) maximized:
        // Even when covering the working area, they retain WS_CAPTION and WS_THICKFRAME
        assert!(!is_game_heuristic(
            "Chrome_WidgetWin_1",
            "Google Chrome",
            true,
            true
        ));
        assert!(!is_game_heuristic(
            "CabinetWClass",
            "File Explorer",
            true,
            true
        ));

        // 2. Borderless fullscreen games (e.g. Overwatch, Cyberpunk, Stremio, RetroArch):
        // They explicitly strip WS_CAPTION and WS_THICKFRAME to cover the whole screen seamlessly
        assert!(is_game_heuristic(
            "TankWindowClass", // Overwatch engine
            "Overwatch",
            false,
            false
        ));
        assert!(is_game_heuristic("SDL_App", "RetroArch", false, false));

        // 3. Maximized productivity app with taskbar hidden (auto-hide taskbar):
        // Covers full screen rect (0,0,1920,1080), but has WS_CAPTION and WS_THICKFRAME:
        let is_fullscreen_rect = is_rect_fullscreen(0, 0, 1920, 1080, 0, 0, 1920, 1080);
        let is_game = is_game_heuristic("Chrome_WidgetWin_1", "Google Chrome", true, true);
        assert!(is_fullscreen_rect);
        assert!(!is_game); // Not a game; Balanced profile won't pause wallpapers!
    }
}
