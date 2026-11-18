//! Search, filtering, and sorting engine for wallpaper assets.

use crate::{ProcessingStatus, Wallpaper};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::Path;

/// Key by which wallpapers can be sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    DateAdded,
    Title,
    Resolution,
    Duration,
    FileSize,
}

impl SortKey {
    pub const ALL: [SortKey; 5] = [
        SortKey::DateAdded,
        SortKey::Title,
        SortKey::Resolution,
        SortKey::Duration,
        SortKey::FileSize,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DateAdded => "Date Added",
            Self::Title => "Name (A-Z)",
            Self::Resolution => "Resolution",
            Self::Duration => "Duration",
            Self::FileSize => "File Size",
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

/// Filter by video resolution category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionFilter {
    #[default]
    All,
    Uhd8k,
    Uhd4k,
    Qhd1440p,
    Fhd1080p,
    Hd720p,
    Sd,
}

impl ResolutionFilter {
    pub const ALL: [ResolutionFilter; 7] = [
        ResolutionFilter::All,
        ResolutionFilter::Uhd8k,
        ResolutionFilter::Uhd4k,
        ResolutionFilter::Qhd1440p,
        ResolutionFilter::Fhd1080p,
        ResolutionFilter::Hd720p,
        ResolutionFilter::Sd,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Resolutions",
            Self::Uhd8k => "8K UHD",
            Self::Uhd4k => "4K UHD",
            Self::Qhd1440p => "1440p QHD",
            Self::Fhd1080p => "1080p FHD",
            Self::Hd720p => "720p HD",
            Self::Sd => "SD (<720p)",
        }
    }

    pub fn matches(self, category: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Uhd8k => category.is_some_and(|c| c.contains("8K")),
            Self::Uhd4k => category.is_some_and(|c| c.contains("4K")),
            Self::Qhd1440p => category.is_some_and(|c| c.contains("1440p")),
            Self::Fhd1080p => category.is_some_and(|c| c.contains("1080p")),
            Self::Hd720p => category.is_some_and(|c| c.contains("720p")),
            Self::Sd => category.is_some_and(|c| c.contains("480p") || c == "SD"),
        }
    }
}

/// Filter by aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectRatioFilter {
    #[default]
    All,
    Wide16x9,
    Ultrawide21x9,
    Standard16x10,
    Portrait9x16,
    Other,
}

impl AspectRatioFilter {
    pub const ALL: [AspectRatioFilter; 6] = [
        AspectRatioFilter::All,
        AspectRatioFilter::Wide16x9,
        AspectRatioFilter::Ultrawide21x9,
        AspectRatioFilter::Standard16x10,
        AspectRatioFilter::Portrait9x16,
        AspectRatioFilter::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Aspect Ratios",
            Self::Wide16x9 => "16:9 Landscape",
            Self::Ultrawide21x9 => "21:9 Ultrawide",
            Self::Standard16x10 => "16:10",
            Self::Portrait9x16 => "9:16 Portrait",
            Self::Other => "Other",
        }
    }

    pub fn matches(self, ratio: Option<f64>) -> bool {
        match self {
            Self::All => true,
            Self::Wide16x9 => ratio.is_some_and(|r| (r - (16.0 / 9.0)).abs() < 0.08),
            Self::Ultrawide21x9 => ratio.is_some_and(|r| r >= 2.2),
            Self::Standard16x10 => ratio.is_some_and(|r| (r - 1.6).abs() < 0.05),
            Self::Portrait9x16 => ratio.is_some_and(|r| r <= 0.8),
            Self::Other => ratio.is_some_and(|r| {
                (r - (16.0 / 9.0)).abs() >= 0.08 && r < 2.2 && (r - 1.6).abs() >= 0.05 && r > 0.8
            }),
        }
    }
}

/// Filter by video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodecFilter {
    #[default]
    All,
    H264,
    Vp9,
    Hevc,
    Av1,
    Other,
}

