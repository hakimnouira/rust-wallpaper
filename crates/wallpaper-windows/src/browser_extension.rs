//! Browser extension registry integration for Chromium browsers (Chrome, Edge, Brave, Opera, Vivaldi).
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
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

pub const EXTENSION_ID: &str = "gemjknbkapmdcopknokbikcneknbpoij";
pub const EXTENSION_VERSION: &str = "1.0.0";
/// The update URL used in HKCU/HKLM External Extensions keys.
/// NOTE: for the policy ExtensionInstallForcelist we use a file:// URL that points
/// directly to the CRX so Chrome/Brave accept it without a live HTTP server.
pub const UPDATE_URL: &str = "http://127.0.0.1:24242/extension/update.xml";
pub const UPDATE_URL_FALLBACK: &str = "https://clients2.google.com/service/update2/crx";

const BROWSER_REG_KEYS: &[&str] = &[
    r"Software\Google\Chrome\Extensions",
    r"Software\Microsoft\Edge\Extensions",
    r"Software\BraveSoftware\Brave-Browser\Extensions",
    r"Software\BraveSoftware\Brave\Extensions",
    r"Software\Vivaldi\Extensions",
    r"Software\Opera Software\Opera Stable\Extensions",
    r"Software\Opera Software\Opera GX Stable\Extensions",
    r"Software\WOW6432Node\Google\Chrome\Extensions",
    r"Software\WOW6432Node\Microsoft\Edge\Extensions",
    r"Software\WOW6432Node\BraveSoftware\Brave-Browser\Extensions",
    r"Software\WOW6432Node\BraveSoftware\Brave\Extensions",
];

const BROWSER_POLICY_ROOTS: &[&str] = &[
    r"Software\Policies\Google\Chrome",
    r"Software\Policies\Microsoft\Edge",
    r"Software\Policies\BraveSoftware\Brave",
    r"Software\Policies\BraveSoftware\Brave-Browser",
];

/// Copies the CRX to canonical LocalAppData and prepares the local update.xml manifest.
/// Returns the canonical CRX path.
pub fn ensure_canonical_crx(source_crx: &Path) -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_fallback().unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\RustWallpaper"))
        });

    let target_dir = local_app_data.join("RustWallpaper").join("extension");
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        warn!(%e, "Could not create canonical extension directory, using source directly");
        return Ok(source_crx.to_path_buf());
    }

    let target_crx = target_dir.join("chrome.crx");
    if source_crx != target_crx {
        if let Err(e) = std::fs::copy(source_crx, &target_crx) {
            warn!(%e, "Failed to copy CRX to canonical path, falling back to source");
            return Ok(source_crx.to_path_buf());
        }
    }

    // Write local update.xml for the HTTP bridge (used by HKCU External Extensions keys only)
    let update_xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n\
<gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>\n  \
  <app appid='{EXTENSION_ID}'>\n    \
    <updatecheck codebase='http://127.0.0.1:24242/extension/rust-wallpaper.crx' version='{EXTENSION_VERSION}' />\n  \
  </app>\n\
</gupdate>\n"
    );
    let target_xml = target_dir.join("update.xml");
    let _ = std::fs::write(target_xml, update_xml);

    Ok(target_crx)
}

/// Builds a `file:///` update URL from a CRX path.
/// Chrome/Brave accept file:// URLs in ExtensionInstallForcelist when no web store is used.
fn file_update_url(crx_path: &Path) -> String {
    // Convert Windows path to file:// URI:  C:\foo\bar.crx → file:///C:/foo/bar.crx
    let forward = crx_path.to_string_lossy().replace('\\', "/");
    // Strip \\?\ prefix added by canonicalize
    let forward = forward.trim_start_matches("//?/");
    format!("file:///{forward}")
}

fn dirs_fallback() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local"))
}

