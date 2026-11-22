#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Native desktop control panel for Rust Wallpaper.
//!
//! The UI is deliberately a protocol client: it never creates wallpaper windows,
//! starts MPV, or mutates daemon runtime state directly.

#[cfg(windows)]
mod tray;

#[cfg(windows)]
mod desktop {
    mod appearance;
    mod browse;
    mod extension;
    mod library;
    mod monitors;
    mod overview;
    mod settings;
    mod theme;
    use eframe::egui;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use theme::{
        accent_color, card_frame, monitor_fill, soft_accent, subtle_frame, UiPrefs, UiTheme,
    };
    use tokio::runtime::Builder;
    use tokio::time::timeout;
    use wallpaper_core::{
        AspectRatioFilter, CodecFilter, FullscreenPauseMode, LibraryQuery, PerformanceProfile,
        PlaybackSettings, ResolutionFilter, SortDirection, SortKey, StatusFilter, Wallpaper,
        WallpaperId,
    };
    use wallpaper_protocol::client::IpcClient;
    use wallpaper_protocol::{Command, MonitorInfo, PlayerStatusInfo, RequestEnvelope, Response};

    #[derive(Default, Clone)]
    struct Snapshot {
        connected: bool,
        last_error: Option<String>,
        monitors: Vec<MonitorInfo>,
        wallpapers: Vec<Wallpaper>,
        playlists: Vec<wallpaper_core::WallpaperPlaylist>,
        players: Vec<PlayerStatusInfo>,
        profile: PerformanceProfile,
        playback_settings: PlaybackSettings,
        target_fps: Option<u32>,
        ram_budget_mb: Option<u32>,
        quality: wallpaper_core::VideoQuality,
        extension_status: ExtensionStatusSnapshot,
        active_playlist_id: Option<String>,
        auto_cycle_enabled: bool,
        auto_cycle_interval_secs: u64,
        auto_cycle_shuffle: bool,
        last_updated: Option<Instant>,
    }

    #[derive(Default, Clone, PartialEq)]
    struct ExtensionStatusSnapshot {
        registered: bool,
        crx_available: bool,
        crx_path: Option<String>,
        unpacked_available: bool,
        browsers: Vec<BrowserStatusEntry>,
        extension_id: String,
        version: String,
        fetched: bool,
    }

