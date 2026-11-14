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
pub use events::{ListenerPoll, SystemEventListener, SystemPlatformEvent};
pub use fullscreen::{
    check_foreground_fullscreen, FullscreenProbe, FullscreenStatus, FullscreenTracker,
};
pub use manager::{WallpaperManager, WallpaperManagerError};
pub use mpv::{find_mpv_executable, MpvError, MpvLaunchConfig, MpvPlayer};
pub use policy::{
    ActualPlayback, ControlCommand, ForegroundObservation, MonitorStatus, PendingWork,
    PlaybackSink, PlaybackSupervisor, PlayerCommand, PlayerSuspensionManager, ProcessBackoff,
    RuntimeEvent, StateDebouncer,
};
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

/// A monitor plus its hardware identity, for tracking monitors across topology changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDetails {
    pub info: MonitorInfo,
    /// Hardware device-interface path from `EnumDisplayDevices`, when Windows provides one.
    pub interface_path: Option<String>,
}

impl MonitorDetails {
    /// Converts into the platform-neutral descriptor used by the runtime.
    pub fn into_descriptor(self) -> wallpaper_core::monitors::MonitorDescriptor {
        use wallpaper_core::monitors::{MonitorDescriptor, MonitorId};
        let id = MonitorId::from_interface_path(
            self.interface_path.as_deref().unwrap_or(""),
            &self.info.device_name,
        );
        MonitorDescriptor {
            id,
            device_name: self.info.device_name,
            x: self.info.x,
            y: self.info.y,
            width: self.info.width,
            height: self.info.height,
            is_primary: self.info.is_primary,
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::MonitorInfo;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
        MONITORINFOEXW,
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

    /// `EDD_GET_DEVICE_INTERFACE_NAME`: ask for the monitor's device-interface path.
    const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 0x0000_0001;
    /// `DISPLAY_DEVICE_ACTIVE`.
    const DISPLAY_DEVICE_ACTIVE: u32 = 0x0000_0001;

    /// Hardware device-interface path of the monitor attached to `adapter_output`
    /// (e.g. `\\.\DISPLAY1`), or `None` if Windows does not report one.
    fn interface_path(adapter_output: &str) -> Option<String> {
        let wide: Vec<u16> = adapter_output
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        for index in 0..8u32 {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let ok = unsafe {
                EnumDisplayDevicesW(
                    PCWSTR(wide.as_ptr()),
                    index,
                    &mut dd,
                    EDD_GET_DEVICE_INTERFACE_NAME,
                )
            };
            if !ok.as_bool() {
                return None;
            }
            if dd.StateFlags & DISPLAY_DEVICE_ACTIVE == 0 {
                continue;
            }
            let len = dd
                .DeviceID
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(dd.DeviceID.len());
            let path = String::from_utf16_lossy(&dd.DeviceID[..len]);
            return (!path.trim().is_empty()).then_some(path);
        }
        None
    }

    /// Enumerates active monitors together with their stable hardware identity.
    pub fn enumerate_monitor_details() -> windows::core::Result<Vec<super::MonitorDetails>> {
        Ok(enumerate_monitors()?
            .into_iter()
            .map(|info| {
                let interface_path = interface_path(&info.device_name);
                super::MonitorDetails {
                    info,
                    interface_path,
                }
            })
            .collect())
    }
}

#[cfg(windows)]
pub use platform::{enumerate_monitor_details, enumerate_monitors};

#[cfg(not(windows))]
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, &'static str> {
    Err("Windows monitor enumeration is available only on Windows")
}

#[cfg(not(windows))]
pub fn enumerate_monitor_details() -> Result<Vec<MonitorDetails>, &'static str> {
    Err("Windows monitor enumeration is available only on Windows")
}
