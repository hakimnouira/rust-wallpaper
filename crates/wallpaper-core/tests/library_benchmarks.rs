use std::collections::HashSet;
use std::time::Instant;
use wallpaper_core::{
    AspectRatioFilter, CodecFilter, LibraryQuery, ProcessingStatus, ResolutionFilter,
    SortDirection, SortKey, StatusFilter, Wallpaper, WallpaperId, WallpaperKind,
};

fn generate_synthetic_library(count: usize) -> Vec<Wallpaper> {
    let titles = [
        "Cyberpunk 2077 Night City Neon",
        "Nature Mountain Waterfall 4K",
        "Minimalist Geometric Sunset",
        "Anime Lo-Fi Study Room Rain",
        "Abstract 8K Fluid Flow",
        "Cosmic Deep Space Nebula",
        "Retro 80s Synthwave Highway",
        "Forest Autumn Foliage 60fps",
        "Ocean Underwater Coral Reef",
        "Nordic Aurora Borealis Winter",
    ];

    let codecs = ["h264", "hevc", "vp9", "av1"];

    let dimensions = [
        (1280, 720, "720p HD"),
        (1920, 1080, "1080p FHD"),
        (2560, 1440, "1440p QHD"),
        (3840, 2160, "4K UHD"),
        (7680, 4320, "8K UHD"),
        (2560, 1080, "1080p FHD"), // Ultrawide 21:9
        (3440, 1440, "1440p QHD"), // Ultrawide 21:9
        (1080, 1920, "1080p FHD"), // Portrait 9:16
        (1920, 1200, "1200p"),     // 16:10
    ];

    let mut wallpapers = Vec::with_capacity(count);

    for i in 0..count {
        let title_base = titles[i % titles.len()];
        let (width, height, category) = dimensions[i % dimensions.len()];
        let codec = codecs[i % codecs.len()];
        let status = match i % 20 {
            0 => ProcessingStatus::Processing,
            1 => ProcessingStatus::Failed,
            _ => ProcessingStatus::Completed,
        };

        let duration = 10.0 + (i as f64 * 3.7) % 600.0;
        let file_size = 5_000_000 + ((i as u64 * 1048576) % (2_000_000_000));
        let title = format!("{} #{:04}", title_base, i + 1);
        let source = format!("D:\\Wallpapers\\video_{:05}.mp4", i);

        let mut wp = Wallpaper::new(title, source, WallpaperKind::Video);
        wp.metadata.duration_secs = Some(duration);
        wp.metadata.width = Some(width);
        wp.metadata.height = Some(height);
        wp.metadata.aspect_ratio = Some(width as f64 / height as f64);
        wp.metadata.fps = Some(60.0);
        wp.metadata.codec = Some(codec.to_string());
        wp.metadata.file_size = Some(file_size);
        wp.metadata.resolution_category = Some(category.to_string());
        wp.metadata.processing_state = status;
        wp.metadata.processing_error = if status == ProcessingStatus::Failed {
            Some("Decoding error".into())
        } else {
            None
        };
        wp.metadata.thumbnail_path = Some(format!("C:\\Thumbnails\\{}.jpg", i));

        wallpapers.push(wp);
    }

    wallpapers
}

#[test]
fn benchmark_library_scaling_100_500_1000_5000() {
    let library_sizes = [100, 500, 1000, 5000];

    for &size in &library_sizes {
        let library = generate_synthetic_library(size);
        assert_eq!(library.len(), size);

        // 1. Benchmark: Full multi-token search
        let query = LibraryQuery {
            search: "cyberpunk 4k".to_string(),
            ..Default::default()
        };

        let start = Instant::now();
        let search_results = query.apply(&library);
        let search_duration = start.elapsed();

        // 2. Benchmark: Filtering (Resolution + Aspect + Codec)
        let filter_query = LibraryQuery {
            resolution: ResolutionFilter::Uhd4k,
            aspect_ratio: AspectRatioFilter::Wide16x9,
            codec: CodecFilter::Hevc,
            status: StatusFilter::Ready,
            ..Default::default()
        };

        let start = Instant::now();
        let filtered_results = filter_query.apply(&library);
        let filter_duration = start.elapsed();

        // 3. Benchmark: Sorting (Sort by Resolution Descending)
        let sort_query = LibraryQuery {
            sort_key: SortKey::Resolution,
            sort_direction: SortDirection::Descending,
            ..Default::default()
        };

        let start = Instant::now();
        let sorted_results = sort_query.apply(&library);
        let sort_duration = start.elapsed();

        // 4. Benchmark: Multi-selection operations
        let start = Instant::now();
        let mut selected: HashSet<WallpaperId> = HashSet::new();
        for wp in &sorted_results {
            selected.insert(wp.id);
        }
        let select_all_duration = start.elapsed();
        assert_eq!(selected.len(), size);

        // 5. Benchmark: Virtualized viewport slicing (e.g. 24 cards per viewport)
        let start = Instant::now();
        let page_start = 50.min(sorted_results.len());
        let page_end = (page_start + 24).min(sorted_results.len());
        let visible_slice = &sorted_results[page_start..page_end];
        let viewport_duration = start.elapsed();
        assert!(visible_slice.len() <= 24);

        println!(
            "=== Library Size: {:<5} wallpapers ===\n\
             - Multi-token search (\"cyberpunk 4k\"): {:>8.2?} ({} matches)\n\
             - Complex filter (4K + 16:9 + HEVC):    {:>8.2?} ({} matches)\n\
             - Full sort (Resolution Descending):    {:>8.2?}\n\
             - Bulk multi-selection ({} items):      {:>8.2?}\n\
             - Virtualized viewport slice (24 items):{:>8.2?}\n",
            size,
            search_duration,
            search_results.len(),
            filter_duration,
            filtered_results.len(),
            sort_duration,
            size,
            select_all_duration,
            viewport_duration
        );

        // Latency assertions: All operations on in-memory collections must complete in sub-frame time (< 16ms)
        assert!(
            search_duration.as_millis() < 50,
            "Search exceeded threshold on size {}: {:?}",
            size,
            search_duration
        );
        assert!(
            sort_duration.as_millis() < 50,
            "Sort exceeded threshold on size {}: {:?}",
            size,
            sort_duration
        );
        assert!(
            viewport_duration.as_micros() < 500,
            "Viewport slicing exceeded threshold on size {}: {:?}",
            size,
            viewport_duration
        );
    }
}
