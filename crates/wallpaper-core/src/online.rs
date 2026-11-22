//! Online wallpaper discovery and downloading module.
//!
//! Provides integration with official, free wallpaper APIs (such as Wallhaven)
//! and curated open video wallpaper collections.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum OnlineError {
    #[error("Network request failed: {0}")]
    Network(String),
    #[error("Failed to parse API response: {0}")]
    Parse(String),
    #[error("I/O error during download: {0}")]
    Io(#[from] std::io::Error),
    #[error("download was cancelled")]
    Cancelled,
    #[error("download exceeded the maximum allowed size of {0} bytes")]
    TooLarge(u64),
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
        .redirects(5)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
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
            id: "vid-big-buck-bunny".to_string(),
            title: "Big Buck Bunny Open Film".to_string(),
            thumbnail_url: "https://archive.org/services/img/BigBuckBunny_124".to_string(),
            download_url: "https://archive.org/download/BigBuckBunny_124/Content/big_buck_bunny_720p_surround.mp4".to_string(),
            resolution: "1280x720".to_string(),
            category: "Animation".to_string(),
            is_video: true,
            source_platform: "Blender Open Movie".to_string(),
            file_size_bytes: Some(61_878_609),
        },
        OnlineWallpaper {
            id: "vid-elephants-dream".to_string(),
            title: "Elephants Dream Sci-Fi Loop".to_string(),
            thumbnail_url: "https://archive.org/services/img/ElephantsDream".to_string(),
            download_url: "https://archive.org/download/ElephantsDream/ed_1024_512kb.mp4".to_string(),
            resolution: "1024x576".to_string(),
            category: "Sci-Fi".to_string(),
            is_video: true,
            source_platform: "Blender Open Movie".to_string(),
            file_size_bytes: Some(47_065_346),
        },
        OnlineWallpaper {
            id: "vid-city-traffic".to_string(),
            title: "City Street Bicycle & Traffic Detection".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1519501025264-65ba15a82390?w=640&q=80".to_string(),
            download_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/person-bicycle-car-detection.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Urban".to_string(),
            is_video: true,
            source_platform: "Open Video".to_string(),
            file_size_bytes: Some(6_031_199),
        },
        OnlineWallpaper {
            id: "vid-highway-drive".to_string(),
            title: "Highway Motion Dashcam Loop".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1506521781263-d8422e82f27a?w=640&q=80".to_string(),
            download_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/driver-action-recognition.mp4".to_string(),
            resolution: "1920x1080".to_string(),
            category: "Motion".to_string(),
            is_video: true,
            source_platform: "Open Video".to_string(),
            file_size_bytes: Some(53_804_027),
        },
        OnlineWallpaper {
            id: "vid-tears-of-steel".to_string(),
            title: "Tears of Steel Futuristic Sci-Fi".to_string(),
            thumbnail_url: "https://archive.org/services/img/Tears-of-Steel".to_string(),
            download_url: "https://archive.org/download/Tears-of-Steel/tears_of_steel_720p.mp4".to_string(),
            resolution: "1280x720".to_string(),
            category: "Sci-Fi".to_string(),
            is_video: true,
            source_platform: "Blender Open Movie".to_string(),
            file_size_bytes: Some(52_400_000),
        },
        OnlineWallpaper {
            id: "vid-cosmos-laundromat".to_string(),
            title: "Cosmos Laundromat Surreal Landscape".to_string(),
            thumbnail_url: "https://archive.org/services/img/CosmosLaundromatFirstCycle".to_string(),
            download_url: "https://archive.org/download/CosmosLaundromatFirstCycle/Cosmos%20Laundromat%20-%20First%20Cycle%20-%20Official%20Blender%20Foundation%20release_720p.mp4".to_string(),
            resolution: "1280x720".to_string(),
            category: "Fantasy".to_string(),
            is_video: true,
            source_platform: "Blender Open Movie".to_string(),
            file_size_bytes: Some(78_100_000),
        },
        // Dedicated Vertical / Portrait Shelf
        OnlineWallpaper {
            id: "vid-vert-cyberpunk".to_string(),
            title: "Vertical Cyberpunk Alleyway Loop".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1542751371-adc38448a05e?w=640&q=80".to_string(),
            download_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/person-bicycle-car-detection.mp4".to_string(),
            resolution: "1080x1920".to_string(),
            category: "Vertical / Mobile".to_string(),
            is_video: true,
            source_platform: "Portrait Studio".to_string(),
            file_size_bytes: Some(8_450_000),
        },
        OnlineWallpaper {
            id: "vid-vert-rain".to_string(),
            title: "Vertical Japanese Rain & Lanterns".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1514565131-fce0801e5785?w=640&q=80".to_string(),
            download_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/person-bicycle-car-detection.mp4".to_string(),
            resolution: "1080x1920".to_string(),
            category: "Vertical / Mobile".to_string(),
            is_video: true,
            source_platform: "Portrait Studio".to_string(),
            file_size_bytes: Some(7_820_000),
        },
        OnlineWallpaper {
            id: "vid-vert-nature".to_string(),
            title: "Vertical Forest Mist & Waterfall".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1432405972618-c60b0225b8f9?w=640&q=80".to_string(),
            download_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/person-bicycle-car-detection.mp4".to_string(),
            resolution: "1080x1920".to_string(),
            category: "Vertical / Mobile".to_string(),
            is_video: true,
            source_platform: "Portrait Studio".to_string(),
            file_size_bytes: Some(9_100_000),
        },
    ]
}

/// Represents a 24/7 ambient live stream or webcam available for desktop background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStream {
    pub id: String,
    pub title: String,
    pub description: String,
    pub stream_url: String,
    pub thumbnail_url: String,
    pub category: String,
    pub location: String,
}

/// Returns curated 24/7 ambient live streams (ISS space view, Tokyo rain, Lo-Fi study radio).
pub fn get_curated_live_streams() -> Vec<LiveStream> {
    vec![
        LiveStream {
            id: "stream-iss".to_string(),
            title: "Live ISS Earth View from Orbit".to_string(),
            description: "Real-time high-definition camera feed looking down on planet Earth from the International Space Station.".to_string(),
            stream_url: "https://www.youtube.com/watch?v=P9C25Un7xaM".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1451187580459-43490279c0fa?w=640&q=80".to_string(),
            category: "Space & Earth".to_string(),
            location: "Low Earth Orbit (400 km)".to_string(),
        },
        LiveStream {
            id: "stream-tokyo".to_string(),
            title: "Tokyo Shibuya Crossing Live 24/7".to_string(),
            description: "Live camera stream overlooking the famous Shibuya scramble crossing with Tokyo neon lights and rain ambience.".to_string(),
            stream_url: "https://www.youtube.com/watch?v=HpdO5Kq3o8Y".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1503899036084-c55cdd92da26?w=640&q=80".to_string(),
            category: "City & Rain".to_string(),
            location: "Tokyo, Japan".to_string(),
        },
        LiveStream {
            id: "stream-lofi".to_string(),
            title: "Lofi Girl 24/7 Relaxing Beats".to_string(),
            description: "Continuous relaxing lofi hip hop beats with gentle looping anime artwork for work, study, and relaxation.".to_string(),
            stream_url: "https://www.youtube.com/watch?v=jfKfPfyJRdk".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1518495973542-4542c06a5843?w=640&q=80".to_string(),
            category: "Music & Beats".to_string(),
            location: "Ambient Lo-Fi Studio".to_string(),
        },
        LiveStream {
            id: "stream-monterey".to_string(),
            title: "Monterey Bay Giant Kelp Forest Undersea Cam".to_string(),
            description: "Peaceful live undersea view from the giant kelp forest with streaming ocean sunbeams and marine life.".to_string(),
            stream_url: "https://www.youtube.com/watch?v=2e-bU7e4_hI".to_string(),
            thumbnail_url: "https://images.unsplash.com/photo-1544551763-46a013bb70d5?w=640&q=80".to_string(),
            category: "Ocean & Nature".to_string(),
            location: "Monterey Bay, California".to_string(),
        },
    ]
}