    #[derive(Default, Clone, PartialEq)]
    struct BrowserStatusEntry {
        name: String,
        installed: bool,
        registered: bool,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Page {
        Overview,
        Library,
        Browse,
        Monitors,
        Appearance,
        Extension,
        Settings,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum LibrarySubtab {
        #[default]
        Wallpapers,
        Playlists,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum FilterOrientation {
        #[default]
        All,
        Horizontal,
        Vertical,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum BrowseTab {
        #[default]
        FeaturedThemes,
        VerticalShelf,
        InteractiveWidgets,
        LiveStreams,
        WebHub,
        LocalWorkshop,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum SteamWorkshopSubtab {
        #[default]
        LiveWorkshop,
        InstalledOnDisk,
    }

    pub(crate) struct SteamSearchRequest {
        pub(crate) search_id: u64,
        pub(crate) query: String,
        pub(crate) sort: String,
    }

    pub(crate) enum SteamSearchResponse {
        Success {
            search_id: u64,
            items: Vec<wallpaper_core::SteamWorkshopOnlineItem>,
        },
        Error {
            search_id: u64,
            error: String,
        },
    }

    enum DownloadMessage {
        Progress {
            id: String,
            status: String,
        },
        Complete {
            id: String,
            path: std::path::PathBuf,
            title: String,
        },
        Failed {
            id: String,
            error: String,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    enum ViewMode {
        #[default]
        Grid,
        List,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    enum ThumbnailScale {
        Small,
        #[default]
        Medium,
        Large,
    }

    impl ThumbnailScale {
        fn dimensions(self) -> egui::Vec2 {
            match self {
                Self::Small => egui::vec2(140.0, 79.0),
                Self::Medium => egui::vec2(200.0, 112.0),
                Self::Large => egui::vec2(280.0, 158.0),
            }
        }
    }

    fn format_duration(secs: f64) -> String {
        let total = secs.round() as u64;
        let m = total / 60;
        let s = total % 60;
        format!("{:02}:{:02}", m, s)
    }

    #[allow(dead_code)]
    fn format_file_size(bytes: u64) -> String {
        if bytes >= 1024 * 1024 * 1024 {
            format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        } else if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{bytes} B")
        }
    }

    struct ThumbnailRequest {
        path: std::path::PathBuf,
        #[allow(dead_code)]
        frame_requested: u64,
    }

    struct DecodedThumbnail {
        path: std::path::PathBuf,
        image: egui::ColorImage,
        byte_size: usize,
    }

    enum ThumbnailDecodeResult {
        Ready(DecodedThumbnail),
        Failed(std::path::PathBuf),
    }

    struct OnlineThumbRequest {
        url: String,
        cache_path: std::path::PathBuf,
        frame_requested: u64,
    }

    enum OnlineThumbResult {
        Saved {
            url: String,
            cache_path: std::path::PathBuf,
            frame_requested: u64,
        },
        Failed {
            url: String,
        },
    }

    struct CachedTexture {
        handle: egui::TextureHandle,
        byte_size: usize,
        last_accessed_frame: u64,
    }

    struct OnlineSearchRequest {
        search_id: u64,
        query: String,
        category: String,
    }

    enum OnlineSearchResponse {
        Success {
            search_id: u64,
            items: Vec<wallpaper_core::OnlineWallpaper>,
        },
        Error {
            search_id: u64,
            error: String,
        },
    }

    const MAX_TEXTURE_CACHE_BYTES: usize = 32 * 1024 * 1024; // 32 MB
    const TARGET_TEXTURE_CACHE_BYTES: usize = 24 * 1024 * 1024; // 24 MB low watermark
    const MAX_ONLINE_THUMB_DISK_BYTES: u64 = 100 * 1024 * 1024; // 100 MB disk cap

    fn prune_online_thumb_disk_cache(dir: &std::path::Path, max_bytes: u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = Vec::new();
        let mut total_bytes = 0u64;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let len = meta.len();
                total_bytes = total_bytes.saturating_add(len);
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((path, len, modified));
            }
        }
        if total_bytes <= max_bytes {
            return;
        }
        files.sort_by_key(|(_, _, modified)| *modified);
        for (path, len, _) in files {
            if total_bytes <= max_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total_bytes = total_bytes.saturating_sub(len);
            }
        }
    }

    struct DesktopApp {
        snapshot: Arc<Mutex<Arc<Snapshot>>>,
        commands: mpsc::Sender<Command>,
        page: Page,
        theme: UiTheme,
        selected_monitor: Option<String>,
        selected_wallpaper: Option<WallpaperId>,
        selected_wallpapers: HashSet<WallpaperId>,
        volumes: HashMap<String, u32>,
        import_path: String,
        import_title: String,
        query: LibraryQuery,
        view_mode: ViewMode,
        thumb_scale: ThumbnailScale,
        notice: String,
        textures: HashMap<std::path::PathBuf, CachedTexture>,
        current_cache_bytes: usize,
        visible_thumbnails: HashSet<std::path::PathBuf>,
        in_flight_thumbnails: HashSet<std::path::PathBuf>,
        failed_thumbnails: HashSet<std::path::PathBuf>,
        thumb_req_tx: mpsc::SyncSender<ThumbnailRequest>,
        thumb_res_rx: mpsc::Receiver<ThumbnailDecodeResult>,
        online_thumb_cache_dir: std::path::PathBuf,
        known_online_files: HashSet<std::path::PathBuf>,
        in_flight_online_thumbs: HashSet<String>,
        failed_online_thumbs: HashSet<String>,
        online_thumb_req_tx: mpsc::SyncSender<OnlineThumbRequest>,
        online_thumb_res_rx: mpsc::Receiver<OnlineThumbResult>,
        frame_counter: u64,
        browse_tab: BrowseTab,
        workshop_items: Vec<wallpaper_core::WorkshopItem>,
        workshop_scanned: bool,
        workshop_scanning: bool,
        last_workshop_auto_scan: Option<Instant>,
        workshop_scan_tx: mpsc::Sender<Vec<std::path::PathBuf>>,
        workshop_scan_rx: mpsc::Receiver<Vec<wallpaper_core::WorkshopItem>>,
        workshop_filter_video_only: bool,
        workshop_search: String,
        custom_steam_path: String,
        online_items: Vec<wallpaper_core::OnlineWallpaper>,
        online_search_query: String,
        online_category: String,
        online_download_status: HashMap<String, String>,
        download_tx: mpsc::Sender<DownloadMessage>,
        download_rx: mpsc::Receiver<DownloadMessage>,
        download_cancellations: HashMap<String, Arc<AtomicBool>>,
        online_search_tx: mpsc::Sender<OnlineSearchRequest>,
        online_search_rx: mpsc::Receiver<OnlineSearchResponse>,
        online_search_id: u64,
        online_is_searching: bool,
        online_search_cache: HashMap<(String, String), Vec<wallpaper_core::OnlineWallpaper>>,
        pending_apply_source: Option<String>,
        pending_delete_wallpaper: Option<(WallpaperId, String)>,
        tray_rx: crate::tray::TrayReceiver,
        tray_icon: Option<crate::tray::TrayIcon>,
        last_tray_state: Option<(bool, bool, PerformanceProfile)>,
        quitting: bool,
        quit_after: Option<Instant>,
        window_visible: bool,
        visibility_flag: Arc<AtomicBool>,
        autostart_enabled: bool,
        direct_url_input: String,
        direct_url_status: Option<String>,
        direct_download_id: Option<String>,
        pending_apply_download_id: Option<String>,
        auto_cycle_enabled: bool,
        auto_cycle_interval_secs: u64,
        auto_cycle_shuffle: bool,
        auto_cycle_index: usize,
        rotation_dirty_until: Option<Instant>,
        last_synced_snapshot_at: Option<Instant>,
        master_volume: u32,
        master_muted: bool,
        pub(crate) active_playlist_id: Option<String>,
        pub(crate) active_playlist_index: usize,
        pub(crate) library_subtab: LibrarySubtab,
        pub(crate) new_playlist_name: String,
        pub(crate) new_playlist_interval_mins: u32,
        pub(crate) new_playlist_shuffle: bool,
        pub(crate) new_playlist_time_slot: wallpaper_core::ScheduleTimeSlot,
        pub(crate) filter_orientation: FilterOrientation,
        pub(crate) hotkey_next_char: String,
        pub(crate) hotkey_pause_char: String,
        pub(crate) hotkey_mute_char: String,
        pub(crate) steam_workshop_subtab: SteamWorkshopSubtab,
        pub(crate) steam_online_items: Vec<wallpaper_core::SteamWorkshopOnlineItem>,
        pub(crate) steam_online_search: String,
        pub(crate) steam_online_sort: String,
        pub(crate) steam_online_searching: bool,
        pub(crate) steam_online_search_id: u64,
        pub(crate) steam_search_tx: mpsc::Sender<SteamSearchRequest>,
        pub(crate) steam_search_rx: mpsc::Receiver<SteamSearchResponse>,
        pub(crate) steam_search_cache:
            HashMap<(String, String), Vec<wallpaper_core::SteamWorkshopOnlineItem>>,
    }

    impl DesktopApp {
        fn new(
            snapshot: Arc<Mutex<Arc<Snapshot>>>,
            commands: mpsc::Sender<Command>,
            tray_rx: crate::tray::TrayReceiver,
            tray_icon: Option<crate::tray::TrayIcon>,
            theme: UiTheme,
            prefs: UiPrefs,
            visibility_flag: Arc<AtomicBool>,
        ) -> Self {
            let (download_tx, download_rx) = mpsc::channel();

            // A single decoder limits peak CPU and memory while browsing large libraries.
            let (thumb_req_tx, thumb_req_rx) = mpsc::sync_channel::<ThumbnailRequest>(32);
            let (thumb_res_tx, thumb_res_rx) = mpsc::channel::<ThumbnailDecodeResult>();
            let thumb_req_rx = Arc::new(Mutex::new(thumb_req_rx));

            for i in 0..1 {
                let req_rx = thumb_req_rx.clone();
                let res_tx = thumb_res_tx.clone();
                std::thread::Builder::new()
                    .name(format!("thumbnail-worker-{i}"))
                    .spawn(move || {
                        loop {
                            let req = {
                                let rx = match req_rx.lock() {
                                    Ok(guard) => guard,
                                    Err(_) => break,
                                };
                                match rx.recv() {
                                    Ok(r) => r,
                                    Err(_) => break, // Channel closed
                                }
                            };

                            let decoded = std::fs::read(&req.path)
                                .ok()
                                .and_then(|bytes| image::load_from_memory(&bytes).ok());

                            if let Some(img) = decoded {
                                // Downsample large preview images to max 320x240 to conserve GPU memory
                                let thumb = if img.width() > 320 || img.height() > 240 {
                                    img.thumbnail(320, 240)
                                } else {
                                    img
                                };
                                let rgba = thumb.to_rgba8();
                                let size = [rgba.width() as usize, rgba.height() as usize];
                                let byte_size = size[0] * size[1] * 4;
                                let pixels = rgba.into_raw();
                                let color_image =
                                    egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                                let _ =
                                    res_tx.send(ThumbnailDecodeResult::Ready(DecodedThumbnail {
                                        path: req.path,
                                        image: color_image,
                                        byte_size,
                                    }));
                            } else {
                                let _ = res_tx.send(ThumbnailDecodeResult::Failed(req.path));
                            }
                        }
                    })
                    .expect("failed to spawn thumbnail worker thread");
            }

            // Initialize online thumbnail cache directory once & pre-index existing files
            let local_app_data = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
            let online_thumb_cache_dir = std::path::PathBuf::from(local_app_data)
                .join("RustWallpaper")
                .join("thumbnails")
                .join("online");
            let _ = std::fs::create_dir_all(&online_thumb_cache_dir);
            let mut known_online_files = HashSet::new();
            if let Ok(entries) = std::fs::read_dir(&online_thumb_cache_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        known_online_files.insert(p);
                    }
                }
            }
            {
                let prune_dir = online_thumb_cache_dir.clone();
                std::thread::Builder::new()
                    .name("online-thumb-prune".into())
                    .spawn(move || {
                        prune_online_thumb_disk_cache(&prune_dir, MAX_ONLINE_THUMB_DISK_BYTES);
                    })
                    .ok();
            }

            // Bounded 4-worker pool for online thumbnail downloads
            let (online_thumb_req_tx, online_thumb_req_rx) =
                mpsc::sync_channel::<OnlineThumbRequest>(64);
            let (online_thumb_res_tx, online_thumb_res_rx) = mpsc::channel::<OnlineThumbResult>();
            let online_thumb_req_rx = Arc::new(Mutex::new(online_thumb_req_rx));
            for i in 0..4 {
                let req_rx = Arc::clone(&online_thumb_req_rx);
                let res_tx = online_thumb_res_tx.clone();
                std::thread::Builder::new()
                    .name(format!("online-thumb-worker-{i}"))
                    .spawn(move || loop {
                        let req = {
                            let Ok(guard) = req_rx.lock() else {
                                break;
                            };
                            match guard.recv() {
                                Ok(r) => r,
                                Err(_) => break,
                            }
                        };
                        let mut saved = false;
                        if let Ok(bytes) = wallpaper_core::fetch_thumbnail_bytes(&req.url) {
                            if !bytes.is_empty() {
                                let temp_file = req.cache_path.with_extension("tmp");
                                if std::fs::write(&temp_file, &bytes).is_ok()
                                    && std::fs::rename(&temp_file, &req.cache_path).is_ok()
                                {
                                    saved = true;
                                } else {
                                    let _ = std::fs::remove_file(&temp_file);
                                }
                            }
                        }
                        if saved {
                            let _ = res_tx.send(OnlineThumbResult::Saved {
                                url: req.url,
                                cache_path: req.cache_path,
                                frame_requested: req.frame_requested,
                            });
                        } else {
                            let _ = res_tx.send(OnlineThumbResult::Failed { url: req.url });
                        }
                    })
                    .expect("failed to spawn online thumb worker");
            }

            // Asynchronous background Steam Workshop disk scanner
            let (workshop_scan_tx, workshop_scan_incoming) =
                mpsc::channel::<Vec<std::path::PathBuf>>();
            let (workshop_scan_res_tx, workshop_scan_rx) =
                mpsc::channel::<Vec<wallpaper_core::WorkshopItem>>();
            std::thread::Builder::new()
                .name("workshop-disk-scanner".into())
                .spawn(move || {
                    while let Ok(custom_paths) = workshop_scan_incoming.recv() {
                        let items = wallpaper_core::scan_wallpaper_engine_items(&custom_paths);
                        let _ = workshop_scan_res_tx.send(items);
                    }
                })
                .expect("failed to spawn workshop scanner worker");

            // Asynchronous online search background worker
            let (online_search_tx, online_search_incoming) = mpsc::channel::<OnlineSearchRequest>();
            let (online_search_res_tx, online_search_rx) = mpsc::channel::<OnlineSearchResponse>();

            std::thread::Builder::new()
                .name("online-search-worker".into())
                .spawn(move || {
                    while let Ok(req) = online_search_incoming.recv() {
                        let cat = if req.category.trim().is_empty() || req.category == "All" {
                            None
                        } else {
                            Some(req.category.as_str())
                        };
                        match wallpaper_core::search_wallhaven(&req.query, cat, 1) {
                            Ok(items) => {
                                let _ = online_search_res_tx.send(OnlineSearchResponse::Success {
                                    search_id: req.search_id,
                                    items,
                                });
                            }
                            Err(e) => {
                                let _ = online_search_res_tx.send(OnlineSearchResponse::Error {
                                    search_id: req.search_id,
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                })
                .expect("failed to spawn online search worker");

            // Asynchronous Steam Workshop search background worker
            let (steam_search_tx, steam_search_incoming) = mpsc::channel::<SteamSearchRequest>();
            let (steam_search_res_tx, steam_search_rx) = mpsc::channel::<SteamSearchResponse>();

            std::thread::Builder::new()
                .name("steam-workshop-search-worker".into())
                .spawn(move || {
                    while let Ok(req) = steam_search_incoming.recv() {
                        match wallpaper_core::fetch_steam_workshop_items(&req.query, &req.sort) {
                            Ok(items) => {
                                let _ = steam_search_res_tx.send(SteamSearchResponse::Success {
                                    search_id: req.search_id,
                                    items,
                                });
                            }
                            Err(e) => {
                                let _ = steam_search_res_tx.send(SteamSearchResponse::Error {
                                    search_id: req.search_id,
                                    error: e,
                                });
                            }
                        }
                    }
                })
                .expect("failed to spawn steam workshop search worker");

            // Run extension registration off the UI thread to avoid blocking startup.
            #[cfg(windows)]
            std::thread::Builder::new()
                .name("ext-register".into())
                .spawn(|| {
                    let _ = wallpaper_windows::register_browser_extension(None);
                })
                .ok();

            Self {
                snapshot,
                commands,
                page: Page::Overview,
                theme,
                selected_monitor: None,
                selected_wallpaper: None,
                selected_wallpapers: HashSet::new(),
                volumes: HashMap::new(),
                import_path: String::new(),
                import_title: String::new(),
                query: LibraryQuery::default(),
                view_mode: if prefs.view_mode_list {
                    ViewMode::List
                } else {
                    ViewMode::Grid
                },
                thumb_scale: match prefs.thumb_scale {
                    0 => ThumbnailScale::Small,
                    2 => ThumbnailScale::Large,
                    _ => ThumbnailScale::Medium,
                },
                notice: "Connect to the wallpaper daemon to get started.".into(),
                textures: HashMap::new(),
                current_cache_bytes: 0,
                visible_thumbnails: HashSet::new(),
                in_flight_thumbnails: HashSet::new(),
                failed_thumbnails: HashSet::new(),
                thumb_req_tx,
                thumb_res_rx,
                online_thumb_cache_dir,
                known_online_files,
                in_flight_online_thumbs: HashSet::new(),
                failed_online_thumbs: HashSet::new(),
                online_thumb_req_tx,
                online_thumb_res_rx,
                frame_counter: 0,
                browse_tab: BrowseTab::FeaturedThemes,
                workshop_items: Vec::new(),
                workshop_scanned: false,
                workshop_scanning: false,
                last_workshop_auto_scan: None,
                workshop_scan_tx,
                workshop_scan_rx,
                workshop_filter_video_only: true,
                workshop_search: String::new(),
                custom_steam_path: String::new(),
                online_items: wallpaper_core::get_curated_video_wallpapers(),
                online_search_query: String::new(),
                online_category: "Videos (Free Loops)".to_string(),
                online_download_status: HashMap::new(),
                download_tx,
                download_rx,
                download_cancellations: HashMap::new(),
                online_search_tx,
                online_search_rx,
                online_search_id: 0,
                online_is_searching: false,
                online_search_cache: HashMap::new(),
                pending_apply_source: None,
                pending_delete_wallpaper: None,
                tray_rx,
                tray_icon,
                last_tray_state: None,
                quitting: false,
                quit_after: None,
                window_visible: visibility_flag.load(Ordering::Relaxed),
                visibility_flag,
                #[cfg(windows)]
                autostart_enabled: wallpaper_windows::is_autostart_enabled(),
                #[cfg(not(windows))]
                autostart_enabled: false,
                direct_url_input: String::new(),
                direct_url_status: None,
                direct_download_id: None,
                pending_apply_download_id: None,
                auto_cycle_enabled: false,
                auto_cycle_interval_secs: 900,
                auto_cycle_shuffle: true,
                auto_cycle_index: 0,
                rotation_dirty_until: None,
                last_synced_snapshot_at: None,
                master_volume: prefs.master_volume,
                master_muted: prefs.master_muted,
                active_playlist_id: None,
                active_playlist_index: 0,
                library_subtab: LibrarySubtab::Wallpapers,
                new_playlist_name: "My Playlist".into(),
                new_playlist_interval_mins: 30,
                new_playlist_shuffle: false,
                new_playlist_time_slot: wallpaper_core::ScheduleTimeSlot::AllDay,
                filter_orientation: FilterOrientation::All,
                hotkey_next_char: prefs.hotkey_next_char,
                hotkey_pause_char: prefs.hotkey_pause_char,
                hotkey_mute_char: prefs.hotkey_mute_char,
                steam_workshop_subtab: SteamWorkshopSubtab::LiveWorkshop,
                steam_online_items: wallpaper_core::get_curated_steam_workshop_items(),
                steam_online_search: String::new(),
                steam_online_sort: "Trending".to_string(),
                steam_online_searching: false,
                steam_online_search_id: 0,
                steam_search_tx,
                steam_search_rx,
                steam_search_cache: HashMap::new(),
            }
        }

        pub(crate) fn save_prefs(&self) {
            let prefs = UiPrefs {
                hotkey_next_char: self.hotkey_next_char.clone(),
                hotkey_pause_char: self.hotkey_pause_char.clone(),
                hotkey_mute_char: self.hotkey_mute_char.clone(),
                view_mode_list: self.view_mode == ViewMode::List,
                thumb_scale: match self.thumb_scale {
                    ThumbnailScale::Small => 0,
                    ThumbnailScale::Medium => 1,
                    ThumbnailScale::Large => 2,
                },
                master_volume: self.master_volume,
                master_muted: self.master_muted,
            };
            prefs.save();
        }

        pub(crate) fn request_workshop_scan(&mut self) {
            if self.workshop_scanning {
                return;
            }
            let custom = if !self.custom_steam_path.trim().is_empty() {
                vec![std::path::PathBuf::from(self.custom_steam_path.trim())]
            } else {
                Vec::new()
            };
            if self.workshop_scan_tx.send(custom).is_ok() {
                self.workshop_scanning = true;
                self.last_workshop_auto_scan = Some(Instant::now());
            }
        }

        fn cycle_next_wallpaper(&mut self, snapshot: &Snapshot) {
            let ready_wallpapers: Vec<_> = snapshot
                .wallpapers
                .iter()
                .filter(|w| w.metadata.is_ready())
                .collect();
            if ready_wallpapers.is_empty() {
                self.notice = "No ready wallpapers in library to rotate.".into();
                return;
            }

            let next_wp = if self.auto_cycle_shuffle {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as usize;
                let idx = (nanos ^ (self.frame_counter as usize)) % ready_wallpapers.len();
                ready_wallpapers[idx]
            } else {
                self.auto_cycle_index = (self.auto_cycle_index + 1) % ready_wallpapers.len();
                ready_wallpapers[self.auto_cycle_index]
            };

            let title = next_wp.title().to_string();
            self.selected_wallpaper = Some(next_wp.id);
            self.send(
                Command::ApplyWallpaper {
                    wallpaper_id: next_wp.id,
                    monitor: self.selected_monitor.clone(),
                },
                &format!("Rotated to wallpaper: '{title}'"),
            );
        }

        pub(crate) fn cycle_playlist_wallpaper(&mut self, snapshot: &Snapshot) {
            // Check if there is a playlist explicitly scheduled for the current hour
            let scheduled_pl = snapshot.playlists.iter().find(|p| {
                p.time_slot != wallpaper_core::ScheduleTimeSlot::AllDay
                    && p.time_slot.is_active_now()
            });

            let playlist = if let Some(scheduled) = scheduled_pl {
                self.active_playlist_id = Some(scheduled.id.clone());
                scheduled.clone()
            } else {
                match self
                    .active_playlist_id
                    .as_ref()
                    .and_then(|id| snapshot.playlists.iter().find(|p| &p.id == id))
                {
                    Some(p) => p.clone(),
                    None => {
                        self.active_playlist_id = None;
                        return;
                    }
                }
            };

            if playlist.wallpaper_ids.is_empty() {
                self.notice = format!("Playlist '{}' has no wallpapers.", playlist.name);
                return;
            }

            let next_wp_id = if playlist.shuffle {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as usize;
                let idx = (nanos ^ (self.frame_counter as usize)) % playlist.wallpaper_ids.len();
                playlist.wallpaper_ids[idx]
            } else {
                self.active_playlist_index =
                    (self.active_playlist_index + 1) % playlist.wallpaper_ids.len();
                playlist.wallpaper_ids[self.active_playlist_index]
            };

            let title = snapshot
                .wallpapers
                .iter()
                .find(|w| w.id == next_wp_id)
                .map(|w| w.title().to_string())
                .unwrap_or_else(|| "wallpaper".to_string());

            self.selected_wallpaper = Some(next_wp_id);
            self.send(
                Command::ApplyWallpaper {
                    wallpaper_id: next_wp_id,
                    monitor: self.selected_monitor.clone(),
                },
                &format!("Playlist '{}' switched to: '{title}'", playlist.name),
            );
        }

        fn nav_tab(&self, ui: &mut egui::Ui, page: Page, icon: &str, label: &str) -> bool {
            let active = self.page == page;
            let text = egui::RichText::new(format!("{icon} {label}"))
                .strong()
                .size(12.5)
                .color(if active {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(185)
                });
            let btn = egui::Button::new(text)
                .fill(if active {
                    accent_color(&self.theme)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .min_size(egui::vec2(0.0, 28.0))
                .rounding(egui::Rounding::same(6.0));
            ui.add(btn).clicked()
        }

        fn send(&mut self, command: Command, message: &str) {
            match self.commands.send(command) {
                Ok(()) => self.notice = message.into(),
                Err(error) => self.notice = format!("Could not queue command: {error}"),
            }
        }

        fn prune_texture_cache(&mut self) {
            if self.current_cache_bytes > MAX_TEXTURE_CACHE_BYTES || self.textures.len() > 300 {
                let mut entries: Vec<(std::path::PathBuf, u64, usize)> = self
                    .textures
                    .iter()
                    .map(|(p, c)| (p.clone(), c.last_accessed_frame, c.byte_size))
                    .collect();
                // Oldest accessed first
                entries.sort_by_key(|(_, frame, _)| *frame);

                for (path, _, byte_size) in entries {
                    if self.current_cache_bytes <= TARGET_TEXTURE_CACHE_BYTES
                        && self.textures.len() <= 150
                    {
                        break;
                    }
                    if !self.visible_thumbnails.contains(&path)
                        && self.textures.remove(&path).is_some()
                    {
                        self.current_cache_bytes =
                            self.current_cache_bytes.saturating_sub(byte_size);
                    }
                }
            }
        }

        fn get_or_load_texture(&mut self, path: &std::path::Path) -> Option<egui::TextureId> {
            self.visible_thumbnails.insert(path.to_path_buf());

            if let Some(cached) = self.textures.get_mut(path) {
                cached.last_accessed_frame = self.frame_counter;
                return Some(cached.handle.id());
            }

            if !self.in_flight_thumbnails.contains(path)
                && !self.failed_thumbnails.contains(path)
                && self.in_flight_thumbnails.len() < 8
                && path.is_file()
                && self
                    .thumb_req_tx
                    .try_send(ThumbnailRequest {
                        path: path.to_path_buf(),
                        frame_requested: self.frame_counter,
                    })
                    .is_ok()
            {
                self.in_flight_thumbnails.insert(path.to_path_buf());
            }

            None
        }

        pub(crate) fn get_or_load_online_thumbnail(
            &mut self,
            url: &str,
        ) -> Option<egui::TextureId> {
            if url.trim().is_empty() {
                return None;
            }

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&url, &mut hasher);
            let hash = std::hash::Hasher::finish(&hasher);
            let cache_path = self
                .online_thumb_cache_dir
                .join(format!("online_{hash:016x}.jpg"));

            self.visible_thumbnails.insert(cache_path.clone());

            if self.known_online_files.contains(&cache_path) {
                return self.get_or_load_texture(&cache_path);
            }

            if !self.in_flight_online_thumbs.contains(url)
                && !self.failed_online_thumbs.contains(url)
                && self.in_flight_online_thumbs.len() < 12
                && self
                    .online_thumb_req_tx
                    .try_send(OnlineThumbRequest {
                        url: url.to_string(),
                        cache_path,
                        frame_requested: self.frame_counter,
                    })
                    .is_ok()
            {
                self.in_flight_online_thumbs.insert(url.to_string());
            }

            None
        }
    }

    impl eframe::App for DesktopApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.frame_counter = self.frame_counter.wrapping_add(1);
            self.visible_thumbnails.clear();

            let minimized = ctx
                .input(|input| input.viewport().minimized)
                .unwrap_or(false);
            let active = self.window_visible && !minimized;
            self.visibility_flag.store(active, Ordering::Relaxed);
            // Hidden tray mode only needs occasional refreshes; input and tray
            // actions still wake egui immediately.
            ctx.request_repaint_after(if active {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(30)
            });

            let snapshot: Arc<Snapshot> = self
                .snapshot
                .lock()
                .map(|state| Arc::clone(&state))
                .unwrap_or_default();

            // Sync daemon-persisted rotation state into UI when refreshed
            if snapshot.last_updated != self.last_synced_snapshot_at {
                self.last_synced_snapshot_at = snapshot.last_updated;
                let dirty = self
                    .rotation_dirty_until
                    .is_some_and(|until| Instant::now() < until);
                if !dirty && snapshot.connected {
                    self.active_playlist_id = snapshot.active_playlist_id.clone();
                    self.auto_cycle_enabled = snapshot.auto_cycle_enabled;
                    if snapshot.auto_cycle_interval_secs >= 10 {
                        self.auto_cycle_interval_secs = snapshot.auto_cycle_interval_secs;
                    }
                    self.auto_cycle_shuffle = snapshot.auto_cycle_shuffle;
                }
            }

            // Keep tray menu labels & profile checkmarks in sync with runtime state
            let any_paused = snapshot.players.iter().any(|p| p.suspended);
            let cur_tray_state = (any_paused, self.master_muted, snapshot.profile);
            if self.last_tray_state != Some(cur_tray_state) {
                if let Some(ref tray) = self.tray_icon {
                    tray.update_state(cur_tray_state.0, cur_tray_state.1, cur_tray_state.2);
                }
                self.last_tray_state = Some(cur_tray_state);
            }

            while let Ok(action) = self.tray_rx.try_recv() {
                match action {
                    crate::tray::TrayAction::Show => {
                        self.window_visible = true;
                        self.visibility_flag.store(true, Ordering::Relaxed);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    crate::tray::TrayAction::NextWallpaper => {
                        self.cycle_next_wallpaper(&snapshot);
                    }
                    crate::tray::TrayAction::TogglePause => {
                        if any_paused {
                            self.send(Command::Resume { monitor: None }, "Resumed playback…");
                        } else {
                            self.send(Command::Pause { monitor: None }, "Paused playback…");
                        }
                    }
                    crate::tray::TrayAction::ToggleMute => {
                        self.master_muted = !self.master_muted;
                        let muted = self.master_muted;
                        self.save_prefs();
                        self.send(
                            Command::SetMute {
                                monitor: None,
                                muted,
                            },
                            if muted {
                                "Muted all audio…"
                            } else {
                                "Unmuted all audio…"
                            },
                        );
                    }
                    crate::tray::TrayAction::SetProfile(profile) => {
                        self.send(
                            Command::SetProfile { profile },
                            &format!("Performance profile switched to {profile:?}"),
                        );
                    }
                    crate::tray::TrayAction::Quit => {
                        if !self.quitting {
                            self.quitting = true;
                            self.quit_after = Some(Instant::now() + Duration::from_millis(750));
                            self.send(Command::Quit, "Quitting Rust Wallpaper…");
                            self.window_visible = false;
                            self.visibility_flag.store(false, Ordering::Relaxed);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                            ctx.request_repaint_after(Duration::from_millis(100));
                        }
                    }
                }
            }

            if self.quitting {
                if self
                    .quit_after
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }

            // The window close button minimizes to the notification area. A real
            // exit is available from the tray menu (or after a tray quit request).
            if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
                if self.tray_icon.is_some() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.window_visible = false;
                    self.visibility_flag.store(false, Ordering::Relaxed);
                    self.notice = "Rust Wallpaper is still running in the notification area. Right-click its icon to quit.".into();
                } else {
                    // If Windows refused to create the tray icon, closing must still
                    // be able to stop the daemon instead of leaving it orphaned.
                    self.quitting = true;
                    self.quit_after = Some(Instant::now());
                    self.window_visible = false;
                    self.visibility_flag.store(false, Ordering::Relaxed);
                    self.send(Command::Quit, "Quitting Rust Wallpaper…");
                }
            }

            // Drain completed online thumbnail downloads and submit to local decoder pool
            let mut online_events = false;
            while let Ok(res) = self.online_thumb_res_rx.try_recv() {
                online_events = true;
                match res {
                    OnlineThumbResult::Saved {
                        url,
                        cache_path,
                        frame_requested,
                    } => {
                        self.in_flight_online_thumbs.remove(&url);
                        self.failed_online_thumbs.remove(&url);
                        self.known_online_files.insert(cache_path.clone());
                        if !self.in_flight_thumbnails.contains(&cache_path)
                            && self
                                .thumb_req_tx
                                .try_send(ThumbnailRequest {
                                    path: cache_path.clone(),
                                    frame_requested,
                                })
                                .is_ok()
                        {
                            self.in_flight_thumbnails.insert(cache_path);
                        }
                    }
                    OnlineThumbResult::Failed { url } => {
                        self.in_flight_online_thumbs.remove(&url);
                        if self.failed_online_thumbs.len() < 1024 {
                            self.failed_online_thumbs.insert(url);
                        }
                    }
                }
            }
            if online_events {
                ctx.request_repaint();
            }

            // Drain completed asynchronous thumbnail decodes and upload to GPU
            while let Ok(res) = self.thumb_res_rx.try_recv() {
                match res {
                    ThumbnailDecodeResult::Ready(decoded) => {
                        self.in_flight_thumbnails.remove(&decoded.path);
                        self.failed_thumbnails.remove(&decoded.path);
                        let handle = ctx.load_texture(
                            decoded.path.to_string_lossy(),
                            decoded.image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.current_cache_bytes += decoded.byte_size;
                        self.textures.insert(
                            decoded.path,
                            CachedTexture {
                                handle,
                                byte_size: decoded.byte_size,
                                last_accessed_frame: self.frame_counter,
                            },
                        );
                        ctx.request_repaint();
                    }
                    ThumbnailDecodeResult::Failed(path) => {
                        self.in_flight_thumbnails.remove(&path);
                        if self.failed_thumbnails.len() < 1024 {
                            self.failed_thumbnails.insert(path);
                        }
                    }
                }
            }

            // Handle OS drag-and-drop import into library
            let (has_hover_files, dropped_files) =
                ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
            if !dropped_files.is_empty() && snapshot.connected {
                let mut imported = 0usize;
                for file in dropped_files {
                    if let Some(path) = file.path {
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_default();
                        if matches!(
                            ext.as_str(),
                            "mp4"
                                | "webm"
                                | "mkv"
                                | "mov"
                                | "gif"
                                | "jpg"
                                | "jpeg"
                                | "png"
                                | "webp"
                                | "bmp"
                                | "html"
                                | "htm"
                        ) {
                            let _ = self.commands.send(Command::ImportWallpaper {
                                path: path.to_string_lossy().to_string(),
                                title: None,
                            });
                            imported += 1;
                        }
                    }
                }
                if imported > 0 {
                    self.page = Page::Library;
                    self.library_subtab = LibrarySubtab::Wallpapers;
                    self.notice = format!(
                        "Importing {imported} dropped wallpaper{} into library…",
                        if imported == 1 { "" } else { "s" }
                    );
                } else {
                    self.notice =
                        "Dropped files did not match supported video, image, or HTML formats."
                            .into();
                }
            }

            // Auto-apply newly imported wallpaper if requested
            if let Some(src) = self.pending_apply_source.clone() {
                if let Some(wp) = snapshot.wallpapers.iter().find(|w| {
                    w.source() == src
                        || std::path::Path::new(w.source()) == std::path::Path::new(&src)
                }) {
                    self.send(
                        Command::ApplyWallpaper {
                            wallpaper_id: wp.id,
                            monitor: self.selected_monitor.clone(),
                        },
                        &format!("Applied '{}' as wallpaper", wp.title()),
                    );
                    self.pending_apply_source = None;
                    self.pending_apply_download_id = None;
                }
            }
            // Top Navigation Bar (Wallpaper Engine Style)
            egui::TopBottomPanel::top("topbar")
                .frame(
                    egui::Frame::none()
                        .fill(if self.theme.dark_mode {
                            egui::Color32::from_rgb(18, 19, 25)
                        } else {
                            egui::Color32::from_rgb(240, 243, 248)
                        })
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            if self.theme.dark_mode {
                                egui::Color32::from_rgb(34, 36, 46)
                            } else {
                                egui::Color32::from_rgb(220, 224, 235)
                            },
                        ))
                        .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Brand Icon & Title (clickable home button -> Overview)
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("❖ Rust Wallpaper")
                                        .strong()
                                        .size(15.5)
                                        .color(accent_color(&self.theme)),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Go to Overview home")
                            .clicked()
                        {
                            self.page = Page::Overview;
                        }

                        ui.add_space(6.0);

                        // Live Engine Status Pill
                        let (dot_color, status_text) = if snapshot.connected {
                            let any_playing = snapshot.players.iter().any(|p| !p.suspended);
                            if any_playing {
                                (egui::Color32::from_rgb(16, 185, 129), "Engine Active")
                            } else {
                                (egui::Color32::from_rgb(245, 158, 11), "Engine Paused")
                            }
                        } else {
                            (egui::Color32::from_rgb(239, 68, 68), "Daemon Offline")
                        };
                        egui::Frame::none()
                            .fill(if self.theme.dark_mode {
                                egui::Color32::from_rgb(26, 28, 36)
                            } else {
                                egui::Color32::from_rgb(230, 234, 242)
                            })
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(dot_color, "●");
                                    ui.small(status_text);
                                });
                            });

                        ui.add_space(8.0);

                        // Top Navigation Tabs
                        if self.nav_tab(ui, Page::Overview, "🏠", "Home") {
                            self.page = Page::Overview;
                        }
                        if self.nav_tab(ui, Page::Library, "▦", "Library") {
                            self.page = Page::Library;
                        }
                        if self.nav_tab(ui, Page::Browse, "🌐", "Discover") {
                            self.page = Page::Browse;
                        }
                        if self.nav_tab(ui, Page::Monitors, "🖥", "Displays") {
                            self.page = Page::Monitors;
                        }
                        if self.nav_tab(ui, Page::Extension, "🧩", "Extension") {
                            self.page = Page::Extension;
                        }
                        if self.nav_tab(ui, Page::Appearance, "🎨", "Theme") {
                            self.page = Page::Appearance;
                        }
                        if self.nav_tab(ui, Page::Settings, "⚙", "Settings") {
                            self.page = Page::Settings;
                        }

                        // Right-aligned controls
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Minimize to Tray
                            if ui
                                .button("⏷ Tray")
                                .on_hover_text("Minimize to Windows system notification area")
                                .clicked()
                            {
                                self.window_visible = false;
                                self.visibility_flag.store(false, Ordering::SeqCst);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                            }

                            ui.separator();

                            // Master Mute
                            let mute_icon = if self.master_muted { "🔇" } else { "🔊" };
                            if ui
                                .button(mute_icon)
                                .on_hover_text("Toggle master audio (Alt+M)")
                                .clicked()
                            {
                                self.master_muted = !self.master_muted;
                                let muted = self.master_muted;
                                self.send(
                                    Command::SetMute {
                                        monitor: None,
                                        muted,
                                    },
                                    if muted {
                                        "Muted all audio…"
                                    } else {
                                        "Unmuted all audio…"
                                    },
                                );
                            }

                            // Quick Monitor Selector
                            egui::ComboBox::from_id_salt("quick_target_monitor")
                                .selected_text(
                                    self.selected_monitor.as_deref().unwrap_or("All Displays"),
                                )
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.selected_monitor.is_none(),
                                            "All Displays",
                                        )
                                        .clicked()
                                    {
                                        self.selected_monitor = None;
                                    }
                                    for m in &snapshot.monitors {
                                        ui.selectable_value(
                                            &mut self.selected_monitor,
                                            Some(m.device_name.clone()),
                                            format!("{}: {}x{}", m.device_name, m.width, m.height),
                                        );
                                    }
                                    if snapshot.monitors.len() > 1 {
                                        ui.selectable_value(
                                            &mut self.selected_monitor,
                                            Some(wallpaper_windows::SPAN_DEVICE_NAME.to_string()),
                                            "🔲 Span All Displays (Panoramic)".to_string(),
                                        );
                                    }
                                });
                            ui.small("Target Display:");
                        });
                    });
                });

            egui::CentralPanel::default().show(ctx, |ui| match self.page {
                Page::Overview => self.render_overview(ui, &snapshot),
                Page::Library => self.render_library(ui, &snapshot),
                Page::Browse => self.render_browse(ui, &snapshot),
                Page::Monitors => self.render_monitors(ui, &snapshot),
                Page::Appearance => self.render_appearance(ui),
                Page::Extension => self.render_extension(ui, &snapshot),
                Page::Settings => self.render_settings(ui, &snapshot),
            });

            egui::TopBottomPanel::bottom("statusbar")
                .frame(
                    egui::Frame::none()
                        .fill(if self.theme.dark_mode {
                            egui::Color32::from_rgb(14, 15, 20)
                        } else {
                            egui::Color32::from_rgb(238, 241, 246)
                        })
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            if self.theme.dark_mode {
                                egui::Color32::from_rgb(28, 30, 40)
                            } else {
                                egui::Color32::from_rgb(220, 224, 235)
                            },
                        ))
                        .inner_margin(egui::Margin::symmetric(14.0, 4.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if !self.notice.is_empty() {
                            ui.small(&self.notice);
                        } else {
                            ui.small("Ready");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small(format!(
                                "{} Displays  ·  {} Wallpapers in Library",
                                snapshot.monitors.len(),
                                snapshot.wallpapers.len()
                            ));
                        });
                    });
                });

            // Keep GPU texture memory bounded and retain only thumbnails used by this frame.
            self.prune_texture_cache();

            // Drag-and-drop overlay indicator
            if has_hover_files {
                let screen_rect = ctx.screen_rect();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("drop_files_overlay"),
                ));
                painter.rect_filled(
                    screen_rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(15, 23, 42, 185),
                );
                let box_rect =
                    egui::Rect::from_center_size(screen_rect.center(), egui::vec2(460.0, 140.0));
                painter.rect(
                    box_rect,
                    egui::Rounding::same(12.0),
                    egui::Color32::from_rgb(22, 30, 46),
                    egui::Stroke::new(2.0_f32, accent_color(&self.theme)),
                );
                painter.text(
                    box_rect.center() - egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    "📂 Drop files to import into Wallpaper Library",
                    egui::FontId::proportional(18.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    box_rect.center() + egui::vec2(0.0, 16.0),
                    egui::Align2::CENTER_CENTER,
                    "Supports MP4, WebM, MKV, MOV, GIF, images, and HTML5 widgets",
                    egui::FontId::proportional(12.5),
                    egui::Color32::from_rgb(160, 175, 200),
                );
            }
        }
    }

    async fn send(client: &mut IpcClient, command: Command) -> Result<Response, String> {
        let request = RequestEnvelope::new(command);
        match timeout(Duration::from_secs(5), client.request(&request)).await {
            Ok(Ok(response)) => Ok(response.result),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("daemon request timed out".into()),
        }
    }

    fn ensure_daemon_started() {
        #[cfg(windows)]
        {
            if let Ok(exe) = std::env::current_exe() {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                const DETACHED_PROCESS: u32 = 0x00000008;

                if let Some(parent) = exe.parent() {
                    let daemon_exe = parent.join("wallpaper-daemon.exe");
                    if daemon_exe.is_file()
                        && std::process::Command::new(&daemon_exe)
                            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                            .spawn()
                            .is_ok()
                    {
                        return;
                    }
                }

                // Single-exe mode: spawn current exe with --daemon detached
                if std::process::Command::new(&exe)
                    .arg("--daemon")
                    .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
        // Fallback: spawn the embedded daemon in background thread
        std::thread::Builder::new()
            .name("embedded-daemon".into())
            .spawn(|| {
                let _ = wallpaper_daemon::run_daemon();
            })
            .ok();
    }

    fn spawn_worker(
        snapshot: Arc<Mutex<Arc<Snapshot>>>,
        visibility: Arc<AtomicBool>,
    ) -> mpsc::Sender<Command> {
        let (tx, rx) = mpsc::channel::<Command>();
        std::thread::Builder::new()
            .name("wallpaper-ui-ipc".into())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        if let Ok(mut state) = snapshot.lock() {
                            let mut next = (**state).clone();
                            next.last_error = Some(error.to_string());
                            *state = Arc::new(next);
                        }
                        return;
                    }
                };
                runtime.block_on(async move {
                    let mut client: Option<IpcClient> = None;
                    let mut next_refresh = Instant::now();
                    let mut attempted_autostart = false;
                    let mut was_active = true;
                    loop {
                        let active = visibility.load(Ordering::Relaxed);
                        if active && !was_active {
                            next_refresh = Instant::now();
                        }
                        was_active = active;
                        if client.is_none() {
                            match IpcClient::connect().await {
                                Ok(connection) => {
                                    client = Some(connection);
                                    if let Ok(mut state) = snapshot.lock() {
                                        let mut next = (**state).clone();
                                        next.connected = true;
                                        next.last_error = None;
                                        *state = Arc::new(next);
                                    }
                                    next_refresh = Instant::now();
                                }
                                Err(error) => {
                                    if !attempted_autostart {
                                        attempted_autostart = true;
                                        ensure_daemon_started();
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        continue;
                                    }
                                    if let Ok(mut state) = snapshot.lock() {
                                        let mut next = (**state).clone();
                                        next.connected = false;
                                        next.last_error = Some(error.to_string());
                                        *state = Arc::new(next);
                                    }
                                    tokio::time::sleep(if active {
                                        Duration::from_secs(1)
                                    } else {
                                        Duration::from_secs(5)
                                    })
                                    .await;
                                    continue;
                                }
                            }
                        }
                        if let Some(connection) = client.as_mut() {
                            let mut disconnected = false;
                            let mut had_commands = false;
                            while let Ok(command) = rx.try_recv() {
                                had_commands = true;
                                match send(connection, command).await {
                                    Ok(Response::Error { message, .. }) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            let mut next = (**state).clone();
                                            next.last_error = Some(message);
                                            *state = Arc::new(next);
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            let mut next = (**state).clone();
                                            next.last_error = Some(error);
                                            *state = Arc::new(next);
                                        }
                                        disconnected = true;
                                        break;
                                    }
                                }
                            }
                            if had_commands && !disconnected {
                                // Refresh dashboard right after mutating commands for instant feedback.
                                next_refresh = Instant::now();
                            }
                            if !disconnected && Instant::now() >= next_refresh {
                                match send(connection, Command::GetDashboardSnapshot).await {
                                    Ok(Response::DashboardSnapshot(dash)) => {
                                        let new_snap = Snapshot {
                                            connected: true,
                                            last_error: None,
                                            monitors: dash.monitors,
                                            wallpapers: dash.wallpapers,
                                            playlists: dash.playlists,
                                            players: dash.players,
                                            profile: dash.profile,
                                            playback_settings: dash.playback_settings,
                                            target_fps: dash.target_fps,
                                            ram_budget_mb: dash.ram_budget_mb,
                                            quality: dash.quality,
                                            extension_status: ExtensionStatusSnapshot {
                                                registered: dash.extension_registered,
                                                crx_available: dash.extension_crx_available,
                                                crx_path: dash.extension_crx_path,
                                                unpacked_available: dash
                                                    .extension_unpacked_available,
                                                browsers: dash
                                                    .extension_browsers
                                                    .into_iter()
                                                    .map(|b| BrowserStatusEntry {
                                                        name: b.name,
                                                        installed: b.exe_path.is_some(),
                                                        registered: b.registered,
                                                    })
                                                    .collect(),
                                                extension_id: dash.extension_id,
                                                version: dash.extension_version,
                                                fetched: true,
                                            },
                                            active_playlist_id: dash.active_playlist_id,
                                            auto_cycle_enabled: dash.auto_cycle_enabled,
                                            auto_cycle_interval_secs: dash.auto_cycle_interval_secs,
                                            auto_cycle_shuffle: dash.auto_cycle_shuffle,
                                            last_updated: Some(Instant::now()),
                                        };
                                        if let Ok(mut state) = snapshot.lock() {
                                            *state = Arc::new(new_snap);
                                        }
                                    }
                                    Ok(Response::Error { message, .. }) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            let mut next = (**state).clone();
                                            next.last_error = Some(message);
                                            *state = Arc::new(next);
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            let mut next = (**state).clone();
                                            next.last_error = Some(error);
                                            *state = Arc::new(next);
                                        }
                                        disconnected = true;
                                    }
                                }
                                if !disconnected {
                                    next_refresh = Instant::now()
                                        + if active {
                                            Duration::from_secs(2)
                                        } else {
                                            Duration::from_secs(30)
                                        };
                                }
                            }
                            if disconnected {
                                client = None;
                                if let Ok(mut state) = snapshot.lock() {
                                    let mut next = (**state).clone();
                                    next.connected = false;
                                    *state = Arc::new(next);
                                }
                            }
                        }
                        tokio::time::sleep(if active {
                            Duration::from_millis(40)
                        } else {
                            Duration::from_millis(250)
                        })
                        .await;
                    }
                });
            })
            .expect("failed to start UI IPC worker");
        tx
    }

    fn configure_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        // Egui's bundled fonts provide fallback glyphs; load only the primary Windows UI font.
        let font_candidates = [("segoeui", "C:\\Windows\\Fonts\\segoeui.ttf")];

        for (name, path) in font_candidates {
            if let Ok(font_data) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(name.to_string(), egui::FontData::from_owned(font_data));
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push(name.to_string());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push(name.to_string());
            }
        }

        ctx.set_fonts(fonts);
    }

    pub fn run(start_hidden: bool) -> eframe::Result<()> {
        wallpaper_daemon::logging::init("ui", true);
        tracing::info!(start_hidden, "Rust Wallpaper UI starting");
        let theme = UiTheme::load();
        let prefs = UiPrefs::load();
        let snapshot = Arc::new(Mutex::new(Arc::new(Snapshot::default())));
        let visibility = Arc::new(AtomicBool::new(!start_hidden));
        let commands = spawn_worker(snapshot.clone(), visibility.clone());
        let (tray_tx, tray_rx) = mpsc::channel();

        let icon_data = image::load_from_memory(include_bytes!("../../../assets/icon.png"))
            .ok()
            .map(|img| {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                egui::IconData {
                    rgba: rgba.into_raw(),
                    width,
                    height,
                }
            });

        let mut viewport = egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0])
            // Start fully on the primary monitor; a saved/resized position can
            // otherwise leave the panel partly off-screen after a restart.
            .with_position([60.0, 60.0])
            .with_title("Rust Wallpaper")
            .with_visible(!start_hidden);

        if let Some(icon) = icon_data {
            viewport = viewport.with_icon(icon);
        }

        let options = eframe::NativeOptions {
            viewport,
            ..Default::default()
        };
        let result = eframe::run_native(
            "Rust Wallpaper",
            options,
            Box::new(move |cc| {
                configure_fonts(&cc.egui_ctx);
                theme.apply(&cc.egui_ctx);
                let tray_icon = crate::tray::TrayIcon::start(
                    cc.egui_ctx.clone(),
                    tray_tx,
                    &prefs.hotkey_next_char,
                    &prefs.hotkey_pause_char,
                    &prefs.hotkey_mute_char,
                )
                .map_err(|error| {
                    tracing::warn!(%error, "Could not create notification-area icon");
                    error
                })
                .ok();
                Ok(Box::new(DesktopApp::new(
                    snapshot, commands, tray_rx, tray_icon, theme, prefs, visibility,
                )))
            }),
        );
        if let Err(error) = &result {
            tracing::error!(%error, "Rust Wallpaper UI event loop failed");
        } else {
            tracing::info!("Rust Wallpaper UI event loop exited");
        }
        result
    }
}

