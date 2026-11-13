//! Windows-specific integration boundary.
//!
//! Provides monitor enumeration, desktop host (`WorkerW`/`Progman`) discovery,
//! native Win32 wallpaper window orchestration, and embedded MPV video playback.

pub mod desktop_host;
pub mod events;
pub mod fullscreen;
pub mod manager;
pub mod mpv;
pub mod policy;
pub mod window;

pub use desktop_host::{ensure_desktop_host, DesktopHost, DesktopHostDiagnostics, DesktopHostKind};
pub use events::{SystemEventListener, SystemPlatformEvent};
pub use fullscreen::{check_foreground_fullscreen, FullscreenStatus};
pub use manager::{WallpaperManager, WallpaperManagerError};
pub use mpv::{find_mpv_executable, MpvError, MpvLaunchConfig, MpvPlayer};
pub use policy::{PlaybackSupervisor, PlayerSuspensionManager, ProcessBackoff, StateDebouncer};
pub use window::WallpaperWindow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[cfg(windows)]
mod platform {
    use super::MonitorInfo;
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };

    /// Enumerates active Windows display monitors.
    pub fn enumerate_monitors() -> windows::core::Result<Vec<MonitorInfo>> {
        struct Context {
            monitors: Vec<MonitorInfo>,
            error: Option<windows::core::Error>,
        }

        unsafe extern "system" fn callback(
            monitor: HMONITOR,
            _dc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            let context = &mut *(data.0 as *mut Context);
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

            if let Err(error) = GetMonitorInfoW(monitor, &mut info.monitorInfo).ok() {
                context.error = Some(error);
                return BOOL(0);
            }

            let rect = info.monitorInfo.rcMonitor;
            let name_len = info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len());
            let device_name = String::from_utf16_lossy(&info.szDevice[..name_len]);

            context.monitors.push(MonitorInfo {
                device_name,
                x: rect.left,
                y: rect.top,
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
                is_primary: (info.monitorInfo.dwFlags & 1) != 0,
            });

            BOOL(1)
        }

        let mut context = Context {
            monitors: Vec::new(),
            error: None,
        };

        unsafe {
            EnumDisplayMonitors(
                HDC::default(),
                None,
                Some(callback),
                LPARAM(&mut context as *mut Context as isize),
            )
            .ok()?;
        }

        if let Some(error) = context.error {
            return Err(error);
        }

        Ok(context.monitors)
    }
}

#[cfg(windows)]
pub use platform::enumerate_monitors;

#[cfg(not(windows))]
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, &'static str> {
    Err("Windows monitor enumeration is available only on Windows")
}
