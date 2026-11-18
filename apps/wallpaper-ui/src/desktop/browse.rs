use super::*;

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

        // Auto-scan local items on first visit
        if !self.workshop_scanned {
            let custom = if !self.custom_steam_path.trim().is_empty() {
                vec![std::path::PathBuf::from(self.custom_steam_path.trim())]
            } else {
                Vec::new()
            };
            self.workshop_items = wallpaper_core::scan_wallpaper_engine_items(&custom);
            self.workshop_scanned = true;
        }

        // Auto-load curated video wallpapers
        if self.online_items.is_empty() {
            self.online_items = wallpaper_core::get_curated_video_wallpapers();
        }

        ui.heading("Browse Wallpapers");
        ui.add_space(4.0);

        // Sub-tabs
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    matches!(self.browse_tab, BrowseTab::OnlineWallpapers),
                    "🌐  Free Video Wallpapers",
                )
                .clicked()
            {
                self.browse_tab = BrowseTab::OnlineWallpapers;
            }
            if ui
                .selectable_label(
                    matches!(self.browse_tab, BrowseTab::LocalWorkshop),
                    "📂  Local & Workshop Folder",
                )
                .clicked()
            {
                self.browse_tab = BrowseTab::LocalWorkshop;
            }
        });

        ui.add_space(6.0);

        // Direct Web Video URL Downloader bar
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(26, 32, 42))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📥 Download from Web URL:").strong());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.direct_url_input)
                            .hint_text("Paste any video link (https://.../video.mp4 or .webm)")
                            .desired_width(320.0),
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

        ui.separator();
        ui.add_space(4.0);

        match self.browse_tab {
            BrowseTab::OnlineWallpapers => self.render_online_wallpapers(ui, snapshot),
            BrowseTab::LocalWorkshop => self.render_local_workshop(ui, snapshot),
        }
    }
    pub(super) fn render_local_workshop(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
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
                self.notice = format!("Found {} video wallpapers", self.workshop_items.len());
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
                        let custom = vec![folder];
                        self.workshop_items = wallpaper_core::scan_wallpaper_engine_items(&custom);
                        self.workshop_scanned = true;
                        self.notice =
                            format!("Found {} items in folder", self.workshop_items.len());
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
                        ui.label("paste a web video link into the URL bar above, or switch to 'Free Video Wallpapers'.");
                        ui.add_space(8.0);
                        if ui.button("🌐 Switch to Free Video Wallpapers").clicked() {
                            self.browse_tab = BrowseTab::OnlineWallpapers;
                        }
                    });
            return;
        }

        ui.label(format!("Showing {} detected items", filtered.len()));
        ui.add_space(4.0);

        let available_width = ui.available_width();
        let card_width = 240.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);

        let mut import_req: Option<(String, String, bool)> = None;
        let mut apply_req: Option<WallpaperId> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in filtered.chunks(cols) {
                ui.horizontal(|ui| {
                    for item in chunk {
                        ui.allocate_ui(egui::vec2(card_width, 224.0), |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 16.0);
                                        ui.set_height(208.0);

                                        // Preview thumbnail
                                        let thumb_size = egui::vec2(card_width - 16.0, 110.0);
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
                                                egui::RichText::new(&item.title).strong(),
                                            )
                                            .truncate(),
                                        );
                                        title_resp.on_hover_text(&item.title);

                                        ui.add_space(2.0);
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
                        });
                    }
                });
                ui.add_space(8.0);
            }
        });

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
    pub(super) fn render_online_wallpapers(&mut self, ui: &mut egui::Ui, _snapshot: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label("🔍 Search:");
            let text_resp = ui.text_edit_singleline(&mut self.online_search_query);
            let hit_enter = text_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            egui::ComboBox::from_id_salt("online_cat")
                .selected_text(&self.online_category)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.online_category,
                        "Videos (Free Loops)".into(),
                        "Videos (Free Loops)",
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
        let card_width = 280.0;
        let cols = ((available_width / (card_width + 16.0)).floor() as usize).max(1);
        let items = self.online_items.clone();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for chunk in items.chunks(cols) {
                ui.horizontal(|ui| {
                    for item in chunk {
                        ui.allocate_ui(egui::vec2(card_width, 145.0), |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(card_width - 20.0);
                                        ui.set_min_height(125.0);

                                        // Category color header bar
                                        let header_color = match item.category.as_str() {
                                            "Animation" => egui::Color32::from_rgb(80, 60, 160),
                                            "Sci-Fi" => egui::Color32::from_rgb(40, 100, 160),
                                            "Urban" | "Motion" => egui::Color32::from_rgb(50, 120, 80),
                                            "Nature" => egui::Color32::from_rgb(40, 130, 60),
                                            "Space" => egui::Color32::from_rgb(20, 40, 100),
                                            _ => egui::Color32::from_rgb(60, 60, 80),
                                        };
                                        egui::Frame::none()
                                            .fill(header_color)
                                            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                            .show(ui, |ui| {
                                                ui.set_width(card_width - 20.0);
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(220, 220, 240),
                                                    egui::RichText::new(&item.category).small(),
                                                );
                                            });
                                        ui.add_space(4.0);

                                        let title_resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&item.title)
                                                    .strong()
                                                    .size(13.0),
                                            )
                                            .truncate(),
                                        );
                                        title_resp.on_hover_text(&item.title);

                                        ui.add_space(3.0);
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                if item.is_video {
                                                    egui::Color32::from_rgb(80, 200, 120)
                                                } else {
                                                    egui::Color32::from_rgb(100, 180, 255)
                                                },
                                                if item.is_video {
                                                    "✔ Video Loop"
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

                                        ui.add_space(6.0);
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
                                                    .on_disabled_hover_text("Static image wallpapers are not supported by the video playback engine yet. Only video wallpapers (.mp4, .webm) are supported.");
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
}