impl CodecFilter {
    pub const ALL: [CodecFilter; 6] = [
        CodecFilter::All,
        CodecFilter::H264,
        CodecFilter::Vp9,
        CodecFilter::Hevc,
        CodecFilter::Av1,
        CodecFilter::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Codecs",
            Self::H264 => "H.264 / AVC",
            Self::Vp9 => "VP9",
            Self::Hevc => "HEVC / H.265",
            Self::Av1 => "AV1",
            Self::Other => "Other",
        }
    }

    pub fn matches(self, codec: Option<&str>) -> bool {
        let c = match codec {
            Some(s) => s.to_lowercase(),
            None => return matches!(self, Self::All),
        };
        match self {
            Self::All => true,
            Self::H264 => c.contains("264") || c.contains("avc"),
            Self::Vp9 => c.contains("vp9"),
            Self::Hevc => c.contains("hevc") || c.contains("265"),
            Self::Av1 => c.contains("av1") || c.contains("av01"),
            Self::Other => {
                !c.contains("264")
                    && !c.contains("avc")
                    && !c.contains("vp9")
                    && !c.contains("hevc")
                    && !c.contains("265")
                    && !c.contains("av1")
                    && !c.contains("av01")
            }
        }
    }
}

/// Filter by wallpaper processing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFilter {
    #[default]
    All,
    Ready,
    Processing,
    Failed,
}

impl StatusFilter {
    pub const ALL: [StatusFilter; 4] = [
        StatusFilter::All,
        StatusFilter::Ready,
        StatusFilter::Processing,
        StatusFilter::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Statuses",
            Self::Ready => "Ready",
            Self::Processing => "Processing",
            Self::Failed => "Failed",
        }
    }

    pub fn matches(self, status: ProcessingStatus) -> bool {
        match self {
            Self::All => true,
            Self::Ready => status == ProcessingStatus::Completed,
            Self::Processing => {
                matches!(
                    status,
                    ProcessingStatus::Pending | ProcessingStatus::Processing
                )
            }
            Self::Failed => status == ProcessingStatus::Failed,
        }
    }
}

/// Unified query configuration containing search text, sort criteria, and filters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryQuery {
    pub search: String,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub resolution: ResolutionFilter,
    pub aspect_ratio: AspectRatioFilter,
    pub codec: CodecFilter,
    pub status: StatusFilter,
}

impl LibraryQuery {
    /// Resets all search and filter fields back to default.
    pub fn reset_filters(&mut self) {
        self.search.clear();
        self.resolution = ResolutionFilter::All;
        self.aspect_ratio = AspectRatioFilter::All;
        self.codec = CodecFilter::All;
        self.status = StatusFilter::All;
    }

    /// Returns `true` if any filter or search query is currently active.
    pub fn is_filtered(&self) -> bool {
        !self.search.trim().is_empty()
            || self.resolution != ResolutionFilter::All
            || self.aspect_ratio != AspectRatioFilter::All
            || self.codec != CodecFilter::All
            || self.status != StatusFilter::All
    }

    /// Evaluates whether a wallpaper satisfies the current search and filter criteria.
    pub fn matches(&self, wallpaper: &Wallpaper) -> bool {
        let normalized_search = self.search.trim().to_lowercase();
        let tokens: Vec<&str> = normalized_search.split_whitespace().collect();
        self.matches_with_tokens(wallpaper, &tokens)
    }

    fn matches_with_tokens(&self, wallpaper: &Wallpaper, tokens: &[&str]) -> bool {
        // 1. Status filter
        if !self.status.matches(wallpaper.metadata.processing_state) {
            return false;
        }

        // 2. Resolution filter
        if !self
            .resolution
            .matches(wallpaper.metadata.resolution_category.as_deref())
        {
            return false;
        }

        // 3. Aspect ratio filter
        if !self.aspect_ratio.matches(wallpaper.metadata.aspect_ratio) {
            return false;
        }

        // 4. Codec filter
        if !self.codec.matches(wallpaper.metadata.codec.as_deref()) {
            return false;
        }

        // 5. Multi-token search matching across title, filename, codec, resolution, and source path
        if !tokens.is_empty() {
            let title = wallpaper.title().to_lowercase();
            let source = wallpaper.source().to_lowercase();
            let filename = Path::new(&source)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let codec = wallpaper
                .metadata
                .codec
                .as_deref()
                .map(|c| c.to_lowercase())
                .unwrap_or_default();
            let resolution = wallpaper
                .metadata
                .resolution_category
                .as_deref()
                .map(|r| r.to_lowercase())
                .unwrap_or_default();

            for token in tokens {
                let matches_token = title.contains(token)
                    || filename.contains(token)
                    || codec.contains(token)
                    || resolution.contains(token)
                    || source.contains(token);

                if !matches_token {
                    return false;
                }
            }
        }

        true
    }

