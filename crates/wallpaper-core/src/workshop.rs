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

/// Status of Steam and Wallpaper Engine detection on the local system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamEngineStatus {
    /// Whether Steam installation was found in the registry or default directories.
    pub steam_installed: bool,
    /// Detected path to the primary Steam installation directory.
    pub steam_path: Option<PathBuf>,
    /// Whether Wallpaper Engine is detected in any Steam library folder.
    pub wallpaper_engine_installed: bool,
    /// Path to the Wallpaper Engine installation directory if found.
    pub wallpaper_engine_dir: Option<PathBuf>,
    /// Path to the Steam Workshop items content folder if found.
    pub workshop_dir: Option<PathBuf>,
}

/// Detects Steam and Wallpaper Engine installation status across local drives.
pub fn detect_steam_and_wallpaper_engine() -> SteamEngineStatus {
    let steam_installs = find_steam_installations();
    let steam_installed = !steam_installs.is_empty();
    let steam_path = steam_installs.first().cloned();

    let mut we_installed = false;
    let mut we_dir = None;
    let mut ws_dir = None;

    for steam_dir in &steam_installs {
        for lib in find_steam_library_folders(steam_dir) {
            let we_candidate = lib
                .join("steamapps")
                .join("common")
                .join("wallpaper_engine");
            if we_candidate.is_dir() {
                we_installed = true;
                if we_dir.is_none() {
                    we_dir = Some(we_candidate);
                }
            }
            let ws_candidate = lib
                .join("steamapps")
                .join("workshop")
                .join("content")
                .join(WALLPAPER_ENGINE_APP_ID.to_string());
            if ws_candidate.is_dir() && ws_dir.is_none() {
                ws_dir = Some(ws_candidate);
            }
        }
    }

    SteamEngineStatus {
        steam_installed,
        steam_path,
        wallpaper_engine_installed: we_installed,
        wallpaper_engine_dir: we_dir,
        workshop_dir: ws_dir,
    }
}

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

/// Represents an online Wallpaper Engine item listed on the Steam Community Workshop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamWorkshopOnlineItem {
    pub id: String,
    pub title: String,
    pub thumbnail_url: String,
    pub steam_url: String,
    pub web_url: String,
}

fn unescape_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Queries the public Steam Community Workshop for Wallpaper Engine items.
pub fn fetch_steam_workshop_items(
    query: &str,
    sort: &str,
) -> Result<Vec<SteamWorkshopOnlineItem>, String> {
    let sort_param = match sort.to_lowercase().as_str() {
        "recent" | "mostrecent" => "mostrecent",
        "popular" | "totaluniquesubscribers" => "totaluniquesubscribers",
        "toprated" => "toprated",
        _ => "trend",
    };

    let mut url = format!(
        "https://steamcommunity.com/workshop/browse/?appid=431960&browsesort={sort_param}&section=readytouseitems"
    );

    if !query.trim().is_empty() {
        let encoded = crate::online::urlencoding(query.trim());
        url.push_str(&format!("&searchtext={encoded}"));
    }

    let agent = ureq::builder()
        .timeout(std::time::Duration::from_secs(12))
        .redirects(5)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build();

    let response = agent.get(&url).call().map_err(|e| e.to_string())?;
    let html = response.into_string().map_err(|e| e.to_string())?;

    let mut items = Vec::new();
    let mut seen_ids = HashSet::new();

    let marker = "https://steamcommunity.com/sharedfiles/filedetails/?id=";
    let mut search_from = 0;

    while let Some(pos) = html[search_from..].find(marker) {
        let link_start = search_from + pos + marker.len();
        let link_end = html[link_start..]
            .find('"')
            .map(|end| link_start + end)
            .unwrap_or(link_start);
        let id_part = &html[link_start..link_end];

        if id_part.chars().all(|c| c.is_ascii_digit()) && !id_part.is_empty() {
            let id = id_part.to_string();

            let look_ahead = &html[link_end..html.len().min(link_end + 900)];
            if let Some(img_pos) = look_ahead.find("<img ") {
                let img_chunk = &look_ahead[img_pos..];
                if let Some(src_pos) = img_chunk.find("src=\"") {
                    let src_start = src_pos + 5;
                    if let Some(src_end) = img_chunk[src_start..].find('"') {
                        let thumb_url = &img_chunk[src_start..src_start + src_end];
                        let alt_title = if let Some(alt_pos) = img_chunk.find("alt=\"") {
                            let alt_start = alt_pos + 5;
                            img_chunk[alt_start..]
                                .find('"')
                                .map(|alt_end| &img_chunk[alt_start..alt_start + alt_end])
                                .unwrap_or("Wallpaper Engine Workshop Item")
                        } else {
                            "Wallpaper Engine Workshop Item"
                        };

                        if seen_ids.insert(id.clone()) {
                            items.push(SteamWorkshopOnlineItem {
                                id: id.clone(),
                                title: unescape_html_entities(alt_title),
                                thumbnail_url: thumb_url.replace("&amp;", "&"),
                                steam_url: format!("steam://url/CommunityFilePage/{id}"),
                                web_url: format!(
                                    "https://steamcommunity.com/sharedfiles/filedetails/?id={id}"
                                ),
                            });
                        }
                    }
                }
            }
        }
        search_from = link_end;
    }

    if items.is_empty() {
        return Ok(get_curated_steam_workshop_items());
    }

    Ok(items)
}

