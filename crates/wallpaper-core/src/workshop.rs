//! Steam Workshop and Wallpaper Engine local content scanner.
//!
//! Scans local Steam libraries for subscribed and created Wallpaper Engine items,
//! parsing their `project.json` descriptors and identifying video wallpapers
//! compatible with the native MPV engine.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// App ID for Wallpaper Engine on Steam.
pub const WALLPAPER_ENGINE_APP_ID: u32 = 431960;

/// Represents a locally detected Steam Workshop or Wallpaper Engine item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopItem {
    /// Unique item ID (folder name or Steam Workshop publication ID).
    pub id: String,
    /// Human-readable title from `project.json`.
    pub title: String,
    /// Description from `project.json` if available.
    pub description: Option<String>,
    /// Content type specified in `project.json` (e.g., "video", "scene", "web").
    pub item_type: String,
    /// Full path to the main file (e.g., video file or scene file).
    pub file_path: PathBuf,
    /// Full path to the preview thumbnail image/gif if present.
    pub preview_path: Option<PathBuf>,
    /// Whether the file is a standard video format playable directly by MPV.
    pub is_compatible_video: bool,
    /// Content tags if defined in `project.json`.
    pub tags: Vec<String>,
    /// Content rating (e.g., "Everyone").
    pub content_rating: Option<String>,
    /// File size in bytes of the target media file.
    pub file_size_bytes: Option<u64>,
    /// Folder containing the workshop item files.
    pub folder_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ProjectJsonRaw {
    title: Option<String>,
    description: Option<String>,
    file: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    preview: Option<String>,
    tags: Option<Vec<String>>,
    contentrating: Option<String>,
}

/// Discovers common Steam install directories on Windows.
pub fn find_steam_installations() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(windows)]
    {
        // Check Registry HKCU\Software\Valve\Steam -> SteamPath
        if let Ok(reg_path) = read_registry_string("Software\\Valve\\Steam", "SteamPath") {
            let p = PathBuf::from(reg_path.replace('/', "\\"));
            if p.is_dir() && !paths.contains(&p) {
                paths.push(p);
            }
        }

        // Check Registry HKLM\SOFTWARE\WOW6432Node\Valve\Steam -> InstallPath
        if let Ok(reg_path) =
            read_registry_string_hklm("SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath")
        {
            let p = PathBuf::from(reg_path.replace('/', "\\"));
            if p.is_dir() && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    // Common standard fallback locations
    let candidates = [
        r"C:\Program Files (x86)\Steam",
        r"D:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"E:\Steam",
    ];

    for candidate in &candidates {
        let p = PathBuf::from(candidate);
        if p.is_dir() && !paths.contains(&p) {
            paths.push(p);
        }
    }

    paths
}

/// Parses `<steam_path>/steamapps/libraryfolders.vdf` to find all Steam library drives.
pub fn find_steam_library_folders(steam_dir: &Path) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    if steam_dir.is_dir() {
        libraries.push(steam_dir.to_path_buf());
    }

    let vdf_path = steam_dir.join("steamapps").join("libraryfolders.vdf");
    if let Ok(content) = fs::read_to_string(&vdf_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            // Look for "path" "D:\\SteamLibrary"
            if trimmed.starts_with("\"path\"") {
                let rest = trimmed.trim_start_matches("\"path\"").trim();
                let clean_path = rest.trim_matches('"').replace("\\\\", "\\");
                let pb = PathBuf::from(clean_path);
                if pb.is_dir() && !libraries.contains(&pb) {
                    libraries.push(pb);
                }
            }
        }
    }

    libraries
}