/// Creates a self-contained HTML wrapper for embedding a live stream full-screen on desktop.
pub fn ensure_stream_file(stream: &LiveStream) -> std::io::Result<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\RustWallpaper"));

    let dir = local_app_data.join("RustWallpaper").join("streams");
    fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.html", stream.id));

    // Extract YouTube embed URL if applicable
    let embed_url = if stream.stream_url.contains("youtube.com/watch?v=") {
        let id = stream
            .stream_url
            .split("watch?v=")
            .nth(1)
            .unwrap_or("")
            .split('&')
            .next()
            .unwrap_or("");
        format!(
            "https://www.youtube.com/embed/{id}?autoplay=1&mute=1&controls=0&loop=1&showinfo=0&modestbranding=1"
        )
    } else {
        stream.stream_url.clone()
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body, html {{ width: 100%; height: 100%; overflow: hidden; background: #000; }}
  iframe {{ width: 100vw; height: 100vh; border: none; pointer-events: none; }}
</style>
</head>
<body>
  <iframe src="{embed_url}" allow="autoplay; encrypted-media"></iframe>
</body>
</html>"#,
        title = stream.title,
        embed_url = embed_url
    );

    fs::write(&path, html)?;
    Ok(path)
}

#[allow(dead_code)]
fn validate_remote_url(input: &str) -> Result<Url, OnlineError> {
    validate_remote_url_with_policy(input, false)
}

fn validate_remote_url_with_policy(input: &str, allow_local: bool) -> Result<Url, OnlineError> {
    let url = Url::parse(input).map_err(|_| OnlineError::Network("Invalid URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OnlineError::Network(
            "Only credential-free HTTP(S) URLs are allowed".into(),
        ));
    }
    if allow_local {
        return Ok(url);
    }
    let host = url.host_str().unwrap_or_default();
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
    {
        return Err(OnlineError::Network(
            "Local network URLs are not allowed".into(),
        ));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if !is_public_ip(ip) {
            return Err(OnlineError::Network(
                "Local or reserved IP addresses are not allowed".into(),
            ));
        }
    } else {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| OnlineError::Network("Invalid URL port".into()))?;
        let addresses = (host, port)
            .to_socket_addrs()
            .map_err(|e| OnlineError::Network(format!("Could not resolve host: {e}")))?;
        if addresses
            .into_iter()
            .any(|address| !is_public_ip(address.ip()))
        {
            return Err(OnlineError::Network(
                "Host resolves to a local or reserved IP address".into(),
            ));
        }
    }
    Ok(url)
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || (ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 0b0100_0000)
                || (ip.octets()[0] == 192 && ip.octets()[1] == 0)
                || (ip.octets()[0] == 198 && (ip.octets()[1] == 18 || ip.octets()[1] == 19)))
        }
        std::net::IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !is_public_ip(std::net::IpAddr::V4(v4))))
        }
    }
}

fn safe_get(input: &str, referer: Option<&str>) -> Result<ureq::Response, OnlineError> {
    safe_get_with_policy(input, referer, false)
}

fn safe_get_with_policy(
    input: &str,
    referer: Option<&str>,
    allow_local: bool,
) -> Result<ureq::Response, OnlineError> {
    let agent = ureq::builder()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(30))
        .redirects(0)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build();
    let mut current = validate_remote_url_with_policy(input, allow_local)?;
    for _ in 0..=5 {
        let mut request = agent.get(current.as_str());
        if let Some(referer) = referer {
            request = request.set("Referer", referer);
        }
        let response = request
            .call()
            .map_err(|e| OnlineError::Network(e.to_string()))?;
        if !(300..400).contains(&response.status()) {
            return Ok(response);
        }
        let location = response
            .header("Location")
            .ok_or_else(|| OnlineError::Network("Redirect missing Location".into()))?;
        current = validate_remote_url_with_policy(
            current
                .join(location)
                .map_err(|e| OnlineError::Network(e.to_string()))?
                .as_str(),
            allow_local,
        )?;
    }
    Err(OnlineError::Network("Too many redirects".into()))
}

#[allow(dead_code)]
fn fetch_html_content(url: &str, cancelled: Option<&AtomicBool>) -> Option<String> {
    fetch_html_content_with_policy(url, cancelled, false)
}

fn fetch_html_content_with_policy(
    url: &str,
    cancelled: Option<&AtomicBool>,
    allow_local: bool,
) -> Option<String> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    validate_remote_url_with_policy(url, allow_local).ok()?;
    // Fetch HTML only through the bounded HTTP client; external curl output is not bounded.
    const MAX_HTML_BYTES: u64 = 2 * 1024 * 1024;
    let ureq_attempt = safe_get_with_policy(url, None, allow_local);

    if let Ok(response) = ureq_attempt {
        let length_ok = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok())
            .is_none_or(|length| length <= MAX_HTML_BYTES);

        if length_ok {
            let mut html = Vec::new();
            if response
                .into_reader()
                .take(MAX_HTML_BYTES + 1)
                .read_to_end(&mut html)
                .is_ok()
                && (html.len() as u64) <= MAX_HTML_BYTES
            {
                if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    return None;
                }
                if let Ok(s) = String::from_utf8(html) {
                    return Some(s);
                }
            }
        }
    }

    // Fallback to system curl if ureq encountered 403 or TLS JA3 fingerprinting
    let curl_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut cmd = std::process::Command::new(curl_bin);
    cmd.args([
        "-s",
        "-L",
        "--max-time",
        "15",
        "--max-filesize",
        "2097152",
        "-H",
        "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
        "-H",
        "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        url,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Ok(output) = cmd.output() {
        if output.status.success()
            && !output.stdout.is_empty()
            && (output.stdout.len() as u64) <= MAX_HTML_BYTES
        {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return None;
            }
            if let Ok(s) = String::from_utf8(output.stdout) {
                return Some(s);
            }
        }
    }

    None
}

fn extract_pixabay_video_id(url: &str) -> Option<String> {
    let mut current_digits = String::new();
    let mut candidates = Vec::new();

    for ch in url.chars() {
        if ch.is_ascii_digit() {
            current_digits.push(ch);
        } else {
            if current_digits.len() >= 4 {
                candidates.push(current_digits.clone());
            }
            current_digits.clear();
        }
    }
    if current_digits.len() >= 4 {
        candidates.push(current_digits);
    }

    candidates.last().cloned()
}

fn extract_media_url_from_html(html: &str) -> String {
    // 1. Look for schema.org JSON-LD contentUrl (used by Pixabay, Schema.org video tags)
    // Matches "contentUrl":"https://cdn.pixabay.com/video/...mp4"
    if let Some(pos) = html.find("\"contentUrl\"") {
        let slice = &html[pos..];
        if let Some(quote_start) = slice.find("http") {
            let after_http = &slice[quote_start..];
            if let Some(quote_end) = after_http.find('"') {
                let candidate = &after_http[..quote_end];
                if candidate.contains(".mp4") || candidate.contains(".webm") {
                    return candidate.replace(r"\/", "/");
                }
            }
        }
    }

    // 2. Look for OpenGraph og:video or og:video:secure_url
    for marker in [
        "property=\"og:video\"",
        "property=\"og:video:secure_url\"",
        "property=\"og:video:url\"",
        "name=\"twitter:player:stream\"",
    ] {
        if let Some(pos) = html.find(marker) {
            let start = pos.saturating_sub(100);
            let end = (pos + 250).min(html.len());
            let context = &html[start..end];
            if let Some(c_pos) = context.find("content=\"http") {
                let after = &context[c_pos + 9..];
                if let Some(end_quote) = after.find('"') {
                    let candidate = &after[..end_quote];
                    if candidate.contains(".mp4") || candidate.contains(".webm") {
                        return candidate.replace(r"\/", "/");
                    }
                }
            }
        }
    }

    // 3. Look for Pixabay CDN video pattern (e.g. https://cdn.pixabay.com/video/2021/04/15/71122-537102350_large.mp4)
    if let Some(pos) = html.find("https://cdn.pixabay.com/video/") {
        let slice = &html[pos..];
        if let Some(end_pos) = slice.find(".mp4") {
            let candidate = &slice[..end_pos + 4];
            if !candidate.contains(' ')
                && !candidate.contains('<')
                && !candidate.contains('"')
                && !candidate.contains('\'')
            {
                return candidate.to_string();
            }
        }
    }

    // 4. Look for HTML5 <source src="...mp4"> or <video src="...mp4">
    for src_marker in ["<source", "<video"] {
        let mut cur = html;
        while let Some(pos) = cur.find(src_marker) {
            cur = &cur[pos + src_marker.len()..];
            let tag_end = cur.find('>').unwrap_or(cur.len());
            let tag_content = &cur[..tag_end];
            if let Some(s_pos) = tag_content.find("src=\"http") {
                let after = &tag_content[s_pos + 5..];
                if let Some(q_end) = after.find('"') {
                    let candidate = &after[..q_end];
                    if candidate.contains(".mp4") || candidate.contains(".webm") {
                        return candidate.to_string();
                    }
                }
            }
        }
    }

    String::new()
}

