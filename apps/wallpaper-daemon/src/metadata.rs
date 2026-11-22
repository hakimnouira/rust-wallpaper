//! Video metadata extraction and thumbnail frame generation.

use std::path::{Path, PathBuf};
use std::process::Command;
use wallpaper_core::categorize_resolution;

#[inline]
fn silent_command<P: AsRef<std::ffi::OsStr>>(program: P) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Extracted video properties and representative thumbnail location.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedMetadata {
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub aspect_ratio: Option<f64>,
    pub fps: Option<f64>,
    pub codec: Option<String>,
    pub file_size: Option<u64>,
    pub resolution_category: Option<String>,
    pub thumbnail_path: String,
    pub source_mtime: Option<u64>,
}

/// Locates `ffmpeg.exe` if available.
pub fn find_ffmpeg_executable() -> Option<PathBuf> {
    // 1. Check directory next to mpv.exe
    if let Ok(mpv_path) = wallpaper_windows::find_mpv_executable() {
        if let Some(parent) = mpv_path.parent() {
            let candidate = parent.join("ffmpeg.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 2. Check bin/ffmpeg.exe or bin/mpv/ffmpeg.exe
    let local = PathBuf::from("bin").join("ffmpeg.exe");
    if local.is_file() {
        return Some(local);
    }
    let local_mpv = PathBuf::from("bin").join("mpv").join("ffmpeg.exe");
    if local_mpv.is_file() {
        return Some(local_mpv);
    }

    // 3. Check %USERPROFILE%\.mpv\ffmpeg.exe
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let profile_path = PathBuf::from(user_profile).join(".mpv").join("ffmpeg.exe");
        if profile_path.is_file() {
            return Some(profile_path);
        }
    }

    // 4. Check system PATH
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("ffmpeg.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Parses stderr output from ffmpeg to extract container and video stream details.
#[allow(clippy::type_complexity)]
pub fn parse_ffmpeg_output(
    stderr: &str,
) -> (
    Option<f64>,
    Option<u32>,
    Option<u32>,
    Option<f64>,
    Option<f64>,
    Option<String>,
) {
    let mut duration_secs = None;
    let mut width = None;
    let mut height = None;
    let mut aspect_ratio = None;
    let mut fps = None;
    let mut codec = None;

    for line in stderr.lines() {
        let trimmed = line.trim();

        // 1. Duration: 00:00:10.00, start: ...
        if let Some(pos) = trimmed.find("Duration: ") {
            let rest = &trimmed[pos + "Duration: ".len()..];
            if let Some(comma) = rest.find(',') {
                let time_str = rest[..comma].trim();
                let parts: Vec<&str> = time_str.split(':').collect();
                if parts.len() == 3 {
                    if let (Ok(h), Ok(m), Ok(s)) = (
                        parts[0].parse::<f64>(),
                        parts[1].parse::<f64>(),
                        parts[2].parse::<f64>(),
                    ) {
                        duration_secs = Some(h * 3600.0 + m * 60.0 + s);
                    }
                }
            }
        }

        // 2. Stream ... Video: codec ..., 1920x1080 ..., 25 fps
        if (trimmed.contains("Stream #") || trimmed.contains("Stream "))
            && trimmed.contains("Video: ")
        {
            if let Some(pos) = trimmed.find("Video: ") {
                let rest = &trimmed[pos + "Video: ".len()..];
                let raw_codec = rest.split([' ', ',', '(']).next().unwrap_or("");
                if !raw_codec.is_empty() {
                    codec = Some(raw_codec.to_lowercase());
                }

                for part in rest.split(',') {
                    let part = part.trim();
                    let token = part.split_whitespace().next().unwrap_or("");
                    if let Some(x_idx) = token.find('x') {
                        let w_str = &token[..x_idx];
                        let h_str = &token[x_idx + 1..];
                        if let (Ok(w), Ok(h)) = (w_str.parse::<u32>(), h_str.parse::<u32>()) {
                            if w > 0 && h > 0 {
                                width = Some(w);
                                height = Some(h);
                            }
                        }
                    }

                    if let Some(dar_pos) = part.find("DAR ") {
                        let dar_str = &part[dar_pos + "DAR ".len()..];
                        let dar_token = dar_str.split([']', ' ', ',']).next().unwrap_or("");
                        let dar_parts: Vec<&str> = dar_token.split(':').collect();
                        if dar_parts.len() == 2 {
                            if let (Ok(num), Ok(den)) =
                                (dar_parts[0].parse::<f64>(), dar_parts[1].parse::<f64>())
                            {
                                if den > 0.0 {
                                    aspect_ratio = Some(num / den);
                                }
                            }
                        }
                    }

                    if part.ends_with(" fps") {
                        let fps_str = part.trim_end_matches(" fps").trim();
                        if let Ok(val) = fps_str.parse::<f64>() {
                            fps = Some(val);
                        }
                    }
                }
            }
        }
    }

    if aspect_ratio.is_none() {
        if let (Some(w), Some(h)) = (width, height) {
            if h > 0 {
                aspect_ratio = Some(w as f64 / h as f64);
            }
        }
    }

    (duration_secs, width, height, aspect_ratio, fps, codec)
}

/// Extracts metadata and generates a thumbnail frame for a video file.
/// Never mutates the source file. Handles corrupt, empty, or unreadable files gracefully.
pub fn extract_video_metadata_and_thumbnail(
    source_path: &Path,
    thumbnail_path: &Path,
) -> Result<ExtractedMetadata, String> {
    if !source_path.exists() {
        return Err(format!("File does not exist: {}", source_path.display()));
    }

    let meta = std::fs::metadata(source_path)
        .map_err(|e| format!("Failed to read file attributes: {e}"))?;

    let file_size = meta.len();
    if file_size == 0 {
        return Err("File is empty (0 bytes)".into());
    }

    let source_mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    if let Some(parent) = thumbnail_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create thumbnail directory: {e}"))?;
    }

    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_web = matches!(ext.as_str(), "html" | "htm");
    if is_web {
        let mut img = image::RgbImage::new(480, 270);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let r = (18.0 + (x as f32 / 480.0) * 15.0) as u8;
            let g = (22.0 + (y as f32 / 270.0) * 20.0) as u8;
            let b = (36.0 + ((x + y) as f32 / 750.0) * 35.0) as u8;
            *pixel = image::Rgb([r, g, b]);
        }
        let _ = img.save_with_format(thumbnail_path, image::ImageFormat::Jpeg);
        return Ok(ExtractedMetadata {
            duration_secs: None,
            width: Some(1920),
            height: Some(1080),
            aspect_ratio: Some(16.0 / 9.0),
            fps: Some(60.0),
            codec: Some("html5".into()),
            resolution_category: Some("1080p".into()),
            thumbnail_path: thumbnail_path.to_string_lossy().to_string(),
            file_size: Some(file_size),
            source_mtime,
        });
    }

    let is_image = matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
    );

    if is_image {
        let image = image::ImageReader::open(source_path)
            .map_err(|error| format!("Failed to open image: {error}"))?
            .with_guessed_format()
            .map_err(|error| format!("Could not determine image format: {error}"))?
            .decode()
            .map_err(|error| format!("Could not decode image: {error}"))?;
        let (width, height) = (image.width(), image.height());
        image
            .thumbnail(480, 270)
            .save_with_format(thumbnail_path, image::ImageFormat::Jpeg)
            .map_err(|error| format!("Failed to write image thumbnail: {error}"))?;
        return Ok(ExtractedMetadata {
            duration_secs: None,
            width: Some(width),
            height: Some(height),
            aspect_ratio: (height > 0).then_some(width as f64 / height as f64),
            fps: None,
            codec: Some(ext),
            resolution_category: Some(categorize_resolution(width, height).to_string()),
            thumbnail_path: thumbnail_path.to_string_lossy().to_string(),
            file_size: Some(file_size),
            source_mtime,
        });
    }

    let ffmpeg_bin = find_ffmpeg_executable();
    let mpv_bin = wallpaper_windows::find_mpv_executable().ok();

    let result = if let Some(ref ffmpeg) = ffmpeg_bin {
        extract_via_ffmpeg(ffmpeg, source_path, thumbnail_path, file_size, source_mtime)
    } else if let Some(ref mpv) = mpv_bin {
        extract_via_mpv(mpv, source_path, thumbnail_path, file_size, source_mtime)
    } else {
        Err("Neither ffmpeg nor mpv executable found on system".into())
    };

    result
}

fn extract_via_ffmpeg(
    ffmpeg: &Path,
    source_path: &Path,
    thumbnail_path: &Path,
    file_size: u64,
    source_mtime: Option<u64>,
) -> Result<ExtractedMetadata, String> {
    // 1. First probe file to get duration and metadata
    let probe_output = silent_command(ffmpeg)
        .arg("-i")
        .arg(source_path)
        .output()
        .map_err(|e| format!("Failed to execute ffmpeg probe: {e}"))?;

    let stderr = String::from_utf8_lossy(&probe_output.stderr);
    let (mut duration, mut width, mut height, mut aspect_ratio, mut fps, mut codec) =
        parse_ffmpeg_output(&stderr);

    if width.is_none() && height.is_none() && codec.is_none() {
        return Err("File does not contain a valid or readable video stream".into());
    }

    // Determine target seek offset for representative frame (around 10% of duration)
    let seek_time = match duration {
        Some(d) if d > 1.0 => format!("{:.2}", (d * 0.10).max(0.5).min(d - 0.1)),
        _ => "00:00:00".to_string(),
    };

    // 2. Extract thumbnail frame
    let _thumb_output = silent_command(ffmpeg)
        .arg("-ss")
        .arg(&seek_time)
        .arg("-i")
        .arg(source_path)
        .arg("-vframes")
        .arg("1")
        .arg("-vf")
        .arg("scale=480:270:force_original_aspect_ratio=decrease")
        .arg("-update")
        .arg("1")
        .arg("-y")
        .arg(thumbnail_path)
        .output()
        .map_err(|e| format!("Failed to run ffmpeg frame extraction: {e}"))?;

    if !thumbnail_path.exists()
        || std::fs::metadata(thumbnail_path)
            .map(|m| m.len())
            .unwrap_or(0)
            == 0
    {
        // Fallback: try seeking to 0s without timestamp offset
        let fallback_output = silent_command(ffmpeg)
            .arg("-ss")
            .arg("00:00:00")
            .arg("-i")
            .arg(source_path)
            .arg("-vframes")
            .arg("1")
            .arg("-vf")
            .arg("scale=480:270:force_original_aspect_ratio=decrease")
            .arg("-update")
            .arg("1")
            .arg("-y")
            .arg(thumbnail_path)
            .output();

        if let Ok(out) = fallback_output {
            let fallback_stderr = String::from_utf8_lossy(&out.stderr);
            if duration.is_none() || width.is_none() {
                let (d, w, h, a, f, c) = parse_ffmpeg_output(&fallback_stderr);
                if duration.is_none() {
                    duration = d;
                }
                if width.is_none() {
                    width = w;
                }
                if height.is_none() {
                    height = h;
                }
                if aspect_ratio.is_none() {
                    aspect_ratio = a;
                }
                if fps.is_none() {
                    fps = f;
                }
                if codec.is_none() {
                    codec = c;
                }
            }
        }
    }

    if !thumbnail_path.exists()
        || std::fs::metadata(thumbnail_path)
            .map(|m| m.len())
            .unwrap_or(0)
            == 0
    {
        if thumbnail_path.exists() {
            let _ = std::fs::remove_file(thumbnail_path);
        }
        let err_snippet = if stderr.len() > 200 {
            &stderr[..200]
        } else {
            &stderr
        };
        return Err(format!(
            "Failed to extract thumbnail image from video: {err_snippet}"
        ));
    }

    let resolution_category = match (width, height) {
        (Some(w), Some(h)) => Some(categorize_resolution(w, h).to_string()),
        _ => None,
    };

    Ok(ExtractedMetadata {
        duration_secs: duration,
        width,
        height,
        aspect_ratio,
        fps,
        codec,
        file_size: Some(file_size),
        resolution_category,
        thumbnail_path: thumbnail_path.to_string_lossy().to_string(),
        source_mtime,
    })
}

fn extract_via_mpv(
    mpv: &Path,
    source_path: &Path,
    thumbnail_path: &Path,
    file_size: u64,
    source_mtime: Option<u64>,
) -> Result<ExtractedMetadata, String> {
    let temp_dir = std::env::temp_dir().join(format!("mpv_thumb_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Failed to create temporary directory: {e}"))?;

    let output = silent_command(mpv)
        .arg("--no-audio")
        .arg("--no-config")
        .arg("--start=10%")
        .arg("--frames=1")
        .arg("--vo=image")
        .arg("--vo-image-format=jpg")
        .arg(format!("--vo-image-outdir={}", temp_dir.display()))
        .arg(source_path)
        .output()
        .map_err(|e| format!("Failed to execute mpv: {e}"))?;

    let gen_file = temp_dir.join("00000001.jpg");
    if gen_file.is_file() {
        let _ = std::fs::copy(&gen_file, thumbnail_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    } else {
        // Fallback to start=0
        let _ = silent_command(mpv)
            .arg("--no-audio")
            .arg("--no-config")
            .arg("--start=0")
            .arg("--frames=1")
            .arg("--vo=image")
            .arg("--vo-image-format=jpg")
            .arg(format!("--vo-image-outdir={}", temp_dir.display()))
            .arg(source_path)
            .output();

        if gen_file.is_file() {
            let _ = std::fs::copy(&gen_file, thumbnail_path);
            let _ = std::fs::remove_dir_all(&temp_dir);
        } else {
            let _ = std::fs::remove_dir_all(&temp_dir);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("mpv failed to produce thumbnail: {stderr}"));
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // Parse mpv console summary e.g. "Video --vid=1 (h264 1920x1080 25 fps 15 kbps)"
    let mut width = None;
    let mut height = None;
    let mut fps = None;
    let codec = None;

    for line in combined.lines() {
        if line.contains("Video") && line.contains("fps") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (idx, p) in parts.iter().enumerate() {
                if p.contains('x') {
                    let dims: Vec<&str> = p.split('x').collect();
                    if dims.len() == 2 {
                        if let (Ok(w), Ok(h)) = (dims[0].parse::<u32>(), dims[1].parse::<u32>()) {
                            width = Some(w);
                            height = Some(h);
                        }
                    }
                }
                if *p == "fps" && idx > 0 {
                    if let Ok(val) = parts[idx - 1].parse::<f64>() {
                        fps = Some(val);
                    }
                }
            }
        }
    }

    let aspect_ratio = match (width, height) {
        (Some(w), Some(h)) if h > 0 => Some(w as f64 / h as f64),
        _ => None,
    };

    let resolution_category = match (width, height) {
        (Some(w), Some(h)) => Some(categorize_resolution(w, h).to_string()),
        _ => None,
    };

    Ok(ExtractedMetadata {
        duration_secs: None,
        width,
        height,
        aspect_ratio,
        fps,
        codec,
        file_size: Some(file_size),
        resolution_category,
        thumbnail_path: thumbnail_path.to_string_lossy().to_string(),
        source_mtime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ffmpeg_output_h264() {
        let sample_stderr = r#"
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'sample.mp4':
  Metadata:
    major_brand     : isom
  Duration: 00:00:15.50, start: 0.000000, bitrate: 1800 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1750 kb/s, 60 fps, 60 tbr
"#;
        let (duration, width, height, aspect, fps, codec) = parse_ffmpeg_output(sample_stderr);
        assert_eq!(duration, Some(15.5));
        assert_eq!(width, Some(1920));
        assert_eq!(height, Some(1080));
        assert_eq!(fps, Some(60.0));
        assert_eq!(codec.as_deref(), Some("h264"));
        assert!((aspect.unwrap() - (16.0 / 9.0)).abs() < 0.001);
    }

    #[test]
    fn test_parse_ffmpeg_output_vp9() {
        let sample_stderr = r#"
Input #0, matroska,webm, from 'sample.webm':
  Duration: 00:01:30.00, start: 0.000000, bitrate: 1000 kb/s
  Stream #0:0: Video: vp9 (Profile 0), yuv420p(tv, progressive), 2560x1440, SAR 1:1 DAR 16:9, 25 fps, 25 tbr
"#;
        let (duration, width, height, aspect, fps, codec) = parse_ffmpeg_output(sample_stderr);
        assert_eq!(duration, Some(90.0));
        assert_eq!(width, Some(2560));
        assert_eq!(height, Some(1440));
        assert_eq!(fps, Some(25.0));
        assert_eq!(codec.as_deref(), Some("vp9"));
        assert!((aspect.unwrap() - (16.0 / 9.0)).abs() < 0.001);
    }

    #[test]
    fn test_extract_video_metadata_real_nature_mp4() {
        let video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            return;
        }

        let temp_thumb =
            std::env::temp_dir().join(format!("test_thumb_{}.jpg", uuid::Uuid::new_v4()));
        let result = extract_video_metadata_and_thumbnail(&video_path, &temp_thumb);
        assert!(result.is_ok(), "Extraction failed: {:?}", result.err());

        let meta = result.unwrap();
        assert_eq!(meta.width, Some(1920));
        assert_eq!(meta.height, Some(1080));
        assert_eq!(meta.resolution_category.as_deref(), Some("1080p FHD"));
        assert!(temp_thumb.is_file());
        assert!(std::fs::metadata(&temp_thumb).unwrap().len() > 0);

        let _ = std::fs::remove_file(temp_thumb);
    }

    #[test]
    fn extracts_real_resolution_and_thumbnail_for_static_image() {
        let temp_dir = std::env::temp_dir().join(format!("rw-image-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let source = temp_dir.join("pin.png");
        let thumbnail = temp_dir.join("thumb.jpg");
        image::RgbImage::from_pixel(2560, 1440, image::Rgb([12, 34, 56]))
            .save(&source)
            .unwrap();

        let metadata = extract_video_metadata_and_thumbnail(&source, &thumbnail).unwrap();
        assert_eq!(metadata.width, Some(2560));
        assert_eq!(metadata.height, Some(1440));
        assert_eq!(metadata.resolution_category.as_deref(), Some("1440p QHD"));
        assert!(thumbnail.is_file());
        assert!(image::ImageReader::open(&thumbnail)
            .unwrap()
            .decode()
            .is_ok());
        std::fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn test_extract_corrupt_or_missing_file() {
        let missing = PathBuf::from("nonexistent_video.mp4");
        let temp_thumb = std::env::temp_dir().join("thumb.jpg");
        let result = extract_video_metadata_and_thumbnail(&missing, &temp_thumb);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));

        // Test 0-byte file
        let zero_byte = std::env::temp_dir().join(format!("zero_{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&zero_byte, b"").unwrap();
        let result2 = extract_video_metadata_and_thumbnail(&zero_byte, &temp_thumb);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().contains("empty"));
        let _ = std::fs::remove_file(zero_byte);
    }
}
