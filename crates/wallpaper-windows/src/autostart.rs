//! Windows startup / autostart management using the current user's Run registry key.
//!
//! Registers the application in `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`
//! so that wallpaper playback automatically begins upon Windows login without requiring
//! administrator privileges.

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "RustWallpaper";

/// Returns `true` if Rust Wallpaper is registered to start automatically on Windows boot.
#[cfg(windows)]
pub fn is_autostart_enabled() -> bool {
    get_autostart_command().is_some()
}

#[cfg(not(windows))]
pub fn is_autostart_enabled() -> bool {
    false
}

/// Returns the registered startup command line if autostart is active.
#[cfg(windows)]
pub fn get_autostart_command() -> Option<String> {
    let wide_key: Vec<u16> = std::ffi::OsStr::new(RUN_KEY)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let wide_val: Vec<u16> = std::ffi::OsStr::new(APP_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut hkey = HKEY::default();
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wide_key.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return None;
        }

        let mut buf = [0u16; 1024];
        let mut buf_len = (buf.len() * 2) as u32;
        let mut val_type = REG_SZ;

        let status = RegQueryValueExW(
            hkey,
            PCWSTR(wide_val.as_ptr()),
            None,
            Some(&mut val_type),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut buf_len),
        );

        let _ = RegCloseKey(hkey);

        if status.is_ok() && val_type == REG_SZ {
            let str_len = (buf_len / 2) as usize;
            let slice = if str_len > 0 && buf[str_len - 1] == 0 {
                &buf[..str_len - 1]
            } else {
                &buf[..str_len]
            };
            Some(String::from_utf16_lossy(slice))
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
pub fn get_autostart_command() -> Option<String> {
    None
}

/// Enables or disables automatic startup with Windows.
///
/// When `enabled` is `true`, it registers either `wallpaper-daemon.exe` or
/// `wallpaper-ui.exe --daemon` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
#[cfg(windows)]
pub fn set_autostart(enabled: bool) -> Result<(), std::io::Error> {
    let wide_key: Vec<u16> = std::ffi::OsStr::new(RUN_KEY)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let wide_val: Vec<u16> = std::ffi::OsStr::new(APP_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut hkey = HKEY::default();
    unsafe {
        if enabled {
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide_key.as_ptr()),
                0,
                KEY_WRITE,
                &mut hkey,
            )
            .is_err()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Failed to open registry Run key for writing",
                ));
            }

            let current_exe = std::env::current_exe()?;
            let daemon_exe = current_exe.parent().map(|p| p.join("wallpaper-daemon.exe"));
            let command = if let Some(daemon) = daemon_exe.filter(|d| d.is_file()) {
                format!("\"{}\"", daemon.display())
            } else {
                format!("\"{}\" --daemon", current_exe.display())
            };

            let wide_cmd: Vec<u16> = std::ffi::OsStr::new(&command)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let byte_slice: &[u8] =
                std::slice::from_raw_parts(wide_cmd.as_ptr() as *const u8, wide_cmd.len() * 2);

            let status =
                RegSetValueExW(hkey, PCWSTR(wide_val.as_ptr()), 0, REG_SZ, Some(byte_slice));

            let _ = RegCloseKey(hkey);

            if status.is_err() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to write registry value: {:?}", status),
                ));
            }
        } else {
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide_key.as_ptr()),
                0,
                KEY_WRITE,
                &mut hkey,
            )
            .is_err()
            {
                return Ok(()); // Key doesn't exist, already disabled
            }

            let _ = RegDeleteValueW(hkey, PCWSTR(wide_val.as_ptr()));
            let _ = RegCloseKey(hkey);
        }
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Autostart is only supported on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autostart_query_does_not_panic() {
        let _ = is_autostart_enabled();
        let _ = get_autostart_command();
    }
}