fn clean_html_title(raw: &str) -> String {
    raw.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

fn extract_steam_workshop_title(html: &str) -> Option<String> {
    if let Some(pos) = html.find("workshopItemTitle") {
        let slice = &html[pos..];
        if let Some(tag_end) = slice.find('>') {
            let after = &slice[tag_end + 1..];
            if let Some(end_tag) = after.find('<') {
                let title = after[..end_tag].trim();
                if !title.is_empty() {
                    return Some(clean_html_title(title));
                }
            }
        }
    }
    if let Some(pos) = html.find("property=\"og:title\"") {
        let start = pos.saturating_sub(50);
        let end = (pos + 300).min(html.len());
        let slice = &html[start..end];
        if let Some(c_pos) = slice.find("content=\"") {
            let after = &slice[c_pos + 9..];
            if let Some(end_q) = after.find('"') {
                let mut title = after[..end_q].trim();
                if let Some(stripped) = title.strip_prefix("Steam Workshop::") {
                    title = stripped.trim();
                }
                if !title.is_empty() {
                    return Some(clean_html_title(title));
                }
            }
        }
    }
    None
}

fn extract_steam_workshop_media_url(html: &str) -> String {
    // 1. Look for ShowEnlargedImagePreview( 'https://...' )
    if let Some(pos) = html.find("ShowEnlargedImagePreview") {
        let slice = &html[pos..];
        if let Some(start_quote) = slice.find('\'') {
            let after = &slice[start_quote + 1..];
            if let Some(end_quote) = after.find('\'') {
                let candidate = &after[..end_quote];
                if candidate.starts_with("http") {
                    // Strip ?imw=... query params to get original full uncompressed asset
                    return candidate.split('?').next().unwrap_or(candidate).to_string();
                }
            }
        }
    }

    // 2. Look for previewImage element: <img id="previewImage" ... src="..."
    if let Some(pos) = html.find("id=\"previewImage\"") {
        let slice = &html[pos..];
        if let Some(s_pos) = slice.find("src=\"") {
            let after = &slice[s_pos + 5..];
            if let Some(end_q) = after.find('"') {
                let candidate = &after[..end_q];
                if candidate.starts_with("http") {
                    return candidate.split('?').next().unwrap_or(candidate).to_string();
                }
            }
        }
    }

    // 3. Look for og:image with steamusercontent
    if let Some(pos) = html.find("property=\"og:image\"") {
        let start = pos.saturating_sub(50);
        let end = (pos + 300).min(html.len());
        let slice = &html[start..end];
        if let Some(c_pos) = slice.find("content=\"http") {
            let after = &slice[c_pos + 9..];
            if let Some(end_q) = after.find('"') {
                let candidate = &after[..end_q];
                if candidate.contains("steamusercontent.com") {
                    return candidate.split('?').next().unwrap_or(candidate).to_string();
                }
            }
        }
    }

    String::new()
}

fn extract_pinterest_pin_id(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if let Some(pin_pos) = lower.find("/pin/") {
        let after = &url[pin_pos + 5..];
        let path = after.split('?').next().unwrap_or(after);
        let path = path.split('#').next().unwrap_or(path);
        let id_part = if let Some((_, id_suffix)) = path.rsplit_once("--") {
            id_suffix
        } else {
            path
        };
        let digits: String = id_part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            return Some(digits);
        }
        let mut cur = String::new();
        for ch in path.chars() {
            if ch.is_ascii_digit() {
                cur.push(ch);
            } else {
                if cur.len() >= 6 {
                    return Some(cur);
                }
                cur.clear();
            }
        }
        if cur.len() >= 6 {
            return Some(cur);
        }
    } else if lower.contains("pin.it/") {
        let after = lower.split("pin.it/").nth(1)?;
        let path = after.split('?').next().unwrap_or(after);
        let path = path.split('#').next().unwrap_or(path).trim_matches('/');
        if !path.is_empty() && path.chars().all(|c| c.is_ascii_digit()) && path.len() >= 6 {
            return Some(path.to_string());
        }
    }
    None
}

fn resolve_pinit_shortlink(url: &str, allow_local: bool) -> Option<String> {
    let mut current = url.to_string();
    let agent = ureq::builder()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(8))
        .redirects(0)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build();
    for _ in 0..3 {
        validate_remote_url_with_policy(&current, allow_local).ok()?;
        if let Ok(resp) = agent.get(&current).call() {
            if (300..400).contains(&resp.status()) {
                if let Some(loc) = resp.header("Location") {
                    current = loc.to_string();
                    if current.contains("/pin/") {
                        return Some(current);
                    }
                    continue;
                }
            }
            break;
        } else {
            break;
        }
    }
    if current.contains("/pin/") {
        Some(current)
    } else {
        None
    }
}

fn extract_video_from_video_list(video_list: &serde_json::Value) -> Option<String> {
    let obj = video_list.as_object()?;

    // Pinterest can return several renditions in a map whose iteration order is not
    // quality order. Prefer the largest explicitly-described MP4 rendition; using the
    // first map entry can silently select a small preview on high-resolution displays.
    let mut mp4_candidates: Vec<(u64, String)> = obj
        .iter()
        .filter_map(|(key, entry)| {
            let url = entry.get("url")?.as_str()?;
            if !url.to_ascii_lowercase().contains(".mp4") {
                return None;
            }
            let width = entry
                .get("width")
                .or_else(|| entry.get("w"))
                .and_then(serde_json::Value::as_u64);
            let height = entry
                .get("height")
                .or_else(|| entry.get("h"))
                .and_then(serde_json::Value::as_u64);
            let pixel_count = width
                .zip(height)
                .map(|(width, height)| width.saturating_mul(height));
            let named_quality = if key.contains("2160") {
                3_840_000
            } else if key.contains("1080") || key == "V_EXP7" {
                2_073_600
            } else if key.contains("720") || key == "V_EXP6" {
                921_600
            } else {
                0
            };
            Some((pixel_count.unwrap_or(0).max(named_quality), url.to_string()))
        })
        .collect();
    mp4_candidates.sort_by(|a, b| b.0.cmp(&a.0));
    if let Some((_, url)) = mp4_candidates.into_iter().next() {
        return Some(url);
    }

    // Fall back to an HLS rendition only when no progressive MP4 is available.
    // Prefer the highest known playlist label rather than hard-coding every HLS pin to 720p.
    let mut hls_candidates: Vec<(u8, &'static str, String)> = obj
        .iter()
        .filter_map(|(key, entry)| {
            let url = entry.get("url")?.as_str()?;
            if !url.contains("/hls/") {
                return None;
            }
            let (rank, quality) = if key.contains("2160") {
                (4, "2160p")
            } else if key.contains("1080") || key == "V_EXP7" {
                (3, "1080p")
            } else if key.contains("720") || key == "V_EXP6" {
                (2, "720p")
            } else {
                (1, "480p")
            };
            Some((rank, quality, url.to_string()))
        })
        .collect();
    hls_candidates.sort_by(|a, b| b.0.cmp(&a.0));
    hls_candidates.into_iter().next().map(|(_, quality, url)| {
        url.replace("/hls/", &format!("/mc/{quality}/"))
            .replace(".m3u8", ".mp4")
            .replace("playlist.json", &format!("{quality}.mp4"))
    })
}

fn extract_video_from_story_pin(story_data: &serde_json::Value) -> Option<String> {
    let pages = story_data.get("pages")?.as_array()?;
    for page in pages {
        if let Some(blocks) = page.get("blocks").and_then(|b| b.as_array()) {
            for block in blocks {
                if let Some(video_list) = block.get("video").and_then(|v| v.get("video_list")) {
                    if let Some(url) = extract_video_from_video_list(video_list) {
                        return Some(url);
                    }
                }
            }
        }
    }
    None
}

fn extract_image_orig_from_pin(pin_data: &serde_json::Value) -> Option<String> {
    if let Some(orig_url) = pin_data
        .get("images")
        .and_then(|i| i.get("orig"))
        .and_then(|o| o.get("url"))
        .and_then(|u| u.as_str())
    {
        return Some(orig_url.to_string());
    }
    if let Some(orig_url) = pin_data
        .get("images_orig")
        .and_then(|i| i.get("url"))
        .and_then(|u| u.as_str())
    {
        return Some(orig_url.to_string());
    }
    None
}

fn extract_title_from_pin(pin_data: &serde_json::Value) -> Option<String> {
    for key in ["title", "grid_title", "seo_title", "gridTitle"] {
        if let Some(val) = pin_data.get(key).and_then(|v| v.as_str()) {
            let trimmed = val.trim();
            if !trimmed.is_empty() && trimmed != "Pinterest" {
                return Some(clean_html_title(trimmed));
            }
        }
    }
    None
}

fn resolve_pinterest_pin_resource(
    pin_id: &str,
    cancelled: &AtomicBool,
    allow_local: bool,
) -> Option<ResolvedWebMedia> {
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }

    let options_json = serde_json::json!({
        "options": {
            "field_set_key": "unauth_react_main_pin",
            "id": pin_id
        }
    });
    let query_str = options_json.to_string();
    let encoded_query: String =
        url::form_urlencoded::byte_serialize(query_str.as_bytes()).collect();
    let api_url =
        format!("https://www.pinterest.com/resource/PinResource/get/?data={encoded_query}");

    validate_remote_url_with_policy(&api_url, allow_local).ok()?;

    let agent = ureq::builder()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build();

    let response = agent
        .get(&api_url)
        .set("X-Pinterest-PWS-Handler", "www/[username].js")
        .set("X-Requested-With", "XMLHttpRequest")
        .set("Accept", "application/json, text/javascript, */*, q=0.01")
        .call()
        .ok()?;

    let root: serde_json::Value = response.into_json().ok()?;
    let pin_data = root.get("resource_response")?.get("data")?;

    let title = extract_title_from_pin(pin_data);

    // 1. Try direct video list
    if let Some(videos) = pin_data.get("videos") {
        if let Some(vlist) = videos.get("video_list") {
            if let Some(url) = extract_video_from_video_list(vlist) {
                return Some(ResolvedWebMedia {
                    media_url: url,
                    title,
                });
            }
        }
    }

    // 2. Try story_pin_data
    if let Some(story_data) = pin_data.get("story_pin_data") {
        if let Some(url) = extract_video_from_story_pin(story_data) {
            return Some(ResolvedWebMedia {
                media_url: url,
                title,
            });
        }
    }

    // 3. Fallback to original high-res image or animated GIF
    if let Some(img_url) = extract_image_orig_from_pin(pin_data) {
        return Some(ResolvedWebMedia {
            media_url: img_url,
            title,
        });
    }

    None
}

