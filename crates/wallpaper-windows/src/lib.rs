//! Windows-specific integration boundary.
//!
//! Desktop attachment is intentionally not implemented yet. Windows desktop
//! hosting varies across Windows builds and must be validated with a real HWND
//! prototype before it becomes a production API.

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
    ///
    /// This provides monitor geometry only. It does not attach a wallpaper
    /// window to the desktop.
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

            if let Err(error) = GetMonitorInfoW(monitor, &mut info.monitorInfo) {
                context.error = Some(error);
                return BOOL(0);
            }

            let rect = info.monitorInfo.rcMonitor;
            let name_len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
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
            )?;
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
