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
        accent_color, card_frame, monitor_fill, muted_color, soft_accent, subtle_frame, UiTheme,
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
        players: Vec<PlayerStatusInfo>,
        profile: PerformanceProfile,
        playback_settings: PlaybackSettings,
        target_fps: Option<u32>,
        ram_budget_mb: Option<u32>,
        quality: wallpaper_core::VideoQuality,
        last_updated: Option<Instant>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Page {
        Overview,
        Library,
        Browse,
        Monitors,
        Appearance,
        Settings,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    enum BrowseTab {
        #[default]
        OnlineWallpapers,
        LocalWorkshop,
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
        frame_requested: u64,
    }

    struct DecodedThumbnail {
        path: std::path::PathBuf,
        image: egui::ColorImage,
        byte_size: usize,
        frame_requested: u64,
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

    struct DesktopApp {
        snapshot: Arc<Mutex<Snapshot>>,
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
        thumb_req_tx: mpsc::SyncSender<ThumbnailRequest>,
        thumb_res_rx: mpsc::Receiver<DecodedThumbnail>,
        frame_counter: u64,
        browse_tab: BrowseTab,
        workshop_items: Vec<wallpaper_core::WorkshopItem>,
        workshop_scanned: bool,
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
        tray_rx: crate::tray::TrayReceiver,
        _tray_icon: Option<crate::tray::TrayIcon>,
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
        auto_cycle_last_time: Option<Instant>,
        auto_cycle_index: usize,
        master_volume: u32,
        master_muted: bool,
    }

    impl DesktopApp {
        fn new(
            snapshot: Arc<Mutex<Snapshot>>,
            commands: mpsc::Sender<Command>,
            tray_rx: crate::tray::TrayReceiver,
            tray_icon: Option<crate::tray::TrayIcon>,
            theme: UiTheme,
            visibility_flag: Arc<AtomicBool>,
        ) -> Self {
            let (download_tx, download_rx) = mpsc::channel();

            // A single decoder limits peak CPU and memory while browsing large libraries.
            let (thumb_req_tx, thumb_req_rx) = mpsc::sync_channel::<ThumbnailRequest>(32);
            let (thumb_res_tx, thumb_res_rx) = mpsc::channel::<DecodedThumbnail>();
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

                            if let Ok(bytes) = std::fs::read(&req.path) {
                                if let Ok(img) = image::load_from_memory(&bytes) {
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
                                    let _ = res_tx.send(DecodedThumbnail {
                                        path: req.path,
                                        image: color_image,
                                        byte_size,
                                        frame_requested: req.frame_requested,
                                    });
                                }
                            }
                        }
                    })
                    .expect("failed to spawn thumbnail worker thread");
            }

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
                view_mode: ViewMode::Grid,
                thumb_scale: ThumbnailScale::Medium,
                notice: "Connect to the wallpaper daemon to get started.".into(),
                textures: HashMap::new(),
                current_cache_bytes: 0,
                visible_thumbnails: HashSet::new(),
                in_flight_thumbnails: HashSet::new(),
                thumb_req_tx,
                thumb_res_rx,
                frame_counter: 0,
                browse_tab: BrowseTab::OnlineWallpapers,
                workshop_items: Vec::new(),
                workshop_scanned: false,
                workshop_filter_video_only: true,
                workshop_search: String::new(),
                custom_steam_path: String::new(),
                online_items: Vec::new(),
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
                tray_rx,
                _tray_icon: tray_icon,
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
                auto_cycle_last_time: None,
                auto_cycle_index: 0,
                master_volume: 100,
                master_muted: false,
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
            self.auto_cycle_last_time = Some(Instant::now());
        }

        fn nav_button(&self, ui: &mut egui::Ui, page: Page, label: &str) -> bool {
            let active = self.page == page;
            let mut button = egui::Button::new(label).frame(true);
            if active {
                button = button.fill(soft_accent(&self.theme));
            }
            let width = ui.available_width().max(120.0);
            ui.add_sized(egui::vec2(width, 30.0), button).clicked()
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
                && path.is_file()
                && self.in_flight_thumbnails.len() < 8
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
    }

    impl eframe::App for DesktopApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

            let snapshot = self
                .snapshot
                .lock()
                .map(|state| state.clone())
                .unwrap_or_default();

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
                        let any_paused = snapshot.players.iter().any(|p| p.suspended);
                        if any_paused {
                            self.send(Command::Resume { monitor: None }, "Resumed playback…");
                        } else {
                            self.send(Command::Pause { monitor: None }, "Paused playback…");
                        }
                    }
                    crate::tray::TrayAction::ToggleMute => {
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
                if self._tray_icon.is_some() {
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

            // Drain completed asynchronous thumbnail decodes and upload to GPU
            while let Ok(decoded) = self.thumb_res_rx.try_recv() {
                self.in_flight_thumbnails.remove(&decoded.path);
                let is_recent = self.frame_counter.saturating_sub(decoded.frame_requested) < 180;
                if is_recent || self.visible_thumbnails.contains(&decoded.path) {
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
            }

            // Hotkeys: Alt+W (Pause/Resume), Alt+N (Next Wallpaper), Alt+M (Mute/Unmute)
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::W)) {
                let any_paused = snapshot.players.iter().any(|p| p.suspended);
                if any_paused {
                    self.send(Command::Resume { monitor: None }, "Resumed playback…");
                } else {
                    self.send(Command::Pause { monitor: None }, "Paused playback…");
                }
            }
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::N)) {
                self.cycle_next_wallpaper(&snapshot);
            }
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::M)) {
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

            // Automatic wallpaper rotation timer
            if self.auto_cycle_enabled && snapshot.connected && !snapshot.wallpapers.is_empty() {
                let interval = Duration::from_secs(self.auto_cycle_interval_secs.max(10));
                let should_rotate = match self.auto_cycle_last_time {
                    Some(last) => last.elapsed() >= interval,
                    None => {
                        self.auto_cycle_last_time = Some(Instant::now());
                        false
                    }
                };
                if should_rotate {
                    self.cycle_next_wallpaper(&snapshot);
                }
                ctx.request_repaint_after(Duration::from_secs(5));
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
            egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.colored_label(accent_color(&self.theme), "◆");
                    ui.strong("Rust Wallpaper");
                    ui.add_space(10.0);
                    let (color, label) = if snapshot.connected {
                        (egui::Color32::from_rgb(90, 200, 140), "Daemon connected")
                    } else {
                        (
                            egui::Color32::from_rgb(230, 110, 110),
                            "Daemon disconnected",
                        )
                    };
                    ui.colored_label(color, format!("● {label}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small(format!(
                            "{} displays  ·  {} wallpapers",
                            snapshot.monitors.len(),
                            snapshot.wallpapers.len()
                        ));
                    });
                });
                ui.add_space(5.0);
            });
            egui::SidePanel::left("navigation")
                .resizable(false)
                .min_width(186.0)
                .max_width(186.0)
                .show(ctx, |ui| {
                    ui.add_space(14.0);
                    ui.colored_label(muted_color(&self.theme), "WORKSPACE");
                    ui.add_space(4.0);
                    if self.nav_button(ui, Page::Overview, "⌂  Overview") {
                        self.page = Page::Overview;
                    }
                    if self.nav_button(ui, Page::Library, "▦  Library") {
                        self.page = Page::Library;
                    }
                    if self.nav_button(ui, Page::Browse, "◈  Browse & Workshop") {
                        self.page = Page::Browse;
                    }
                    if self.nav_button(ui, Page::Monitors, "▣  Monitors") {
                        self.page = Page::Monitors;
                    }

                    ui.add_space(14.0);
                    ui.separator();
                    ui.colored_label(muted_color(&self.theme), "PERSONALIZE");
                    ui.add_space(4.0);
                    if self.nav_button(ui, Page::Appearance, "◐  Appearance") {
                        self.page = Page::Appearance;
                    }
                    if self.nav_button(ui, Page::Settings, "⚙  Settings") {
                        self.page = Page::Settings;
                    }

                    ui.add_space(14.0);
                    ui.separator();
                    ui.small("Assignment target");
                    egui::ComboBox::from_id_salt("target_monitor")
                        .selected_text(
                            self.selected_monitor
                                .clone()
                                .unwrap_or_else(|| "All monitors".into()),
                        )
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.selected_monitor.is_none(), "All monitors")
                                .clicked()
                            {
                                self.selected_monitor = None;
                            }
                            for monitor in &snapshot.monitors {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    Some(monitor.device_name.clone()),
                                    &monitor.device_name,
                                );
                            }
                        });
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.small("v0.1 · UI build 3 · Early access");
                    });
                });
            egui::CentralPanel::default().show(ctx, |ui| match self.page {
                Page::Overview => self.render_overview(ui, &snapshot),
                Page::Library => self.render_library(ui, &snapshot),
                Page::Browse => self.render_browse(ui, &snapshot),
                Page::Monitors => self.render_monitors(ui, &snapshot),
                Page::Appearance => self.render_appearance(ui),
                Page::Settings => self.render_settings(ui, &snapshot),
            });
            egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.notice);
                    if let Some(updated) = snapshot.last_updated {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small(format!("Updated {}s ago", updated.elapsed().as_secs()));
                        });
                    }
                });
            });
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
        snapshot: Arc<Mutex<Snapshot>>,
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
                            state.last_error = Some(error.to_string());
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
                                        state.connected = true;
                                        state.last_error = None;
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
                                        state.connected = false;
                                        state.last_error = Some(error.to_string());
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
                            while let Ok(command) = rx.try_recv() {
                                match send(connection, command).await {
                                    Ok(Response::Error { message, .. }) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            state.last_error = Some(message);
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        if let Ok(mut state) = snapshot.lock() {
                                            state.last_error = Some(error);
                                        }
                                        disconnected = true;
                                        break;
                                    }
                                }
                            }
                            if !disconnected && Instant::now() >= next_refresh {
                                for command in [
                                    Command::ListMonitors,
                                    Command::ListWallpapers,
                                    Command::GetStatus { monitor: None },
                                    Command::GetProfile,
                                    Command::GetPlaybackSettings,
                                    Command::GetPerformanceSettings,
                                ] {
                                    match send(connection, command).await {
                                        Ok(Response::MonitorList(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.monitors = items;
                                            }
                                        }
                                        Ok(Response::WallpaperList(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.wallpapers = items;
                                            }
                                        }
                                        Ok(Response::PlayerStatus(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.players = items;
                                            }
                                        }
                                        Ok(Response::Profile(profile)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.profile = profile;
                                            }
                                        }
                                        Ok(Response::PlaybackSettings(settings)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.playback_settings = settings;
                                            }
                                        }
                                        Ok(Response::PerformanceSettings {
                                            target_fps,
                                            ram_budget_mb,
                                            quality,
                                        }) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.target_fps = target_fps;
                                                state.ram_budget_mb = ram_budget_mb;
                                                state.quality = quality;
                                            }
                                        }
                                        Ok(Response::Error { message, .. }) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.last_error = Some(message);
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.last_error = Some(error);
                                            }
                                            disconnected = true;
                                            break;
                                        }
                                    }
                                }
                                next_refresh = Instant::now()
                                    + if active {
                                        Duration::from_secs(2)
                                    } else {
                                        Duration::from_secs(30)
                                    };
                                if let Ok(mut state) = snapshot.lock() {
                                    state.last_updated = Some(Instant::now());
                                }
                            }
                            if disconnected {
                                client = None;
                                if let Ok(mut state) = snapshot.lock() {
                                    state.connected = false;
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
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
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
                let tray_icon = crate::tray::TrayIcon::start(cc.egui_ctx.clone(), tray_tx)
                    .map_err(|error| {
                        tracing::warn!(%error, "Could not create notification-area icon");
                        error
                    })
                    .ok();
                Ok(Box::new(DesktopApp::new(
                    snapshot, commands, tray_rx, tray_icon, theme, visibility,
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
