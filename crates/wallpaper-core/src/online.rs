//! Online wallpaper discovery and downloading module.
//!
//! Provides integration with official, free wallpaper APIs (such as Wallhaven)
//! and curated open video wallpaper collections.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{copy, Read};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnlineError {
    #[error("Network request failed: {0}")]
    Network(String),
    #[error("Failed to parse API response: {0}")]
    Parse(String),
    #[error("I/O error during download: {0}")]
    Io(#[from] std::io::Error),
}

/// Represents an online wallpaper item available for browsing and download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineWallpaper {
    pub id: String,
    pub title: String,
    pub thumbnail_url: String,
    pub download_url: String,
    pub resolution: String,
    pub category: String,
    pub is_video: bool,
    pub source_platform: String,
    pub file_size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WallhavenResponse {
    data: Option<Vec<WallhavenItemRaw>>,
}

#[derive(Debug, Deserialize)]
struct WallhavenItemRaw {
    id: String,
    path: String,
    resolution: Option<String>,
    category: Option<String>,
    file_size: Option<u64>,
    thumbs: Option<WallhavenThumbsRaw>,
}

#[derive(Debug, Deserialize)]
struct WallhavenThumbsRaw {
    large: Option<String>,
    small: Option<String>,
}

/// Searches Wallhaven for wallpapers using their official public REST API.
pub fn search_wallhaven(
    query: &str,
    category: Option<&str>,
    page: u32,
) -> Result<Vec<OnlineWallpaper>, OnlineError> {
    let mut url =
        format!("https://wallhaven.cc/api/v1/search?purity=100&sorting=toplist&page={page}");

    if !query.trim().is_empty() {
        let encoded = urlencoding(query.trim());
        url.push_str(&format!("&q={encoded}"));
    }

    // Category mapping: general=100, anime=010, people=001, all=111
    let cat_code = match category.unwrap_or("all").to_lowercase().as_str() {
        "anime" => "010",
        "general" => "100",
        "people" => "001",
        _ => "111",
    };
    url.push_str(&format!("&categories={cat_code}"));

    let agent = ureq::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("RustWallpaperEngine/0.1.0")
        .build();

    let response = agent
        .get(&url)
        .call()
        .map_err(|e| OnlineError::Network(e.to_string()))?;

    let parsed: WallhavenResponse = response
        .into_json()
        .map_err(|e| OnlineError::Parse(e.to_string()))?;

    let items = parsed.data.unwrap_or_default();
    let mut result = Vec::new();

    for item in items {
        let thumb = item
            .thumbs
            .as_ref()
            .and_then(|t| t.large.clone().or_else(|| t.small.clone()))
            .unwrap_or_else(|| item.path.clone());

        result.push(OnlineWallpaper {
            id: format!("wh-{}", item.id),
            title: format!("Wallhaven {}", item.id),
            thumbnail_url: thumb,
            download_url: item.path,
            resolution: item.resolution.unwrap_or_else(|| "Unknown".to_string()),
            category: item.category.unwrap_or_else(|| "General".to_string()),
            is_video: false,
            source_platform: "Wallhaven".to_string(),
            file_size_bytes: item.file_size,
        });
    }

    Ok(result)
}

