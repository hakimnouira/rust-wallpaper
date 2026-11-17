#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let is_silent = args
        .iter()
        .any(|a| a == "/S" || a == "--silent" || a == "-s");

    if !is_silent {
        #[cfg(windows)]
        {
            use windows::core::w;
            use windows::Win32::UI::WindowsAndMessaging::{
                MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO,
            };
            let ret = unsafe {
                MessageBoxW(
                    None,
                    w!("Are you sure you want to completely remove Rust Wallpaper and all its components?"),
                    w!("Rust Wallpaper Uninstall"),
                    MB_YESNO | MB_ICONQUESTION,
                )
            };
            if ret != IDYES {
                return;
            }
        }
    }

    // 1. Terminate running application processes if any
    let _ = std::process::Command::new("taskkill")
        .args([
            "/F",
            "/IM",
            "RustWallpaper.exe",
            "/IM",
            "wallpaper-ui.exe",
            "/IM",
            "wallpaper-daemon.exe",
        ])
        .output();

    // 2. Remove desktop shortcut
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let desktop_lnk = PathBuf::from(&userprofile)
            .join("Desktop")
            .join("Rust Wallpaper.lnk");
        let _ = std::fs::remove_file(desktop_lnk);
    }

    // 3. Remove Start Menu shortcut
    if let Ok(appdata) = std::env::var("APPDATA") {
        let start_lnk = PathBuf::from(&appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Rust Wallpaper.lnk");
        let _ = std::fs::remove_file(start_lnk);
    }

    // 4. Remove Windows Registry entries
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{
            RegDeleteKeyW, RegDeleteValueW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
        };

        // Remove from Run key
        unsafe {
            let mut hkey = windows::Win32::System::Registry::HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            )
            .is_ok()
            {
                let _ = RegDeleteValueW(hkey, w!("RustWallpaper"));
                let _ = windows::Win32::System::Registry::RegCloseKey(hkey);
            }
        }

        // Remove from Uninstall key
        unsafe {
            let mut hkey_uninstall = windows::Win32::System::Registry::HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall"),
                0,
                KEY_SET_VALUE,
                &mut hkey_uninstall,
            )
            .is_ok()
            {
                let _ = RegDeleteKeyW(hkey_uninstall, w!("RustWallpaper"));
                let _ = windows::Win32::System::Registry::RegCloseKey(hkey_uninstall);
            }
        }

        // Unregister browser extension keys
        wallpaper_windows::unregister_browser_extension();
    }

    // 5. Schedule directory deletion on exit
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(install_dir) = current_exe.parent() {
            let dir_str = install_dir.to_string_lossy().to_string();
            let _ = std::process::Command::new("cmd.exe")
                .args([
                    "/c",
                    &format!("timeout /t 1 /nobreak >nul & rmdir /s /q \"{}\"", dir_str),
                ])
                .spawn();
        }
    }

    if !is_silent {
        #[cfg(windows)]
        {
            use windows::core::w;
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
            unsafe {
                MessageBoxW(
                    None,
                    w!("Rust Wallpaper was successfully removed from your computer."),
                    w!("Rust Wallpaper Uninstall"),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        }
    }
}