    /// Compares two wallpapers based on sort key and direction.
    pub fn compare(a: &Wallpaper, b: &Wallpaper, key: SortKey, dir: SortDirection) -> Ordering {
        let ord = match key {
            SortKey::DateAdded => {
                // If source_mtime is known, compare it; otherwise tie-break with UUID
                let time_a = a.metadata.source_mtime.unwrap_or(0);
                let time_b = b.metadata.source_mtime.unwrap_or(0);
                time_a.cmp(&time_b)
            }
            SortKey::Title => a.title().to_lowercase().cmp(&b.title().to_lowercase()),
            SortKey::Resolution => {
                let pixels_a =
                    a.metadata.width.unwrap_or(0) as u64 * a.metadata.height.unwrap_or(0) as u64;
                let pixels_b =
                    b.metadata.width.unwrap_or(0) as u64 * b.metadata.height.unwrap_or(0) as u64;
                pixels_a.cmp(&pixels_b)
            }
            SortKey::Duration => {
                let dur_a = a.metadata.duration_secs.unwrap_or(0.0);
                let dur_b = b.metadata.duration_secs.unwrap_or(0.0);
                dur_a.partial_cmp(&dur_b).unwrap_or(Ordering::Equal)
            }
            SortKey::FileSize => {
                let size_a = a.metadata.file_size.unwrap_or(0);
                let size_b = b.metadata.file_size.unwrap_or(0);
                size_a.cmp(&size_b)
            }
        };

        let final_ord = match dir {
            SortDirection::Ascending => ord,
            SortDirection::Descending => ord.reverse(),
        };

        // Deterministic secondary tie-breaker using UUID
        if final_ord == Ordering::Equal {
            a.id.0.cmp(&b.id.0)
        } else {
            final_ord
        }
    }

