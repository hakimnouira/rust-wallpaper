//! Embedded local HTTP API server for browser extension and Web-to-Desktop integration.
//!
//! Listens strictly on `127.0.0.1:24242` to securely allow browser extensions,
//! userscripts, and local web applications to set online wallpapers, query
//! active monitors, and control playback.

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};
use wallpaper_protocol::{Command, RequestEnvelope, Response};

use crate::ipc::PendingRequest;

pub const DEFAULT_PORT: u16 = 24242;

/// Retrieves the existing API bearer token or securely generates and persists a new 32-hex secret token.
pub fn get_or_create_api_token() -> String {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\RustWallpaper"));
    let token_file = local_app_data.join("RustWallpaper").join("api_token.txt");
    if let Ok(token) = std::fs::read_to_string(&token_file) {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let new_token = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
    let _ = std::fs::create_dir_all(token_file.parent().unwrap_or(&local_app_data));
    let _ = std::fs::write(&token_file, &new_token);
    new_token
}

/// Starts the embedded Web API HTTP server in a background thread.
pub fn start(tx: SyncSender<PendingRequest>, port: u16) {
    std::thread::Builder::new()
        .name("wallpaper-web-api".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!(%e, "Failed to create Web API tokio runtime");
                    return;
                }
            };
            runtime.block_on(server_loop(tx, port));
        })
        .expect("failed to spawn Web API thread");
}

