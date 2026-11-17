#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Native desktop control panel for Rust Wallpaper.
//!
//! The UI is deliberately a protocol client: it never creates wallpaper windows,
//! starts MPV, or mutates daemon runtime state directly.

#[cfg(windows)]
mod desktop {
    mod browse;
    mod library;
    mod monitors;
    mod settings;
    use eframe::egui;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::runtime::Builder;
    use tokio::time::timeout;
    use wallpaper_core::{
        AspectRatioFilter, CodecFilter, LibraryQuery, PerformanceProfile, ResolutionFilter,
        SortDirection, SortKey, StatusFilter, Wallpaper, WallpaperId,
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
        target_fps: Option<u32>,
        ram_budget_mb: Option<u32>,
        quality: wallpaper_core::VideoQuality,
        last_updated: Option<Instant>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Page {
        Library,
        Browse,
        Monitors,
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

    const MAX_TEXTURE_CACHE_BYTES: usize = 64 * 1024 * 1024; // 64 MB
    const TARGET_TEXTURE_CACHE_BYTES: usize = 48 * 1024 * 1024; // 48 MB low watermark

    struct DesktopApp {
        snapshot: Arc<Mutex<Snapshot>>,
        commands: mpsc::Sender<Command>,
        page: Page,
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
        autostart_enabled: bool,
        direct_url_input: String,
        direct_url_status: Option<String>,
        direct_download_id: Option<String>,
        pending_apply_download_id: Option<String>,
    }

    impl DesktopApp {
        fn new(snapshot: Arc<Mutex<Snapshot>>, commands: mpsc::Sender<Command>) -> Self {
            let (download_tx, download_rx) = mpsc::channel();

            // Asynchronous thumbnail decoding worker pool
            let (thumb_req_tx, thumb_req_rx) = mpsc::sync_channel::<ThumbnailRequest>(128);
            let (thumb_res_tx, thumb_res_rx) = mpsc::channel::<DecodedThumbnail>();
            let thumb_req_rx = Arc::new(Mutex::new(thumb_req_rx));

            for i in 0..2 {
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

            #[cfg(windows)]
            {
                if !wallpaper_windows::is_browser_extension_registered() {
                    let _ = wallpaper_windows::register_browser_extension(None);
                }
            }

            Self {
                snapshot,
                commands,
                page: Page::Library,
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
                #[cfg(windows)]
                autostart_enabled: wallpaper_windows::is_autostart_enabled(),
                #[cfg(not(windows))]
                autostart_enabled: false,
                direct_url_input: String::new(),
                direct_url_status: None,
                direct_download_id: None,
                pending_apply_download_id: None,
            }
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
                && self.in_flight_thumbnails.len() < 32
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
            ctx.request_repaint_after(Duration::from_millis(100));

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

            let snapshot = self
                .snapshot
                .lock()
                .map(|state| state.clone())
                .unwrap_or_default();

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
                ui.horizontal(|ui| {
                    ui.heading("Rust Wallpaper");
                    ui.separator();
                    let (color, label) = if snapshot.connected {
                        (egui::Color32::LIGHT_GREEN, "Daemon connected")
                    } else {
                        (egui::Color32::LIGHT_RED, "Daemon disconnected")
                    };
                    ui.colored_label(color, label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!(
                            "{} monitors · {} wallpapers",
                            snapshot.monitors.len(),
                            snapshot.wallpapers.len()
                        ));
                    });
                });
            });
            egui::SidePanel::left("navigation")
                .resizable(false)
                .default_width(170.0)
                .show(ctx, |ui| {
                    ui.add_space(12.0);
                    if ui
                        .selectable_label(matches!(self.page, Page::Library), "▦  Library")
                        .clicked()
                    {
                        self.page = Page::Library;
                    }
                    if ui
                        .selectable_label(
                            matches!(self.page, Page::Browse),
                            "🌐  Browse & Workshop",
                        )
                        .clicked()
                    {
                        self.page = Page::Browse;
                    }
                    if ui
                        .selectable_label(matches!(self.page, Page::Monitors), "▣  Monitors")
                        .clicked()
                    {
                        self.page = Page::Monitors;
                    }
                    if ui
                        .selectable_label(matches!(self.page, Page::Settings), "⚙  Settings")
                        .clicked()
                    {
                        self.page = Page::Settings;
                    }
                    ui.add_space(20.0);
                    ui.separator();
                    ui.label("Assignment target");
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
                        ui.label("v0.1 · Early access");
                    });
                });
            egui::CentralPanel::default().show(ctx, |ui| match self.page {
                Page::Library => self.render_library(ui, &snapshot),
                Page::Browse => self.render_browse(ui, &snapshot),
                Page::Monitors => self.render_monitors(ui, &snapshot),
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
                    if daemon_exe.is_file() {
                        if std::process::Command::new(&daemon_exe)
                            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                            .spawn()
                            .is_ok()
                        {
                            return;
                        }
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

    fn spawn_worker(snapshot: Arc<Mutex<Snapshot>>) -> mpsc::Sender<Command> {
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
                    loop {
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
                                    tokio::time::sleep(Duration::from_secs(1)).await;
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
                                next_refresh = Instant::now() + Duration::from_secs(2);
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
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                });
            })
            .expect("failed to start UI IPC worker");
        tx
    }

    fn configure_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let font_candidates = [
            ("malgun", "C:\\Windows\\Fonts\\malgun.ttf"),
            ("segoeui", "C:\\Windows\\Fonts\\segoeui.ttf"),
            ("seguisym", "C:\\Windows\\Fonts\\seguisym.ttf"),
            ("seguiemj", "C:\\Windows\\Fonts\\seguiemj.ttf"),
        ];

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

    pub fn run() -> eframe::Result<()> {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let commands = spawn_worker(snapshot.clone());

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
            .with_title("Rust Wallpaper");

        if let Some(icon) = icon_data {
            viewport = viewport.with_icon(icon);
        }

        let options = eframe::NativeOptions {
            viewport,
            ..Default::default()
        };
        eframe::run_native(
            "Rust Wallpaper",
            options,
            Box::new(move |cc| {
                configure_fonts(&cc.egui_ctx);
                Ok(Box::new(DesktopApp::new(snapshot, commands)))
            }),
        )
    }
}

#[cfg(windows)]
fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|a| a == "--daemon" || a == "--headless" || a == "-d")
    {
        if let Err(e) = wallpaper_daemon::run_daemon() {
            eprintln!("Daemon error: {e}");
        }
        return Ok(());
    }
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
        println!("  --daemon, -d   Run wallpaper engine daemon in background/headless mode");
        println!("  --version, -v  Print version information");
        println!("  --help, -h     Print this help message");
        return Ok(());
    }
    desktop::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("wallpaper-ui is currently a Windows desktop application.");
}
