//! Fullscreen application and game detection.
//!
//! Turns raw Win32 window facts into a [`ForegroundKind`] per monitor. The classification
//! rules themselves live in `wallpaper_core::runtime` (pure, unit-tested); this module only
//! gathers facts:
//!
//! * the foreground window and the monitor it is on (multi-monitor aware),
//! * the owning process image name (for logs and shell-overlay filtering),
//! * the shell's own "an exclusive Direct3D fullscreen app is running" signal
//!   (`SHQueryUserNotificationState`).
//!
//! # Windows that lose focus
//!
//! A borderless game on monitor 2 stays visible when the user clicks a window on monitor 1.
//! [`FullscreenTracker`] therefore remembers windows that qualified as maximized-or-better and
//! keeps re-inspecting them until they are closed, minimized or stop covering their monitor.
//! This avoids both resuming the wallpaper behind a still-visible game and the cost and
//! false positives of enumerating every window (cloaked UWP windows, for instance).

use tracing::debug;
use wallpaper_core::runtime::ForegroundObservation;
use wallpaper_core::ForegroundKind;

pub use wallpaper_core::runtime::{is_game_heuristic, is_rect_fullscreen};

/// Snapshot of the foreground window, kept for logging and backwards compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullscreenStatus {
    /// Covers its monitor without normal window chrome (borderless or exclusive).
    pub is_fullscreen: bool,
    /// Classified as a game / immersive app (same condition as `is_fullscreen`).
    pub is_game: bool,
    pub kind: ForegroundKind,
    /// Monitor device name, e.g. `\\.\DISPLAY1`; empty if unknown.
    pub monitor_device: String,
    pub process_name: String,
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
            kind: ForegroundKind::None,
            monitor_device: String::new(),
            process_name: String::new(),
            window_title: String::new(),
            class_name: String::new(),
            monitor_rect: (0, 0, 0, 0),
            window_rect: (0, 0, 0, 0),
        }
    }
}

/// Result of one detection pass.
#[derive(Debug, Clone, Default)]
pub struct FullscreenProbe {
    /// The current foreground window.
    pub foreground: FullscreenStatus,
    /// Strongest window per monitor (foreground plus remembered covering windows).
    pub observations: Vec<ForegroundObservation>,
}

/// Maximum number of covering windows remembered at once.
const MAX_TRACKED: usize = 16;

#[cfg(windows)]
pub use platform::FullscreenTracker;

#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct FullscreenTracker;

#[cfg(not(windows))]
impl FullscreenTracker {
    pub fn new() -> Self {
        Self
    }
    pub fn probe(&mut self) -> FullscreenProbe {
        FullscreenProbe::default()
    }
}

