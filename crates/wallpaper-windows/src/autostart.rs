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

/// Chooses the installed UI executable so autostart includes the tray icon.
fn autostart_command() -> Result<String, std::io::Error> {
    let current_exe = std::env::current_exe()?;
    let directory = current_exe
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for name in ["RustWallpaper.exe", "wallpaper-ui.exe"] {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(format!("\"{}\" --tray", candidate.display()));
        }
    }
    let daemon = directory.join("wallpaper-daemon.exe");
    if daemon.is_file() {
        return Ok(format!("\"{}\"", daemon.display()));
    }
    Ok(format!("\"{}\" --tray", current_exe.display()))
}

/// Writes an explicit command to the current user's startup registration.
#[cfg(windows)]
pub fn set_autostart_command(enabled: bool, command: Option<&str>) -> Result<(), std::io::Error> {
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

            let command = command
                .map(str::to_owned)
                .map(Ok)
                .unwrap_or_else(autostart_command)?;
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
                return Err(std::io::Error::other(format!(
                    "Failed to write registry value: {:?}",
                    status
                )));
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

/// Registers the installed UI so Windows starts it hidden in the notification area.
#[cfg(windows)]
pub fn set_autostart_for(executable: &std::path::Path) -> Result<(), std::io::Error> {
    let command = format!("\"{}\" --tray", executable.display());
    set_autostart_command(true, Some(&command))
}

#[cfg(windows)]
pub fn set_autostart(enabled: bool) -> Result<(), std::io::Error> {
    set_autostart_command(enabled, None)
}

#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Autostart is only supported on Windows",
    ))
}

#[cfg(not(windows))]
pub fn set_autostart_for(_executable: &std::path::Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Autostart is only supported on Windows",
    ))
}

#[cfg(not(windows))]
pub fn set_autostart_command(_enabled: bool, _command: Option<&str>) -> Result<(), std::io::Error> {
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