/// Registers the RustWallpaper Companion extension in both current user and machine registry.
pub fn register_browser_extension(custom_crx_path: Option<&Path>) -> Result<(), String> {
    #[cfg(windows)]
    {
        let raw_crx = match custom_crx_path {
            Some(p) if p.is_file() => std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
            _ => find_crx_candidate().ok_or_else(|| {
                "Could not find chrome.crx package to register with browsers".to_string()
            })?,
        };

        let crx_path = ensure_canonical_crx(&raw_crx).unwrap_or(raw_crx);

        let mut crx_str = crx_path.to_string_lossy().to_string();
        if let Some(stripped) = crx_str.strip_prefix(r"\\?\") {
            crx_str = stripped.to_string();
        }

        // Build the file:// URL for policy-based forced installation.
        // Chrome/Brave reject localhost update_urls in ExtensionInstallForcelist;
        // a file:// pointing directly to the CRX is accepted without a live server.
        let policy_update_url = file_update_url(&crx_path);

        let mut registered_browsers = 0;
        let mut errors = Vec::new();

        // 1. Register in standard Chromium External Extensions (both HKCU and HKLM)
        //    These use the HTTP update_url for version checks once installed.
        for base_key in BROWSER_REG_KEYS {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");

            // Try HKCU
            match set_reg_values(
                HKEY_CURRENT_USER,
                &full_key,
                &[
                    ("path", &crx_str),
                    ("version", EXTENSION_VERSION),
                    ("update_url", UPDATE_URL),
                ],
            ) {
                Ok(()) => {
                    registered_browsers += 1;
                    info!(%full_key, "Successfully registered browser external extension in HKCU");
                }
                Err(error) => {
                    warn!(%full_key, %error, "Failed to write browser external extension key in HKCU");
                    errors.push(format!("HKCU\\{full_key}: {error}"));
                }
            }

            // Try HKLM (succeeds when running with Administrator privileges or in installer)
            let _ = set_reg_values(
                HKEY_LOCAL_MACHINE,
                &full_key,
                &[
                    ("path", &crx_str),
                    ("version", EXTENSION_VERSION),
                    ("update_url", UPDATE_URL),
                ],
            );
        }

        // 2. Configure Chromium Enterprise Policies (ExtensionSettings, Forcelist, Sources)
        // CRITICAL: Use file:// URL in policy keys — Chrome/Brave reject localhost update URLs
        // in ExtensionInstallForcelist. file:// points directly to the CRX on disk.
        let settings_json = format!(
            r#"{{"{EXTENSION_ID}":{{"installation_mode":"normal_installed","update_url":"{policy_update_url}"}}}}"#
        );

        for policy_root in BROWSER_POLICY_ROOTS {
            for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
                // ExtensionSettings JSON — preferred modern policy key
                let _ = set_reg_values(root, policy_root, &[("ExtensionSettings", &settings_json)]);

                // ExtensionInstallForcelist — legacy key, still works in all Chromium browsers
                // Format: "1" = "<extension-id>;<update-url>"
                let forcelist_key = format!(r"{policy_root}\ExtensionInstallForcelist");
                let forcelist_val = format!("{EXTENSION_ID};{policy_update_url}");
                let _ = set_reg_values(root, &forcelist_key, &[("1", &forcelist_val)]);

                // ExtensionInstallSources: whitelist file:// and localhost origins
                let sources_key = format!(r"{policy_root}\ExtensionInstallSources");
                let _ = set_reg_values(
                    root,
                    &sources_key,
                    &[
                        ("1", "file:///*"),
                        ("2", "http://127.0.0.1:24242/*"),
                        ("3", "<all_urls>"),
                    ],
                );
            }
        }

        if registered_browsers == 0 {
            return Err(format!(
                "Could not register the extension in any supported browser: {}",
                errors.join("; ")
            ));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        Err("Browser extension registration is only supported on Windows".into())
    }
}