/// Scans local Steam libraries and optional custom directories for Wallpaper Engine items.
pub fn scan_wallpaper_engine_items(custom_dirs: &[PathBuf]) -> Vec<WorkshopItem> {
    let mut items = Vec::new();
    let mut seen_ids = HashSet::new();

    // 1. Gather all library roots
    let mut roots = Vec::new();
    for steam_dir in find_steam_installations() {
        for lib in find_steam_library_folders(&steam_dir) {
            if !roots.contains(&lib) {
                roots.push(lib);
            }
        }
    }

    // 2. Scan standard Wallpaper Engine locations across all Steam libraries
    for root in &roots {
        let targets = [
            root.join("steamapps")
                .join("workshop")
                .join("content")
                .join(WALLPAPER_ENGINE_APP_ID.to_string()),
            root.join("steamapps")
                .join("common")
                .join("wallpaper_engine")
                .join("projects")
                .join("myprojects"),
            root.join("steamapps")
                .join("common")
                .join("wallpaper_engine")
                .join("projects")
                .join("defaultprojects"),
        ];

        for target in &targets {
            if target.is_dir() {
                scan_directory_for_items(target, &mut items, &mut seen_ids);
            }
        }
    }

    // 3. Scan custom directories provided by user
    for custom in custom_dirs {
        if custom.is_dir() {
            scan_directory_for_items(custom, &mut items, &mut seen_ids);
        }
    }

    // Sort items by title alphabetically
    items.sort_by_key(|a| a.title.to_lowercase());
    items
}

fn scan_directory_for_items(
    dir: &Path,
    items: &mut Vec<WorkshopItem>,
    seen_ids: &mut HashSet<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(item) = parse_workshop_directory(&path) {
                if !seen_ids.contains(&item.id) {
                    seen_ids.insert(item.id.clone());
                    items.push(item);
                }
            }
        }
    }
}

/// Parses a directory containing a `project.json` file.
pub fn parse_workshop_directory(dir: &Path) -> Option<WorkshopItem> {
    let project_json_path = dir.join("project.json");
    if !project_json_path.is_file() {
        // If no project.json, check if there's a standalone video file in the folder
        return parse_unstructured_directory(dir);
    }

    let content = fs::read_to_string(&project_json_path).ok()?;
    let raw: ProjectJsonRaw = serde_json::from_str(&content).ok()?;

    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let title = raw
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| id.clone());

    let item_type = raw.item_type.unwrap_or_else(|| "unknown".to_string());

    // Resolve main file
    let (file_path, is_compatible) = if let Some(ref rel_file) = raw.file {
        let target = dir.join(rel_file);
        let ext = target
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let compatible = ext == "mp4" || ext == "webm" || ext == "mkv" || ext == "mov";
        (target, compatible)
    } else {
        // Find first video file if unspecified
        find_first_video(dir)
            .map(|p| (p, true))
            .unwrap_or_else(|| (project_json_path.clone(), false))
    };

    // Resolve preview thumbnail
    let preview_path = if let Some(ref rel_prev) = raw.preview {
        let prev_path = dir.join(rel_prev);
        if prev_path.is_file() {
            Some(prev_path)
        } else {
            find_first_preview(dir)
        }
    } else {
        find_first_preview(dir)
    };

    let file_size_bytes = fs::metadata(&file_path).ok().map(|m| m.len());

    Some(WorkshopItem {
        id,
        title,
        description: raw.description,
        item_type,
        file_path,
        preview_path,
        is_compatible_video: is_compatible,
        tags: raw.tags.unwrap_or_default(),
        content_rating: raw.contentrating,
        file_size_bytes,
        folder_path: dir.to_path_buf(),
    })
}

fn parse_unstructured_directory(dir: &Path) -> Option<WorkshopItem> {
    let video_path = find_first_video(dir)?;
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let title = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&id)
        .to_string();
    let preview_path = find_first_preview(dir);
    let file_size_bytes = fs::metadata(&video_path).ok().map(|m| m.len());

    Some(WorkshopItem {
        id,
        title,
        description: None,
        item_type: "video".to_string(),
        file_path: video_path,
        preview_path,
        is_compatible_video: true,
        tags: Vec::new(),
        content_rating: None,
        file_size_bytes,
        folder_path: dir.to_path_buf(),
    })
}