#[cfg(windows)]
fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|a| a == "--daemon" || a == "--headless" || a == "-d")
    {
        // The daemon itself owns the single-instance lock, so embedded and
        // standalone daemon launches use exactly the same guard.
        if let Err(e) = wallpaper_daemon::run_daemon() {
            eprintln!("Daemon error: {e}");
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--register-extension") {
        match wallpaper_windows::register_browser_extension(None) {
            Ok(()) => {
                println!("Browser extension successfully registered for Chrome, Edge, and Brave!")
            }
            Err(e) => eprintln!("Failed to register browser extension: {e}"),
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--unregister-extension") {
        wallpaper_windows::unregister_browser_extension();
        println!("Browser extension successfully unregistered from Chrome, Edge, and Brave.");
        return Ok(());
    }

    // A normal UI launch starts the daemon and the tray icon together.
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("Rust Wallpaper v0.1.0 (production release)");
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Rust Wallpaper - Next-Generation Live Video Wallpaper Engine");
        println!();
        println!("Usage: RustWallpaper.exe [OPTIONS]");
        println!();
        println!("Options:");
        println!("  (no args)      Launch the interactive UI control panel");
        println!("  --tray         Start hidden in the Windows notification area");
        println!("  --daemon, -d   Run wallpaper engine daemon in background/headless mode");
        println!("  --version, -v  Print version information");
        println!("  --help, -h     Print this help message");
        return Ok(());
    }
    let _ui_guard = match wallpaper_windows::SingleInstanceGuard::acquire_ui() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            if !wallpaper_windows::focus_existing_ui_window() {
                eprintln!("Rust Wallpaper is already running, but its window could not be found.");
            }
            return Ok(());
        }
        Err(error) => {
            eprintln!("Could not acquire the UI single-instance lock: {error}");
            return Ok(());
        }
    };
    let start_hidden = args
        .iter()
        .any(|arg| arg == "--tray" || arg == "--minimized");
    desktop::run(start_hidden)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("wallpaper-ui is currently a Windows desktop application.");
}