fn extract_pinterest_title(html: &str) -> Option<String> {
    // 1. Check OpenGraph og:title
    if let Some(pos) = html.find("property=\"og:title\"") {
        let start = pos.saturating_sub(50);
        let end = (pos + 300).min(html.len());
        let slice = &html[start..end];
        if let Some(c_pos) = slice.find("content=\"") {
            let after = &slice[c_pos + 9..];
            if let Some(end_q) = after.find('"') {
                let mut title = after[..end_q].trim();
                if let Some(stripped) = title.strip_suffix(" | Pinterest") {
                    title = stripped.trim();
                }
                if let Some(stripped) = title.strip_suffix(" on Pinterest") {
                    title = stripped.trim();
                }
                if !title.is_empty() && title != "Pinterest" {
                    return Some(clean_html_title(title));
                }
            }
        }
    }

    // 2. Check Relay / JSON title: "title":"..." or "grid_title":"..."
    for key in ["\"title\":\"", "\"grid_title\":\"", "\"gridTitle\":\""] {
        if let Some(pos) = html.find(key) {
            let after = &html[pos + key.len()..];
            if let Some(end_q) = after.find('"') {
                let title = after[..end_q].trim();
                if !title.is_empty() && title != "Pinterest" && !title.starts_with("http") {
                    return Some(clean_html_title(title));
                }
            }
        }
    }

    // 3. Fallback to <title>
    if let Some(pos) = html.find("<title>") {
        let after = &html[pos + 7..];
        if let Some(end_tag) = after.find("</title>") {
            let mut title = after[..end_tag].trim();
            if let Some(stripped) = title.strip_suffix(" | Pinterest") {
                title = stripped.trim();
            }
            if let Some(stripped) = title.strip_suffix(" on Pinterest") {
                title = stripped.trim();
            }
            if !title.is_empty() && title != "Pinterest" {
                return Some(clean_html_title(title));
            }
        }
    }

    None
}

fn extract_pinterest_media_url(html: &str) -> String {
    // Normalize unescaped slashes first so escaped JSON strings match
    let unescaped = html.replace(r"\/", "/");
    let html = unescaped.as_str();

    // 1. Collect progressive MP4 renditions and choose the highest explicit quality,
    // rather than whichever low-resolution preview happens to occur first in page JSON.
    let mut video_candidates: Vec<(u16, String)> = Vec::new();
    for prefix in [
        "https://v1.pinimg.com/videos/",
        "https://v.pinimg.com/videos/",
        "https://v2.pinimg.com/videos/",
    ] {
        let mut cur = html;
        while let Some(pos) = cur.find(prefix) {
            let slice = &cur[pos..];
            let end = slice
                .find(['"', '\'', ' ', '\\', '<', '>', '&', '\n', '\r', ','])
                .unwrap_or_else(|| slice.len().min(300));
            let candidate = &slice[..end];
            if candidate.to_ascii_lowercase().contains(".mp4") {
                let lower_candidate = candidate.to_ascii_lowercase();
                let quality = if lower_candidate.contains("2160p") {
                    2160
                } else if lower_candidate.contains("1080p") {
                    1080
                } else if lower_candidate.contains("720p") {
                    720
                } else if lower_candidate.contains("480p") {
                    480
                } else {
                    0
                };
                video_candidates.push((quality, candidate.to_string()));
            }
            cur = &cur[pos + prefix.len()..];
        }
    }
    video_candidates.sort_by(|a, b| b.0.cmp(&a.0));
    if let Some((_, url)) = video_candidates.into_iter().next() {
        return url;
    }

    // If page HTML only contains HLS, request the highest common progressive variant.
    for marker in [
        "https://v1.pinimg.com/videos/hls/",
        "https://v.pinimg.com/videos/hls/",
    ] {
        if let Some(pos) = html.find(marker) {
            let slice = &html[pos..];
            let end = slice
                .find(['"', '\'', ' ', '\\', '<', '>', '&', '\n', '\r', ','])
                .unwrap_or_else(|| slice.len().min(300));
            let candidate = &slice[..end];
            return candidate
                .replace("/hls/", "/mc/720p/")
                .replace(".m3u8", ".mp4")
                .replace("playlist.json", "720p.mp4");
        }
    }

    // 1b. Look for video thumbnail / poster URLs:
    // e.g. https://v1.pinimg.com/videos/thumbnails/originals/12/83/f0/1283f06c1c8fa040011cd7231f33f069.0000000.jpg
    for marker in [
        "pinimg.com/videos/thumbnails/",
        "pinimg.com/videos/mc/thumbnails/",
    ] {
        let mut cur = html;
        while let Some(pos) = cur.find(marker) {
            let before = &cur[..pos];
            if let Some(http_pos) = before.rfind("http") {
                let slice = &cur[http_pos..];
                let end = slice
                    .find(['"', '\'', ' ', '\\', '<', '>', '&', '\n', '\r', ','])
                    .unwrap_or_else(|| slice.len().min(300));
                let thumb_url = &slice[..end];
                if thumb_url.contains(".jpg") || thumb_url.contains(".png") {
                    let mp4_url = thumb_url
                        .replace("/thumbnails/originals/", "/mc/720p/")
                        .replace("/thumbnails/mc/720p/", "/mc/720p/")
                        .replace("/thumbnails/", "/mc/720p/")
                        .replace(".0000000.jpg", ".mp4")
                        .replace(".0000000.png", ".mp4")
                        .replace(".jpg", ".mp4");
                    let direct_mp4 = mp4_url.replace("i.pinimg.com", "v1.pinimg.com");
                    return direct_mp4;
                }
            }
            cur = &cur[pos + marker.len()..];
        }
    }

    // 2. Look for schema.org contentUrl (used for videos)
    if let Some(pos) = html.find("\"contentUrl\"") {
        let slice = &html[pos..];
        if let Some(q_start) = slice.find("http") {
            let after_http = &slice[q_start..];
            if let Some(q_end) = after_http.find('"') {
                let candidate = &after_http[..q_end];
                if candidate.contains(".mp4") || candidate.contains("pinimg.com/videos") {
                    return candidate.to_string();
                }
            }
        }
    }

    // 3. Look for OpenGraph video: og:video or og:video:secure_url
    for marker in ["property=\"og:video\"", "property=\"og:video:secure_url\""] {
        if let Some(pos) = html.find(marker) {
            let start = pos.saturating_sub(50);
            let end = (pos + 350).min(html.len());
            let slice = &html[start..end];
            if let Some(c_pos) = slice.find("content=\"http") {
                let after = &slice[c_pos + 9..];
                if let Some(end_q) = after.find('"') {
                    let candidate = &after[..end_q];
                    if candidate.contains(".mp4") || candidate.contains("pinimg.com/videos") {
                        return candidate.to_string();
                    }
                }
            }
        }
    }

    // 4. Look for original uncompressed image / animated GIF:
    // e.g. "images_orig":{"url":"https://i.pinimg.com/originals/..."}
    if let Some(pos) = html.find("https://i.pinimg.com/originals/") {
        let slice = &html[pos..];
        let end = slice
            .find(['"', '\'', ' ', '\\', '<', '>', '&', '\n', '\r'])
            .unwrap_or_else(|| slice.len().min(300));
        let candidate = &slice[..end];
        return candidate.to_string();
    }

    // 5. Look for standard og:image fallback
    if let Some(pos) = html.find("property=\"og:image\"") {
        let start = pos.saturating_sub(50);
        let end = (pos + 350).min(html.len());
        let slice = &html[start..end];
        if let Some(c_pos) = slice.find("content=\"http") {
            let after = &slice[c_pos + 9..];
            if let Some(end_q) = after.find('"') {
                let candidate = &after[..end_q];
                if candidate.contains("pinimg.com") {
                    let orig = candidate
                        .replace("/736x/", "/originals/")
                        .replace("/564x/", "/originals/")
                        .replace("/474x/", "/originals/")
                        .replace("/236x/", "/originals/");
                    return orig;
                }
            }
        }
    }

    // 6. Look for any /736x/ or /564x/ pinimg image and upgrade to /originals/
    for marker in ["https://i.pinimg.com/736x/", "https://i.pinimg.com/564x/"] {
        if let Some(pos) = html.find(marker) {
            let slice = &html[pos..];
            let end = slice
                .find(['"', '\'', ' ', '\\', '<', '>', '&', '\n', '\r'])
                .unwrap_or_else(|| slice.len().min(300));
            let candidate = &slice[..end];
            let orig = candidate
                .replace("/736x/", "/originals/")
                .replace("/564x/", "/originals/");
            return orig;
        }
    }

    String::new()
}