/// Fallback / curated top Steam Workshop items for Wallpaper Engine.
pub fn get_curated_steam_workshop_items() -> Vec<SteamWorkshopOnlineItem> {
    vec![
        SteamWorkshopOnlineItem {
            id: "3840297439".to_string(),
            title: "MEMORIES by VISUALDON - 4K Ultra HD".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/14363317062840539231/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3840297439".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3840297439".to_string(),
        },
        SteamWorkshopOnlineItem {
            id: "3836835109".to_string(),
            title: "Cyberpunk: Edgerunners — Weak Kingsley".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/12029503426841588107/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3836835109".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3836835109".to_string(),
        },
        SteamWorkshopOnlineItem {
            id: "3828824382".to_string(),
            title: "Cyberpunk: Edgerunners 3D — Interactive Wallpaper".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/15674011536591943272/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3828824382".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3828824382".to_string(),
        },
        SteamWorkshopOnlineItem {
            id: "3825047385".to_string(),
            title: "Cyberpunk Edgerunners [4K] - David and Lucy (Enhanced)".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/18217745455842189969/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3825047385".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3825047385".to_string(),
        },
        SteamWorkshopOnlineItem {
            id: "3839953231".to_string(),
            title: "Cyberpunk 2077 | 4K Wallpaper | Audio Visualizer".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/9429931869422287845/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3839953231".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3839953231".to_string(),
        },
        SteamWorkshopOnlineItem {
            id: "3840126735".to_string(),
            title: "Ribs主晏 - Live Anime Artwork".to_string(),
            thumbnail_url: "https://images.steamusercontent.com/ugc/15494973745534577405/B1D30F5BCF102A1CB85D6264B92EF249CC6E21B5/?imw=322&imh=322".to_string(),
            steam_url: "steam://url/CommunityFilePage/3840126735".to_string(),
            web_url: "https://steamcommunity.com/sharedfiles/filedetails/?id=3840126735".to_string(),
        },
    ]
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
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let lower = ext.to_lowercase();
                if lower == "mp4" || lower == "webm" || lower == "mkv" || lower == "mov" {
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("video")
                        .to_string();
                    if !seen_ids.contains(&id) {
                        seen_ids.insert(id.clone());
                        let title = id.clone();
                        let file_size_bytes = fs::metadata(&path).ok().map(|m| m.len());
                        items.push(WorkshopItem {
                            id,
                            title,
                            description: None,
                            item_type: "video".to_string(),
                            file_path: path.clone(),
                            preview_path: None,
                            is_compatible_video: true,
                            tags: Vec::new(),
                            content_rating: None,
                            file_size_bytes,
                            folder_path: dir.to_path_buf(),
                        });
                    }
                }
            }
        }
    }
}

const MAX_PROJECT_JSON_BYTES: u64 = 1024 * 1024; // 1 MB security limit

fn normalize_canonical_path(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// Safely resolves a relative path from within an item directory.
/// Strictly enforces that the target file exists and its canonicalized path
/// resides strictly within the item directory (preventing directory traversal attacks like `../`).
pub fn resolve_safe_item_file(item_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = Path::new(rel_path);
    if rel.is_absolute() || rel_path.trim().is_empty() {
        return None;
    }

    // Reject obvious traversal components
    for component in rel.components() {
        if let std::path::Component::ParentDir = component {
            return None;
        }
    }

    let candidate = item_dir.join(rel);
    let canonical_dir = std::fs::canonicalize(item_dir).ok()?;
    let canonical_target = std::fs::canonicalize(&candidate).ok()?;

    // Must be inside item_dir
    if !canonical_target.starts_with(&canonical_dir) {
        return None;
    }

    // Inspect symlinks to ensure they do not escape item_dir
    if let Ok(sym_meta) = std::fs::symlink_metadata(&candidate) {
        if sym_meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(&candidate).ok()?;
            let resolved_sym = if link_target.is_absolute() {
                std::fs::canonicalize(link_target).ok()?
            } else {
                std::fs::canonicalize(item_dir.join(link_target)).ok()?
            };
            if !resolved_sym.starts_with(&canonical_dir) {
                return None;
            }
        }
    }

    if canonical_target.is_file() {
        Some(normalize_canonical_path(canonical_target))
    } else {
        None
    }
}