/// Stateless convenience probe of the current foreground window only.
pub fn check_foreground_fullscreen() -> FullscreenStatus {
    FullscreenTracker::new().probe().foreground
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::collections::HashMap;
    use wallpaper_core::runtime::{classify_foreground, Rect, WindowFacts};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONULL,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, GWL_STYLE,
        WS_CAPTION, WS_THICKFRAME,
    };

    // `SHQueryUserNotificationState` lives in shell32 and returns an HRESULT; the generated
    // bindings would need an extra crate feature, so declare it directly.
    #[link(name = "shell32")]
    extern "system" {
        fn SHQueryUserNotificationState(pquns: *mut i32) -> i32;
    }
    /// `QUNS_RUNNING_D3D_FULL_SCREEN` (verified against the `windows` crate metadata).
    const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;

    /// True when the shell reports that an exclusive Direct3D fullscreen app is running.
    fn exclusive_fullscreen_running() -> bool {
        let mut state = 0i32;
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        hr >= 0 && state == QUNS_RUNNING_D3D_FULL_SCREEN
    }

    struct Inspected {
        kind: ForegroundKind,
        device: String,
        description: String,
        status: FullscreenStatus,
    }

    fn lossy(buf: &[u16], len: i32) -> String {
        String::from_utf16_lossy(&buf[..(len.max(0) as usize).min(buf.len())])
    }

    /// Lower-cased image file name of the window's process, or empty if inaccessible.
    unsafe fn process_name(hwnd: HWND) -> String {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        full.rsplit('\\').next().unwrap_or("").to_ascii_lowercase()
    }

    fn rect(r: RECT) -> Rect {
        Rect::new(r.left, r.top, r.right, r.bottom)
    }

    /// Gathers facts for one window and classifies it. `None` if it is not a candidate
    /// (gone, hidden or minimized).
    unsafe fn inspect(hwnd: HWND, exclusive_hint: bool) -> Option<Inspected> {
        if hwnd == HWND::default()
            || !IsWindow(hwnd).as_bool()
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
        {
            return None;
        }

        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf);
        let class_name = lossy(&class_buf, class_len);

        let mut title_buf = [0u16; 256];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        let window_title = lossy(&title_buf, title_len);

        let mut win_rect = RECT::default();
        if GetWindowRect(hwnd, &mut win_rect).is_err() {
            return None;
        }

        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        if hmon.is_invalid() {
            return None;
        }
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // Pass the whole MONITORINFOEXW so the extended `szDevice` field is filled in.
        if !GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        let name_len = mi
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(mi.szDevice.len());
        let device = String::from_utf16_lossy(&mi.szDevice[..name_len]);

        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let has_caption = (style & WS_CAPTION.0) == WS_CAPTION.0;
        let has_thick_frame = (style & WS_THICKFRAME.0) == WS_THICKFRAME.0;
        let process = process_name(hwnd);

        let facts = WindowFacts {
            class_name: class_name.clone(),
            title: window_title.clone(),
            process_name: process.clone(),
            is_zoomed: IsZoomed(hwnd).as_bool(),
            has_caption,
            has_thick_frame,
            window_rect: rect(win_rect),
            monitor_rect: rect(mi.monitorInfo.rcMonitor),
            work_rect: rect(mi.monitorInfo.rcWork),
            exclusive_fullscreen_hint: exclusive_hint,
        };
        let kind = classify_foreground(&facts);

        let shown_title: String = window_title.chars().take(60).collect();
        let description = format!("{process} \"{shown_title}\" [{class_name}]");
        let mon = mi.monitorInfo.rcMonitor;
        let status = FullscreenStatus {
            is_fullscreen: kind >= ForegroundKind::BorderlessFullscreen,
            is_game: kind >= ForegroundKind::BorderlessFullscreen,
            kind,
            monitor_device: device.clone(),
            process_name: process,
            window_title,
            class_name,
            monitor_rect: (mon.left, mon.top, mon.right, mon.bottom),
            window_rect: (win_rect.left, win_rect.top, win_rect.right, win_rect.bottom),
        };

        Some(Inspected {
            kind,
            device,
            description,
            status,
        })
    }

    /// Detects covering windows across monitors, remembering ones that lose focus.
    #[derive(Debug, Default)]
    pub struct FullscreenTracker {
        /// Window handles (as integers) that qualified as `Maximized` or better.
        tracked: Vec<isize>,
    }

    impl FullscreenTracker {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn probe(&mut self) -> FullscreenProbe {
            let exclusive = exclusive_fullscreen_running();
            let foreground_hwnd = unsafe { GetForegroundWindow() };
            let foreground_id = foreground_hwnd.0 as isize;

            let mut candidates = self.tracked.clone();
            if foreground_id != 0 && !candidates.contains(&foreground_id) {
                candidates.push(foreground_id);
            }

            let mut next_tracked = Vec::new();
            let mut foreground = FullscreenStatus::default();
            let mut strongest: HashMap<String, (ForegroundKind, String)> = HashMap::new();

            for id in candidates {
                let hwnd = HWND(id as *mut core::ffi::c_void);
                let is_foreground = id == foreground_id;
                // The shell's exclusive-fullscreen signal describes the foreground app.
                let Some(info) = (unsafe { inspect(hwnd, exclusive && is_foreground) }) else {
                    continue;
                };

                if info.kind >= ForegroundKind::Maximized && next_tracked.len() < MAX_TRACKED {
                    next_tracked.push(id);
                }
                if info.kind > ForegroundKind::None {
                    let entry = strongest
                        .entry(info.device.clone())
                        .or_insert((ForegroundKind::None, String::new()));
                    if info.kind > entry.0 {
                        *entry = (info.kind, info.description.clone());
                    }
                }
                if is_foreground {
                    debug!(
                        kind = ?info.kind,
                        device = %info.device,
                        window = %info.description,
                        "Foreground window evaluation"
                    );
                    foreground = info.status;
                }
            }

            self.tracked = next_tracked;

            let mut observations: Vec<ForegroundObservation> = strongest
                .into_iter()
                .map(|(device, (kind, description))| ForegroundObservation {
                    device,
                    kind,
                    description,
                })
                .collect();
            observations.sort_by(|a, b| a.device.cmp(&b.device));

            FullscreenProbe {
                foreground,
                observations,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_inert() {
        let s = FullscreenStatus::default();
        assert!(!s.is_fullscreen && !s.is_game);
        assert_eq!(s.kind, ForegroundKind::None);
    }

    #[test]
    fn rect_and_heuristic_helpers_remain_available() {
        assert!(is_rect_fullscreen(0, 0, 1920, 1080, 0, 0, 1920, 1080));
        assert!(!is_rect_fullscreen(0, 0, 1920, 1040, 0, 0, 1920, 1080));
        assert!(is_game_heuristic("SDL_App", "RetroArch", false, false));
        assert!(!is_game_heuristic(
            "Chrome_WidgetWin_1",
            "Google Chrome",
            true,
            true
        ));
        assert!(!is_game_heuristic("Shell_TrayWnd", "", false, false));
    }

    /// Smoke test: probing the live desktop must never panic and must be self-consistent.
    #[test]
    fn probe_is_consistent_on_the_live_desktop() {
        let mut tracker = FullscreenTracker::new();
        for _ in 0..3 {
            let probe = tracker.probe();
            for o in &probe.observations {
                assert!(o.kind > ForegroundKind::None);
            }
        }
    }
}
