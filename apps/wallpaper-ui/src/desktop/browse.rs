use super::*;

fn open_browser_url(url: &str) {
    if url.starts_with("steam://") {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", url])
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = open::that(url);
        }
        return;
    }
    #[cfg(windows)]
    {
        let _ = wallpaper_windows::launch_browser_with_extension(Some(url)).or_else(|_| {
            std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", url])
                .spawn()
                .map(|_| ())
        });
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(url);
    }
}

fn open_folder_path(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
}

impl DesktopApp {
    pub(super) fn start_direct_url_download(&mut self, auto_apply: bool) {
        let raw_url = self.direct_url_input.trim().to_string();
        if raw_url.is_empty() {
            return;
        }
        if !raw_url.starts_with("http://") && !raw_url.starts_with("https://") {
            self.direct_url_status =
                Some("Error: URL must start with http:// or https://".to_string());
            return;
        }

        let url = raw_url;
        let id = format!("url-{}", uuid::Uuid::new_v4());

        if auto_apply {
            self.pending_apply_download_id = Some(id.clone());
        }

        self.direct_url_status = Some("Connecting & resolving media…".into());
        self.direct_download_id = Some(id.clone());
        let tx = self.download_tx.clone();
        let id_clone = id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.download_cancellations
            .insert(id_clone.clone(), Arc::clone(&cancelled));

        std::thread::spawn(move || {
            let _ = tx.send(DownloadMessage::Progress {
                id: id_clone.clone(),
                status: "Resolving media from page…".into(),
            });

            let resolved = wallpaper_core::resolve_web_media_info(&url);
            let media_url = resolved.media_url;
            let display_title = resolved.title.unwrap_or_else(|| {
                media_url
                    .split('?')
                    .next()
                    .and_then(|u| u.split('/').rfind(|s| !s.is_empty()))
                    .map(|s| {
                        s.rsplit_once('.')
                            .map(|(base, _)| base)
                            .unwrap_or(s)
                            .replace(['_', '-'], " ")
                    })
                    .unwrap_or_else(|| "Web Wallpaper".into())
            });

            let local_app_data = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
            let downloads_dir = std::path::PathBuf::from(local_app_data)
                .join("RustWallpaper")
                .join("downloads");
            let _ = std::fs::create_dir_all(&downloads_dir);

            let temp_dest = downloads_dir.join(format!("{id_clone}.download"));

            let _ = tx.send(DownloadMessage::Progress {
                id: id_clone.clone(),
                status: format!("Downloading '{display_title}'…"),
            });

            let mut last_reported = 0u64;
            match wallpaper_core::download_online_file_cancellable(
                &media_url,
                &temp_dest,
                &cancelled,
                2 * 1024 * 1024 * 1024,
                |received, total| {
                    if received.saturating_sub(last_reported) >= 1024 * 1024
                        || total == Some(received)
                    {
                        last_reported = received;
                        let status = total.map_or_else(
                            || format!("Downloading… {} bytes", received),
                            |total| format!("Downloading… {} / {} bytes", received, total),
                        );
                        let _ = tx.send(DownloadMessage::Progress {
                            id: id_clone.clone(),
                            status,
                        });
                    }
                },
            ) {
                Ok(_) => {
                    let is_html = if let Ok(mut f) = std::fs::File::open(&temp_dest) {
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
                        let _ = std::fs::remove_file(&temp_dest);
                        let _ = tx.send(DownloadMessage::Failed {
                            id: id_clone,
                            error:
                                "Downloaded content is a web page HTML document, not a media file."
                                    .into(),
                        });
                        return;
                    }

                    let detected_ext =
                        wallpaper_core::detect_media_extension_from_magic_bytes(&temp_dest)
                            .unwrap_or("mp4");
                    let final_dest = downloads_dir.join(format!("{id_clone}.{detected_ext}"));
                    let _ = std::fs::rename(&temp_dest, &final_dest);

                    let _ = tx.send(DownloadMessage::Complete {
                        id: id_clone,
                        path: final_dest,
                        title: display_title,
                    });
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&temp_dest);
                    let _ = tx.send(DownloadMessage::Failed {
                        id: id_clone,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    pub(super) fn start_curated_download(
        &mut self,
        item: wallpaper_core::OnlineWallpaper,
        apply: bool,
    ) {
        let local_app_data = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
        let downloads_dir = std::path::PathBuf::from(local_app_data)
            .join("RustWallpaper")
            .join("downloads");
        let extension = if item.is_video { "mp4" } else { "jpg" };
        let destination = downloads_dir.join(format!("{}.{}", item.id, extension));
        if apply {
            self.pending_apply_source = Some(destination.to_string_lossy().to_string());
            self.pending_apply_download_id = Some(item.id.clone());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.download_cancellations
            .insert(item.id.clone(), Arc::clone(&cancelled));
        let tx = self.download_tx.clone();
        std::thread::spawn(move || {
            let label = if apply {
                "Downloading & Applying…"
            } else {
                "Downloading…"
            };
            let _ = tx.send(DownloadMessage::Progress {
                id: item.id.clone(),
                status: label.into(),
            });
            let mut last_reported = 0u64;
            let result = wallpaper_core::download_online_file_cancellable(
                &item.download_url,
                &destination,
                &cancelled,
                2 * 1024 * 1024 * 1024,
                |received, total| {
                    if received.saturating_sub(last_reported) >= 1024 * 1024
                        || total == Some(received)
                    {
                        last_reported = received;
                        let status = total.map_or_else(
                            || format!("Downloading… {} bytes", received),
                            |total| format!("Downloading… {} / {} bytes", received, total),
                        );
                        let _ = tx.send(DownloadMessage::Progress {
                            id: item.id.clone(),
                            status,
                        });
                    }
                },
            );
            match result {
                Ok(_) => {
                    let _ = tx.send(DownloadMessage::Complete {
                        id: item.id,
                        path: destination,
                        title: item.title,
                    });
                }
                Err(error) => {
                    let _ = tx.send(DownloadMessage::Failed {
                        id: item.id,
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    pub(super) fn render_browse(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        // Process any background download completion messages
        while let Ok(msg) = self.download_rx.try_recv() {
            match msg {
                DownloadMessage::Progress { id, status } => {
                    self.online_download_status.insert(id, status);
                }
                DownloadMessage::Complete { id, path, title } => {
                    self.download_cancellations.remove(&id);
                    if self.direct_download_id.as_deref() == Some(id.as_str()) {
                        self.direct_download_id = None;
                    }
                    if self.pending_apply_download_id.as_deref() == Some(id.as_str()) {
                        self.pending_apply_source = Some(path.to_string_lossy().to_string());
                        self.pending_apply_download_id = None;
                    }
                    self.online_download_status
                        .insert(id.clone(), "Downloaded! Adding to library…".into());
                    if id.starts_with("url-") {
                        self.direct_url_status = Some("Downloaded & imported to library!".into());
                    }
                    self.send(
                        Command::ImportWallpaper {
                            path: path.to_string_lossy().to_string(),
                            title: Some(title),
                        },
                        "Online wallpaper imported to library",
                    );
                }
                DownloadMessage::Failed { id, error } => {
                    self.download_cancellations.remove(&id);
                    if self.direct_download_id.as_deref() == Some(id.as_str()) {
                        self.direct_download_id = None;
                    }
                    if self.pending_apply_download_id.as_deref() == Some(id.as_str()) {
                        self.pending_apply_download_id = None;
                        self.pending_apply_source = None;
                    }
                    if id.starts_with("url-") {
                        self.direct_url_status = Some(format!("Download failed: {error}"));
                    }
                    self.online_download_status
                        .insert(id, format!("Failed: {error}"));
                }
            }
        }

        // Process any background online search responses
        while let Ok(resp) = self.online_search_rx.try_recv() {
            match resp {
                OnlineSearchResponse::Success { search_id, items } => {
                    if search_id == self.online_search_id {
                        let key = (
                            self.online_search_query.trim().to_lowercase(),
                            self.online_category.clone(),
                        );
                        self.online_search_cache.insert(key, items.clone());
                        self.online_items = items;
                        self.online_is_searching = false;
                        self.notice = format!(
                            "Found {} wallpapers from Wallhaven",
                            self.online_items.len()
                        );
                    }
                }
                OnlineSearchResponse::Error { search_id, error } => {
                    if search_id == self.online_search_id {
                        self.online_is_searching = false;
                        self.notice = format!("Wallhaven search error: {error}");
                    }
                }
            }
        }

        // Process any background Steam Workshop search response messages
        while let Ok(msg) = self.steam_search_rx.try_recv() {
            match msg {
                SteamSearchResponse::Success { search_id, items } => {
                    if search_id == self.steam_online_search_id {
                        let key = (
                            self.steam_online_search.trim().to_lowercase(),
                            self.steam_online_sort.clone(),
                        );
                        self.steam_search_cache.insert(key, items.clone());
                        self.steam_online_items = items;
                        self.steam_online_searching = false;
                        self.notice = format!(
                            "Found {} items on Steam Community Workshop",
                            self.steam_online_items.len()
                        );
                    }
                }
                SteamSearchResponse::Error { search_id, error } => {
                    if search_id == self.steam_online_search_id {
                        self.steam_online_searching = false;
                        self.notice = format!("Steam Workshop search error: {error}");
                    }
                }
            }
        }

        // Process background Steam Workshop local disk scan completions
        while let Ok(scanned_items) = self.workshop_scan_rx.try_recv() {
            self.workshop_items = scanned_items;
            self.workshop_scanned = true;
            self.workshop_scanning = false;
        }

        // Auto-scan local items asynchronously on first visit
        if !self.workshop_scanned {
            self.request_workshop_scan();
        }

        // Auto-load curated video wallpapers
        if self.online_items.is_empty() {
            self.online_items = wallpaper_core::get_curated_video_wallpapers();
        }

        ui.horizontal(|ui| {
            ui.heading("Discover & Community Hub");
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Explore 60FPS loops, vertical mobile themes, interactive HTML5 widgets, and 24/7 webcams")
                    .size(12.0)
                    .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        });
        ui.add_space(6.0);

        // Sleek Sub-Tabs Bar
        egui::Frame::none()
            .fill(if self.theme.dark_mode {
                egui::Color32::from_rgb(22, 25, 34)
            } else {
                egui::Color32::from_rgb(230, 235, 245)
            })
            .rounding(8.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let tabs = [
                        (BrowseTab::FeaturedThemes, "🌟  60FPS Curated"),
                        (BrowseTab::VerticalShelf, "📱  Vertical Shelf"),
                        (BrowseTab::InteractiveWidgets, "⚡  Interactive Widgets"),
                        (BrowseTab::LiveStreams, "🛰  24/7 Live Streams"),
                        (BrowseTab::WebHub, "🌐  Web & Extension Hub"),
                        (BrowseTab::LocalWorkshop, "🎮  Steam Workshop Hub"),
                    ];
                    for (tab, label) in tabs {
                        let is_active = self.browse_tab == tab;
                        if ui
                            .selectable_label(is_active, egui::RichText::new(label).strong())
                            .clicked()
                        {
                            self.browse_tab = tab;
                        }
                        ui.add_space(4.0);
                    }
                });
            });

        ui.add_space(8.0);

        match self.browse_tab {
            BrowseTab::FeaturedThemes => self.render_featured_themes(ui, snapshot),
            BrowseTab::VerticalShelf => self.render_vertical_shelf(ui, snapshot),
            BrowseTab::InteractiveWidgets => self.render_interactive_widgets(ui, snapshot),
            BrowseTab::LiveStreams => self.render_live_streams(ui, snapshot),
            BrowseTab::WebHub => self.render_web_hub(ui, snapshot),
            BrowseTab::LocalWorkshop => self.render_local_workshop(ui, snapshot),
        }
    }

    /// Tab 1: 🌟 60FPS Curated Video Loops & Wallhaven Search
    pub(super) fn render_featured_themes(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label("🔍 Search:");
            let text_resp = ui.text_edit_singleline(&mut self.online_search_query);
            let hit_enter = text_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            egui::ComboBox::from_id_salt("featured_cat")
                .selected_text(&self.online_category)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.online_category,
                        "Videos (Free Loops)".into(),
                        "Videos (Curated 60FPS)",
                    );
                    ui.selectable_value(&mut self.online_category, "All".into(), "All (Wallhaven)");
                    ui.selectable_value(&mut self.online_category, "Anime".into(), "Anime");
                    ui.selectable_value(&mut self.online_category, "General".into(), "General");
                });

            let search_clicked = ui.button("Search / Browse").clicked();
            if search_clicked || hit_enter {
                if self.online_category == "Videos (Free Loops)" {
                    self.online_items = wallpaper_core::get_curated_video_wallpapers();
                    self.online_is_searching = false;
                    self.notice = format!("Loaded {} curated video loops", self.online_items.len());
                } else {
                    let key = (
                        self.online_search_query.trim().to_lowercase(),
                        self.online_category.clone(),
                    );
                    if let Some(cached) = self.online_search_cache.get(&key) {
                        self.online_items = cached.clone();
                        self.online_is_searching = false;
                        self.notice =
                            format!("Loaded {} cached wallpapers", self.online_items.len());
                    } else {
                        self.online_search_id += 1;
                        self.online_is_searching = true;
                        let _ = self.online_search_tx.send(OnlineSearchRequest {
                            search_id: self.online_search_id,
                            query: self.online_search_query.clone(),
                            category: self.online_category.clone(),
                        });
                        self.notice = "Searching Wallhaven in background…".into();
                    }
                }
            }

            if self.online_is_searching {
                ui.spinner();
                ui.small("Searching…");
                if ui.button("Cancel").clicked() {
                    self.online_search_id += 1;
                    self.online_is_searching = false;
                    self.notice = "Search cancelled".into();
                }
            }
        });

        ui.add_space(6.0);

        // Filter items: In featured horizontal themes, hide vertical-only items
        let items: Vec<wallpaper_core::OnlineWallpaper> = self
            .online_items
            .iter()
            .filter(|i| !i.category.contains("Vertical"))
            .cloned()
            .collect();

        if items.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("No wallpapers found");
                ui.label("Select 'Videos (Curated 60FPS)' or search Wallhaven.");
            });
            return;
        }

        ui.label(format!("Showing {} curated video themes", items.len()));
        ui.add_space(4.0);

        let available_width = ui.available_width();
        let card_width = 260.0;
        let card_height = 275.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in items.chunks(cols) {
                ui.horizontal(|ui| {
                    for item in chunk {
                        ui.allocate_ui(egui::vec2(card_width, card_height), |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 16.0);
                                        ui.set_height(card_height - 16.0);

                                        // Thumbnail Preview
                                        let thumb_size = egui::vec2(card_width - 16.0, 136.0);
                                        let tex = self.get_or_load_online_thumbnail(&item.thumbnail_url);
                                        ui.allocate_ui(thumb_size, |ui| {
                                            ui.centered_and_justified(|ui| {
                                                if let Some(tex_id) = tex {
                                                    ui.add(
                                                        egui::Image::new((tex_id, thumb_size))
                                                            .max_size(thumb_size)
                                                            .rounding(6.0),
                                                    );
                                                } else {
                                                    ui.painter().rect_filled(
                                                        ui.available_rect_before_wrap(),
                                                        egui::Rounding::same(6.0),
                                                        egui::Color32::from_rgb(20, 24, 32),
                                                    );
                                                    ui.vertical_centered(|ui| {
                                                        ui.add_space(38.0);
                                                        ui.label(egui::RichText::new("🎬").size(22.0));
                                                        ui.small("Loading preview…");
                                                    });
                                                }
                                            });
                                        });

                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(100, 160, 255),
                                                format!("[{}]", item.category),
                                            );
                                            let title_resp = ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&item.title)
                                                        .strong()
                                                        .size(13.0),
                                                )
                                                .truncate(),
                                            );
                                            title_resp.on_hover_text(&item.title);
                                        });

                                        ui.add_space(2.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                if item.is_video {
                                                    egui::Color32::from_rgb(80, 200, 120)
                                                } else {
                                                    egui::Color32::from_rgb(100, 180, 255)
                                                },
                                                if item.is_video {
                                                    "✔ 60FPS Loop"
                                                } else {
                                                    "Image"
                                                },
                                            );
                                            ui.label("•");
                                            ui.small(&item.resolution);
                                        });