async fn server_loop(tx: SyncSender<PendingRequest>, port: u16) {
    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            info!(endpoint = %addr, "Web API bridge server listening on loopback");
            l
        }
        Err(e) => {
            warn!(%e, endpoint = %addr, "Could not bind Web API port (already running?)");
            return;
        }
    };

    loop {
        match listener.accept().await {
            Ok((stream, client_addr)) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, tx).await {
                        tracing::debug!(%e, %client_addr, "Web API connection closed with error");
                    }
                });
            }
            Err(e) => {
                warn!(%e, "Web API accept error; retrying in 1s");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    tx: SyncSender<PendingRequest>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::with_capacity(4096);
    let mut temp = [0u8; 2048];

    // Read headers
    let mut headers_end = None;
    while headers_end.is_none() {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut temp)).await??;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&temp[..n]);

        if buffer.len() > 16 * 1024 {
            // 16KB request header limit
            send_response(&mut stream, 413, "Payload Too Large", "Request too large").await?;
            return Ok(());
        }

        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            headers_end = Some(pos);
        }
    }

    let headers_pos = headers_end.unwrap();
    let header_bytes = &buffer[..headers_pos];
    let header_str = String::from_utf8_lossy(header_bytes);

    let mut lines = header_str.lines();
    let req_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    if parts.len() < 2 {
        send_response(&mut stream, 400, "Bad Request", "Malformed request line").await?;
        return Ok(());
    }

    let method = parts[0];
    let path = parts[1];
    let host = lines.clone().find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.trim().to_ascii_lowercase())
    });
    let expected_port = stream.local_addr()?.port();
    if !host.is_some_and(|value| {
        value == format!("127.0.0.1:{expected_port}")
            || value == format!("localhost:{expected_port}")
    }) {
        send_response(&mut stream, 403, "Forbidden", "Host is not allowed").await?;
        return Ok(());
    }
    let origin = lines.clone().find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("origin"))
            .map(|(_, value)| value.trim().to_owned())
    });

    if origin
        .as_deref()
        .is_some_and(|value| !is_allowed_origin(value))
    {
        send_response(&mut stream, 403, "Forbidden", "Origin is not allowed").await?;
        return Ok(());
    }

    // Handle CORS preflight
    if method == "OPTIONS" {
        send_cors_preflight(&mut stream).await?;
        return Ok(());
    }

    // Extract Content-Length
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }

    if content_length > 1024 * 1024 {
        send_response(
            &mut stream,
            413,
            "Payload Too Large",
            "Request body too large",
        )
        .await?;
        return Ok(());
    }

    // Read remaining body if necessary
    let body_start = headers_pos + 4;
    let mut body_bytes = buffer[body_start..].to_vec();
    while body_bytes.len() < content_length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut temp)).await??;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&temp[..n]);
        if body_bytes.len() > 5 * 1024 * 1024 {
            // 5MB body limit
            send_response(&mut stream, 413, "Payload Too Large", "Body too large").await?;
            return Ok(());
        }
    }

    match (method, path) {
        ("GET", "/api/auth/token") => {
            let token = get_or_create_api_token();
            let json = serde_json::json!({ "token": token });
            send_json_response(&mut stream, 200, "OK", &json).await?;
            return Ok(());
        }
        _ => {}
    }

    // Verify Bearer authorization token or our exact signed companion extension Origin
    if path.starts_with("/api/wallpaper") || path.starts_with("/api/action") {
        let expected = get_or_create_api_token();
        let expected_ext_origin = format!("chrome-extension://{}", wallpaper_windows::EXTENSION_ID);
        let auth_header = header_str.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim().to_owned())
        });
        let token_header = header_str.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("x-rustwallpaper-token"))
                .map(|(_, value)| value.trim().to_owned())
        });
        let is_authed = origin.as_deref() == Some(expected_ext_origin.as_str())
            || auth_header.as_deref().is_some_and(|h| {
                h.strip_prefix("Bearer ")
                    .map_or(false, |tok| tok.trim() == expected)
            })
            || token_header.as_deref().is_some_and(|h| h == expected)
            || path.contains(&format!("token={expected}"));

        if !is_authed {
            warn!(
                path,
                "Rejected Web API request: missing or invalid bearer token"
            );
            let json = serde_json::json!({
                "error": "Unauthorized",
                "message": "Missing or invalid Bearer authorization token"
            });
            send_json_response(&mut stream, 401, "Unauthorized", &json).await?;
            return Ok(());
        }
    }

    match (method, path) {
        ("GET", "/api/status") | ("GET", "/api/status/") => {
            handle_status(&mut stream, &tx).await?;
        }
        ("POST", "/api/wallpaper") | ("POST", "/api/wallpaper/") => {
            let body_str = String::from_utf8_lossy(&body_bytes);
            handle_wallpaper(&mut stream, &tx, &body_str).await?;
        }
        ("POST", "/api/action") | ("POST", "/api/action/") => {
            let body_str = String::from_utf8_lossy(&body_bytes);
            handle_action(&mut stream, &tx, &body_str).await?;
        }
        ("GET", "/extension/update.xml") => {
            handle_extension_update_manifest(&mut stream).await?;
        }
        ("GET", "/extension/rust-wallpaper.crx")
        | ("GET", "/extension/chrome.crx")
        | ("GET", "/extension/extension.crx") => {
            handle_extension_crx_download(&mut stream).await?;
        }
        ("POST", "/extension/register") | ("POST", "/extension/register/") => {
            handle_extension_register(&mut stream).await?;
        }
        ("GET", "/extension/install") | ("GET", "/extension/install/") => {
            handle_extension_install_page(&mut stream).await?;
        }
        _ => {
            let json = serde_json::json!({
                "error": "Not Found",
                "message": format!("Route '{method} {path}' not found on Rust Wallpaper API")
            });
            send_json_response(&mut stream, 404, "Not Found", &json).await?;
        }
    }

    Ok(())
}

async fn handle_status(
    stream: &mut TcpStream,
    tx: &SyncSender<PendingRequest>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let monitors_res = dispatch_command(tx, Command::ListMonitors).await;
    let players_res = dispatch_command(tx, Command::GetStatus { monitor: None }).await;

    let monitors_val = match monitors_res {
        Ok(Response::MonitorList(list)) => serde_json::to_value(list).unwrap_or_default(),
        _ => serde_json::json!([]),
    };

    let players_val = match players_res {
        Ok(Response::PlayerStatus(list)) => serde_json::to_value(list).unwrap_or_default(),
        _ => serde_json::json!([]),
    };

    let payload = serde_json::json!({
        "status": "online",
        "version": "0.1.0",
        "app": "Rust Wallpaper",
        "monitors": monitors_val,
        "active_players": players_val,
    });

    send_json_response(stream, 200, "OK", &payload).await
}