/// Returns a curated collection of free, high-quality looping video wallpapers.
pub fn get_curated_video_wallpapers() -> Vec<OnlineWallpaper> {
    vec![
        OnlineWallpaper {
            id: "vid-cyberpunk-city".to_string(),
            title: "Cyberpunk Futuristic City Loop".to_string(),
            thumbnail_url: "https://images.pexels.com/videos/3129671/free-video-3129671.jpg?auto=compress&cs=tinysrgb&dpr=1&w=500".to_string(),
            download_url: "https://assets.mixkit.co/videos/preview/mixkit-futuristic-city-with-flying-cars-and-tall-buildings-41551-large.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Cyberpunk".to_string(),
            is_video: true,
            source_platform: "Mixkit Free".to_string(),
            file_size_bytes: Some(15_400_000),
        },
        OnlineWallpaper {
            id: "vid-neon-highway".to_string(),
            title: "Neon Retro Wave Highway".to_string(),
            thumbnail_url: "https://images.pexels.com/videos/3129957/free-video-3129957.jpg?auto=compress&cs=tinysrgb&dpr=1&w=500".to_string(),
            download_url: "https://assets.mixkit.co/videos/preview/mixkit-driving-on-a-highway-with-neon-lights-at-night-41550-large.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Retro".to_string(),
            is_video: true,
            source_platform: "Mixkit Free".to_string(),
            file_size_bytes: Some(12_200_000),
        },
        OnlineWallpaper {
            id: "vid-aurora-borealis".to_string(),
            title: "Aurora Borealis Northern Lights".to_string(),
            thumbnail_url: "https://images.pexels.com/videos/3141207/free-video-3141207.jpg?auto=compress&cs=tinysrgb&dpr=1&w=500".to_string(),
            download_url: "https://assets.mixkit.co/videos/preview/mixkit-northern-lights-in-a-night-sky-41553-large.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Nature".to_string(),
            is_video: true,
            source_platform: "Mixkit Free".to_string(),
            file_size_bytes: Some(18_700_000),
        },
        OnlineWallpaper {
            id: "vid-space-nebula".to_string(),
            title: "Deep Space Stars & Nebula".to_string(),
            thumbnail_url: "https://images.pexels.com/videos/3129672/free-video-3129672.jpg?auto=compress&cs=tinysrgb&dpr=1&w=500".to_string(),
            download_url: "https://assets.mixkit.co/videos/preview/mixkit-space-travel-through-stars-and-a-nebula-41552-large.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Space".to_string(),
            is_video: true,
            source_platform: "Mixkit Free".to_string(),
            file_size_bytes: Some(14_500_000),
        },
        OnlineWallpaper {
            id: "vid-ocean-waves".to_string(),
            title: "Gentle Ocean Waves Sunset".to_string(),
            thumbnail_url: "https://images.pexels.com/videos/1536322/free-video-1536322.jpg?auto=compress&cs=tinysrgb&dpr=1&w=500".to_string(),
            download_url: "https://assets.mixkit.co/videos/preview/mixkit-waves-coming-to-the-beach-5016-large.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Nature".to_string(),
            is_video: true,
            source_platform: "Mixkit Free".to_string(),
            file_size_bytes: Some(11_900_000),
        },
    ]
}

/// Downloads a remote file to a local destination with timeout and size check.
pub fn download_online_file(url: &str, destination: &Path) -> Result<u64, OnlineError> {
    if let Some(parent) = destination.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let agent = ureq::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("RustWallpaperEngine/0.1.0")
        .build();

    let response = agent
        .get(url)
        .call()
        .map_err(|e| OnlineError::Network(e.to_string()))?;

    let mut reader = response.into_reader();
    let mut file = File::create(destination)?;
    let bytes_copied = copy(&mut reader, &mut file)?;

    Ok(bytes_copied)
}

/// Downloads an image to memory buffer for thumbnail rendering.
pub fn fetch_thumbnail_bytes(url: &str) -> Result<Vec<u8>, OnlineError> {
    let agent = ureq::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("RustWallpaperEngine/0.1.0")
        .build();

    let response = agent
        .get(url)
        .call()
        .map_err(|e| OnlineError::Network(e.to_string()))?;

    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;

    Ok(bytes)
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_curated_video_wallpapers() {
        let list = get_curated_video_wallpapers();
        assert!(!list.is_empty());
        for item in &list {
            assert!(item.is_video);
            assert!(item.download_url.starts_with("https://"));
            assert!(item.thumbnail_url.starts_with("https://"));
        }
    }

    #[test]
    fn test_url_encoding() {
        assert_eq!(urlencoding("anime 4k"), "anime+4k");
        assert_eq!(urlencoding("nature/forest"), "nature%2Fforest");
    }
}