fn find_first_video(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let lower = ext.to_lowercase();
                if lower == "mp4" || lower == "webm" || lower == "mkv" || lower == "mov" {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn find_first_preview(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        "preview.jpg",
        "preview.gif",
        "preview.png",
        "preview.webp",
        "thumbnail.jpg",
        "thumbnail.png",
    ];
    for name in &candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(windows)]
fn read_registry_string(sub_key: &str, value_name: &str) -> std::io::Result<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ,
    };

    let wide_key: Vec<u16> = std::ffi::OsStr::new(sub_key)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let wide_val: Vec<u16> = std::ffi::OsStr::new(value_name)
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
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Registry key not found",
            ));
        }

        let mut buf = [0u16; 512];
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
            Ok(String::from_utf16_lossy(slice))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Registry value query failed",
            ))
        }
    }
}

#[cfg(windows)]
fn read_registry_string_hklm(sub_key: &str, value_name: &str) -> std::io::Result<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    let wide_key: Vec<u16> = std::ffi::OsStr::new(sub_key)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let wide_val: Vec<u16> = std::ffi::OsStr::new(value_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut hkey = HKEY::default();
    unsafe {
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wide_key.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Registry key not found",
            ));
        }

        let mut buf = [0u16; 512];
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
            Ok(String::from_utf16_lossy(slice))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Registry value query failed",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_project_json_video() {
        let json = r#"{
            "title": "Neon Sunset",
            "description": "Chill synthwave vibes",
            "file": "neon.mp4",
            "preview": "preview.gif",
            "type": "video",
            "tags": ["Anime", "Synthwave"],
            "contentrating": "Everyone"
        }"#;

        let temp_dir = std::env::temp_dir().join("test_wp_video_parse");
        let _ = fs::create_dir_all(&temp_dir);
        let project_json = temp_dir.join("project.json");
        fs::write(&project_json, json).unwrap();
        let video_file = temp_dir.join("neon.mp4");
        fs::write(&video_file, b"dummy video bytes").unwrap();
        let prev_file = temp_dir.join("preview.gif");
        fs::write(&prev_file, b"dummy gif bytes").unwrap();

        let item = parse_workshop_directory(&temp_dir).expect("Should parse video project");
        assert_eq!(item.title, "Neon Sunset");
        assert_eq!(item.item_type, "video");
        assert!(item.is_compatible_video);
        assert_eq!(item.tags, vec!["Anime", "Synthwave"]);
        assert_eq!(item.file_path, video_file);
        assert_eq!(item.preview_path, Some(prev_file));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_project_json_scene_not_compatible_video() {
        let json = r#"{
            "title": "3D Shading Room",
            "file": "scene.pkg",
            "preview": "preview.jpg",
            "type": "scene"
        }"#;

        let temp_dir = std::env::temp_dir().join("test_wp_scene_parse");
        let _ = fs::create_dir_all(&temp_dir);
        let project_json = temp_dir.join("project.json");
        fs::write(&project_json, json).unwrap();

        let item = parse_workshop_directory(&temp_dir).expect("Should parse scene project");
        assert_eq!(item.title, "3D Shading Room");
        assert_eq!(item.item_type, "scene");
        assert!(!item.is_compatible_video);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_vdf_library_folders() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path" "C:\\Program Files (x86)\\Steam"
    }
    "1"
    {
        "path" "D:\\SteamLibrary"
    }
}
        "#;
        let temp_dir = std::env::temp_dir().join("test_steam_vdf");
        let _ = fs::create_dir_all(temp_dir.join("steamapps"));
        fs::write(temp_dir.join("steamapps").join("libraryfolders.vdf"), vdf).unwrap();

        let folders = find_steam_library_folders(&temp_dir);
        assert!(folders.contains(&temp_dir));
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