#[derive(serde::Deserialize)]
struct WallpaperRequest {
    url: String,
    title: Option<String>,
    monitor: Option<String>,
    apply: Option<bool>,
}

async fn handle_wallpaper(
    stream: &mut TcpStream,
    tx: &SyncSender<PendingRequest>,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let req: WallpaperRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            let json = serde_json::json!({
                "error": "Bad Request",
                "message": format!("Invalid JSON body: {e}")
            });
            send_json_response(stream, 400, "Bad Request", &json).await?;
            return Ok(());
        }
    };

    let raw_url = req.url.trim().to_string();
    if !raw_url.starts_with("http://") && !raw_url.starts_with("https://") {
        let json = serde_json::json!({
            "error": "Bad Request",
            "message": "URL must begin with http:// or https://"
        });
        send_json_response(stream, 400, "Bad Request", &json).await?;
        return Ok(());
    }

    let (final_dest, title) = if let Some((steam_path, steam_title)) = {
        if raw_url.contains("steamcommunity.com") && raw_url.contains("id=") {
            raw_url
                .split("id=")
                .nth(1)
                .and_then(|s| s.split('&').next())
                .and_then(|id_str| {
                    wallpaper_core::scan_wallpaper_engine_items(&[])
                        .into_iter()
                        .find(|i| i.id == id_str && i.is_compatible_video)
                        .map(|i| (i.file_path, i.title))
                })
        } else {
            None
        }
    } {
        let title = req
            .title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(steam_title);
        (steam_path, title)
    } else {
        let resolved = match tokio::task::spawn_blocking({
            let raw_url = raw_url.clone();
            move || {
                wallpaper_core::resolve_web_media_info_with_policy(
                    &raw_url,
                    &std::sync::atomic::AtomicBool::new(false),
                    false,
                )
            }
        })
        .await
        {
            Ok(Ok(resolved)) => resolved,
            Ok(Err(error)) => {
                let json = serde_json::json!({
                    "error": "Media Resolution Failed",
                    "message": error.to_string()
                });
                send_json_response(stream, 422, "Unprocessable Entity", &json).await?;
                return Ok(());
            }
            Err(error) => {
                let json = serde_json::json!({
                    "error": "Media Resolution Failed",
                    "message": format!("Media resolver failed: {error}")
                });
                send_json_response(stream, 502, "Bad Gateway", &json).await?;
                return Ok(());
            }
        };
        let url = resolved.media_url;

        // Determine filename
        let parsed_filename = url
            .split('?')
            .next()
            .and_then(|u| u.split('/').next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("web_wallpaper.mp4");

        let parsed_filename_lower = parsed_filename.to_ascii_lowercase();
        let has_ext = [
            ".mp4", ".webm", ".mkv", ".mov", ".gif", ".jpg", ".jpeg", ".png", ".webp", ".bmp",
        ]
        .iter()
        .any(|extension| parsed_filename_lower.ends_with(extension));

        let stem = if has_ext {
            parsed_filename
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(parsed_filename)
        } else {
            parsed_filename
        };

        let ext = if has_ext {
            parsed_filename_lower
                .rsplit_once('.')
                .map(|(_, e)| e)
                .unwrap_or("mp4")
        } else if url.to_ascii_lowercase().contains("image") {
            "jpg"
        } else {
            "mp4"
        };

        let safe_stem: String = stem
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe_name = if safe_stem.is_empty() {
            "web_wallpaper".to_string()
        } else {
            safe_stem
        };

        let local_app_data = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
        let downloads_dir = PathBuf::from(local_app_data)
            .join("RustWallpaper")
            .join("downloads");
        let dest = downloads_dir.join(format!("{safe_name}.{ext}"));

        let title = req
            .title
            .filter(|t| !t.trim().is_empty())
            .or(resolved.title)
            .unwrap_or_else(|| safe_name.replace('_', " "));

        // Download in blocking task
        let download_url = url.clone();
        let dest_clone = dest.clone();
        let download_result = tokio::task::spawn_blocking(move || {
            wallpaper_core::download_online_file(&download_url, &dest_clone)
        })
        .await?;

        if let Err(e) = download_result {
            let json = serde_json::json!({
                "error": "Download Failed",
                "message": format!("Failed to download video file: {e}")
            });
            send_json_response(stream, 502, "Bad Gateway", &json).await?;
            return Ok(());
        }

        let is_html = if let Ok(mut f) = std::fs::File::open(&dest) {
            let mut buf = [0u8; 128];
            if let Ok(n) = std::io::Read::read(&mut f, &mut buf) {
                let s = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                s.contains("<!doctype")
                    || s.contains("<html")
                    || s.contains("<body")
                    || s.contains("<head")
            } else {
                false
            }
        } else {
            false
        };
        if is_html {
            let _ = std::fs::remove_file(&dest);
            let json = serde_json::json!({
                "error": "Invalid Media Format",
                "message": "Downloaded content is a web page HTML document, not a playable video or wallpaper file."
            });
            send_json_response(stream, 422, "Unprocessable Entity", &json).await?;
            return Ok(());
        }

        let mut final_dest = dest.clone();
        if let Some(detected_ext) = wallpaper_core::detect_media_extension_from_magic_bytes(&dest) {
            let candidate = dest.with_extension(detected_ext);
            if candidate != dest {
                if let Ok(()) = std::fs::rename(&dest, &candidate) {
                    final_dest = candidate;
                }
            }
        }
        (final_dest, title)
    };

    // Import into library via IPC
    let import_cmd = Command::ImportWallpaper {
        path: final_dest.to_string_lossy().to_string(),
        title: Some(title.clone()),
    };

    let import_res = dispatch_command(tx, import_cmd).await;
    match import_res {
        Ok(Response::Accepted) => {}
        Ok(Response::Error { message, .. }) => {
            let json = serde_json::json!({
                "error": "Import Failed",
                "message": message
            });
            send_json_response(stream, 500, "Internal Server Error", &json).await?;
            return Ok(());
        }
        Err(e) => {
            let json = serde_json::json!({
                "error": "IPC Communication Error",
                "message": e
            });
            send_json_response(stream, 500, "Internal Server Error", &json).await?;
            return Ok(());
        }
        _ => {}
    }

    // Find the newly imported wallpaper in the library
    let mut wallpaper_id = None;
    let mut resolved_title = title.clone();
    let mut width = None;
    let mut height = None;
    let mut media_type = "video";
    let downloaded_ext = final_dest
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        downloaded_ext.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
    ) {
        media_type = "image";
    }

    // Metadata extraction is asynchronous in the library. Wait briefly for the actual
    // dimensions so the extension can report what was downloaded, not just the URL hint.
    let metadata_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Response::WallpaperList(list)) =
            dispatch_command(tx, Command::ListWallpapers).await
        {
            let final_dest_str = final_dest.to_string_lossy();
            if let Some(wp) = list.iter().find(|w| {
                w.source() == final_dest_str
                    || std::path::Path::new(w.source()) == final_dest.as_path()
            }) {
                wallpaper_id = Some(wp.id);
                resolved_title = wp.title().to_string();
                width = wp.metadata.width;
                height = wp.metadata.height;
                if (width.is_some() && height.is_some() && !wp.metadata.is_processing())
                    || wp.metadata.is_failed()
                {
                    break;
                }
            }
        }
        if tokio::time::Instant::now() >= metadata_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Read image dimensions directly if the asynchronous worker has not finished yet.
    if media_type == "image" && (width.is_none() || height.is_none()) {
        if let Ok(reader) = image::ImageReader::open(&final_dest) {
            if let Ok(reader) = reader.with_guessed_format() {
                if let Ok(image) = reader.decode() {
                    width = Some(image.width());
                    height = Some(image.height());
                }
            }
        }
    }

    // Apply wallpaper if requested (defaults to true)
    let should_apply = req.apply.unwrap_or(true);
    let mut applied = false;
    if should_apply {
        if let Some(id) = wallpaper_id {
            let apply_cmd = Command::ApplyWallpaper {
                wallpaper_id: id,
                monitor: req.monitor.clone(),
            };
            match dispatch_command(tx, apply_cmd).await {
                Ok(Response::Accepted) => applied = true,
                Ok(Response::Error { message, .. }) => {
                    let json = serde_json::json!({
                        "error": "Wallpaper Apply Failed",
                        "message": format!("Downloaded and added to the library, but could not apply it: {message}"),
                        "downloaded": true,
                        "wallpaper_id": id.to_string(),
                        "media_type": media_type,
                        "width": width,
                        "height": height
                    });
                    send_json_response(stream, 422, "Unprocessable Entity", &json).await?;
                    return Ok(());
                }
                Err(error) => {
                    let json = serde_json::json!({
                        "error": "Wallpaper Apply Failed",
                        "message": format!("Downloaded and added to the library, but the app could not apply it: {error}"),
                        "downloaded": true,
                        "wallpaper_id": id.to_string(),
                        "media_type": media_type,
                        "width": width,
                        "height": height
                    });
                    send_json_response(stream, 502, "Bad Gateway", &json).await?;
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    let payload = serde_json::json!({
        "success": true,
        "message": if applied { "Wallpaper downloaded and applied successfully" } else { "Wallpaper downloaded and added to library" },
        "applied": applied,
        "wallpaper_id": wallpaper_id.map(|id| id.to_string()),
        "title": resolved_title,
        "media_type": media_type,
        "width": width,
        "height": height,
        "resolution": match (width, height) {
            (Some(width), Some(height)) => format!("{width} × {height}"),
            _ => "Resolution unavailable".to_string()
        },
        "source": final_dest.to_string_lossy(),
        "monitor": req.monitor.unwrap_or_else(|| "all".into())
    });

    send_json_response(stream, 200, "OK", &payload).await
}

#[derive(serde::Deserialize)]
struct ActionRequest {
    action: String,
    monitor: Option<String>,
    profile: Option<String>,
}

async fn handle_action(
    stream: &mut TcpStream,
    tx: &SyncSender<PendingRequest>,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let req: ActionRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            let json = serde_json::json!({
                "error": "Bad Request",
                "message": format!("Invalid JSON body: {e}")
            });
            send_json_response(stream, 400, "Bad Request", &json).await?;
            return Ok(());
        }
    };

    let cmd = match req.action.to_lowercase().as_str() {
        "pause" => Command::Pause {
            monitor: req.monitor,
        },
        "resume" => Command::Resume {
            monitor: req.monitor,
        },
        "profile" => {
            if let Some(p) = req
                .profile
                .and_then(|s| wallpaper_core::PerformanceProfile::parse(&s))
            {
                Command::SetProfile { profile: p }
            } else {
                let json = serde_json::json!({
                    "error": "Bad Request",
                    "message": "Expected profile: eco, balanced, or performance"
                });
                send_json_response(stream, 400, "Bad Request", &json).await?;
                return Ok(());
            }
        }
        act => {
            let json = serde_json::json!({
                "error": "Unknown Action",
                "message": format!("Action '{act}' is not supported")
            });
            send_json_response(stream, 400, "Bad Request", &json).await?;
            return Ok(());
        }
    };

    match dispatch_command(tx, cmd).await {
        Ok(Response::Accepted)
        | Ok(Response::PlaybackChanged { .. })
        | Ok(Response::Profile(..)) => {
            let json = serde_json::json!({ "success": true });
            send_json_response(stream, 200, "OK", &json).await
        }
        Ok(Response::Error { message, .. }) => {
            let json = serde_json::json!({ "error": message });
            send_json_response(stream, 500, "Internal Server Error", &json).await
        }
        Err(e) => {
            let json = serde_json::json!({ "error": e });
            send_json_response(stream, 500, "Internal Server Error", &json).await
        }
        _ => {
            let json = serde_json::json!({ "success": true });
            send_json_response(stream, 200, "OK", &json).await
        }
    }
}

async fn handle_extension_update_manifest(
    stream: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n\
<gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>\n  \
  <app appid='{}'>\n    \
    <updatecheck codebase='http://127.0.0.1:24242/extension/rust-wallpaper.crx' version='{}' />\n  \
  </app>\n\
</gupdate>\n",
        wallpaper_windows::EXTENSION_ID,
        wallpaper_windows::EXTENSION_VERSION
    );

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
Content-Type: application/xml; charset=utf-8\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n\
{}",
        xml.len(),
        xml
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_extension_crx_download(
    stream: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let crx_path = match wallpaper_windows::find_crx_candidate() {
        Some(p) => p,
        None => {
            let json = serde_json::json!({
                "error": "Not Found",
                "message": "Packaged chrome.crx file not found on disk"
            });
            send_json_response(stream, 404, "Not Found", &json).await?;
            return Ok(());
        }
    };

    let bytes = match tokio::fs::read(&crx_path).await {
        Ok(b) => b,
        Err(e) => {
            let json = serde_json::json!({
                "error": "Read Error",
                "message": format!("Failed to read CRX file: {e}")
            });
            send_json_response(stream, 500, "Internal Server Error", &json).await?;
            return Ok(());
        }
    };

    let header = format!(
        "HTTP/1.1 200 OK\r\n\
Content-Type: application/x-chrome-extension\r\n\
Content-Disposition: attachment; filename=\"rust-wallpaper.crx\"\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_extension_register(
    stream: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match wallpaper_windows::register_browser_extension(None) {
        Ok(()) => {
            let json = serde_json::json!({
                "success": true,
                "registered": true,
                "extension_id": wallpaper_windows::EXTENSION_ID,
                "message": "Browser extension registered in Windows Registry"
            });
            send_json_response(stream, 200, "OK", &json).await?;
        }
        Err(e) => {
            let json = serde_json::json!({
                "success": false,
                "error": e
            });
            send_json_response(stream, 500, "Internal Server Error", &json).await?;
        }
    }
    Ok(())
}

async fn handle_extension_install_page(
    stream: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let is_reg = wallpaper_windows::is_browser_extension_registered();
    let status_html = if is_reg {
        r#"<span style="color:#10b981;font-weight:600;">Active & Registered</span>"#
    } else {
        r#"<span style="color:#f59e0b;font-weight:600;">Not Registered</span>"#
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>RustWallpaper Companion Integration</title>
  <style>
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: #0f172a;
      color: #f8fafc;
      display: flex;
      align-items: center;
      justify-content: center;
      min-height: 100vh;
      margin: 0;
      padding: 20px;
    }}
    .card {{
      background: #1e293b;
      border: 1px solid rgba(255,255,255,0.1);
      border-radius: 12px;
      padding: 32px;
      max-width: 520px;
      width: 100%;
      box-shadow: 0 8px 30px rgba(0,0,0,0.5);
    }}
    h1 {{ font-size: 20px; margin-top: 0; display: flex; align-items: center; gap: 10px; }}
    p {{ font-size: 14px; line-height: 1.6; color: #94a3b8; }}
    .badge {{
      background: #0f172a;
      border: 1px solid rgba(255,255,255,0.1);
      border-radius: 6px;
      padding: 12px 16px;
      margin: 16px 0;
      font-size: 13px;
    }}
    .btn {{
      display: inline-block;
      padding: 10px 18px;
      background: #4f46e5;
      color: #fff;
      text-decoration: none;
      border-radius: 6px;
      font-weight: 600;
      font-size: 13px;
      cursor: pointer;
      border: none;
      margin-right: 8px;
    }}
    .btn:hover {{ background: #4338ca; }}
    .btn-secondary {{ background: #334155; }}
    .btn-secondary:hover {{ background: #475569; }}
    code {{ background: #0f172a; padding: 2px 6px; border-radius: 4px; font-size: 12px; }}
  </style>
</head>
<body>
  <div class="card">
    <h1>🖼 RustWallpaper Companion</h1>
    <p>The companion extension allows sending live videos, GIFs, and wallpapers directly from Chrome, Edge, and Brave to your desktop wallpaper engine.</p>
    <div class="badge">
      <strong>Registry Status:</strong> {}<br>
      <strong>Extension ID:</strong> <code>{}</code>
    </div>
    <p>When you launch your browser, Chromium will automatically detect the extension and prompt you to enable it.</p>
    <div style="margin-top: 24px;">
      <a href="/extension/rust-wallpaper.crx" class="btn">Download .CRX Package</a>
      <button onclick="register()" class="btn btn-secondary">Re-register in Registry</button>
    </div>
  </div>
  <script>
    async function register() {{
      const res = await fetch('/extension/register', {{ method: 'POST' }});
      const data = await res.json();
      if (data.success) {{
        alert('Successfully registered extension in Chrome, Edge, and Brave registry!');
        location.reload();
      }} else {{
        alert('Registration failed: ' + (data.error || 'Unknown error'));
      }}
    }}
  </script>
</body>
</html>"#,
        status_html,
        wallpaper_windows::EXTENSION_ID
    );

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n\
{}",
        html.len(),
        html
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn dispatch_command(
    tx: &SyncSender<PendingRequest>,
    command: Command,
) -> Result<Response, String> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let request = RequestEnvelope::new(command);
    tx.send(PendingRequest {
        request,
        reply: reply_tx,
    })
    .map_err(|e| format!("Daemon IPC queue disconnected: {e}"))?;

    match tokio::time::timeout(Duration::from_secs(10), reply_rx).await {
        Ok(Ok(response_env)) => Ok(response_env.result),
        Ok(Err(_)) => Err("Daemon dropped the response channel".into()),
        Err(_) => Err("Timed out waiting for daemon response".into()),
    }
}

fn is_allowed_origin(origin: &str) -> bool {
    origin == format!("chrome-extension://{}", wallpaper_windows::EXTENSION_ID)
        || origin == format!("http://127.0.0.1:{DEFAULT_PORT}")
        || origin == format!("http://localhost:{DEFAULT_PORT}")
}

async fn send_cors_preflight(
    stream: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = format!(
        "HTTP/1.1 204 No Content\r\n\
Access-Control-Allow-Origin: chrome-extension://{}\r\n\
Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
Access-Control-Allow-Headers: Content-Type, Authorization, X-RustWallpaper-Token\r\n\
Access-Control-Max-Age: 86400\r\n\
Vary: Origin\r\n\
Content-Length: 0\r\n\
Connection: close\r\n\r\n",
        wallpaper_windows::EXTENSION_ID
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn send_json_response(
    stream: &mut TcpStream,
    status_code: u16,
    status_text: &str,
    payload: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let body = serde_json::to_string_pretty(payload)?;
    let response = format!(
        "HTTP/1.1 {status_code} {status_text}\r\n\
Content-Type: application/json; charset=utf-8\r\n\
Access-Control-Allow-Origin: chrome-extension://{}\r\n\
Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
Access-Control-Allow-Headers: Content-Type, Authorization, X-RustWallpaper-Token\r\n\
Vary: Origin\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n\
{}",
        wallpaper_windows::EXTENSION_ID,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn send_response(
    stream: &mut TcpStream,
    status_code: u16,
    status_text: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = serde_json::json!({
        "status": status_code,
        "message": message
    });
    send_json_response(stream, status_code, status_text, &json).await
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn only_extension_and_loopback_origins_are_allowed() {
        assert!(is_allowed_origin(&format!(
            "chrome-extension://{}",
            wallpaper_windows::EXTENSION_ID
        )));
        assert!(is_allowed_origin("http://127.0.0.1:24242"));
        assert!(is_allowed_origin("http://localhost:24242"));
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1.evil.example:24242",
        ] {
            assert!(!is_allowed_origin(origin), "allowed {origin}");
        }
    }
}
