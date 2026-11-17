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
            thumbnail_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/person-bicycle-car-detection.mp4".to_string(),
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
            thumbnail_url: "https://raw.githubusercontent.com/intel-iot-devkit/sample-videos/master/driver-action-recognition.mp4".to_string(),
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
    ]
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
            let start = if pos > 100 { pos - 100 } else { 0 };
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
        let mut cur = &html[..];
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

/// Resolves a video webpage URL (such as Pixabay, Pexels, or open media pages)
/// into a direct playable/downloadable media stream URL (e.g. .mp4 or .webm).
pub fn resolve_direct_video_url(url: &str) -> String {
    resolve_direct_video_url_with_cancel(url, &AtomicBool::new(false))
        .unwrap_or_else(|_| url.trim().to_string())
}

fn resolve_direct_video_url_with_cancel(
    url: &str,
    cancelled: &AtomicBool,
) -> Result<String, OnlineError> {
    resolve_direct_video_url_with_policy(url, cancelled, false)
}

fn resolve_direct_video_url_with_policy(
    url: &str,
    cancelled: &AtomicBool,
    allow_local: bool,
) -> Result<String, OnlineError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(OnlineError::Cancelled);
    }
    let trimmed = url.trim();
    let lower = trimmed.to_lowercase();

    // Special handling for Pixabay:
    // If it's on pixabay.com and NOT cdn.pixabay.com:
    // Even if it ends with .mp4 (like /videos/download/x-173656_medium.mp4),
    // it is a webpage or download captcha gate, NOT a direct CDN stream!
    if lower.contains("pixabay.com") && !lower.contains("cdn.pixabay.com") {
        let mut target_page = trimmed.to_string();
        if let Some(id) = extract_pixabay_video_id(trimmed) {
            target_page = format!("https://pixabay.com/videos/id-{id}/");
        }
        validate_remote_url_with_policy(&target_page, allow_local)?;
        if let Some(html) =
            fetch_html_content_with_policy(&target_page, Some(cancelled), allow_local)
        {
            let extracted = extract_media_url_from_html(&html);
            if !extracted.is_empty() {
                validate_remote_url_with_policy(&extracted, allow_local)?;
                return Ok(extracted);
            }
        }
    }

    // If it's already a direct video or audio/image link, return as-is
    if lower.ends_with(".mp4")
        || lower.ends_with(".webm")
        || lower.ends_with(".mkv")
        || lower.ends_with(".mov")
        || lower.ends_with(".gif")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.contains(".mp4?")
        || lower.contains(".webm?")
    {
        return Ok(trimmed.to_string());
    }

    let Some(html) = fetch_html_content_with_policy(trimmed, Some(cancelled), allow_local) else {
        if cancelled.load(Ordering::Relaxed) {
            return Err(OnlineError::Cancelled);
        }
        return Ok(trimmed.to_string());
    };

    let extracted = extract_media_url_from_html(&html);
    if !extracted.is_empty() {
        validate_remote_url_with_policy(&extracted, allow_local)?;
        return Ok(extracted);
    }

    Ok(trimmed.to_string())
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
        return unsafe {
            MoveFileExW(
                windows::core::PCWSTR(source.as_ptr()),
                windows::core::PCWSTR(target.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| std::io::Error::other(error.to_string()));
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
}