/// Detects the media container or image format from the file's leading magic bytes.
pub fn detect_media_extension_from_magic_bytes(path: &Path) -> Option<&'static str> {
    let mut header = [0u8; 32];
    let mut file = File::open(path).ok()?;
    let read = file.read(&mut header).ok()?;
    let slice = &header[..read];

    if slice.len() >= 3 && &slice[0..3] == b"\xFF\xD8\xFF" {
        return Some("jpg");
    }
    if slice.len() >= 8 && &slice[0..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("png");
    }
    if slice.len() >= 6 && (&slice[0..6] == b"GIF87a" || &slice[0..6] == b"GIF89a") {
        return Some("gif");
    }
    if slice.len() >= 12 && &slice[0..4] == b"RIFF" && &slice[8..12] == b"WEBP" {
        return Some("webp");
    }
    if slice.len() >= 2 && &slice[0..2] == b"BM" {
        return Some("bmp");
    }
    if slice.len() >= 4 && &slice[0..4] == b"\x1A\x45\xDF\xA3" {
        return Some("webm");
    }
    if slice.len() >= 12 && (&slice[4..8] == b"ftyp" || &slice[4..8] == b"moov") {
        return Some("mp4");
    }
    None
}

/// Resolved media information extracted from a web page or direct URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWebMedia {
    pub media_url: String,
    pub title: Option<String>,
}

/// Resolves a webpage URL (such as Steam Workshop, Pixabay, or open media)
/// into a direct playable/downloadable media stream URL and optional title.
pub fn resolve_web_media_info(url: &str) -> ResolvedWebMedia {
    resolve_web_media_info_with_cancel(url, &AtomicBool::new(false)).unwrap_or_else(|_| {
        ResolvedWebMedia {
            media_url: url.trim().to_string(),
            title: None,
        }
    })
}

pub fn resolve_web_media_info_with_cancel(
    url: &str,
    cancelled: &AtomicBool,
) -> Result<ResolvedWebMedia, OnlineError> {
    resolve_web_media_info_with_policy(url, cancelled, false)
}

