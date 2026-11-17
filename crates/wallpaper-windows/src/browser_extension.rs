//! Browser extension registry integration for Chromium browsers (Chrome, Edge, Brave, Opera).
//!
//! Configures the Windows Registry so that when users launch their browser,
//! Chromium detects the external extension and displays the native prompt:
//! "'RustWallpaper Companion' was added by another program on your computer. [Enable extension]"

use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

pub const EXTENSION_ID: &str = "gemjknbkapmdcopknokbikcneknbpoij";
pub const EXTENSION_VERSION: &str = "1.0.0";
pub const UPDATE_URL: &str = "http://127.0.0.1:24242/extension/update.xml";

const BROWSER_REG_KEYS: &[&str] = &[
    r"Software\Google\Chrome\Extensions",
    r"Software\Microsoft\Edge\Extensions",
    r"Software\BraveSoftware\Brave-Browser\Extensions",
];

const BROWSER_POLICY_KEYS: &[&str] = &[
    r"Software\Policies\Google\Chrome\ExtensionSettings",
    r"Software\Policies\Microsoft\Edge\ExtensionSettings",
    r"Software\Policies\BraveSoftware\Brave-Browser\ExtensionSettings",
];

/// Registers the RustWallpaper Companion extension in the current user's registry.
pub fn register_browser_extension(custom_crx_path: Option<&Path>) -> Result<(), String> {
    #[cfg(windows)]
    {
        let crx_path = match custom_crx_path {
            Some(p) if p.is_file() => std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
            _ => find_crx_candidate().ok_or_else(|| {
                "Could not find chrome.crx package to register with browsers".to_string()
            })?,
        };

        let mut crx_str = crx_path.to_string_lossy().to_string();
        if let Some(stripped) = crx_str.strip_prefix(r"\\?\") {
            crx_str = stripped.to_string();
        }

        // 1. Register in standard Chromium External Extensions
        for base_key in BROWSER_REG_KEYS {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");
            if let Err(e) = set_reg_values(
                &full_key,
                &[
                    ("path", &crx_str),
                    ("version", EXTENSION_VERSION),
                    ("update_url", UPDATE_URL),
                ],
            ) {
                warn!(%full_key, %e, "Failed to write browser external extension key");
            } else {
                info!(%full_key, "Successfully registered browser external extension");
            }
        }

        // 2. Register in Chromium Policy ExtensionSettings (enables prompt even on strict channels)
        for base_key in BROWSER_POLICY_KEYS {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");
            if let Err(e) = set_reg_values(
                &full_key,
                &[
                    ("installation_mode", "normal_installed"),
                    ("update_url", UPDATE_URL),
                ],
            ) {
                warn!(%full_key, %e, "Failed to write browser extension policy key");
            } else {
                info!(%full_key, "Successfully registered browser extension policy");
            }
        }

        Ok(())
    }

    #[cfg(not(windows))]
    {
        Err("Browser extension registration is only supported on Windows".into())
    }
}

/// Checks whether the browser extension is registered in any supported browser registry.
pub fn is_browser_extension_registered() -> bool {
    #[cfg(windows)]
    {
        for base_key in BROWSER_REG_KEYS {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");
            let wide_key: Vec<u16> = std::ffi::OsStr::new(&full_key)
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
                .is_ok()
                {
                    let _ = RegCloseKey(hkey);
                    return true;
                }
            }
        }
        false
    }

    #[cfg(not(windows))]
    {
        false
    }
}

/// Unregisters the browser extension from all supported browser registry keys.
pub fn unregister_browser_extension() {
    #[cfg(windows)]
    {
        for base_key in BROWSER_REG_KEYS.iter().chain(BROWSER_POLICY_KEYS.iter()) {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");
            let wide_key: Vec<u16> = std::ffi::OsStr::new(&full_key)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide_key.as_ptr()));
            }
        }
    }
}

/// Attempts to locate the bundled or installed `chrome.crx` package.
pub fn find_crx_candidate() -> Option<PathBuf> {
    // 1. Next to current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for sub in [
                "extension/chrome.crx",
                "chrome.crx",
                "RustWallpaper-Extension.crx",
            ] {
                let p = parent.join(sub);
                if p.is_file() {
                    return std::fs::canonicalize(&p).ok().or(Some(p));
                }
            }
        }
    }

    // 2. Relative to working directory (development & test paths)
    for dir in [
        "extensions",
        "../../extensions",
        "dist/extension",
        "../../dist/extension",
        "dist",
        "../../dist",
        "target/release/extension",
        "target/debug/extension",
    ] {
        let p = PathBuf::from(dir).join("chrome.crx");
        if p.is_file() {
            return std::fs::canonicalize(&p).ok().or(Some(p));
        }
    }

    None
}

#[cfg(windows)]
fn set_reg_values(subkey: &str, values: &[(&str, &str)]) -> Result<(), String> {
    let wide_subkey: Vec<u16> = std::ffi::OsStr::new(subkey)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut hkey = HKEY::default();
    unsafe {
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wide_subkey.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_err()
        {
            return Err(format!("Failed to create registry key: {subkey}"));
        }

        for (val_name, val_data) in values {
            let wide_name: Vec<u16> = std::ffi::OsStr::new(val_name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let wide_data: Vec<u16> = std::ffi::OsStr::new(val_data)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let bytes =
                std::slice::from_raw_parts(wide_data.as_ptr() as *const u8, wide_data.len() * 2);

            let _ = RegSetValueExW(hkey, PCWSTR(wide_name.as_ptr()), 0, REG_SZ, Some(bytes));
        }

        let _ = RegCloseKey(hkey);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_extension_lifecycle() {
        // Test registering candidate CRX
        let crx = find_crx_candidate();
        assert!(crx.is_some(), "CRX candidate should be found in workspace");

        let res = register_browser_extension(None);
        assert!(res.is_ok(), "Registration should succeed: {:?}", res.err());

        assert!(
            is_browser_extension_registered(),
            "Extension should be detected as registered"
        );
    }
}