/// Triggers elevated registration via UAC so machine-wide HKLM keys and enterprise policies are applied.
pub fn register_browser_extension_elevated() -> Result<(), String> {
    #[cfg(windows)]
    {
        // First register current user keys
        let _ = register_browser_extension(None);

        let crx = find_crx_candidate()
            .map(|p| ensure_canonical_crx(&p).unwrap_or(p))
            .ok_or_else(|| "Could not locate chrome.crx package".to_string())?;

        let crx_str = crx.to_string_lossy().replace('\\', "\\\\");

        let script = format!(
            r#"$extId = '{EXTENSION_ID}'; $crx = '{crx_str}'; $upd = '{UPDATE_URL}';
$hklmPaths = @('Software\Google\Chrome\Extensions', 'Software\Microsoft\Edge\Extensions', 'Software\BraveSoftware\Brave-Browser\Extensions', 'Software\BraveSoftware\Brave\Extensions', 'Software\WOW6432Node\Google\Chrome\Extensions', 'Software\WOW6432Node\Microsoft\Edge\Extensions', 'Software\WOW6432Node\BraveSoftware\Brave-Browser\Extensions', 'Software\WOW6432Node\BraveSoftware\Brave\Extensions');
foreach ($p in $hklmPaths) {{
    $k = "HKLM:\$p\$extId";
    New-Item -Path $k -Force | Out-Null;
    Set-ItemProperty -Path $k -Name 'path' -Value $crx;
    Set-ItemProperty -Path $k -Name 'version' -Value '1.0.0';
    Set-ItemProperty -Path $k -Name 'update_url' -Value $upd;
}}
$policyRoots = @('Software\Policies\Google\Chrome', 'Software\Policies\Microsoft\Edge', 'Software\Policies\BraveSoftware\Brave', 'Software\Policies\BraveSoftware\Brave-Browser');
foreach ($pr in $policyRoots) {{
    $fk = "HKLM:\$pr\ExtensionInstallForcelist";
    New-Item -Path $fk -Force | Out-Null;
    Set-ItemProperty -Path $fk -Name '1' -Value "$extId;$upd";
    $sk = "HKLM:\$pr\ExtensionInstallSources";
    New-Item -Path $sk -Force | Out-Null;
    Set-ItemProperty -Path $sk -Name '1' -Value 'http://127.0.0.1:24242/*';
    Set-ItemProperty -Path $sk -Name '2' -Value '<all_urls>';
    New-Item -Path "HKLM:\$pr" -Force | Out-Null;
    Set-ItemProperty -Path "HKLM:\$pr" -Name 'ExtensionSettings' -Value ('{{\"' + $extId + '\":{{\"installation_mode\":\"normal_installed\",\"update_url\":\"' + $upd + '\"}}}}');
}}
"#
        );

        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &format!(
                    "Start-Process powershell -WindowStyle Hidden -Verb RunAs -ArgumentList '-NoProfile -Command \"{}\"'",
                    script.replace('"', "\\\"")
                ),
            ])
            .status()
            .map_err(|e| format!("Failed to spawn elevated PowerShell: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err("Elevation request was rejected or failed".into())
        }
    }

    #[cfg(not(windows))]
    {
        Err("Elevated registration is only supported on Windows".into())
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

                if RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
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
        for base_key in BROWSER_REG_KEYS {
            let full_key = format!(r"{base_key}\{EXTENSION_ID}");
            let wide_key: Vec<u16> = std::ffi::OsStr::new(&full_key)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide_key.as_ptr()));
                let _ = RegDeleteKeyW(HKEY_LOCAL_MACHINE, PCWSTR(wide_key.as_ptr()));
            }
        }

        for policy_root in BROWSER_POLICY_ROOTS {
            let forcelist = format!(r"{policy_root}\ExtensionInstallForcelist");
            let wide_forcelist: Vec<u16> = std::ffi::OsStr::new(&forcelist)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let sources = format!(r"{policy_root}\ExtensionInstallSources");
            let wide_sources: Vec<u16> = std::ffi::OsStr::new(&sources)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide_forcelist.as_ptr()));
                let _ = RegDeleteKeyW(HKEY_LOCAL_MACHINE, PCWSTR(wide_forcelist.as_ptr()));
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide_sources.as_ptr()));
                let _ = RegDeleteKeyW(HKEY_LOCAL_MACHINE, PCWSTR(wide_sources.as_ptr()));
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
                "extensions/chrome.crx",
            ] {
                let p = parent.join(sub);
                if p.is_file() {
                    return std::fs::canonicalize(&p).ok().or(Some(p));
                }
            }
        }
    }

    // 2. Canonical LocalAppData path
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local_app_data)
            .join("RustWallpaper")
            .join("extension")
            .join("chrome.crx");
        if p.is_file() {
            return std::fs::canonicalize(&p).ok().or(Some(p));
        }
    }

    // 3. Relative to working directory (development & test paths)
    for dir in [
        "extensions",
        "../../extensions",
        "dist/extension",
        "../../dist/extension",
        "dist/extensions",
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
fn set_reg_values(root: HKEY, subkey: &str, values: &[(&str, &str)]) -> Result<(), String> {
    let wide_subkey: Vec<u16> = std::ffi::OsStr::new(subkey)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut hkey = HKEY::default();
    unsafe {
        if RegCreateKeyExW(
            root,
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

            let status = RegSetValueExW(hkey, PCWSTR(wide_name.as_ptr()), 0, REG_SZ, Some(bytes));
            if status.0 != 0 {
                let _ = RegCloseKey(hkey);
                return Err(format!(
                    "Failed to write registry value '{val_name}': Windows error {}",
                    status.0
                ));
            }
        }

        let _ = RegCloseKey(hkey);
    }

    Ok(())
}

/// Finds the candidate directory containing the unpacked extension (manifest.json).
pub fn find_unpacked_candidate() -> Option<PathBuf> {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local_app_data)
            .join("RustWallpaper")
            .join("extension")
            .join("unpacked");
        if p.join("manifest.json").is_file() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for sub in [
                "extension",
                "extensions/chrome",
                "dist/extension",
                "dist/extensions/chrome",
            ] {
                let p = parent.join(sub);
                if p.join("manifest.json").is_file() {
                    return std::fs::canonicalize(&p).ok().or(Some(p));
                }
            }
        }
    }

    for dir in [
        "extensions/chrome",
        "../../extensions/chrome",
        "dist/extension",
        "../../dist/extension",
    ] {
        let p = PathBuf::from(dir);
        if p.join("manifest.json").is_file() {
            return std::fs::canonicalize(&p).ok().or(Some(p));
        }
    }

    None
}