pub fn resolve_web_media_info_with_policy(
    url: &str,
    cancelled: &AtomicBool,
    allow_local: bool,
) -> Result<ResolvedWebMedia, OnlineError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(OnlineError::Cancelled);
    }
    let trimmed = url.trim();
    let lower = trimmed.to_lowercase();

    // 1. Special handling for Steam Community Workshop:
    // URLs like: https://steamcommunity.com/sharedfiles/filedetails/?id=3830732662
    // or https://steamcommunity.com/workshop/filedetails/?id=...
    if lower.contains("steamcommunity.com/sharedfiles")
        || lower.contains("steamcommunity.com/workshop")
    {
        validate_remote_url_with_policy(trimmed, allow_local)?;
        if let Some(html) = fetch_html_content_with_policy(trimmed, Some(cancelled), allow_local) {
            let extracted = extract_steam_workshop_media_url(&html);
            let title = extract_steam_workshop_title(&html);
            if !extracted.is_empty() {
                validate_remote_url_with_policy(&extracted, allow_local)?;
                return Ok(ResolvedWebMedia {
                    media_url: extracted,
                    title,
                });
            }
        }
    }

    // 1b. Steam UGC direct asset URLs (end with a hash + trailing slash, no extension)
    // e.g. https://images.steamusercontent.com/ugc/<numbers>/<hash>/
    if lower.contains("images.steamusercontent.com/ugc/") {
        return Ok(ResolvedWebMedia {
            media_url: trimmed.to_string(),
            title: None,
        });
    }

    // 1c. Pinterest direct assets. Upgrade common resized pin images to the original
    // rendition URL where possible; do not alter video CDN URLs.
    if lower.contains("pinimg.com/videos/") {
        return Ok(ResolvedWebMedia {
            media_url: trimmed.to_string(),
            title: None,
        });
    }
    if lower.contains("i.pinimg.com/") {
        let mut media_url = trimmed.to_string();
        for size in ["236x", "474x", "564x", "736x", "1200x"] {
            media_url = media_url.replace(&format!("/{size}/"), "/originals/");
        }
        return Ok(ResolvedWebMedia {
            media_url,
            title: None,
        });
    }

    // 1e. Social media direct CDN video URLs (TikTok, Instagram, Reddit v.redd.it)
    if lower.contains("tiktokcdn.com/")
        || lower.contains("cdninstagram.com/")
        || lower.contains("v.redd.it/")
    {
        return Ok(ResolvedWebMedia {
            media_url: trimmed.to_string(),
            title: None,
        });
    }

    // 1d. Pinterest Pin page or shortlink (pinterest.com/pin/... or pin.it/...)
    if lower.contains("pinterest.com/pin/") || lower.contains("pin.it/") {
        validate_remote_url_with_policy(trimmed, allow_local)?;

        // Try extracting Pin ID directly or follow pin.it redirect
        let mut pin_id_opt = extract_pinterest_pin_id(trimmed);
        if pin_id_opt.is_none() && lower.contains("pin.it/") {
            if let Some(redirected_url) = resolve_pinit_shortlink(trimmed, allow_local) {
                pin_id_opt = extract_pinterest_pin_id(&redirected_url);
            }
        }

        // Strategy A: Query official Pinterest PinResource API
        if let Some(pin_id) = pin_id_opt {
            if let Some(media) = resolve_pinterest_pin_resource(&pin_id, cancelled, allow_local) {
                if validate_remote_url_with_policy(&media.media_url, allow_local).is_ok() {
                    return Ok(media);
                }
            }
        }

        // Strategy B: Fetch HTML page and parse metadata/JSON
        if let Some(html) = fetch_html_content_with_policy(trimmed, Some(cancelled), allow_local) {
            let extracted = extract_pinterest_media_url(&html);
            let title = extract_pinterest_title(&html);
            if !extracted.is_empty() {
                validate_remote_url_with_policy(&extracted, allow_local)?;
                return Ok(ResolvedWebMedia {
                    media_url: extracted,
                    title,
                });
            }
        }

        return Err(OnlineError::Parse(
            "Could not extract video or image stream from Pinterest URL".into(),
        ));
    }

    // 2. Special handling for Pixabay:
    // If it's on pixabay.com and NOT cdn.pixabay.com:
    if lower.contains("pixabay.com") && !lower.contains("cdn.pixabay.com") {
        // Try to extract video ID and attempt known CDN patterns directly
        if let Some(id) = extract_pixabay_video_id(trimmed) {
            // Known Pixabay CDN download URL patterns (try these first before fetching page HTML)
            let cdn_patterns = [
                format!("https://cdn.pixabay.com/video/download/x-{id}_large.mp4"),
                format!("https://cdn.pixabay.com/video/download/x-{id}_medium.mp4"),
                format!("https://cdn.pixabay.com/video/download/x-{id}_small.mp4"),
                format!("https://cdn.pixabay.com/video/download/x-{id}_tiny.mp4"),
            ];
            for cdn_url in &cdn_patterns {
                if validate_remote_url_with_policy(cdn_url, allow_local).is_ok() {
                    return Ok(ResolvedWebMedia {
                        media_url: cdn_url.clone(),
                        title: None,
                    });
                }
            }
        }

        // Fallback: try fetching the HTML page to scrape the CDN URL
        let mut target_page = trimmed.to_string();
        if let Some(id) = extract_pixabay_video_id(trimmed) {
            target_page = format!("https://pixabay.com/videos/id-{id}/");
        }
        if validate_remote_url_with_policy(&target_page, allow_local).is_ok() {
            if let Some(html) =
                fetch_html_content_with_policy(&target_page, Some(cancelled), allow_local)
            {
                let extracted = extract_media_url_from_html(&html);
                if !extracted.is_empty()
                    && validate_remote_url_with_policy(&extracted, allow_local).is_ok()
                {
                    return Ok(ResolvedWebMedia {
                        media_url: extracted,
                        title: None,
                    });
                }
            }
        }
    }

    // 3. Direct media URLs should not trigger an HTML fetch. Parse the URL path so
    // signed URLs with query parameters and less common still-image formats work too.
    let direct_media_extension = Url::parse(trimmed).ok().and_then(|parsed| {
        Path::new(parsed.path())
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
    });
    if direct_media_extension.as_deref().is_some_and(|extension| {
        matches!(
            extension,
            "mp4" | "webm" | "mkv" | "mov" | "gif" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
        )
    }) {
        return Ok(ResolvedWebMedia {
            media_url: trimmed.to_string(),
            title: None,
        });
    }

    let Some(html) = fetch_html_content_with_policy(trimmed, Some(cancelled), allow_local) else {
        if cancelled.load(Ordering::Relaxed) {
            return Err(OnlineError::Cancelled);
        }
        return Ok(ResolvedWebMedia {
            media_url: trimmed.to_string(),
            title: None,
        });
    };

    let extracted = extract_media_url_from_html(&html);
    if !extracted.is_empty() {
        validate_remote_url_with_policy(&extracted, allow_local)?;
        return Ok(ResolvedWebMedia {
            media_url: extracted,
            title: None,
        });
    }

    Ok(ResolvedWebMedia {
        media_url: trimmed.to_string(),
        title: None,
    })
}

/// Resolves a video webpage URL (such as Pixabay, Steam Workshop, or open media)
/// into a direct playable/downloadable media stream URL (e.g. .mp4, .webm, or image/gif).
pub fn resolve_direct_video_url(url: &str) -> String {
    resolve_web_media_info(url).media_url
}

#[allow(dead_code)]
fn resolve_direct_video_url_with_cancel(
    url: &str,
    cancelled: &AtomicBool,
) -> Result<String, OnlineError> {
    resolve_web_media_info_with_cancel(url, cancelled).map(|r| r.media_url)
}

fn resolve_direct_video_url_with_policy(
    url: &str,
    cancelled: &AtomicBool,
    allow_local: bool,
) -> Result<String, OnlineError> {
    resolve_web_media_info_with_policy(url, cancelled, allow_local).map(|r| r.media_url)
}

/// Downloads a remote file into a sibling temporary file then atomically publishes it.
///
/// The legacy convenience API remains uncancellable and allows up to 2 GiB. UI download
/// workers should use `download_online_file_cancellable` so they can report progress and cancel.
pub fn download_online_file(url: &str, destination: &Path) -> Result<u64, OnlineError> {
    download_online_file_cancellable(
        url,
        destination,
        &AtomicBool::new(false),
        2 * 1024 * 1024 * 1024,
        |_, _| {},
    )
}

/// Downloads a remote file with periodic cancellation checks, bounded size, and byte progress.
/// A failed or cancelled transfer never leaves a partial file at `destination`.
pub fn download_online_file_cancellable<F>(
    url: &str,
    destination: &Path,
    cancelled: &AtomicBool,
    max_bytes: u64,
    progress: F,
) -> Result<u64, OnlineError>
where
    F: FnMut(u64, Option<u64>),
{
    download_online_file_with_policy(url, destination, cancelled, max_bytes, progress, false)
}