                                        ui.add_space(2.0);
                                        ui.horizontal(|ui| {
                                            ui.small(format!("Source: {}", item.source_platform));
                                            if let Some(size) = item.file_size_bytes {
                                                ui.small(format!("({})", format_file_size(size)));
                                            }
                                        });

                                        ui.add_space(4.0);
                                        let status = self.online_download_status.get(&item.id);
                                        if let Some(status_text) = status {
                                            ui.horizontal(|ui| {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(255, 200, 80),
                                                    status_text,
                                                );
                                                if let Some(cancelled) =
                                                    self.download_cancellations.get(&item.id)
                                                {
                                                    if ui.small_button("Cancel").clicked() {
                                                        cancelled.store(true, Ordering::Relaxed);
                                                    }
                                                }
                                            });
                                        } else if !item.is_video {
                                            ui.add_enabled_ui(false, |ui| {
                                                ui.button("🖼 Image (Video Only)")
                                                    .on_disabled_hover_text("Only video and HTML5 wallpapers are supported.");
                                            });
                                        } else {
                                            ui.horizontal(|ui| {
                                                if ui.button("⬇ Import").clicked() {
                                                    self.start_curated_download(item.clone(), false);
                                                }
                                                if ui.button("▶ Apply").clicked() {
                                                    self.start_curated_download(item.clone(), true);
                                                }
                                            });
                                        }
                                    });
                                });
                        });
                    }
                });
                ui.add_space(8.0);
            }
        });
    }

    /// Tab 2: 📱 Dedicated Vertical / Portrait Shelf (1080x1920)
    pub(super) fn render_vertical_shelf(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(24, 28, 40))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📱 Vertical & Portrait Video Loops (1080x1920)").strong().size(15.0));
                });
                ui.label("Specially formatted loops for vertical secondary monitors, portrait setups, and mobile-aspect screens. 1-Click apply automatically plays seamlessly on portrait displays.");
            });

        ui.add_space(8.0);

        let vertical_items: Vec<wallpaper_core::OnlineWallpaper> =
            wallpaper_core::get_curated_video_wallpapers()
                .into_iter()
                .filter(|i| i.category.contains("Vertical") || i.resolution == "1080x1920")
                .collect();

        let available_width = ui.available_width();
        let card_width = 240.0;
        let card_height = 360.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in vertical_items.chunks(cols) {
                ui.horizontal(|ui| {
                    for item in chunk {
                        ui.allocate_ui(egui::vec2(card_width, card_height), |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 16.0);
                                        ui.set_height(card_height - 16.0);

                                        // Portrait Aspect Thumbnail (9:16)
                                        let thumb_size = egui::vec2(card_width - 16.0, 220.0);
                                        let tex = self.get_or_load_online_thumbnail(&item.thumbnail_url);
                                        ui.allocate_ui(thumb_size, |ui| {
                                            ui.centered_and_justified(|ui| {
                                                if let Some(tex_id) = tex {
                                                    ui.add(
                                                        egui::Image::new((tex_id, thumb_size))
                                                            .max_size(thumb_size)
                                                            .rounding(6.0),
                                                    );
                                                } else {
                                                    ui.painter().rect_filled(
                                                        ui.available_rect_before_wrap(),
                                                        egui::Rounding::same(6.0),
                                                        egui::Color32::from_rgb(20, 24, 32),
                                                    );
                                                    ui.vertical_centered(|ui| {
                                                        ui.add_space(80.0);
                                                        ui.label(egui::RichText::new("📱").size(24.0));
                                                        ui.small("Loading preview…");
                                                    });
                                                }
                                            });
                                        });

                                        ui.add_space(6.0);
                                        ui.colored_label(
                                            egui::Color32::from_rgb(180, 120, 255),
                                            "📱 1080x1920 Portrait",
                                        );
                                        let title_resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&item.title)
                                                    .strong()
                                                    .size(13.0),
                                            )
                                            .truncate(),
                                        );
                                        title_resp.on_hover_text(&item.title);

                                        ui.add_space(2.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(80, 200, 120),
                                                "✔ 60FPS Video",
                                            );
                                            if let Some(size) = item.file_size_bytes {
                                                ui.small(format_file_size(size));
                                            }
                                        });

                                        ui.add_space(6.0);
                                        let status = self.online_download_status.get(&item.id);
                                        if let Some(status_text) = status {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(255, 200, 80),
                                                status_text,
                                            );
                                        } else {
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .button(egui::RichText::new("▶ Set as Portrait").strong())
                                                    .on_hover_text("Download loop and apply directly to vertical monitor")
                                                    .clicked()
                                                {
                                                    self.start_curated_download(item.clone(), true);
                                                }
                                                if ui.button("⬇ Save").clicked() {
                                                    self.start_curated_download(item.clone(), false);
                                                }
                                            });
                                        }
                                    });
                                });
                        });
                    }
                });
                ui.add_space(8.0);
            }
        });
    }

    /// Tab 3: ⚡ Interactive HTML5 / Canvas / Audio Reactive Live Wallpapers
    pub(super) fn render_interactive_widgets(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(22, 30, 42))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("⚡ Built-In Interactive Live Wallpapers (HTML5 / WebGL / Canvas)").strong().size(15.0));
                });
                ui.label("100% self-contained offline apps. Auto-pauses with zero CPU usage when covered by windows. Responsive to system time and audio spectrum.");
            });

        ui.add_space(8.0);

        let widgets = wallpaper_core::get_builtin_widgets();
        let available_width = ui.available_width();
        let card_width = 300.0;
        let card_height = 240.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in widgets.chunks(cols) {
                ui.horizontal(|ui| {
                    for widget in chunk {
                        ui.allocate_ui(egui::vec2(card_width, card_height), |ui| {
                            egui::Frame::group(ui.style())
                                .fill(egui::Color32::from_rgb(20, 24, 34))
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 20.0);
                                        ui.set_height(card_height - 20.0);

                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(56, 189, 248),
                                                &widget.badge,
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.colored_label(
                                                        egui::Color32::from_rgb(148, 163, 184),
                                                        &widget.category,
                                                    );
                                                },
                                            );
                                        });

                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(&widget.title)
                                                .strong()
                                                .size(15.0),
                                        );

                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(&widget.description)
                                                .size(12.0)
                                                .color(egui::Color32::from_rgb(180, 190, 205)),
                                        );

                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(80, 200, 120),
                                                "✔ Zero Background CPU",
                                            );
                                            ui.label("•");
                                            ui.colored_label(
                                                egui::Color32::from_rgb(100, 180, 255),
                                                "✔ Offline",
                                            );
                                        });

                                        ui.add_space(8.0);
                                        ui.horizontal(|ui| {
                                            if ui
                                                .button(egui::RichText::new("▶ Apply Live Desktop").strong())
                                                .on_hover_text("Set this interactive HTML5 wallpaper to your desktop immediately")
                                                .clicked()
                                            {
                                                if let Ok(path) = wallpaper_core::ensure_widget_file(widget) {
                                                    let path_str = path.to_string_lossy().to_string();
                                                    self.pending_apply_source = Some(path_str.clone());
                                                    self.send(
                                                        Command::ImportWallpaper {
                                                            path: path_str,
                                                            title: Some(widget.title.clone()),
                                                        },
                                                        &format!("Importing & applying '{}'…", widget.title),
                                                    );
                                                }
                                            }

                                            if ui
                                                .button("👁 Preview")
                                                .on_hover_text("Open in browser to preview live interactions")
                                                .clicked()
                                            {
                                                if let Ok(path) = wallpaper_core::ensure_widget_file(widget) {
                                                    let path_str = path.to_string_lossy().to_string();
                                                    open_browser_url(&format!("file:///{}", path_str.replace('\\', "/")));
                                                }
                                            }
                                        });
                                    });
                                });
                        });
                    }
                });
                ui.add_space(10.0);
            }
        });
    }

    /// Tab 4: 🛰 24/7 Live Ambient Streams & Webcams
    pub(super) fn render_live_streams(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(26, 24, 38))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🛰 24/7 Live Ambient Streams & Webcams").strong().size(15.0));
                });
                ui.label("Stream real-time high-definition camera feeds from Earth orbit, Tokyo neon rain, and relaxing study beats directly onto your desktop.");
            });

        ui.add_space(8.0);

        let streams = wallpaper_core::get_curated_live_streams();
        let available_width = ui.available_width();
        let card_width = 280.0;
        let card_height = 290.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in streams.chunks(cols) {
                ui.horizontal(|ui| {
                    for stream in chunk {
                        ui.allocate_ui(egui::vec2(card_width, card_height), |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 16.0);
                                        ui.set_height(card_height - 16.0);

                                        // Thumbnail Preview with Live badge overlay
                                        let thumb_size = egui::vec2(card_width - 16.0, 130.0);
                                        let tex = self.get_or_load_online_thumbnail(&stream.thumbnail_url);
                                        ui.allocate_ui(thumb_size, |ui| {
                                            ui.centered_and_justified(|ui| {
                                                if let Some(tex_id) = tex {
                                                    ui.add(
                                                        egui::Image::new((tex_id, thumb_size))
                                                            .max_size(thumb_size)
                                                            .rounding(6.0),
                                                    );
                                                } else {
                                                    ui.painter().rect_filled(
                                                        ui.available_rect_before_wrap(),
                                                        egui::Rounding::same(6.0),
                                                        egui::Color32::from_rgb(20, 24, 32),
                                                    );
                                                    ui.vertical_centered(|ui| {
                                                        ui.add_space(38.0);
                                                        ui.label(egui::RichText::new("🛰").size(22.0));
                                                        ui.small("Loading stream preview…");
                                                    });
                                                }
                                            });
                                        });

                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(255, 75, 75),
                                                "🔴 LIVE",
                                            );
                                            ui.small(format!("📍 {}", stream.location));
                                        });

                                        ui.add_space(2.0);
                                        let title_resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&stream.title)
                                                    .strong()
                                                    .size(13.0),
                                            )
                                            .truncate(),
                                        );
                                        title_resp.on_hover_text(&stream.title);

                                        ui.add_space(2.0);
                                        ui.label(
                                            egui::RichText::new(&stream.description)
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(160, 170, 185)),
                                        );

                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            if ui
                                                .button(egui::RichText::new("▶ Apply Stream").strong())
                                                .on_hover_text("Embed this live 24/7 camera stream directly onto desktop background")
                                                .clicked()
                                            {
                                                if let Ok(path) = wallpaper_core::ensure_stream_file(stream) {
                                                    let path_str = path.to_string_lossy().to_string();
                                                    self.pending_apply_source = Some(path_str.clone());
                                                    self.send(
                                                        Command::ImportWallpaper {
                                                            path: path_str,
                                                            title: Some(stream.title.clone()),
                                                        },
                                                        &format!("Setting live stream '{}'…", stream.title),
                                                    );
                                                }
                                            }

                                            if ui
                                                .button("🌐 Open")
                                                .on_hover_text("Open stream directly in browser")
                                                .clicked()
                                            {
                                                open_browser_url(&stream.stream_url);
                                            }
                                        });
                                    });
                                });
                        });
                    }
                });
                ui.add_space(8.0);
            }
        });
    }

    /// Tab 5: 🌐 Web Wallpaper Hub & Browser Extension Launchers
    pub(super) fn render_web_hub(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        // Extension Status Banner
        let ext_registered = snapshot.extension_status.registered
            || wallpaper_windows::is_browser_extension_registered();

        egui::Frame::group(ui.style())
            .fill(if ext_registered {
                egui::Color32::from_rgb(20, 36, 32)
            } else {
                egui::Color32::from_rgb(38, 30, 20)
            })
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ext_registered {
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 220, 140),
                            "✔ Browser Extension Active & Paired",
                        );
                        ui.label("Right-click any web video, GIF, or image in Chrome/Edge to set as desktop wallpaper!");
                    } else {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 180, 60),
                            "⚠ Browser Extension Not Yet Paired",
                        );
                        ui.label("Pair the extension to capture wallpapers directly from your browser.");
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🧩 Extension Settings").clicked() {
                            self.page = Page::Extension;
                        }
                        if ui
                            .button("🌐 Launch Browser with Extension")
                            .on_hover_text("Opens Chromium browser with RustWallpaper extension automatically loaded")
                            .clicked()
                        {
                            let _ = wallpaper_windows::launch_browser_with_extension(None);
                        }
                    });
                });
            });

        ui.add_space(8.0);

        // Direct Web Video URL Downloader bar
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(26, 32, 42))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📥 Direct Web Video / Stream Downloader:").strong());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.direct_url_input)
                            .hint_text("Paste any video link (YouTube, Reddit, Pinterest, or .mp4/.webm URL)")
                            .desired_width(340.0),
                    );
                    let has_url = self.direct_url_input.trim().starts_with("http://")
                        || self.direct_url_input.trim().starts_with("https://");

                    if ui
                        .add_enabled(has_url, egui::Button::new("⬇ Download"))
                        .on_hover_text("Download video and save to your wallpaper library")
                        .clicked()
                    {
                        self.start_direct_url_download(false);
                    }

                    if ui
                        .add_enabled(has_url, egui::Button::new("▶ Download & Apply"))
                        .on_hover_text(
                            "Download video, add to library, and apply directly to desktop",
                        )
                        .clicked()
                    {
                        self.start_direct_url_download(true);
                    }

                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && has_url
                    {
                        self.start_direct_url_download(true);
                    }
                });

                if let Some(status) = &self.direct_url_status {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::from_rgb(100, 200, 255), status);
                        if let Some(id) = &self.direct_download_id {
                            if let Some(cancelled) = self.download_cancellations.get(id) {
                                if ui.small_button("Cancel").clicked() {
                                    cancelled.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                    });
                }
            });

        ui.add_space(10.0);

        // 1-Click Discovery Launchers
        ui.label(
            egui::RichText::new("🌟 1-Click Wallpaper Discovery Hub")
                .strong()
                .size(14.0),
        );
        ui.label("Launch curated wallpaper channels in your browser with the extension ready to capture:");
        ui.add_space(6.0);

        let launchers = [
            (
                "📌 Pinterest Video Wallpapers",
                "Endless scrolling loop aesthetics, cyberpunk art, and cozy rooms.",
                "https://www.pinterest.com/search/pins/?q=live+wallpapers+loop",
                egui::Color32::from_rgb(230, 0, 35),
            ),
            (
                "▶ YouTube 4K Ambient Loops",
                "Relaxing 4K nature loops, sci-fi city flyovers, and cozy fireplaces.",
                "https://www.youtube.com/results?search_query=4k+relaxing+loop+wallpaper",
                egui::Color32::from_rgb(255, 0, 0),
            ),
            (
                "👾 Reddit /r/Cinemagraphs",
                "Subtle living photographs with seamless repeating motion.",
                "https://www.reddit.com/r/Cinemagraphs/",
                egui::Color32::from_rgb(255, 69, 0),
            ),
            (
                "🎵 TikTok Live Wallpapers",
                "Trending mobile video wallpapers, gaming highlights, and anime edits.",
                "https://www.tiktok.com/tag/livewallpaper",
                egui::Color32::from_rgb(0, 242, 234),
            ),
            (
                "🌌 Wallpaper Cave 4K",
                "High-res community desktop wallpapers in crisp Ultra-HD resolutions.",
                "https://wallpapercave.com/",
                egui::Color32::from_rgb(100, 160, 255),
            ),
            (
                "🌊 Pexels Free 4K Loops",
                "Royalty-free high quality nature, drone, and city video loops.",
                "https://www.pexels.com/search/videos/loop/",
                egui::Color32::from_rgb(0, 160, 130),
            ),
        ];

        let available_width = ui.available_width();
        let card_width = 240.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        for chunk in launchers.chunks(cols) {
            ui.horizontal(|ui| {
                for (name, desc, url, color) in chunk {
                    ui.allocate_ui(egui::vec2(card_width, 110.0), |ui| {
                        egui::Frame::group(ui.style())
                            .fill(egui::Color32::from_rgb(20, 24, 32))
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(card_width - 16.0);
                                    ui.colored_label(*color, egui::RichText::new(*name).strong());
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new(*desc)
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(160, 170, 185)),
                                    );
                                    ui.add_space(4.0);
                                    if ui
                                        .button("Open Portal ↗")
                                        .on_hover_text(
                                            "Open website in browser with extension loaded",
                                        )
                                        .clicked()
                                    {
                                        open_browser_url(url);
                                    }
                                });
                            });
                    });
                }
            });
            ui.add_space(6.0);
        }

        ui.add_space(10.0);

        // Recent Downloads Feed
        let local_app_data = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
        let downloads_dir = std::path::PathBuf::from(local_app_data)
            .join("RustWallpaper")
            .join("downloads");

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("📁 Recent Downloads & Browser Captures")
                    .strong()
                    .size(14.0),
            );
            if ui.button("📂 Open Downloads Folder").clicked() {
                open_folder_path(&downloads_dir);
            }
        });

        if let Ok(entries) = std::fs::read_dir(&downloads_dir) {
            let mut recent_files: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && !p.extension().map_or(false, |ext| ext == "download"))
                .collect();

            recent_files.sort_by_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            recent_files.reverse();

            if recent_files.is_empty() {
                ui.small("No downloaded files yet. Capture videos from your browser or paste a link above.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for path in recent_files.iter().take(6) {
                            ui.horizontal(|ui| {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                ui.label(format!("🎬 {name}"));
                                if let Ok(meta) = std::fs::metadata(path) {
                                    ui.small(format_file_size(meta.len()));
                                }
                                if ui.button("▶ Apply").clicked() {
                                    let path_str = path.to_string_lossy().to_string();
                                    self.pending_apply_source = Some(path_str.clone());
                                    self.send(
                                        Command::ImportWallpaper {
                                            path: path_str,
                                            title: Some(name.to_string()),
                                        },
                                        &format!("Applying '{name}'…"),
                                    );
                                }
                            });
                        }
                    });
            }
        }
    }

    /// Tab 6: 🎮 Steam Workshop Hub (Live Workshop Browser & Installed Importer)
    pub(super) fn render_local_workshop(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let mut import_req: Option<(String, String, bool)> = None;
        let mut apply_req: Option<WallpaperId> = None;

        // Subtab Navigation Bar
        ui.horizontal(|ui| {
            let tabs = [
                (
                    SteamWorkshopSubtab::LiveWorkshop,
                    "🌐 Live Steam Workshop (Browse & Subscribe)",
                ),
                (
                    SteamWorkshopSubtab::InstalledOnDisk,
                    "📂 Installed by Steam (On Disk)",
                ),
            ];
            for (tab, label) in tabs {
                let is_active = self.steam_workshop_subtab == tab;
                let bg = if is_active {
                    egui::Color32::from_rgb(45, 90, 160)
                } else if self.theme.dark_mode {
                    egui::Color32::from_rgb(30, 34, 45)
                } else {
                    egui::Color32::from_rgb(220, 226, 238)
                };
                let text_color = if is_active {
                    egui::Color32::WHITE
                } else if self.theme.dark_mode {
                    egui::Color32::from_rgb(170, 180, 200)
                } else {
                    egui::Color32::from_rgb(60, 70, 90)
                };

                let resp = egui::Frame::none()
                    .fill(bg)
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 7.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .size(13.0)
                                .color(text_color)
                                .strong(),
                        );
                    });
                if resp.response.interact(egui::Sense::click()).clicked() {
                    self.steam_workshop_subtab = tab;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.workshop_scanning {
                    ui.spinner();
                    ui.small("Scanning…");
                } else if ui.button("⟳ Scan Installed Disk Items").clicked() {
                    self.request_workshop_scan();
                    self.notice = "Scanning local Steam Workshop folders…".into();
                }
            });
        });

        ui.add_space(8.0);

        match self.steam_workshop_subtab {
            SteamWorkshopSubtab::LiveWorkshop => {
                // Auto-refresh local disk detection every 5 seconds while LiveWorkshop is open
                if !self.workshop_scanning
                    && self
                        .last_workshop_auto_scan
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
                {
                    self.request_workshop_scan();
                }
                // Info Banner
                egui::Frame::none()
                    .fill(if self.theme.dark_mode {
                        egui::Color32::from_rgb(18, 25, 38)
                    } else {
                        egui::Color32::from_rgb(235, 242, 252)
                    })
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if self.theme.dark_mode {
                            egui::Color32::from_rgb(45, 65, 95)
                        } else {
                            egui::Color32::from_rgb(190, 210, 235)
                        },
                    ))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("💡 1-Click Steam Community Integration:")
                                    .strong()
                                    .color(egui::Color32::from_rgb(100, 190, 255)),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Browse live items from Wallpaper Engine's Steam Workshop (AppID 431960). Click '➕ Subscribe in Steam' to launch Steam and subscribe. Once Steam downloads the item to your disk, RustWallpaper automatically detects it with '✔ Installed by Steam' and lets you apply it instantly!"
                                )
                                .size(12.0)
                                .color(if self.theme.dark_mode {
                                    egui::Color32::from_rgb(180, 195, 215)
                                } else {
                                    egui::Color32::from_rgb(50, 60, 75)
                                }),
                            );
                        });
                    });

                ui.add_space(8.0);

                // Live Workshop Search & Filter Controls
                let mut do_search = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🔍 Search:").strong());
                    let search_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.steam_online_search)
                            .hint_text("Search Steam Workshop (e.g. Cyberpunk, Anime, Nature)…")
                            .desired_width(260.0),
                    );
                    if search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        do_search = true;
                    }

                    ui.add_space(8.0);
                    ui.label("Sort:");
                    let prev_sort = self.steam_online_sort.clone();
                    egui::ComboBox::from_id_salt("steam_workshop_sort_combo")
                        .selected_text(&self.steam_online_sort)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.steam_online_sort,
                                "Trending".to_string(),
                                "🔥 Trending",
                            );
                            ui.selectable_value(
                                &mut self.steam_online_sort,
                                "Most Subscribed".to_string(),
                                "⭐ Most Subscribed",
                            );
                            ui.selectable_value(
                                &mut self.steam_online_sort,
                                "Top Rated".to_string(),
                                "🏆 Top Rated",
                            );
                            ui.selectable_value(
                                &mut self.steam_online_sort,
                                "Recent".to_string(),
                                "🕐 Recent",
                            );
                        });
                    if self.steam_online_sort != prev_sort {
                        do_search = true;
                    }

                    if ui.button("🔍 Search Steam").clicked() {
                        do_search = true;
                    }

                    if ui.button("🌟 Curated Showcase").clicked() {
                        self.steam_online_search.clear();
                        self.steam_online_sort = "Trending".to_string();
                        self.steam_online_items =
                            wallpaper_core::get_curated_steam_workshop_items();
                        self.notice = format!(
                            "Loaded {} curated Steam Workshop favorites",
                            self.steam_online_items.len()
                        );
                    }

                    if self.steam_online_searching {
                        ui.add_space(6.0);
                        ui.spinner();
                        ui.colored_label(
                            egui::Color32::from_rgb(100, 180, 255),
                            "Querying Steam Community Workshop…",
                        );
                    }
                });

                if do_search && !self.steam_online_searching {
                    let key = (
                        self.steam_online_search.trim().to_lowercase(),
                        self.steam_online_sort.clone(),
                    );
                    if let Some(cached) = self.steam_search_cache.get(&key) {
                        self.steam_online_items = cached.clone();
                        self.notice = format!(
                            "Showing {} cached Steam Workshop items",
                            self.steam_online_items.len()
                        );
                    } else {
                        self.steam_online_searching = true;
                        self.steam_online_search_id += 1;
                        let sort_param = match self.steam_online_sort.as_str() {
                            "Most Subscribed" => "popular",
                            "Top Rated" => "toprated",
                            "Recent" => "recent",
                            _ => "trend",
                        };
                        let _ = self.steam_search_tx.send(SteamSearchRequest {
                            search_id: self.steam_online_search_id,
                            query: self.steam_online_search.clone(),
                            sort: sort_param.to_string(),
                        });
                        self.notice = "Contacting Steam Community Workshop…".into();
                    }
                }

                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Showing {} workshop wallpapers",
                        self.steam_online_items.len()
                    ))
                    .small()
                    .color(egui::Color32::from_rgb(140, 150, 170)),
                );
                ui.add_space(4.0);

                let available_width = ui.available_width();
                let card_width = 240.0;
                let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);
                let steam_items = self.steam_online_items.clone();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for chunk in steam_items.chunks(cols) {
                        ui.horizontal(|ui| {
                            for item in chunk {
                                ui.allocate_ui(egui::vec2(card_width, 248.0), |ui| {
                                    egui::Frame::group(ui.style())
                                        .inner_margin(8.0)
                                        .show(ui, |ui| {
                                            ui.vertical(|ui| {
                                                ui.set_width(card_width - 16.0);
                                                ui.set_height(232.0);

                                                // Thumbnail preview
                                                let thumb_size =
                                                    egui::vec2(card_width - 16.0, 122.0);
                                                let tex = self.get_or_load_online_thumbnail(
                                                    &item.thumbnail_url,
                                                );
                                                ui.allocate_ui(thumb_size, |ui| {
                                                    ui.centered_and_justified(|ui| {
                                                        if let Some(tex_id) = tex {
                                                            ui.add(egui::Image::new((
                                                                tex_id, thumb_size,
                                                            )));
                                                        } else {
                                                            ui.small("Loading preview…");
                                                        }
                                                    });
                                                });

                                                ui.add_space(4.0);
                                                let title_resp = ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&item.title).strong(),
                                                    )
                                                    .truncate(),
                                                );
                                                title_resp.on_hover_text(&item.title);

                                                ui.small(
                                                    egui::RichText::new(format!(
                                                        "Workshop ID: {}",
                                                        item.id
                                                    ))
                                                    .color(egui::Color32::from_rgb(140, 150, 170)),
                                                );

                                                // Real-time disk detection: Is item downloaded in local Steam workshop content folder?
                                                let installed_item = self
                                                    .workshop_items
                                                    .iter()
                                                    .find(|w| w.id == item.id);
                                                let in_library_wp =
                                                    installed_item.and_then(|inst| {
                                                        snapshot.wallpapers.iter().find(|lib_wp| {
                                                            lib_wp.source()
                                                                == inst.file_path.to_string_lossy()
                                                                || std::path::Path::new(
                                                                    lib_wp.source(),
                                                                ) == inst.file_path
                                                        })
                                                    });

                                                ui.add_space(4.0);
                                                if let Some(installed) = installed_item {
                                                    ui.horizontal(|ui| {
                                                        ui.colored_label(
                                                            egui::Color32::from_rgb(80, 230, 130),
                                                            "✔ Installed by Steam",
                                                        );
                                                    });
                                                    ui.horizontal(|ui| {
                                                        if let Some(wp) = in_library_wp {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    100, 180, 255,
                                                                ),
                                                                "✔ In Library",
                                                            );
                                                            if ui.button("▶ Apply").clicked() {
                                                                apply_req = Some(wp.id);
                                                            }
                                                        } else if installed.is_compatible_video {
                                                            if ui
                                                                .button("▶ 1-Click Apply")
                                                                .clicked()
                                                            {
                                                                import_req = Some((
                                                                    installed
                                                                        .file_path
                                                                        .to_string_lossy()
                                                                        .to_string(),
                                                                    installed.title.clone(),
                                                                    true,
                                                                ));
                                                            }
                                                            if ui.button("⬇ Import").clicked() {
                                                                import_req = Some((
                                                                    installed
                                                                        .file_path
                                                                        .to_string_lossy()
                                                                        .to_string(),
                                                                    installed.title.clone(),
                                                                    false,
                                                                ));
                                                            }
                                                        } else {
                                                            ui.small("(Scene/Web type)");
                                                        }
                                                    });
                                                } else {
                                                    ui.horizontal(|ui| {
                                                        if ui
                                                            .button(
                                                                egui::RichText::new(
                                                                    "➕ Subscribe in Steam",
                                                                )
                                                                .color(egui::Color32::from_rgb(
                                                                    120, 210, 255,
                                                                ))
                                                                .strong(),
                                                            )
                                                            .on_hover_text(
                                                                "Opens Steam directly to subscribe and download this wallpaper",
                                                            )
                                                            .clicked()
                                                        {
                                                            open_browser_url(&item.steam_url);
                                                            self.notice = format!(
                                                                "Opened Steam for '{}' (ID: {}). Click 'Subscribe' in Steam to download!",
                                                                item.title, item.id
                                                            );
                                                        }

                                                        if ui
                                                            .button("🌐 Web")
                                                            .on_hover_text(
                                                                "View workshop item page in web browser",
                                                            )
                                                            .clicked()
                                                        {
                                                            open_browser_url(&item.web_url);
                                                        }
                                                    });
                                                }
                                            });
                                        });
                                });
                            }
                        });
                        ui.add_space(8.0);
                    }
                });
            }

            SteamWorkshopSubtab::InstalledOnDisk => {
                ui.horizontal(|ui| {
                    ui.label("🔍 Search:");
                    ui.text_edit_singleline(&mut self.workshop_search);
                    if ui.button("⟳ Refresh / Scan").clicked() {
                        self.request_workshop_scan();
                        self.notice = "Scanning local workshop folders…".into();
                    }
                    ui.checkbox(
                        &mut self.workshop_filter_video_only,
                        "Playable videos only (.mp4, .webm)",
                    );
                });

                ui.horizontal(|ui| {
                    ui.small("Folder to scan (Steam Workshop, Videos, or custom folder):");
                    ui.text_edit_singleline(&mut self.custom_steam_path);
                    if ui.button("📁 Browse Folder…").clicked() {
                        #[cfg(windows)]
                        {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                self.custom_steam_path = folder.display().to_string();
                                self.request_workshop_scan();
                                self.notice = "Scanning selected folder…".into();
                            }
                        }
                    }
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
                        ui.add_space(30.0);
                        ui.heading("No video wallpapers found in folder");
                        ui.label("Click '📁 Browse Folder…' to select any folder containing .mp4 or .webm files,");
                        ui.label("or switch to '🌐 Live Steam Workshop' to browse and subscribe directly in Steam!");
                        ui.add_space(8.0);
                        if ui.button("🌐 Switch to Live Steam Workshop").clicked() {
                            self.steam_workshop_subtab = SteamWorkshopSubtab::LiveWorkshop;
                        }
                    });
                } else {
                    ui.label(format!("Showing {} detected items on disk", filtered.len()));
                    ui.add_space(4.0);

                    let available_width = ui.available_width();
                    let card_width = 240.0;
                    let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for chunk in filtered.chunks(cols) {
                            ui.horizontal(|ui| {
                                for item in chunk {
                                    ui.allocate_ui(egui::vec2(card_width, 224.0), |ui| {
                                        egui::Frame::group(ui.style()).inner_margin(8.0).show(
                                            ui,
                                            |ui| {
                                                ui.vertical(|ui| {
                                                    ui.set_width(card_width - 16.0);
                                                    ui.set_height(208.0);

                                                    // Preview thumbnail
                                                    let thumb_size =
                                                        egui::vec2(card_width - 16.0, 110.0);
                                                    let tex = item
                                                        .preview_path
                                                        .as_deref()
                                                        .and_then(|p| self.get_or_load_texture(p));

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
                                                    let title_resp = ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(&item.title)
                                                                .strong(),
                                                        )
                                                        .truncate(),
                                                    );
                                                    title_resp.on_hover_text(&item.title);

                                                    ui.add_space(2.0);
                                                    ui.horizontal(|ui| {
                                                        if item.is_compatible_video {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    80, 200, 120,
                                                                ),
                                                                "✔ Video",
                                                            );
                                                        } else {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    180, 180, 180,
                                                                ),
                                                                &item.item_type,
                                                            );
                                                        }
                                                        if let Some(size) = item.file_size_bytes {
                                                            ui.small(format_file_size(size));
                                                        }
                                                    });

                                                    let in_library_wp =
                                                        snapshot.wallpapers.iter().find(|w| {
                                                            w.source()
                                                                == item.file_path.to_string_lossy()
                                                                || std::path::Path::new(w.source())
                                                                    == item.file_path
                                                        });

                                                    ui.add_space(4.0);
                                                    if let Some(wp) = in_library_wp {
                                                        ui.horizontal(|ui| {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    100, 180, 255,
                                                                ),
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
                                            },
                                        );
                                    });
                                }
                            });
                            ui.add_space(8.0);
                        }
                    });
                }
            }
        }

        if let Some((path, title, auto_apply)) = import_req {
            if auto_apply {
                self.pending_apply_source = Some(path.clone());
                self.pending_apply_download_id = None;
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
}