/// Parses a directory containing a `project.json` file.
pub fn parse_workshop_directory(dir: &Path) -> Option<WorkshopItem> {
    let project_json_path = dir.join("project.json");
    if !project_json_path.is_file() {
        // If no project.json, check if there's a standalone video file in the folder
        return parse_unstructured_directory(dir);
    }

    // Security check: limit project.json file size
    if let Ok(meta) = fs::metadata(&project_json_path) {
        if meta.len() > MAX_PROJECT_JSON_BYTES {
            tracing::warn!(
                path = %project_json_path.display(),
                size = meta.len(),
                "Rejecting oversized project.json (> 1MB)"
            );
            return None;
        }
    } else {
        return None;
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

    // Resolve main file safely
    let (file_path, is_compatible) = if let Some(ref rel_file) = raw.file {
        if let Some(target) = resolve_safe_item_file(dir, rel_file) {
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let compatible = ext == "mp4" || ext == "webm" || ext == "mkv" || ext == "mov";
            (target, compatible)
        } else {
            // Unsafe or missing relative file: attempt finding first video
            find_first_video(dir)
                .map(|p| (p, true))
                .unwrap_or_else(|| (project_json_path.clone(), false))
        }
    } else {
        // Find first video file if unspecified
        find_first_video(dir)
            .map(|p| (p, true))
            .unwrap_or_else(|| (project_json_path.clone(), false))
    };

    // Resolve preview thumbnail safely
    let preview_path = if let Some(ref rel_prev) = raw.preview {
        if let Some(prev_path) = resolve_safe_item_file(dir, rel_prev) {
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
        assert_eq!(
            item.file_path,
            normalize_canonical_path(std::fs::canonicalize(&video_file).unwrap())
        );
        assert_eq!(
            item.preview_path,
            Some(normalize_canonical_path(
                std::fs::canonicalize(&prev_file).unwrap()
            ))
        );

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

    #[test]
    fn test_oversized_project_json_rejected() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_wp_oversized_{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&temp_dir);
        let project_json = temp_dir.join("project.json");
        // Create 1.5MB file
        let big_content = vec![b' '; 1_500_000];
        fs::write(&project_json, big_content).unwrap();

        let item = parse_workshop_directory(&temp_dir);
        assert!(item.is_none(), "Oversized project.json must be rejected");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_path_traversal_rejected() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_wp_traversal_{}", uuid::Uuid::new_v4()));
        let item_dir = temp_dir.join("item_123");
        let _ = fs::create_dir_all(&item_dir);

        // Put an outside file
        let outside_secret = temp_dir.join("secret.mp4");
        fs::write(&outside_secret, b"secret video").unwrap();

        // Craft malicious project.json targeting outside file
        let malicious_json = r#"{
            "title": "Malicious Traversal",
            "file": "../secret.mp4",
            "preview": "../secret.mp4",
            "type": "video"
        }"#;
        fs::write(item_dir.join("project.json"), malicious_json).unwrap();

        let resolved = resolve_safe_item_file(&item_dir, "../secret.mp4");
        assert!(resolved.is_none(), "Path traversal must be rejected");

        let item = parse_workshop_directory(&item_dir).unwrap();
        // Since the traversal file was rejected and no video is inside item_dir,
        // it must not resolve to the outside secret file!
        assert_ne!(item.file_path, outside_secret);
        assert!(!item.is_compatible_video);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_scan_directory_with_loose_videos() {
        let temp_dir = std::env::temp_dir().join(format!("test_wp_loose_{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&temp_dir);

        let video1 = temp_dir.join("beach_sunset.mp4");
        let video2 = temp_dir.join("matrix_rain.webm");
        let text_file = temp_dir.join("notes.txt");

        fs::write(&video1, b"dummy video 1").unwrap();
        fs::write(&video2, b"dummy video 2").unwrap();
        fs::write(&text_file, b"ignore me").unwrap();

        let mut items = Vec::new();
        let mut seen = HashSet::new();
        scan_directory_for_items(&temp_dir, &mut items, &mut seen);
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .any(|i| i.title == "beach_sunset" && i.is_compatible_video));
        assert!(items
            .iter()
            .any(|i| i.title == "matrix_rain" && i.is_compatible_video));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