    /// Filters and sorts borrowed wallpaper items.
    pub fn apply<'a>(&self, items: impl IntoIterator<Item = &'a Wallpaper>) -> Vec<&'a Wallpaper> {
        // Normalize and tokenize once per library query, not once per wallpaper.
        let normalized_search = self.search.trim().to_lowercase();
        let tokens: Vec<&str> = normalized_search.split_whitespace().collect();
        let mut filtered: Vec<&'a Wallpaper> = items
            .into_iter()
            .filter(|wallpaper| self.matches_with_tokens(wallpaper, &tokens))
            .collect();
        filtered.sort_by(|a, b| Self::compare(a, b, self.sort_key, self.sort_direction));
        filtered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WallpaperKind;

    #[allow(clippy::too_many_arguments)]
    fn make_test_wallpaper(
        title: &str,
        source: &str,
        width: u32,
        height: u32,
        codec: &str,
        duration: f64,
        size: u64,
        status: ProcessingStatus,
    ) -> Wallpaper {
        let mut wp = Wallpaper::new(title, source, WallpaperKind::Video);
        wp.metadata.width = Some(width);
        wp.metadata.height = Some(height);
        wp.metadata.aspect_ratio = Some(width as f64 / height as f64);
        wp.metadata.codec = Some(codec.into());
        wp.metadata.duration_secs = Some(duration);
        wp.metadata.file_size = Some(size);
        wp.metadata.resolution_category = Some(crate::categorize_resolution(width, height).into());
        wp.metadata.processing_state = status;
        wp
    }

    #[test]
    fn test_tokenized_multi_field_search() {
        let wp1 = make_test_wallpaper(
            "Nature Forest",
            "C:\\videos\\autumn_trees.mp4",
            1920,
            1080,
            "h264",
            12.0,
            5000,
            ProcessingStatus::Completed,
        );
        let wp2 = make_test_wallpaper(
            "Cyberpunk City",
            "D:\\assets\\neon_drive.webm",
            3840,
            2160,
            "vp9",
            60.0,
            50000,
            ProcessingStatus::Completed,
        );

        let mut query = LibraryQuery {
            search: "forest".into(),
            ..Default::default()
        };

        // Search by title token
        assert!(query.matches(&wp1));
        assert!(!query.matches(&wp2));

        // Search by filename token
        query.search = "neon_drive".into();
        assert!(!query.matches(&wp1));
        assert!(query.matches(&wp2));

        // Search by codec token
        query.search = "vp9".into();
        assert!(!query.matches(&wp1));
        assert!(query.matches(&wp2));

        // Search by resolution category token
        query.search = "4K".into();
        assert!(!query.matches(&wp1));
        assert!(query.matches(&wp2));

        // Multi-token: "forest 1080p"
        query.search = "forest 1080p".into();
        assert!(query.matches(&wp1));
        assert!(!query.matches(&wp2));

        // Non-matching token
        query.search = "forest 4K".into();
        assert!(!query.matches(&wp1));
        assert!(!query.matches(&wp2));
    }

    #[test]
    fn test_resolution_and_aspect_ratio_filters() {
        let wp_fhd = make_test_wallpaper(
            "Standard FHD",
            "fhd.mp4",
            1920,
            1080,
            "h264",
            10.0,
            1000,
            ProcessingStatus::Completed,
        );
        let wp_ultra = make_test_wallpaper(
            "Ultrawide View",
            "ultra.mp4",
            3440,
            1440,
            "h264",
            10.0,
            1000,
            ProcessingStatus::Completed,
        );
        let wp_portrait = make_test_wallpaper(
            "Phone Reel",
            "reel.mp4",
            1080,
            1920,
            "h264",
            10.0,
            1000,
            ProcessingStatus::Completed,
        );

        let mut query = LibraryQuery {
            aspect_ratio: AspectRatioFilter::Wide16x9,
            ..Default::default()
        };

        // Aspect ratio 16:9
        assert!(query.matches(&wp_fhd));
        assert!(!query.matches(&wp_ultra));
        assert!(!query.matches(&wp_portrait));

        // Aspect ratio Ultrawide
        query.aspect_ratio = AspectRatioFilter::Ultrawide21x9;
        assert!(!query.matches(&wp_fhd));
        assert!(query.matches(&wp_ultra));
        assert!(!query.matches(&wp_portrait));

        // Aspect ratio Portrait
        query.aspect_ratio = AspectRatioFilter::Portrait9x16;
        assert!(!query.matches(&wp_fhd));
        assert!(!query.matches(&wp_ultra));
        assert!(query.matches(&wp_portrait));
    }

    #[test]
    fn test_sorting_order() {
        let wp_a = make_test_wallpaper(
            "Alpha",
            "a.mp4",
            1280,
            720,
            "h264",
            30.0,
            100,
            ProcessingStatus::Completed,
        );
        let wp_b = make_test_wallpaper(
            "Beta",
            "b.mp4",
            3840,
            2160,
            "hevc",
            10.0,
            500,
            ProcessingStatus::Completed,
        );
        let wp_c = make_test_wallpaper(
            "Gamma",
            "c.mp4",
            1920,
            1080,
            "vp9",
            20.0,
            250,
            ProcessingStatus::Completed,
        );

        let items = [&wp_a, &wp_b, &wp_c];

        // Sort by Title Ascending
        let query_title = LibraryQuery {
            sort_key: SortKey::Title,
            sort_direction: SortDirection::Ascending,
            ..Default::default()
        };
        let sorted = query_title.apply(items.iter().copied());
        assert_eq!(sorted[0].title(), "Alpha");
        assert_eq!(sorted[1].title(), "Beta");
        assert_eq!(sorted[2].title(), "Gamma");

        // Sort by Resolution Descending
        let query_res = LibraryQuery {
            sort_key: SortKey::Resolution,
            sort_direction: SortDirection::Descending,
            ..Default::default()
        };
        let sorted = query_res.apply(items.iter().copied());
        assert_eq!(sorted[0].title(), "Beta"); // 4K
        assert_eq!(sorted[1].title(), "Gamma"); // 1080p
        assert_eq!(sorted[2].title(), "Alpha"); // 720p

        // Sort by Duration Ascending
        let query_dur = LibraryQuery {
            sort_key: SortKey::Duration,
            sort_direction: SortDirection::Ascending,
            ..Default::default()
        };
        let sorted = query_dur.apply(items.iter().copied());
        assert_eq!(sorted[0].title(), "Beta"); // 10.0s
        assert_eq!(sorted[1].title(), "Gamma"); // 20.0s
        assert_eq!(sorted[2].title(), "Alpha"); // 30.0s

        // Sort by FileSize Descending
        let query_size = LibraryQuery {
            sort_key: SortKey::FileSize,
            sort_direction: SortDirection::Descending,
            ..Default::default()
        };
        let sorted = query_size.apply(items.iter().copied());
        assert_eq!(sorted[0].title(), "Beta"); // 500
        assert_eq!(sorted[1].title(), "Gamma"); // 250
        assert_eq!(sorted[2].title(), "Alpha"); // 100
    }
}