/// Copies the unpacked extension to `%LOCALAPPDATA%\RustWallpaper\extension\unpacked`.
pub fn ensure_canonical_unpacked() -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_fallback().unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\RustWallpaper"))
        });

    let target_dir = local_app_data
        .join("RustWallpaper")
        .join("extension")
        .join("unpacked");
    let _ = std::fs::create_dir_all(&target_dir);

    if let Some(source) = find_unpacked_candidate() {
        if source != target_dir {
            if let Ok(entries) = std::fs::read_dir(&source) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let dest = target_dir.join(entry.file_name());
                    if path.is_dir() {
                        let _ = std::fs::create_dir_all(&dest);
                        if let Ok(subentries) = std::fs::read_dir(&path) {
                            for sub in subentries.flatten() {
                                let _ = std::fs::copy(sub.path(), dest.join(sub.file_name()));
                            }
                        }
                    } else {
                        let _ = std::fs::copy(&path, &dest);
                    }
                }
            }
        }
    }

    if target_dir.join("manifest.json").is_file() {
        Ok(target_dir)
    } else {
        Err("Unpacked extension manifest.json not found".into())
    }
}

/// Finds the primary installed Chromium browser (Brave, Chrome, Edge).
pub fn find_installed_browser() -> Option<PathBuf> {
    let candidates = [
        r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];

    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        for sub in [
            r"BraveSoftware\Brave-Browser\Application\brave.exe",
            r"Google\Chrome\Application\chrome.exe",
        ] {
            let p = PathBuf::from(&local_app_data).join(sub);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

/// Injects `--load-extension="<unpacked_path>"` into all user browser shortcuts (Desktop, Taskbar, Start Menu).
pub fn enable_browser_shortcut_integration() -> Result<usize, String> {
    #[cfg(windows)]
    {
        let unpacked = ensure_canonical_unpacked()?;
        let unpacked_str = unpacked.to_string_lossy().replace('\\', "\\\\");

        let ps_script = format!(
            r#"$extPath = '{unpacked_str}';
$sh = New-Object -ComObject WScript.Shell;
$count = 0;
$searchDirs = @(
    "$([Environment]::GetFolderPath('Desktop'))",
    "$env:PUBLIC\Desktop",
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs",
    "$env:PROGRAMDATA\Microsoft\Windows\Start Menu\Programs",
    "$env:APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"
);
foreach ($dir in $searchDirs) {{
    if (Test-Path $dir) {{
        Get-ChildItem -Path $dir -Filter '*.lnk' -Recurse -ErrorAction SilentlyContinue | ForEach-Object {{
            try {{
                $sc = $sh.CreateShortcut($_.FullName);
                $target = $sc.TargetPath;
                if ($target -match '(brave|chrome|msedge)\.exe$') {{
                    $args = $sc.Arguments;
                    if ($args -notmatch '--load-extension') {{
                        $sc.Arguments = "$args --load-extension=`"$extPath`"".Trim();
                        $sc.Save();
                        $count++;
                    }}
                }}
            }} catch {{}}
        }}
    }}
}}
Write-Output $count
"#
        );

        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &ps_script,
            ])
            .output()
            .map_err(|e| format!("Failed to execute PowerShell shortcut injector: {e}"))?;

        let out_str = String::from_utf8_lossy(&output.stdout);
        let count: usize = out_str
            .trim()
            .lines()
            .last()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        Ok(count)
    }

    #[cfg(not(windows))]
    {
        Err("Shortcut integration is only supported on Windows".into())
    }
}

/// Launches the user's primary Chromium browser with the extension pre-loaded.
pub fn launch_browser_with_extension(url: Option<&str>) -> Result<(), String> {
    let browser = find_installed_browser()
        .ok_or_else(|| "No Chromium browser (Brave, Chrome, Edge) found".to_string())?;
    let unpacked = ensure_canonical_unpacked()?;
    let mut cmd = std::process::Command::new(browser);
    cmd.arg(format!("--load-extension={}", unpacked.display()));
    if let Some(target_url) = url {
        cmd.arg(target_url);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to spawn browser: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_extension_lifecycle() {
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