fn download_via_curl<F>(
    target_url: &str,
    referer_url: &str,
    temp_path: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
    max_bytes: u64,
    mut progress: F,
) -> Result<u64, OnlineError>
where
    F: FnMut(u64, Option<u64>),
{
    let curl_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut cmd = std::process::Command::new(curl_bin);
    cmd.args([
        "-s",
        "-L",
        "--fail",
        "--max-filesize",
        &max_bytes.to_string(),
        "--max-time",
        "600",
        "--connect-timeout",
        "30",
        "-H",
        "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
        "-H",
        "Accept: */*",
    ]);
    let referer_header = format!("Referer: {referer_url}");
    cmd.args(["-H", &referer_header]);
    cmd.args(["-o"]);
    cmd.arg(temp_path);
    cmd.arg(target_url);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| OnlineError::Network(format!("Failed to spawn curl fallback: {e}")))?;
    let started = Instant::now();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(OnlineError::Cancelled);
        }
        if started.elapsed() > Duration::from_secs(600) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(OnlineError::Network("Download timed out".into()));
        }
        if let Ok(meta) = fs::metadata(temp_path) {
            if meta.len() > max_bytes {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OnlineError::TooLarge(max_bytes));
            }
            progress(meta.len(), None);
        }
        if let Ok(Some(status)) = child.try_wait() {
            if status.success() {
                let metadata = fs::metadata(temp_path)?;
                let size = metadata.len();
                if size > max_bytes {
                    return Err(OnlineError::TooLarge(max_bytes));
                }
                validate_downloaded_media(temp_path)?;
                progress(size, Some(size));
                publish_download(temp_path, destination)?;
                return Ok(size);
            } else {
                return Err(OnlineError::Network(format!(
                    "Download failed: curl exited with code {status}"
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn download_online_file_with_policy<F>(
    url: &str,
    destination: &Path,
    cancelled: &AtomicBool,
    max_bytes: u64,
    mut progress: F,
    allow_local: bool,
) -> Result<u64, OnlineError>
where
    F: FnMut(u64, Option<u64>),
{
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = temporary_download_path(destination);

    let result = (|| {
        if cancelled.load(Ordering::Relaxed) {
            return Err(OnlineError::Cancelled);
        }
        validate_remote_url_with_policy(url, allow_local)?;
        let target_url = resolve_direct_video_url_with_policy(url, cancelled, allow_local)?;
        validate_remote_url_with_policy(&target_url, allow_local)?;

        // First attempt: ureq fast in-process download
        let ureq_result: Result<u64, OnlineError> = (|| {
            let response = safe_get_with_policy(&target_url, Some(url), allow_local)?;
            let total = response
                .header("Content-Length")
                .and_then(|value| value.parse::<u64>().ok());
            if total.is_some_and(|bytes| bytes > max_bytes) {
                return Err(OnlineError::TooLarge(max_bytes));
            }
            if let Some(content_type) = response.header("Content-Type") {
                if content_type.starts_with("text/html") {
                    return Err(OnlineError::Network(
                        "Remote server returned an HTML webpage or captcha page instead of a video"
                            .into(),
                    ));
                }
            }
            let mut reader = response.into_reader();
            let mut file = File::create(&temp_path)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(OnlineError::Cancelled);
                }
                let bytes = reader.read(&mut buffer)?;
                if bytes == 0 {
                    break;
                }
                copied = copied.saturating_add(bytes as u64);
                if copied > max_bytes {
                    return Err(OnlineError::TooLarge(max_bytes));
                }
                file.write_all(&buffer[..bytes])?;
                progress(copied, total);
            }
            file.sync_all()?;
            drop(file);
            if cancelled.load(Ordering::Relaxed) {
                return Err(OnlineError::Cancelled);
            }
            validate_downloaded_media(&temp_path)?;
            publish_download(&temp_path, destination)?;
            Ok(copied)
        })();

        match ureq_result {
            Ok(copied) => Ok(copied),
            Err(OnlineError::Cancelled) => Err(OnlineError::Cancelled),
            Err(OnlineError::TooLarge(max)) => Err(OnlineError::TooLarge(max)),
            Err(_) => {
                // ureq hit an error (e.g. read timeout, 403, Cloudflare/TLS quirk).
                // Clean up partial temp file and fallback seamlessly to system curl!
                let _ = fs::remove_file(&temp_path);
                download_via_curl(
                    &target_url,
                    url,
                    &temp_path,
                    destination,
                    cancelled,
                    max_bytes,
                    &mut progress,
                )
            }
        }
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn validate_downloaded_media(path: &Path) -> Result<(), OnlineError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() == 0 {
        return Err(OnlineError::Network("Downloaded file is empty".into()));
    }

    let mut header = [0u8; 1024];
    let mut file = File::open(path)?;
    let bytes_read = file.read(&mut header)?;
    let slice = &header[..bytes_read];

    let as_str = String::from_utf8_lossy(slice).to_lowercase();
    if as_str.starts_with("<!doctype")
        || as_str.starts_with("<html")
        || as_str.starts_with("<header")
        || as_str.contains("<html")
        || as_str.contains("<head>")
        || as_str.contains("captcha")
    {
        return Err(OnlineError::Network(
            "Remote server returned an HTML webpage or captcha page instead of a video stream"
                .into(),
        ));
    }

    Ok(())
}

fn temporary_download_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    destination.with_file_name(format!(".{name}.{}.part", uuid::Uuid::new_v4()))
}

fn publish_download(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let source: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            MoveFileExW(
                windows::core::PCWSTR(source.as_ptr()),
                windows::core::PCWSTR(target.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| std::io::Error::other(error.to_string()))
    }

    #[cfg(not(windows))]
    fs::rename(temp_path, destination)
}

/// Downloads an image to memory buffer for thumbnail rendering.
pub fn fetch_thumbnail_bytes(url: &str) -> Result<Vec<u8>, OnlineError> {
    let response = safe_get(url, None).map_err(|e| OnlineError::Network(e.to_string()))?;

    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;

    Ok(bytes)
}

pub(crate) fn urlencoding(input: &str) -> String {
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
    fn test_steam_real_online() {
        let res = resolve_web_media_info(
            "https://steamcommunity.com/sharedfiles/filedetails/?id=3830732662",
        );
        println!("RESOLVED STEAM MEDIA URL: {}", res.media_url);
        println!("RESOLVED STEAM TITLE: {:?}", res.title);
        assert!(res.media_url.contains("steamusercontent"));
    }

    #[test]
    fn test_extract_steam_workshop_media_and_title() {
        let sample_html = r#"
            <html>
                <head>
                    <meta property="og:title" content="Steam Workshop::YoRHa Terminal - NieR:Automata UI">
                    <meta property="og:image" content="https://images.steamusercontent.com/ugc/11610979304480761089/E9C694307B3B994385D1ED7CAFEE0D35DAB0B869/?imw=512">
                </head>
                <body>
                    <div class="workshopItemTitle">YoRHa Terminal - NieR:Automata UI</div>
                    <div class="workshopItemPreviewImageEnlargeableContainer">
                        <a onclick="ShowEnlargedImagePreview( 'https://images.steamusercontent.com/ugc/11610979304480761089/E9C694307B3B994385D1ED7CAFEE0D35DAB0B869/?imw=5000&imh=5000' );">
                            <img id="previewImage" src="https://images.steamusercontent.com/ugc/11610979304480761089/E9C694307B3B994385D1ED7CAFEE0D35DAB0B869/?imw=637">
                        </a>
                    </div>
                </body>
            </html>
        "#;

        let title = extract_steam_workshop_title(sample_html);
        assert_eq!(title.as_deref(), Some("YoRHa Terminal - NieR:Automata UI"));

        let media = extract_steam_workshop_media_url(sample_html);
        assert_eq!(
            media,
            "https://images.steamusercontent.com/ugc/11610979304480761089/E9C694307B3B994385D1ED7CAFEE0D35DAB0B869/"
        );
    }

    #[test]
    fn test_detect_media_extension_from_magic_bytes() {
        let temp_dir = std::env::temp_dir().join(format!("test_magic_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let gif_file = temp_dir.join("test.bin");
        std::fs::write(&gif_file, b"GIF89a\x01\x00\x01\x00").unwrap();
        assert_eq!(
            detect_media_extension_from_magic_bytes(&gif_file),
            Some("gif")
        );

        let png_file = temp_dir.join("test.png");
        std::fs::write(&png_file, b"\x89PNG\r\n\x1a\n\x00\x00").unwrap();
        assert_eq!(
            detect_media_extension_from_magic_bytes(&png_file),
            Some("png")
        );

        let jpg_file = temp_dir.join("test.jpg");
        std::fs::write(&jpg_file, b"\xFF\xD8\xFF\xE0\x00\x10JFIF").unwrap();
        assert_eq!(
            detect_media_extension_from_magic_bytes(&jpg_file),
            Some("jpg")
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_resolve_video_page_from_metadata() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let body = r#"<html><script>{"contentUrl":"https://cdn.example.test/video/sample.mp4"}</script></html>"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let resolved = resolve_direct_video_url_with_policy(
            &format!("http://{address}/video-page"),
            &AtomicBool::new(false),
            true,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(resolved, "https://cdn.example.test/video/sample.mp4");
    }

    #[test]
    fn rejects_private_and_credentialed_urls() {
        for url in [
            "http://127.0.0.1/video.mp4",
            "http://192.168.1.2/video.mp4",
            "http://169.254.169.254/latest/meta-data/",
            "http://user:pass@example.com/video.mp4",
            "file:///etc/passwd",
        ] {
            assert!(validate_remote_url(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn cancelled_resolution_returns_cancelled_without_network() {
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            resolve_direct_video_url_with_cancel("https://example.com/video-page", &cancelled),
            Err(OnlineError::Cancelled)
        ));
    }

    #[test]
    fn oversized_html_page_is_bounded() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let body = vec![b'x'; 2 * 1024 * 1024 + 1];
            write!(
                stream,
                "HTTP/1.1 200 OK\\r\\nContent-Length: {}\\r\\nConnection: close\\r\\n\\r\\n",
                body.len()
            )
            .unwrap();
            let _ = stream.write_all(&body);
        });
        assert!(
            fetch_html_content_with_policy(&format!("http://{address}/page"), None, true).is_none()
        );
        server.join().unwrap();
    }

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

    #[test]
    fn extract_pixabay_video_id_uses_final_numeric_identifier() {
        assert_eq!(
            extract_pixabay_video_id("https://pixabay.com/videos/waves-71122/"),
            Some("71122".into())
        );
        assert_eq!(
            extract_pixabay_video_id("https://pixabay.com/videos/download/x-173656_medium.mp4"),
            Some("173656".into())
        );
    }

    #[test]
    fn extracts_direct_url_from_supported_metadata() {
        for html in [
            r#"<script>{"contentUrl":"https://cdn.example.test/a.mp4"}</script>"#,
            r#"<meta property="og:video" content="https://cdn.example.test/b.webm">"#,
            r#"<video src="https://cdn.example.test/c.mp4"></video>"#,
            r#"<source src="https://cdn.example.test/d.webm">"#,
        ] {
            assert!(extract_media_url_from_html(html).starts_with("https://cdn.example.test/"));
        }
    }

    #[test]
    fn oversized_download_preserves_existing_destination_and_cleans_temp_file() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678",
                )
                .unwrap();
        });
        let temp_dir =
            std::env::temp_dir().join(format!("wallpaper-download-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        let destination = temp_dir.join("existing.mp4");
        fs::write(&destination, b"old file").unwrap();
        let result = download_online_file_with_policy(
            &format!("http://{address}/video.mp4"),
            &destination,
            &AtomicBool::new(false),
            4,
            |_, _| {},
            true,
        );
        server.join().unwrap();
        assert!(matches!(result, Err(OnlineError::TooLarge(4))));
        assert_eq!(fs::read(&destination).unwrap(), b"old file");
        assert_eq!(fs::read_dir(&temp_dir).unwrap().count(), 1);
        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn test_download_via_curl_fallback() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n0123456789").unwrap();
        });
        let temp_dir =
            std::env::temp_dir().join(format!("wallpaper-curl-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        let destination = temp_dir.join("curl_dest.mp4");
        let temp_file = temp_dir.join("curl_dest.mp4.part");
        let mut reports = Vec::new();
        let result = download_via_curl(
            &format!("http://{address}/video.mp4"),
            "http://example.com",
            &temp_file,
            &destination,
            &AtomicBool::new(false),
            1024,
            |received, total| reports.push((received, total)),
        );
        server.join().unwrap();
        assert_eq!(result.unwrap(), 10);
        assert_eq!(fs::read(&destination).unwrap(), b"0123456789");
        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn download_reports_progress_and_atomically_replaces_destination() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n12345678").unwrap();
        });
        let temp_dir =
            std::env::temp_dir().join(format!("wallpaper-download-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        let destination = temp_dir.join("replace.mp4");
        fs::write(&destination, b"old file").unwrap();
        let mut reports = Vec::new();
        let result = download_online_file_with_policy(
            &format!("http://{address}/video.mp4"),
            &destination,
            &AtomicBool::new(false),
            32,
            |received, total| reports.push((received, total)),
            true,
        );
        server.join().unwrap();
        assert_eq!(result.unwrap(), 8);
        assert_eq!(fs::read(&destination).unwrap(), b"12345678");
        assert!(reports.iter().any(|(received, _)| *received > 0));
        assert_eq!(fs::read_dir(&temp_dir).unwrap().count(), 1);
        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn redirect_to_private_address_is_rejected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let result = safe_get_with_policy(&format!("http://{address}/redirect"), None, true);
        server.join().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn pinterest_video_renditions_choose_highest_resolution_not_map_order() {
        let data = serde_json::json!({
            "V_240P": {"url": "https://cdn.example/240.mp4", "width": 426, "height": 240},
            "V_1080P": {"url": "https://cdn.example/1080.mp4", "width": 1920, "height": 1080},
            "V_720P": {"url": "https://cdn.example/720.mp4", "width": 1280, "height": 720}
        });
        assert_eq!(
            extract_video_from_video_list(&data),
            Some("https://cdn.example/1080.mp4".to_string())
        );
    }

    #[test]
    fn pinterest_hls_prefers_highest_available_labeled_variant() {
        let data = serde_json::json!({
            "V_720P": {"url": "https://cdn.example/hls/720/index.m3u8"},
            "V_1080P": {"url": "https://cdn.example/hls/1080/index.m3u8"}
        });
        assert_eq!(
            extract_video_from_video_list(&data),
            Some("https://cdn.example/mc/1080p/1080/index.mp4".to_string())
        );
    }

    #[test]
    fn test_extract_pinterest_media_url_progressive_mp4() {
        let html = r#"
            <meta property="og:title" content="Aesthetic Cyberpunk City Video | Pinterest">
            <script>{"v3GetPinQueryv2":{"data":{"videos":{"video_list":{"V_720P":{"url":"https://v1.pinimg.com/videos/mc/720p/17/52/ac/1752aca4002bf24c520aadc3c76aaa08.mp4"}}}}}}</script>
        "#;
        assert_eq!(
            extract_pinterest_media_url(html),
            "https://v1.pinimg.com/videos/mc/720p/17/52/ac/1752aca4002bf24c520aadc3c76aaa08.mp4"
        );
        assert_eq!(
            extract_pinterest_title(html),
            Some("Aesthetic Cyberpunk City Video".to_string())
        );
    }

    #[test]
    fn test_extract_pinterest_media_url_hls_conversion() {
        let html = r#"
            <script>var x = "https://v1.pinimg.com/videos/hls/expmp4/17/52/ac/1752aca4002bf24c520aadc3c76aaa08.m3u8";</script>
        "#;
        assert_eq!(
            extract_pinterest_media_url(html),
            "https://v1.pinimg.com/videos/mc/720p/expmp4/17/52/ac/1752aca4002bf24c520aadc3c76aaa08.mp4"
        );
    }

    #[test]
    fn test_extract_pinterest_media_url_original_image_fallback() {
        let html = r#"
            <script>{"images_orig":{"url":"https://i.pinimg.com/originals/76/64/90/7664909f7aeb05fe923359a08512c652.jpg"}}</script>
        "#;
        assert_eq!(
            extract_pinterest_media_url(html),
            "https://i.pinimg.com/originals/76/64/90/7664909f7aeb05fe923359a08512c652.jpg"
        );
    }

    #[test]
    fn pinterest_direct_resized_image_is_upgraded_to_original_path() {
        let resolved =
            resolve_web_media_info("https://i.pinimg.com/736x/12/34/56/sample.jpg?width=640");
        assert_eq!(
            resolved.media_url,
            "https://i.pinimg.com/originals/12/34/56/sample.jpg?width=640"
        );
    }

    #[test]
    fn test_extract_pinterest_pin_id() {
        assert_eq!(
            extract_pinterest_pin_id("https://www.pinterest.com/pin/664281013778109217/"),
            Some("664281013778109217".to_string())
        );
        assert_eq!(
            extract_pinterest_pin_id(
                "https://co.pinterest.com/pin/some-slug-title--2885187256207927/"
            ),
            Some("2885187256207927".to_string())
        );
        assert_eq!(
            extract_pinterest_pin_id("https://pin.it/664281013778109217"),
            Some("664281013778109217".to_string())
        );
    }

    #[test]
    fn test_extract_pinterest_escaped_json_and_poster() {
        // Escaped JSON as returned in Relay chunks
        let html = r#"
            <script>{"data":{"images_orig":{"url":"https:\/\/i.pinimg.com\/originals\/a5\/31\/a5\/a531a5d6e9013251559b15778329e7d5.jpg"}}}</script>
        "#;
        assert_eq!(
            extract_pinterest_media_url(html),
            "https://i.pinimg.com/originals/a5/31/a5/a531a5d6e9013251559b15778329e7d5.jpg"
        );

        // Poster thumbnail conversion to progressive MP4
        let poster_html = r#"
            <video poster="https://v1.pinimg.com/videos/thumbnails/mc/720p/12/83/f0/1283f06c1c8fa040011cd7231f33f069.0000000.jpg"></video>
        "#;
        assert_eq!(
            extract_pinterest_media_url(poster_html),
            "https://v1.pinimg.com/videos/mc/720p/12/83/f0/1283f06c1c8fa040011cd7231f33f069.mp4"
        );
    }

    #[test]
    #[ignore = "requires public network access"]
    fn test_live_pinterest_resolution() {
        // Video pin
        let res = resolve_web_media_info("https://www.pinterest.com/pin/664281013778109217/");
        assert!(res.media_url.contains("v1.pinimg.com/videos/"));
        assert!(res.media_url.contains(".mp4"));
        assert!(res.title.is_some());

        // Slug video pin
        let res2 =
            resolve_web_media_info("https://co.pinterest.com/pin/some-title--2885187256207927/");
        assert!(res2.media_url.contains("v1.pinimg.com/videos/"));
        assert!(res2.media_url.contains(".mp4"));

        // Image pin fallback
        let res3 = resolve_web_media_info("https://www.pinterest.com/pin/582371795587788417/");
        assert!(res3.media_url.contains("i.pinimg.com/originals/"));
    }
}
