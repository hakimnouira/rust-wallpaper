//! Native desktop control panel for Rust Wallpaper.
//!
//! The UI is deliberately a protocol client: it never creates wallpaper windows,
//! starts MPV, or mutates daemon runtime state directly.

#[cfg(windows)]
mod desktop {
    use eframe::egui;
    use std::collections::{HashMap, HashSet};
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
        SteamWorkshop,
        OnlineWallpapers,
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
        textures: HashMap<std::path::PathBuf, (egui::TextureHandle, u64)>,
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
        pending_apply_source: Option<String>,
    }

    impl DesktopApp {
        fn new(snapshot: Arc<Mutex<Snapshot>>, commands: mpsc::Sender<Command>) -> Self {
            let (download_tx, download_rx) = mpsc::channel();
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
                frame_counter: 0,
                browse_tab: BrowseTab::SteamWorkshop,
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
                pending_apply_source: None,
            }
        }

        fn send(&mut self, command: Command, message: &str) {
            match self.commands.send(command) {
                Ok(()) => self.notice = message.into(),
                Err(error) => self.notice = format!("Could not queue command: {error}"),
            }
        }

        fn prune_texture_cache(&mut self) {
            if self.textures.len() > 120 {
                let mut entries: Vec<(std::path::PathBuf, u64)> = self
                    .textures
                    .iter()
                    .map(|(p, (_, frame))| (p.clone(), *frame))
                    .collect();
                entries.sort_by_key(|(_, frame)| *frame);
                let to_remove = entries.len().saturating_sub(80);
                for (path, _) in entries.into_iter().take(to_remove) {
                    self.textures.remove(&path);
                }
            }
        }

        fn get_or_load_texture(
            &mut self,
            ctx: &egui::Context,
            path: &std::path::Path,
            decodes_left: &mut usize,
        ) -> Option<egui::TextureId> {
            if let Some((handle, frame)) = self.textures.get_mut(path) {
                *frame = self.frame_counter;
                return Some(handle.id());
            }

            if *decodes_left > 0 && path.is_file() {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let pixels = rgba.into_raw();
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                        let handle = ctx.load_texture(
                            path.to_string_lossy(),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        *decodes_left -= 1;
                        ctx.request_repaint();
                        let id = handle.id();
                        self.textures
                            .insert(path.to_path_buf(), (handle, self.frame_counter));
                        return Some(id);
                    }
                }
            }

            None
        }

        fn render_library(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            self.frame_counter = self.frame_counter.wrapping_add(1);
            self.prune_texture_cache();
            let mut decodes_budget: usize = 4;

            ui.heading("Wallpaper library");
            ui.label("Import local videos, filter, and assign them to your displays.");
            ui.add_space(8.0);

            // Import Box
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("Import:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_path)
                            .hint_text(r"Full path, e.g. D:\Wallpapers\nature.mp4"),
                    );
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Video files", &["mp4", "webm", "mkv", "mov"])
                            .pick_file()
                        {
                            self.import_path = path.display().to_string();
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_title)
                            .hint_text("Display name (optional)"),
                    );
                    if ui
                        .add_enabled(
                            snapshot.connected && !self.import_path.trim().is_empty(),
                            egui::Button::new("Import video"),
                        )
                        .clicked()
                    {
                        let path = self.import_path.trim().to_string();
                        let title = if self.import_title.trim().is_empty() {
                            None
                        } else {
                            Some(self.import_title.trim().to_string())
                        };
                        self.send(
                            Command::ImportWallpaper { path, title },
                            "Import requested…",
                        );
                    }
                });
            });

            ui.add_space(6.0);

            // Row 1: Search bar + View Mode + Thumb Scale
            ui.horizontal(|ui| {
                ui.label("🔍");
                ui.add(
                    egui::TextEdit::singleline(&mut self.query.search)
                        .hint_text("Search title, file, codec, resolution…")
                        .desired_width(260.0),
                );
                if !self.query.search.is_empty() && ui.small_button("✖").clicked() {
                    self.query.search.clear();
                }

                ui.separator();

                // View Mode
                if ui
                    .selectable_label(self.view_mode == ViewMode::Grid, "⊞ Grid")
                    .clicked()
                {
                    self.view_mode = ViewMode::Grid;
                }
                if ui
                    .selectable_label(self.view_mode == ViewMode::List, "☰ List")
                    .clicked()
                {
                    self.view_mode = ViewMode::List;
                }

                ui.separator();

                // Thumbnail scale
                if self.view_mode == ViewMode::Grid {
                    egui::ComboBox::from_id_salt("thumb_scale_combo")
                        .selected_text(match self.thumb_scale {
                            ThumbnailScale::Small => "Thumb: Small",
                            ThumbnailScale::Medium => "Thumb: Medium",
                            ThumbnailScale::Large => "Thumb: Large",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.thumb_scale,
                                ThumbnailScale::Small,
                                "Small (140px)",
                            );
                            ui.selectable_value(
                                &mut self.thumb_scale,
                                ThumbnailScale::Medium,
                                "Medium (200px)",
                            );
                            ui.selectable_value(
                                &mut self.thumb_scale,
                                ThumbnailScale::Large,
                                "Large (280px)",
                            );
                        });
                }
            });

            ui.add_space(4.0);

            // Row 2: Sort and Filters
            ui.horizontal_wrapped(|ui| {
                // Sort Key
                egui::ComboBox::from_id_salt("sort_key_combo")
                    .selected_text(format!("Sort: {}", self.query.sort_key.label()))
                    .show_ui(ui, |ui| {
                        for key in SortKey::ALL {
                            ui.selectable_value(&mut self.query.sort_key, key, key.label());
                        }
                    });

                let dir_text = match self.query.sort_direction {
                    SortDirection::Ascending => "▲ Asc",
                    SortDirection::Descending => "▼ Desc",
                };
                if ui.button(dir_text).clicked() {
                    self.query.sort_direction = match self.query.sort_direction {
                        SortDirection::Ascending => SortDirection::Descending,
                        SortDirection::Descending => SortDirection::Ascending,
                    };
                }

                ui.separator();

                // Resolution
                egui::ComboBox::from_id_salt("res_filter_combo")
                    .selected_text(self.query.resolution.label())
                    .show_ui(ui, |ui| {
                        for item in ResolutionFilter::ALL {
                            ui.selectable_value(&mut self.query.resolution, item, item.label());
                        }
                    });

                // Aspect ratio
                egui::ComboBox::from_id_salt("aspect_filter_combo")
                    .selected_text(self.query.aspect_ratio.label())
                    .show_ui(ui, |ui| {
                        for item in AspectRatioFilter::ALL {
                            ui.selectable_value(&mut self.query.aspect_ratio, item, item.label());
                        }
                    });

                // Codec
                egui::ComboBox::from_id_salt("codec_filter_combo")
                    .selected_text(self.query.codec.label())
                    .show_ui(ui, |ui| {
                        for item in CodecFilter::ALL {
                            ui.selectable_value(&mut self.query.codec, item, item.label());
                        }
                    });

                // Status
                egui::ComboBox::from_id_salt("status_filter_combo")
                    .selected_text(self.query.status.label())
                    .show_ui(ui, |ui| {
                        for item in StatusFilter::ALL {
                            ui.selectable_value(&mut self.query.status, item, item.label());
                        }
                    });

                if self.query.is_filtered() && ui.button("✖ Reset Filters").clicked() {
                    self.query.reset_filters();
                }
            });

            ui.add_space(6.0);

            // Filtered results
            let filtered: Vec<&Wallpaper> = self.query.apply(&snapshot.wallpapers);

            // Multi-Selection and Bulk Actions Bar
            ui.horizontal(|ui| {
                ui.small(format!(
                    "Showing {} of {} wallpapers",
                    filtered.len(),
                    snapshot.wallpapers.len()
                ));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.selected_wallpapers.is_empty() {
                        if ui
                            .add_enabled(
                                snapshot.connected,
                                egui::Button::new(
                                    egui::RichText::new("🗑 Remove Selected")
                                        .color(egui::Color32::from_rgb(230, 80, 80)),
                                ),
                            )
                            .clicked()
                        {
                            let to_remove: Vec<WallpaperId> =
                                self.selected_wallpapers.iter().copied().collect();
                            for id in to_remove {
                                self.send(
                                    Command::RemoveWallpaper { wallpaper_id: id },
                                    "Bulk removal requested…",
                                );
                            }
                            self.selected_wallpapers.clear();
                        }

                        if ui.button("Clear Selection").clicked() {
                            self.selected_wallpapers.clear();
                        }

                        ui.label(format!("✓ {} selected", self.selected_wallpapers.len()));
                    } else if !filtered.is_empty() && ui.button("Select All Visible").clicked() {
                        for wp in &filtered {
                            self.selected_wallpapers.insert(wp.id);
                        }
                    }
                });
            });

            ui.separator();

            // Empty & No-Results States
            if snapshot.wallpapers.is_empty() {
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.heading("📂 Your library is empty");
                    ui.label("Import an MP4, WebM, MKV or MOV video file to get started.");
                    ui.add_space(8.0);
                    if ui.button("Browse files…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Video files", &["mp4", "webm", "mkv", "mov"])
                            .pick_file()
                        {
                            self.import_path = path.display().to_string();
                        }
                    }
                });
                return;
            }

            if filtered.is_empty() {
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.heading("🔍 No matching wallpapers");
                    ui.label("No wallpapers match the active search and filter criteria.");
                    ui.add_space(8.0);
                    if ui.button("Reset Filters").clicked() {
                        self.query.reset_filters();
                    }
                });
                return;
            }

            // Virtualized Viewport Rendering
            let selected_monitor = self.selected_monitor.clone();
            let mut assign_req = None;
            let mut select_req = None;
            let mut retry_req = None;
            let mut remove_req = None;
            let mut toggle_select_req = None;

            let ctx = ui.ctx().clone();

            match self.view_mode {
                ViewMode::List => {
                    let row_height = 56.0;
                    let total_items = filtered.len();

                    egui::ScrollArea::vertical().show_rows(
                        ui,
                        row_height,
                        total_items,
                        |ui, row_range| {
                            for idx in row_range {
                                let wallpaper = filtered[idx];
                                let is_selected = self.selected_wallpapers.contains(&wallpaper.id);
                                let is_current = self.selected_wallpaper == Some(wallpaper.id);

                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        // Checkbox for multi-selection
                                        let mut checked = is_selected;
                                        if ui.checkbox(&mut checked, "").changed() {
                                            toggle_select_req = Some((wallpaper.id, checked));
                                        }

                                        // Thumbnail small preview
                                        let thumb_size = egui::vec2(64.0, 36.0);
                                        let thumb_path = wallpaper
                                            .metadata
                                            .thumbnail_path
                                            .as_ref()
                                            .map(std::path::PathBuf::from);

                                        let tex_id = thumb_path.as_deref().and_then(|p| {
                                            self.get_or_load_texture(&ctx, p, &mut decodes_budget)
                                        });

                                        if let Some(id) = tex_id {
                                            ui.image((id, thumb_size));
                                        } else if wallpaper.metadata.is_processing() {
                                            ui.allocate_ui(thumb_size, |ui| {
                                                ui.centered_and_justified(|ui| {
                                                    ui.spinner();
                                                });
                                            });
                                        } else {
                                            ui.allocate_ui(thumb_size, |ui| {
                                                ui.centered_and_justified(|ui| {
                                                    ui.small("No thumb");
                                                });
                                            });
                                        }

                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.strong(wallpaper.title());
                                                if let Some(ref cat) =
                                                    wallpaper.metadata.resolution_category
                                                {
                                                    ui.colored_label(
                                                        egui::Color32::from_rgb(60, 160, 240),
                                                        format!("[{}]", cat),
                                                    );
                                                }
                                                if let Some(d) = wallpaper.metadata.duration_secs {
                                                    ui.colored_label(
                                                        egui::Color32::from_rgb(180, 180, 180),
                                                        format_duration(d),
                                                    );
                                                }
                                            });
                                            ui.small(wallpaper.source());
                                        });

                                        // Actions on the right
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        snapshot.connected,
                                                        egui::Button::new("Assign"),
                                                    )
                                                    .clicked()
                                                {
                                                    assign_req = Some(wallpaper.id);
                                                }
                                                if ui
                                                    .selectable_label(is_current, "Select")
                                                    .clicked()
                                                {
                                                    select_req = Some(wallpaper.id);
                                                }
                                                if wallpaper.metadata.is_failed()
                                                    && ui.button("Retry").clicked()
                                                {
                                                    retry_req = Some(wallpaper.id);
                                                }
                                                if ui
                                                    .add_enabled(
                                                        snapshot.connected,
                                                        egui::Button::new("Remove"),
                                                    )
                                                    .clicked()
                                                {
                                                    remove_req = Some(wallpaper.id);
                                                }
                                            },
                                        );
                                    });
                                });
                            }
                        },
                    );
                }
                ViewMode::Grid => {
                    let thumb_dim = self.thumb_scale.dimensions();
                    let card_width = thumb_dim.x + 20.0;
                    let card_height = thumb_dim.y + 115.0;
                    let avail_w = ui.available_width().max(card_width);
                    let spacing = 8.0;
                    let cols = ((avail_w + spacing) / (card_width + spacing))
                        .floor()
                        .max(1.0) as usize;
                    let total_rows = filtered.len().div_ceil(cols);

                    egui::ScrollArea::vertical().show_rows(
                        ui,
                        card_height + spacing,
                        total_rows,
                        |ui, row_range| {
                            for row_idx in row_range {
                                ui.horizontal(|ui| {
                                    for col_idx in 0..cols {
                                        let item_idx = row_idx * cols + col_idx;
                                        if item_idx >= filtered.len() {
                                            break;
                                        }
                                        let wallpaper = filtered[item_idx];
                                        let is_selected =
                                            self.selected_wallpapers.contains(&wallpaper.id);
                                        let is_current =
                                            self.selected_wallpaper == Some(wallpaper.id);

                                        ui.allocate_ui(egui::vec2(card_width, card_height), |ui| {
                                            ui.group(|ui| {
                                                ui.vertical(|ui| {
                                                    // Top: Selection checkbox + Title
                                                    ui.horizontal(|ui| {
                                                        let mut checked = is_selected;
                                                        if ui.checkbox(&mut checked, "").changed() {
                                                            toggle_select_req =
                                                                Some((wallpaper.id, checked));
                                                        }
                                                        ui.strong(wallpaper.title());
                                                    });

                                                    // Thumbnail preview
                                                    let thumb_path = wallpaper
                                                        .metadata
                                                        .thumbnail_path
                                                        .as_ref()
                                                        .map(std::path::PathBuf::from);

                                                    let tex_id =
                                                        thumb_path.as_deref().and_then(|p| {
                                                            self.get_or_load_texture(
                                                                &ctx,
                                                                p,
                                                                &mut decodes_budget,
                                                            )
                                                        });

                                                    if let Some(id) = tex_id {
                                                        ui.image((id, thumb_dim));
                                                    } else if wallpaper.metadata.is_processing() {
                                                        ui.allocate_ui(thumb_dim, |ui| {
                                                            ui.centered_and_justified(|ui| {
                                                                ui.vertical_centered(|ui| {
                                                                    ui.spinner();
                                                                    ui.small("Extracting…");
                                                                });
                                                            });
                                                        });
                                                    } else if wallpaper.metadata.is_failed() {
                                                        ui.allocate_ui(thumb_dim, |ui| {
                                                            ui.centered_and_justified(|ui| {
                                                                ui.vertical_centered(|ui| {
                                                                    ui.colored_label(
                                                                        egui::Color32::from_rgb(
                                                                            230, 80, 80,
                                                                        ),
                                                                        "⚠ Error",
                                                                    );
                                                                });
                                                            });
                                                        });
                                                    } else {
                                                        ui.allocate_ui(thumb_dim, |ui| {
                                                            ui.centered_and_justified(|ui| {
                                                                ui.small("No preview");
                                                            });
                                                        });
                                                    }

                                                    // Badges
                                                    ui.horizontal_wrapped(|ui| {
                                                        if let Some(ref cat) =
                                                            wallpaper.metadata.resolution_category
                                                        {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    60, 160, 240,
                                                                ),
                                                                format!("[{}]", cat),
                                                            );
                                                        }
                                                        if let Some(d) =
                                                            wallpaper.metadata.duration_secs
                                                        {
                                                            ui.label(format_duration(d));
                                                        }
                                                        if let Some(ref codec) =
                                                            wallpaper.metadata.codec
                                                        {
                                                            ui.label(codec.to_uppercase());
                                                        }
                                                    });

                                                    // Action buttons
                                                    ui.horizontal(|ui| {
                                                        if ui
                                                            .add_enabled(
                                                                snapshot.connected,
                                                                egui::Button::new("Assign"),
                                                            )
                                                            .clicked()
                                                        {
                                                            assign_req = Some(wallpaper.id);
                                                        }
                                                        if ui
                                                            .selectable_label(is_current, "Select")
                                                            .clicked()
                                                        {
                                                            select_req = Some(wallpaper.id);
                                                        }
                                                        if wallpaper.metadata.is_failed()
                                                            && ui.button("Retry").clicked()
                                                        {
                                                            retry_req = Some(wallpaper.id);
                                                        }
                                                        if ui
                                                            .add_enabled(
                                                                snapshot.connected,
                                                                egui::Button::new("Remove"),
                                                            )
                                                            .clicked()
                                                        {
                                                            remove_req = Some(wallpaper.id);
                                                        }
                                                    });
                                                });
                                            });
                                        });
                                        ui.add_space(spacing);
                                    }
                                });
                                ui.add_space(spacing);
                            }
                        },
                    );
                }
            }

            // Apply queued actions
            if let Some((id, checked)) = toggle_select_req {
                if checked {
                    self.selected_wallpapers.insert(id);
                } else {
                    self.selected_wallpapers.remove(&id);
                }
            }

            if let Some(id) = assign_req {
                self.send(
                    Command::ApplyWallpaper {
                        wallpaper_id: id,
                        monitor: selected_monitor,
                    },
                    "Wallpaper assignment requested…",
                );
            }

            if let Some(id) = select_req {
                self.selected_wallpaper = Some(id);
            }

            if let Some(id) = retry_req {
                self.send(
                    Command::RetryProcessing { wallpaper_id: id },
                    "Retrying processing…",
                );
            }

            if let Some(id) = remove_req {
                self.send(
                    Command::RemoveWallpaper { wallpaper_id: id },
                    "Removal requested…",
                );
            }
        }

        fn render_browse(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            // Process any background download completion messages
            while let Ok(msg) = self.download_rx.try_recv() {
                match msg {
                    DownloadMessage::Progress { id, status } => {
                        self.online_download_status.insert(id, status);
                    }
                    DownloadMessage::Complete { id, path, title } => {
                        self.online_download_status
                            .insert(id.clone(), "Downloaded! Adding to library…".into());
                        self.send(
                            Command::ImportWallpaper {
                                path: path.to_string_lossy().to_string(),
                                title: Some(title),
                            },
                            "Online wallpaper imported to library",
                        );
                    }
                    DownloadMessage::Failed { id, error } => {
                        self.online_download_status
                            .insert(id, format!("Failed: {error}"));
                    }
                }
            }

            // Auto-scan Steam Workshop on first visit
            if !self.workshop_scanned {
                let custom = if !self.custom_steam_path.trim().is_empty() {
                    vec![std::path::PathBuf::from(self.custom_steam_path.trim())]
                } else {
                    Vec::new()
                };
                self.workshop_items = wallpaper_core::scan_wallpaper_engine_items(&custom);
                self.workshop_scanned = true;
            }

            ui.heading("Browse & Workshop");
            ui.add_space(4.0);

            // Sub-tabs
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        matches!(self.browse_tab, BrowseTab::SteamWorkshop),
                        "📂  Steam Workshop (Local)",
                    )
                    .clicked()
                {
                    self.browse_tab = BrowseTab::SteamWorkshop;
                }
                if ui
                    .selectable_label(
                        matches!(self.browse_tab, BrowseTab::OnlineWallpapers),
                        "🌐  Online Wallpapers (Wallhaven & Videos)",
                    )
                    .clicked()
                {
                    self.browse_tab = BrowseTab::OnlineWallpapers;
                    if self.online_items.is_empty() {
                        self.online_items = wallpaper_core::get_curated_video_wallpapers();
                    }
                }
            });

            ui.separator();
            ui.add_space(6.0);

            match self.browse_tab {
                BrowseTab::SteamWorkshop => self.render_steam_workshop(ui, snapshot),
                BrowseTab::OnlineWallpapers => self.render_online_wallpapers(ui, snapshot),
            }
        }

        fn render_steam_workshop(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            ui.horizontal(|ui| {
                ui.label("🔍 Search:");
                ui.text_edit_singleline(&mut self.workshop_search);
                if ui.button("⟳ Refresh / Scan").clicked() {
                    let custom = if !self.custom_steam_path.trim().is_empty() {
                        vec![std::path::PathBuf::from(self.custom_steam_path.trim())]
                    } else {
                        Vec::new()
                    };
                    self.workshop_items = wallpaper_core::scan_wallpaper_engine_items(&custom);
                    self.workshop_scanned = true;
                    self.notice = format!("Found {} Steam items", self.workshop_items.len());
                }
                ui.checkbox(
                    &mut self.workshop_filter_video_only,
                    "Playable videos only (.mp4, .webm)",
                );
            });

            ui.horizontal(|ui| {
                ui.small("Custom Steam / Wallpaper Engine folder (optional):");
                ui.text_edit_singleline(&mut self.custom_steam_path);
            });

            ui.add_space(6.0);

            let search_term = self.workshop_search.trim().to_lowercase();
            let filtered: Vec<wallpaper_core::WorkshopItem> = self
                .workshop_items
                .iter()
                .filter(|item| {
                    if self.workshop_filter_video_only && !item.is_compatible_video {
                        return false;
                    }
                    if !search_term.is_empty() {
                        let in_title = item.title.to_lowercase().contains(&search_term);
                        let in_tags = item
                            .tags
                            .iter()
                            .any(|t| t.to_lowercase().contains(&search_term));
                        if !in_title && !in_tags {
                            return false;
                        }
                    }
                    true
                })
                .cloned()
                .collect();

            if filtered.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading("No Workshop items found");
                    ui.label(
                        "Subscribe to video wallpapers in Wallpaper Engine or Steam Workshop, or click 'Refresh / Scan'.",
                    );
                });
                return;
            }

            ui.label(format!("Showing {} detected items", filtered.len()));
            ui.add_space(4.0);

            let available_width = ui.available_width();
            let card_width = 240.0;
            let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

            let ctx = ui.ctx().clone();
            let mut decodes_left = 4;
            let mut import_req: Option<(String, String, bool)> = None;
            let mut apply_req: Option<WallpaperId> = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                for chunk in filtered.chunks(cols) {
                    ui.horizontal(|ui| {
                        for item in chunk {
                            ui.vertical(|ui| {
                                ui.set_width(card_width);
                                egui::Frame::group(ui.style())
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        ui.set_width(card_width - 16.0);

                                        // Preview thumbnail
                                        let thumb_size = egui::vec2(card_width - 16.0, 110.0);
                                        let tex = item.preview_path.as_deref().and_then(|p| {
                                            self.get_or_load_texture(&ctx, p, &mut decodes_left)
                                        });

                                        if let Some(tex_id) = tex {
                                            ui.image((tex_id, thumb_size));
                                        } else {
                                            ui.allocate_ui(thumb_size, |ui| {
                                                ui.centered_and_justified(|ui| {
                                                    ui.small("No preview");
                                                });
                                            });
                                        }

                                        ui.add_space(4.0);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&item.title).strong(),
                                            )
                                            .truncate(),
                                        );

                                        ui.horizontal(|ui| {
                                            if item.is_compatible_video {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(80, 200, 120),
                                                    "✔ Video",
                                                );
                                            } else {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(180, 180, 180),
                                                    &item.item_type,
                                                );
                                            }
                                            if let Some(size) = item.file_size_bytes {
                                                ui.small(format_file_size(size));
                                            }
                                        });

                                        let in_library_wp = snapshot.wallpapers.iter().find(|w| {
                                            w.source() == item.file_path.to_string_lossy()
                                                || std::path::Path::new(w.source())
                                                    == item.file_path
                                        });

                                        ui.add_space(4.0);
                                        if let Some(wp) = in_library_wp {
                                            ui.horizontal(|ui| {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(100, 180, 255),
                                                    "✔ In Library",
                                                );
                                                if ui.button("▶ Apply").clicked() {
                                                    apply_req = Some(wp.id);
                                                }
                                            });
                                        } else if item.is_compatible_video {
                                            ui.horizontal(|ui| {
                                                if ui.button("⬇ Import").clicked() {
                                                    import_req = Some((
                                                        item.file_path
                                                            .to_string_lossy()
                                                            .to_string(),
                                                        item.title.clone(),
                                                        false,
                                                    ));
                                                }
                                                if ui.button("▶ Apply").clicked() {
                                                    import_req = Some((
                                                        item.file_path
                                                            .to_string_lossy()
                                                            .to_string(),
                                                        item.title.clone(),
                                                        true,
                                                    ));
                                                }
                                            });
                                        } else {
                                            ui.small("(Scene / WE proprietary)");
                                        }
                                    });
                            });
                        }
                    });
                    ui.add_space(8.0);
                }
            });

            if let Some((path, title, auto_apply)) = import_req {
                if auto_apply {
                    self.pending_apply_source = Some(path.clone());
                }
                self.send(
                    Command::ImportWallpaper {
                        path,
                        title: Some(title.clone()),
                    },
                    &format!("Importing '{title}' to library…"),
                );
            }

            if let Some(wp_id) = apply_req {
                self.send(
                    Command::ApplyWallpaper {
                        wallpaper_id: wp_id,
                        monitor: self.selected_monitor.clone(),
                    },
                    "Wallpaper assignment requested…",
                );
            }
        }

        fn render_online_wallpapers(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
            ui.horizontal(|ui| {
                ui.label("🔍 Search:");
                let text_resp = ui.text_edit_singleline(&mut self.online_search_query);
                let hit_enter =
                    text_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                egui::ComboBox::from_id_salt("online_cat")
                    .selected_text(&self.online_category)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.online_category,
                            "Videos (Free Loops)".into(),
                            "Videos (Free Loops)",
                        );
                        ui.selectable_value(
                            &mut self.online_category,
                            "All".into(),
                            "All (Wallhaven)",
                        );
                        ui.selectable_value(&mut self.online_category, "Anime".into(), "Anime");
                        ui.selectable_value(&mut self.online_category, "General".into(), "General");
                    });

                let search_clicked = ui.button("Search / Browse").clicked();
                if search_clicked || hit_enter {
                    if self.online_category == "Videos (Free Loops)" {
                        self.online_items = wallpaper_core::get_curated_video_wallpapers();
                    } else {
                        let query = self.online_search_query.clone();
                        let cat = self.online_category.clone();
                        match wallpaper_core::search_wallhaven(&query, Some(&cat), 1) {
                            Ok(results) => {
                                self.online_items = results;
                                self.notice = format!(
                                    "Found {} wallpapers from Wallhaven",
                                    self.online_items.len()
                                );
                            }
                            Err(e) => {
                                self.notice = format!("Wallhaven search error: {e}");
                            }
                        }
                    }
                }
            });

            ui.add_space(6.0);

            if self.online_items.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading("No online wallpapers loaded");
                    ui.label("Select a category or type a search query, then click Search.");
                });
                return;
            }

            ui.label(format!("Showing {} wallpapers", self.online_items.len()));
            ui.add_space(4.0);

            let available_width = ui.available_width();
            let card_width = 240.0;
            let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);
            let items = self.online_items.clone();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for chunk in items.chunks(cols) {
                    ui.horizontal(|ui| {
                        for item in chunk {
                            ui.vertical(|ui| {
                                ui.set_width(card_width);
                                egui::Frame::group(ui.style())
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        ui.set_width(card_width - 16.0);

                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&item.title).strong(),
                                            )
                                            .truncate(),
                                        );

                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                if item.is_video {
                                                    egui::Color32::from_rgb(80, 200, 120)
                                                } else {
                                                    egui::Color32::from_rgb(100, 180, 255)
                                                },
                                                if item.is_video { "Video Loop" } else { "Image" },
                                            );
                                            ui.small(&item.resolution);
                                        });

                                        ui.small(format!("Source: {}", item.source_platform));
                                        ui.add_space(4.0);

                                        let status = self.online_download_status.get(&item.id);
                                        if let Some(status_text) = status {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(255, 200, 80),
                                                status_text,
                                            );
                                        } else {
                                            ui.horizontal(|ui| {
                                                if ui.button("⬇ Import").clicked() {
                                                    let tx = self.download_tx.clone();
                                                    let it = item.clone();
                                                    std::thread::spawn(move || {
                                                        let _ =
                                                            tx.send(DownloadMessage::Progress {
                                                                id: it.id.clone(),
                                                                status: "Downloading…".into(),
                                                            });
                                                        let local_app_data =
                                                            std::env::var_os("LOCALAPPDATA")
                                                                .unwrap_or_default();
                                                        let downloads_dir =
                                                            std::path::PathBuf::from(
                                                                local_app_data,
                                                            )
                                                            .join("RustWallpaper")
                                                            .join("downloads");
                                                        let ext =
                                                            if it.is_video { "mp4" } else { "jpg" };
                                                        let dest = downloads_dir
                                                            .join(format!("{}.{}", it.id, ext));

                                                        match wallpaper_core::download_online_file(
                                                            &it.download_url,
                                                            &dest,
                                                        ) {
                                                            Ok(_) => {
                                                                let _ = tx.send(
                                                                    DownloadMessage::Complete {
                                                                        id: it.id,
                                                                        path: dest,
                                                                        title: it.title,
                                                                    },
                                                                );
                                                            }
                                                            Err(e) => {
                                                                let _ = tx.send(
                                                                    DownloadMessage::Failed {
                                                                        id: it.id,
                                                                        error: e.to_string(),
                                                                    },
                                                                );
                                                            }
                                                        }
                                                    });
                                                }
                                                if ui.button("▶ Apply").clicked() {
                                                    let tx = self.download_tx.clone();
                                                    let it = item.clone();
                                                    let local_app_data =
                                                        std::env::var_os("LOCALAPPDATA")
                                                            .unwrap_or_default();
                                                    let downloads_dir =
                                                        std::path::PathBuf::from(local_app_data)
                                                            .join("RustWallpaper")
                                                            .join("downloads");
                                                    let ext =
                                                        if it.is_video { "mp4" } else { "jpg" };
                                                    let dest = downloads_dir
                                                        .join(format!("{}.{}", it.id, ext));

                                                    self.pending_apply_source =
                                                        Some(dest.to_string_lossy().to_string());

                                                    std::thread::spawn(move || {
                                                        let _ =
                                                            tx.send(DownloadMessage::Progress {
                                                                id: it.id.clone(),
                                                                status: "Downloading & Applying…"
                                                                    .into(),
                                                            });

                                                        match wallpaper_core::download_online_file(
                                                            &it.download_url,
                                                            &dest,
                                                        ) {
                                                            Ok(_) => {
                                                                let _ = tx.send(
                                                                    DownloadMessage::Complete {
                                                                        id: it.id,
                                                                        path: dest,
                                                                        title: it.title,
                                                                    },
                                                                );
                                                            }
                                                            Err(e) => {
                                                                let _ = tx.send(
                                                                    DownloadMessage::Failed {
                                                                        id: it.id,
                                                                        error: e.to_string(),
                                                                    },
                                                                );
                                                            }
                                                        }
                                                    });
                                                }
                                            });
                                        }
                                    });
                            });
                        }
                    });
                    ui.add_space(8.0);
                }
            });
        }

        fn render_monitors(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            ui.heading("Monitor manager");
            ui.label("Each display is controlled through the daemon's runtime supervisor.");
            ui.add_space(12.0);
            if snapshot.monitors.is_empty() {
                ui.label("No monitors reported yet.");
                return;
            }

            ui.label("Display layout · click a screen to select its target");
            let min_x = snapshot.monitors.iter().map(|m| m.x).min().unwrap_or(0);
            let min_y = snapshot.monitors.iter().map(|m| m.y).min().unwrap_or(0);
            let max_x = snapshot
                .monitors
                .iter()
                .map(|m| m.x.saturating_add(m.width as i32))
                .max()
                .unwrap_or(1);
            let max_y = snapshot
                .monitors
                .iter()
                .map(|m| m.y.saturating_add(m.height as i32))
                .max()
                .unwrap_or(1);
            let desktop_width = (max_x - min_x).max(1) as f32;
            let desktop_height = (max_y - min_y).max(1) as f32;
            let (layout_response, painter) = ui.allocate_painter(
                egui::vec2(ui.available_width(), 190.0),
                egui::Sense::click(),
            );
            let bounds = layout_response.rect.shrink(12.0);
            let scale =
                ((bounds.width() / desktop_width).min(bounds.height() / desktop_height)).max(0.01);
            let rendered_size = egui::vec2(desktop_width * scale, desktop_height * scale);
            let origin = bounds.center() - rendered_size * 0.5;
            let pointer = layout_response.interact_pointer_pos();
            for monitor in &snapshot.monitors {
                let position = egui::pos2(
                    origin.x + (monitor.x - min_x) as f32 * scale,
                    origin.y + (monitor.y - min_y) as f32 * scale,
                );
                let size = egui::vec2(
                    (monitor.width as f32 * scale).max(28.0),
                    (monitor.height as f32 * scale).max(24.0),
                );
                let monitor_rect = egui::Rect::from_min_size(position, size);
                let selected =
                    self.selected_monitor.as_deref() == Some(monitor.device_name.as_str());
                let fill = if selected {
                    egui::Color32::from_rgb(54, 105, 154)
                } else if monitor.is_primary {
                    egui::Color32::from_rgb(57, 78, 96)
                } else {
                    egui::Color32::from_rgb(65, 70, 80)
                };
                painter.rect_filled(monitor_rect, 5.0, fill);
                painter.rect_stroke(
                    monitor_rect,
                    5.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::LIGHT_GRAY),
                );
                painter.text(
                    monitor_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &monitor.device_name,
                    egui::FontId::proportional(12.0),
                    egui::Color32::WHITE,
                );
                if layout_response.clicked()
                    && pointer.is_some_and(|position| monitor_rect.contains(position))
                {
                    self.selected_monitor = Some(monitor.device_name.clone());
                }
            }
            ui.add_space(8.0);
            for monitor in &snapshot.monitors {
                let device = monitor.device_name.clone();
                let status = snapshot
                    .players
                    .iter()
                    .find(|player| player.monitor == device);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(&monitor.device_name);
                            ui.label(format!(
                                "{} × {} · position ({}, {}){}",
                                monitor.width,
                                monitor.height,
                                monitor.x,
                                monitor.y,
                                if monitor.is_primary {
                                    " · Primary"
                                } else {
                                    ""
                                }
                            ));
                            if let Some(player) = status {
                                ui.label(format!(
                                    "{:?} · {} · {} MB",
                                    player.state,
                                    player.active_decoder,
                                    player.memory_bytes / (1024 * 1024)
                                ));
                                if player.suspended && !player.suspension_reasons.is_empty() {
                                    ui.small(format!(
                                        "Paused: {}",
                                        player.suspension_reasons.join(", ")
                                    ));
                                }
                            } else {
                                ui.label("No active video player");
                            }
                            ui.horizontal(|ui| {
                                let mut volume = self.volumes.get(&device).copied().unwrap_or(100);
                                if ui
                                    .add(egui::Slider::new(&mut volume, 0..=100).text("Volume"))
                                    .changed()
                                {
                                    self.volumes.insert(device.clone(), volume);
                                    self.send(
                                        Command::SetVolume {
                                            monitor: Some(device.clone()),
                                            volume,
                                        },
                                        "Volume updated…",
                                    );
                                }
                                if ui.button("Mute").clicked() {
                                    self.send(
                                        Command::SetMute {
                                            monitor: Some(device.clone()),
                                            muted: true,
                                        },
                                        "Mute enabled…",
                                    );
                                }
                                if ui.button("Unmute").clicked() {
                                    self.send(
                                        Command::SetMute {
                                            monitor: Some(device.clone()),
                                            muted: false,
                                        },
                                        "Mute disabled…",
                                    );
                                }
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(snapshot.connected, egui::Button::new("Resume"))
                                .clicked()
                            {
                                self.send(
                                    Command::Resume {
                                        monitor: Some(device.clone()),
                                    },
                                    "Resume requested…",
                                );
                            }
                            if ui
                                .add_enabled(snapshot.connected, egui::Button::new("Pause"))
                                .clicked()
                            {
                                self.send(
                                    Command::Pause {
                                        monitor: Some(device.clone()),
                                    },
                                    "Pause requested…",
                                );
                            }
                            if ui
                                .selectable_label(
                                    self.selected_monitor.as_deref() == Some(device.as_str()),
                                    "Target",
                                )
                                .clicked()
                            {
                                self.selected_monitor = Some(device.clone());
                            }
                            if ui.button("Default").clicked() {
                                self.send(
                                    Command::UnassignWallpaper {
                                        monitor: device.clone(),
                                    },
                                    "Restoring default assignment…",
                                );
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
            ui.add_space(8.0);
            if ui
                .add_enabled(snapshot.connected, egui::Button::new("Pause all"))
                .clicked()
            {
                self.send(Command::Pause { monitor: None }, "Pause all requested…");
            }
            if ui
                .add_enabled(snapshot.connected, egui::Button::new("Resume all"))
                .clicked()
            {
                self.send(Command::Resume { monitor: None }, "Resume all requested…");
            }
        }

        fn render_settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            ui.heading("Settings & diagnostics");
            ui.add_space(12.0);
            ui.label("Connection");
            ui.label(if snapshot.connected {
                "Connected to local wallpaper daemon"
            } else {
                "Disconnected"
            });
            if let Some(error) = &snapshot.last_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            ui.separator();
            ui.label("Performance profile");
            ui.horizontal(|ui| {
                for (profile, label) in [
                    (PerformanceProfile::Eco, "Eco"),
                    (PerformanceProfile::Balanced, "Balanced"),
                    (PerformanceProfile::Performance, "Performance"),
                ] {
                    if ui
                        .selectable_label(snapshot.profile == profile, label)
                        .clicked()
                    {
                        self.send(
                            Command::SetProfile { profile },
                            "Performance profile updated…",
                        );
                    }
                }
            });
            ui.separator();
            ui.label("Current implementation");
            ui.label(
                "The daemon owns Win32 desktop integration, monitor lifecycle and MPV processes.",
            );
            ui.label("This UI communicates only through the versioned named-pipe protocol.");
            ui.separator();
            ui.label("Not yet available");
            ui.label("WebView2 wallpapers, interactive scenes, startup settings and updater are future milestones.");
        }
    }

    impl eframe::App for DesktopApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(300));
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
            ("msyh", "C:\\Windows\\Fonts\\msyh.ttc"),
            ("msyh_ttf", "C:\\Windows\\Fonts\\msyh.ttf"),
            ("simsun", "C:\\Windows\\Fonts\\simsun.ttc"),
            ("malgun", "C:\\Windows\\Fonts\\malgun.ttf"),
            ("segoeui", "C:\\Windows\\Fonts\\segoeui.ttf"),
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
                break;
            }
        }

        ctx.set_fonts(fonts);
    }

    pub fn run() -> eframe::Result<()> {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let commands = spawn_worker(snapshot.clone());
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size([860.0, 560.0]),
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
    desktop::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("wallpaper-ui is currently a Windows desktop application.");
}
