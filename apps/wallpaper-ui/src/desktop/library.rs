use super::*;

fn wallpaper_playing_monitors<'a>(snapshot: &'a Snapshot, source: &str) -> Vec<&'a str> {
    let target_path = std::path::Path::new(source);
    snapshot
        .players
        .iter()
        .filter(|player| {
            player.source.as_deref().is_some_and(|src| {
                src.eq_ignore_ascii_case(source) || std::path::Path::new(src) == target_path
            })
        })
        .map(|p| p.monitor.as_str())
        .collect()
}

impl DesktopApp {
    pub(super) fn render_library(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Wallpaper library");
        ui.label("Import local videos, filter, and assign them to your displays.");
        ui.add_space(8.0);

        // Subtabs: Wallpapers vs Playlists
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    self.library_subtab == LibrarySubtab::Wallpapers,
                    format!("🖼 All Wallpapers ({})", snapshot.wallpapers.len()),
                )
                .clicked()
            {
                self.library_subtab = LibrarySubtab::Wallpapers;
            }
            if ui
                .selectable_label(
                    self.library_subtab == LibrarySubtab::Playlists,
                    format!("📋 Playlists ({})", snapshot.playlists.len()),
                )
                .clicked()
            {
                self.library_subtab = LibrarySubtab::Playlists;
            }
        });

        if self.library_subtab == LibrarySubtab::Playlists {
            self.render_playlists_subtab(ui, snapshot);
            return;
        }

        // Active Playlist Banner if currently playing
        if let Some(ref pl_id) = self.active_playlist_id {
            if let Some(pl) = snapshot.playlists.iter().find(|p| &p.id == pl_id) {
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(30, 45, 60))
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 220, 150),
                                "▶ Active Playlist Playing:",
                            );
                            ui.strong(&pl.name);
                            ui.small(format!(
                                "({} wallpapers • rotates every {}m)",
                                pl.wallpaper_ids.len(),
                                pl.interval_secs / 60
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("⏹ Stop Playlist").clicked() {
                                        self.active_playlist_id = None;
                                        self.rotation_dirty_until =
                                            Some(Instant::now() + Duration::from_secs(2));
                                        self.send(
                                            Command::SetActivePlaylist { playlist_id: None },
                                            "Playlist stopped.",
                                        );
                                    }
                                    if ui.button("⏭ Next Wallpaper").clicked() {
                                        self.cycle_playlist_wallpaper(snapshot);
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(4.0);
            }
        }

        // Import Box
        card_frame(&self.theme).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Import:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_path)
                        .hint_text(r"Full path, or drop/select multiple files…"),
                );
                if ui.button("Browse…").clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter(
                            "Media & Web Wallpapers",
                            &[
                                "mp4", "webm", "mkv", "mov", "gif", "jpg", "jpeg", "png", "webp",
                                "bmp", "html", "htm",
                            ],
                        )
                        .pick_files()
                    {
                        if paths.len() == 1 {
                            self.import_path = paths[0].display().to_string();
                        } else if !paths.is_empty() && snapshot.connected {
                            let count = paths.len();
                            for p in paths {
                                let _ = self.commands.send(Command::ImportWallpaper {
                                    path: p.display().to_string(),
                                    title: None,
                                });
                            }
                            self.notice = format!("Queued {count} wallpapers for batch import…");
                        }
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
        subtle_frame(&self.theme).show(ui, |ui| {
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
                    && self.view_mode != ViewMode::Grid
                {
                    self.view_mode = ViewMode::Grid;
                    self.save_prefs();
                }
                if ui
                    .selectable_label(self.view_mode == ViewMode::List, "☰ List")
                    .clicked()
                    && self.view_mode != ViewMode::List
                {
                    self.view_mode = ViewMode::List;
                    self.save_prefs();
                }

                ui.separator();

                // Thumbnail scale
                if self.view_mode == ViewMode::Grid {
                    let prev_scale = self.thumb_scale;
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
                    if self.thumb_scale != prev_scale {
                        self.save_prefs();
                    }
                }
            });
        });

        ui.add_space(4.0);

        // Row 2: Sort and Filters
        subtle_frame(&self.theme).show(ui, |ui| {
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

                // Orientation filter
                let orient_text = match self.filter_orientation {
                    FilterOrientation::All => "All Orientations",
                    FilterOrientation::Horizontal => "🖥 Horizontal (Landscape)",
                    FilterOrientation::Vertical => "📱 Vertical (Portrait)",
                };
                egui::ComboBox::from_id_salt("orient_filter_combo")
                    .selected_text(orient_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.filter_orientation,
                            FilterOrientation::All,
                            "All Orientations",
                        );
                        ui.selectable_value(
                            &mut self.filter_orientation,
                            FilterOrientation::Horizontal,
                            "🖥 Horizontal (Landscape)",
                        );
                        ui.selectable_value(
                            &mut self.filter_orientation,
                            FilterOrientation::Vertical,
                            "📱 Vertical (Portrait)",
                        );
                    });

                if ui
                    .button("🔄 Turn Horizontal to Vertical")
                    .on_hover_text(
                        "Rotate horizontal wallpapers in your library into vertical (90° portrait)",
                    )
                    .clicked()
                {
                    for wp in &snapshot.wallpapers {
                        if wp.is_effective_horizontal() {
                            self.send(
                                Command::SetWallpaperRotation {
                                    wallpaper_id: wp.id,
                                    rotation: 90,
                                },
                                "Converted to vertical",
                            );
                        }
                    }
                }

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

                ui.toggle_value(&mut self.query.favorites_only, "⭐ Favorites")
                    .on_hover_text("Show only favorited wallpapers");

                if self.query.is_filtered() && ui.button("✖ Reset Filters").clicked() {
                    self.query.reset_filters();
                }
            });
        });

        ui.add_space(6.0);

        // Filtered results
        let filtered: Vec<&Wallpaper> = self
            .query
            .apply(&snapshot.wallpapers)
            .into_iter()
            .filter(|wp| match self.filter_orientation {
                FilterOrientation::All => true,
                FilterOrientation::Horizontal => wp.is_effective_horizontal(),
                FilterOrientation::Vertical => wp.is_effective_vertical(),
            })
            .collect();

        // Multi-Selection and Bulk Actions Bar
        ui.horizontal(|ui| {
            ui.small(format!(
                "Showing {} of {} wallpapers",
                filtered.len(),
                snapshot.wallpapers.len()
            ));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.selected_wallpapers.is_empty() {
                    let sel_count = self.selected_wallpapers.len();

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

                    if ui
                        .button(format!("📋 Create Playlist ({sel_count})"))
                        .on_hover_text("Create an auto-cycling playlist from these wallpapers")
                        .clicked()
                    {
                        let ids: Vec<WallpaperId> =
                            self.selected_wallpapers.iter().copied().collect();
                        let name = format!("Playlist ({sel_count} items)");
                        let pl = wallpaper_core::WallpaperPlaylist::new(
                            name,
                            ids,
                            (self.new_playlist_interval_mins as u64) * 60,
                            self.new_playlist_shuffle,
                        );
                        let pl_id = pl.id.clone();
                        self.send(Command::SavePlaylist { playlist: pl }, "Playlist created!");
                        self.active_playlist_id = Some(pl_id.clone());
                        self.active_playlist_index = 0;
                        self.rotation_dirty_until = Some(Instant::now() + Duration::from_secs(2));
                        self.send(
                            Command::SetActivePlaylist {
                                playlist_id: Some(pl_id),
                            },
                            &format!(
                                "Created and started playlist with {sel_count} wallpapers (cycles every {}m)",
                                self.new_playlist_interval_mins
                            ),
                        );
                        self.selected_wallpapers.clear();
                    }

                    if ui
                        .button("↕ Make Vertical (90°)")
                        .on_hover_text("Rotate all selected wallpapers into vertical (portrait) orientation")
                        .clicked()
                    {
                        let to_rotate: Vec<WallpaperId> =
                            self.selected_wallpapers.iter().copied().collect();
                        for id in to_rotate {
                            self.send(
                                Command::SetWallpaperRotation {
                                    wallpaper_id: id,
                                    rotation: 90,
                                },
                                "Rotating to vertical…",
                            );
                        }
                    }

                    if ui
                        .button("↔ Make Horizontal (0°)")
                        .on_hover_text("Reset all selected wallpapers to horizontal (landscape) orientation")
                        .clicked()
                    {
                        let to_rotate: Vec<WallpaperId> =
                            self.selected_wallpapers.iter().copied().collect();
                        for id in to_rotate {
                            self.send(
                                Command::SetWallpaperRotation {
                                    wallpaper_id: id,
                                    rotation: 0,
                                },
                                "Resetting to horizontal…",
                            );
                        }
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
        let mut select_req: Option<Option<WallpaperId>> = None;
        let mut retry_req = None;
        let mut remove_prompt_req: Option<(WallpaperId, String)> = None;
        let mut fav_req: Option<(WallpaperId, bool)> = None;
        let mut toggle_select_req = None;
        let mut rotate_req = None;
        let mut benchmark_req: Option<WallpaperId> = None;

        // Side Inspector Panel (Wallpaper Engine style)
        if self.selected_wallpaper.is_none() {
            if let Some(first) = snapshot.wallpapers.first() {
                self.selected_wallpaper = Some(first.id);
            }
        }

        if let Some(selected_id) = self.selected_wallpaper {
            if let Some(wp) = snapshot.wallpapers.iter().find(|w| w.id == selected_id) {
                let wp = wp.clone();
                egui::SidePanel::right("inspector_drawer")
                    .resizable(true)
                    .default_width(310.0)
                    .min_width(260.0)
                    .max_width(420.0)
                    .frame(
                        egui::Frame::none()
                            .fill(if self.theme.dark_mode {
                                egui::Color32::from_rgb(20, 22, 28)
                            } else {
                                egui::Color32::from_rgb(244, 246, 250)
                            })
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                if self.theme.dark_mode {
                                    egui::Color32::from_rgb(34, 36, 48)
                                } else {
                                    egui::Color32::from_rgb(220, 224, 235)
                                },
                            ))
                            .inner_margin(egui::Margin::same(12.0)),
                    )
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            // Header + Close button
                            ui.horizontal(|ui| {
                                ui.strong(egui::RichText::new("Wallpaper Details").size(15.0));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("✖").clicked() {
                                            select_req = Some(None);
                                        }
                                    },
                                );
                            });
                            ui.separator();
                            ui.add_space(4.0);

                            // 16:9 Large preview
                            let preview_w = ui.available_width().max(200.0);
                            let preview_h = (preview_w * 9.0 / 16.0).round();
                            let preview_size = egui::vec2(preview_w, preview_h);
                            let thumb_path = wp
                                .metadata
                                .thumbnail_path
                                .as_ref()
                                .map(std::path::PathBuf::from);
                            let tex_id = thumb_path
                                .as_deref()
                                .and_then(|p| self.get_or_load_texture(p));

                            ui.allocate_ui(preview_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    if let Some(id) = tex_id {
                                        ui.add(
                                            egui::Image::new((id, preview_size))
                                                .max_size(preview_size)
                                                .rounding(6.0),
                                        );
                                    } else {
                                        ui.painter().rect_filled(
                                            ui.available_rect_before_wrap(),
                                            egui::Rounding::same(6.0),
                                            egui::Color32::from_rgb(16, 17, 22),
                                        );
                                        ui.small("No Preview");
                                    }
                                });
                            });

                            ui.add_space(6.0);
                            let inspector_playing =
                                wallpaper_playing_monitors(snapshot, wp.source());
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(wp.title()).strong().size(14.0),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.horizontal(|ui| {
                                let fav_label = if wp.metadata.is_favorite {
                                    "★ Favorited"
                                } else {
                                    "☆ Favorite"
                                };
                                if ui
                                    .selectable_label(wp.metadata.is_favorite, fav_label)
                                    .clicked()
                                {
                                    fav_req = Some((wp.id, !wp.metadata.is_favorite));
                                }
                                if !inspector_playing.is_empty() {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(16, 185, 129),
                                        format!("▶ Playing on {}", inspector_playing.join(", ")),
                                    );
                                }
                            });

                            ui.add_space(6.0);
                            // Primary Apply Button
                            let apply_btn = egui::Button::new(
                                egui::RichText::new("▶  Set as Desktop Wallpaper")
                                    .strong()
                                    .size(13.5)
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(accent_color(&self.theme))
                            .rounding(egui::Rounding::same(6.0));

                            if ui
                                .add_sized(egui::vec2(ui.available_width(), 36.0), apply_btn)
                                .clicked()
                            {
                                assign_req = Some(wp.id);
                            }

                            if snapshot.monitors.len() > 1 {
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.small("Apply to screen:");
                                    for mon in &snapshot.monitors {
                                        let is_sel = self.selected_monitor.as_deref()
                                            == Some(&mon.device_name);
                                        let label = if mon.is_primary {
                                            format!("🖥 {} (Primary)", mon.device_name)
                                        } else {
                                            format!("🖥 {}", mon.device_name)
                                        };
                                        if ui.selectable_label(is_sel, label).clicked() {
                                            self.selected_monitor = Some(mon.device_name.clone());
                                            assign_req = Some(wp.id);
                                        }
                                    }
                                });
                            }

                            ui.add_space(8.0);
                            ui.separator();

                            // Orientation & Rotation Controls
                            ui.strong("Screen Orientation");
                            let current_rot = wp.config.video_rotation.unwrap_or(0);
                            let is_vert = wp.is_effective_vertical();

                            ui.horizontal(|ui| {
                                let horiz_btn = egui::Button::new(
                                    egui::RichText::new("↔ Horizontal")
                                        .strong()
                                        .color(if !is_vert { egui::Color32::WHITE } else { egui::Color32::from_gray(170) }),
                                )
                                .fill(if !is_vert { egui::Color32::from_rgb(45, 52, 70) } else { egui::Color32::from_rgb(24, 26, 34) })
                                .stroke(egui::Stroke::new(1.0_f32, if !is_vert { accent_color(&self.theme) } else { egui::Color32::from_rgb(45, 48, 60) }))
                                .rounding(egui::Rounding::same(5.0));

                                let vert_btn = egui::Button::new(
                                    egui::RichText::new("↕ Vertical (90°)")
                                        .strong()
                                        .color(if is_vert { egui::Color32::WHITE } else { egui::Color32::from_gray(170) }),
                                )
                                .fill(if is_vert { egui::Color32::from_rgb(45, 52, 70) } else { egui::Color32::from_rgb(24, 26, 34) })
                                .stroke(egui::Stroke::new(1.0_f32, if is_vert { accent_color(&self.theme) } else { egui::Color32::from_rgb(45, 48, 60) }))
                                .rounding(egui::Rounding::same(5.0));

                                let half_w = (ui.available_width() - 6.0) / 2.0;
                                if ui.add_sized(egui::vec2(half_w, 32.0), horiz_btn).clicked() {
                                    rotate_req = Some((wp.id, 0));
                                }
                                if ui.add_sized(egui::vec2(half_w, 32.0), vert_btn).clicked() {
                                    rotate_req = Some((wp.id, 90));
                                }
                            });

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.small("Angle:");
                                for deg in [0, 90, 180, 270] {
                                    let label = format!("{deg}°");
                                    if ui.selectable_label(current_rot == deg, label).clicked() {
                                        rotate_req = Some((wp.id, deg));
                                    }
                                }
                                if ui.button("🔄 +90°").on_hover_text("Rotate 90° clockwise").clicked() {
                                    rotate_req = Some((wp.id, (current_rot + 90) % 360));
                                }
                            });

                            let orient_desc = if is_vert {
                                "📱 Portrait (Vertical) active — fits vertical monitors & phone videos."
                            } else {
                                "🖥 Landscape (Horizontal) active — fits standard widescreen monitors."
                            };
                            ui.small(orient_desc);

                            ui.add_space(8.0);
                            ui.separator();

                            // Resource Usage & 10s Benchmark
                            ui.strong("⚡ Resource Usage (10s Benchmark)");
                            if let (Some(ram), Some(cpu)) =
                                (wp.metadata.peak_ram_mb, wp.metadata.peak_cpu_percent)
                            {
                                ui.colored_label(
                                    egui::Color32::from_rgb(16, 185, 129),
                                    format!("Peak RAM: {ram} MB  •  Peak CPU: {cpu:.1}%"),
                                );
                                ui.small("GPU Acceleration: Active (Fluid playback)");
                                ui.small(
                                    "ℹ Profiler ran for first 10s, then automatically stopped.",
                                );
                                if ui.button("⚡ Re-Benchmark (10s)").clicked() {
                                    benchmark_req = Some(wp.id);
                                }
                            } else {
                                ui.colored_label(egui::Color32::from_gray(180), "Not measured yet");
                                ui.small("Measures peak usage for first 10s, then turns off.");
                                if ui.button("⚡ Measure Peak Usage (10s)").clicked() {
                                    benchmark_req = Some(wp.id);
                                }
                            }

                            ui.add_space(8.0);
                            ui.separator();

                            // Add to Playlist
                            ui.strong("Playlists");
                            if !snapshot.playlists.is_empty() {
                                egui::ComboBox::from_id_salt("inspector_add_pl")
                                    .selected_text("➕ Add to Playlist…")
                                    .show_ui(ui, |ui| {
                                        for pl in &snapshot.playlists {
                                            if !pl.wallpaper_ids.contains(&wp.id) {
                                                if ui.button(&pl.name).clicked() {
                                                    let mut updated = pl.clone();
                                                    updated.wallpaper_ids.push(wp.id);
                                                    self.send(
                                                        Command::SavePlaylist { playlist: updated },
                                                        "Added to playlist!",
                                                    );
                                                }
                                            }
                                        }
                                    });
                            } else {
                                ui.small("No playlists created yet.");
                            }

                            ui.add_space(10.0);
                            ui.separator();

                            // Actions: Open Explorer & Remove
                            ui.horizontal(|ui| {
                                if ui.button("📂 Open Folder").clicked() {
                                    let p = std::path::PathBuf::from(wp.source());
                                    if let Some(parent) = p.parent() {
                                        let _ = std::process::Command::new("explorer.exe")
                                            .arg(parent)
                                            .spawn();
                                    }
                                }
                                if ui
                                    .button(
                                        egui::RichText::new("🗑 Remove")
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    )
                                    .clicked()
                                {
                                    remove_prompt_req = Some((wp.id, wp.title().to_string()));
                                }
                            });
                        });
                    });
            }
        }

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

                                    let tex_id = thumb_path
                                        .as_deref()
                                        .and_then(|p| self.get_or_load_texture(p));

                                    ui.allocate_ui(thumb_size, |ui| {
                                        ui.centered_and_justified(|ui| {
                                            if let Some(id) = tex_id {
                                                ui.add(
                                                    egui::Image::new((id, thumb_size))
                                                        .max_size(thumb_size)
                                                        .rounding(2.0),
                                                );
                                            } else if wallpaper.metadata.is_processing() {
                                                ui.spinner();
                                            } else {
                                                ui.small("No thumb");
                                            }
                                        });
                                    });

                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            let star = if wallpaper.metadata.is_favorite {
                                                "★"
                                            } else {
                                                "☆"
                                            };
                                            let star_color = if wallpaper.metadata.is_favorite {
                                                egui::Color32::from_rgb(250, 204, 21)
                                            } else {
                                                egui::Color32::from_gray(140)
                                            };
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        egui::RichText::new(star).color(star_color),
                                                    )
                                                    .frame(false),
                                                )
                                                .on_hover_text("Toggle favorite")
                                                .clicked()
                                            {
                                                fav_req = Some((
                                                    wallpaper.id,
                                                    !wallpaper.metadata.is_favorite,
                                                ));
                                            }
                                            ui.strong(wallpaper.title());
                                            let row_playing = wallpaper_playing_monitors(
                                                snapshot,
                                                wallpaper.source(),
                                            );
                                            if !row_playing.is_empty() {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(16, 185, 129),
                                                    format!(
                                                        "▶ Playing ({})",
                                                        row_playing.join(", ")
                                                    ),
                                                );
                                            }
                                            if let Some(ref cat) =
                                                wallpaper.metadata.resolution_category
                                            {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(60, 160, 240),
                                                    format!("[{}]", cat),
                                                );
                                            }
                                            if wallpaper.metadata.is_horizontal() {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(100, 180, 255),
                                                    "[Landscape]",
                                                );
                                            } else if wallpaper.metadata.is_vertical() {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(200, 130, 240),
                                                    "[Portrait]",
                                                );
                                            }
                                            if let (Some(ram), Some(cpu)) = (
                                                wallpaper.metadata.peak_ram_mb,
                                                wallpaper.metadata.peak_cpu_percent,
                                            ) {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(80, 210, 120),
                                                    format!("[⚡ {ram}MB • {cpu:.1}%]"),
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
                                            if ui.selectable_label(is_current, "Details").clicked()
                                            {
                                                select_req = Some(Some(wallpaper.id));
                                            }
                                            if wallpaper.is_effective_vertical() {
                                                if ui
                                                    .button("↔ Horizontal")
                                                    .on_hover_text("Change to horizontal (0°)")
                                                    .clicked()
                                                {
                                                    rotate_req = Some((wallpaper.id, 0));
                                                }
                                            } else {
                                                if ui
                                                    .button("↕ Vertical")
                                                    .on_hover_text("Change to vertical (90°)")
                                                    .clicked()
                                                {
                                                    rotate_req = Some((wallpaper.id, 90));
                                                }
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
                                                remove_prompt_req = Some((
                                                    wallpaper.id,
                                                    wallpaper.title().to_string(),
                                                ));
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
                let card_width = thumb_dim.x;
                let card_height = thumb_dim.y + 48.0;
                let avail_w = ui.available_width().max(card_width);
                let spacing = 10.0;
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
                                    let is_current = self.selected_wallpaper == Some(wallpaper.id);

                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(card_width, card_height),
                                        egui::Sense::click(),
                                    );

                                    // Double-click: Instant Apply!
                                    if response.double_clicked() {
                                        assign_req = Some(wallpaper.id);
                                    } else if response.clicked() {
                                        select_req = Some(Some(wallpaper.id));
                                    }

                                    // Context menu (Right-click)
                                    response.context_menu(|ui| {
                                        if ui.button("▶ Set as Wallpaper").clicked() {
                                            assign_req = Some(wallpaper.id);
                                            ui.close_menu();
                                        }
                                        if snapshot.monitors.len() > 1 {
                                            ui.menu_button(
                                                "🖥 Set on Specific Monitor…",
                                                |ui| {
                                                    for mon in &snapshot.monitors {
                                                        let label = if mon.is_primary {
                                                            format!("{} (Primary)", mon.device_name)
                                                        } else {
                                                            mon.device_name.clone()
                                                        };
                                                        if ui.button(label).clicked() {
                                                            self.selected_monitor =
                                                                Some(mon.device_name.clone());
                                                            assign_req = Some(wallpaper.id);
                                                            ui.close_menu();
                                                        }
                                                    }
                                                },
                                            );
                                        }
                                        ui.separator();
                                        if wallpaper.is_effective_vertical() {
                                            if ui.button("↔ Turn Horizontal (0°)").clicked() {
                                                rotate_req = Some((wallpaper.id, 0));
                                                ui.close_menu();
                                            }
                                        } else {
                                            if ui.button("↕ Turn Vertical (90°)").clicked() {
                                                rotate_req = Some((wallpaper.id, 90));
                                                ui.close_menu();
                                            }
                                        }
                                        if ui.button("🔄 Rotate 90° Clockwise").clicked() {
                                            let cur = wallpaper.config.video_rotation.unwrap_or(0);
                                            rotate_req = Some((wallpaper.id, (cur + 90) % 360));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        let fav_ctx_label = if wallpaper.metadata.is_favorite {
                                            "☆ Remove from Favorites"
                                        } else {
                                            "⭐ Add to Favorites"
                                        };
                                        if ui.button(fav_ctx_label).clicked() {
                                            fav_req = Some((
                                                wallpaper.id,
                                                !wallpaper.metadata.is_favorite,
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui.button("⚡ Run 10s Benchmark").clicked() {
                                            benchmark_req = Some(wallpaper.id);
                                            ui.close_menu();
                                        }
                                        if ui.button("📂 Open File Location").clicked() {
                                            let p = std::path::PathBuf::from(wallpaper.source());
                                            if let Some(f) = p.parent() {
                                                let _ = std::process::Command::new("explorer.exe")
                                                    .arg(f)
                                                    .spawn();
                                            }
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui
                                            .button(
                                                egui::RichText::new("🗑 Remove Wallpaper")
                                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                                            )
                                            .clicked()
                                        {
                                            remove_prompt_req =
                                                Some((wallpaper.id, wallpaper.title().to_string()));
                                            ui.close_menu();
                                        }
                                    });

                                    let card_playing =
                                        wallpaper_playing_monitors(snapshot, wallpaper.source());
                                    let is_playing = !card_playing.is_empty();

                                    // Draw card background
                                    let bg_color = if is_current {
                                        egui::Color32::from_rgb(32, 35, 48)
                                    } else if response.hovered() {
                                        egui::Color32::from_rgb(28, 30, 42)
                                    } else {
                                        egui::Color32::from_rgb(22, 23, 30)
                                    };

                                    let border_color = if is_current {
                                        accent_color(&self.theme)
                                    } else if is_playing {
                                        egui::Color32::from_rgb(16, 185, 129)
                                    } else if response.hovered() {
                                        egui::Color32::from_rgb(75, 80, 105)
                                    } else {
                                        egui::Color32::from_rgb(38, 40, 52)
                                    };

                                    ui.painter().rect(
                                        rect,
                                        egui::Rounding::same(8.0),
                                        bg_color,
                                        egui::Stroke::new(
                                            if is_current || is_playing {
                                                1.5_f32
                                            } else {
                                                1.0_f32
                                            },
                                            border_color,
                                        ),
                                    );

                                    // Thumbnail rect
                                    let thumb_rect = egui::Rect::from_min_size(
                                        rect.min + egui::vec2(2.0, 2.0),
                                        egui::vec2(card_width - 4.0, thumb_dim.y),
                                    );

                                    let thumb_path = wallpaper
                                        .metadata
                                        .thumbnail_path
                                        .as_ref()
                                        .map(std::path::PathBuf::from);

                                    let tex_id = thumb_path
                                        .as_deref()
                                        .and_then(|p| self.get_or_load_texture(p));

                                    if let Some(id) = tex_id {
                                        ui.painter().image(
                                            id,
                                            thumb_rect,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            egui::Color32::WHITE,
                                        );
                                    } else {
                                        ui.painter().rect_filled(
                                            thumb_rect,
                                            egui::Rounding::same(6.0),
                                            egui::Color32::from_rgb(16, 17, 22),
                                        );
                                    }

                                    // Bottom-Right duration/res pill
                                    let duration_str =
                                        wallpaper.metadata.duration_secs.map(format_duration);
                                    let res_str = wallpaper.metadata.resolution_category.clone();
                                    let pill_str = match (res_str, duration_str) {
                                        (Some(r), Some(d)) => format!("{r} • {d}"),
                                        (Some(r), None) => r,
                                        (None, Some(d)) => d,
                                        (None, None) => "Video".into(),
                                    };
                                    let pill_galley = ui.painter().layout_no_wrap(
                                        pill_str,
                                        egui::FontId::new(9.5, egui::FontFamily::Proportional),
                                        egui::Color32::from_gray(230),
                                    );
                                    let pill_rect = egui::Rect::from_min_size(
                                        thumb_rect.max
                                            - egui::vec2(
                                                pill_galley.size().x + 8.0,
                                                pill_galley.size().y + 4.0,
                                            ),
                                        pill_galley.size() + egui::vec2(8.0, 4.0),
                                    );
                                    ui.painter().rect_filled(
                                        pill_rect,
                                        egui::Rounding::same(4.0),
                                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190),
                                    );
                                    ui.painter().galley(
                                        pill_rect.min + egui::vec2(4.0, 2.0),
                                        pill_galley,
                                        egui::Color32::from_gray(230),
                                    );

                                    // Top-Right orientation badge
                                    let rot = wallpaper.config.video_rotation.unwrap_or(0);
                                    let is_vert = wallpaper.is_effective_vertical();
                                    let (orient_tag, tag_bg, tag_fg) = if rot == 90 {
                                        (
                                            Some("↕ Vertical (90°)"),
                                            egui::Color32::from_rgb(99, 102, 241),
                                            egui::Color32::WHITE,
                                        )
                                    } else if rot == 180 {
                                        (
                                            Some("🔄 180°"),
                                            egui::Color32::from_rgb(139, 92, 246),
                                            egui::Color32::WHITE,
                                        )
                                    } else if rot == 270 {
                                        (
                                            Some("↕ Vertical (270°)"),
                                            egui::Color32::from_rgb(99, 102, 241),
                                            egui::Color32::WHITE,
                                        )
                                    } else if is_vert {
                                        (
                                            Some("📱 Portrait"),
                                            egui::Color32::from_rgba_unmultiplied(30, 41, 59, 220),
                                            egui::Color32::from_rgb(203, 213, 225),
                                        )
                                    } else {
                                        (
                                            None,
                                            egui::Color32::TRANSPARENT,
                                            egui::Color32::TRANSPARENT,
                                        )
                                    };

                                    if let Some(tag) = orient_tag {
                                        let tag_galley = ui.painter().layout_no_wrap(
                                            tag.into(),
                                            egui::FontId::new(9.5, egui::FontFamily::Proportional),
                                            tag_fg,
                                        );
                                        let tag_rect = egui::Rect::from_min_size(
                                            egui::pos2(
                                                thumb_rect.max.x - tag_galley.size().x - 8.0,
                                                thumb_rect.min.y + 4.0,
                                            ),
                                            tag_galley.size() + egui::vec2(8.0, 4.0),
                                        );
                                        ui.painter().rect_filled(
                                            tag_rect,
                                            egui::Rounding::same(4.0),
                                            tag_bg,
                                        );
                                        ui.painter().galley(
                                            tag_rect.min + egui::vec2(4.0, 2.0),
                                            tag_galley,
                                            tag_fg,
                                        );
                                    }

                                    // Quick one-click Orientation Switcher (always visible on thumbnail)
                                    let toggle_rect = egui::Rect::from_min_size(
                                        thumb_rect.min + egui::vec2(6.0, 6.0),
                                        egui::vec2(if is_vert { 88.0 } else { 82.0 }, 22.0),
                                    );
                                    let toggle_resp = ui.interact(
                                        toggle_rect,
                                        ui.id().with(("rot_toggle", wallpaper.id)),
                                        egui::Sense::click(),
                                    );
                                    if toggle_resp.clicked() {
                                        rotate_req =
                                            Some((wallpaper.id, if is_vert { 0 } else { 90 }));
                                    }
                                    let btn_bg = if toggle_resp.hovered() {
                                        accent_color(&self.theme)
                                    } else {
                                        egui::Color32::from_rgba_unmultiplied(22, 26, 38, 220)
                                    };
                                    ui.painter().rect_filled(
                                        toggle_rect,
                                        egui::Rounding::same(4.0),
                                        btn_bg,
                                    );
                                    ui.painter().rect_stroke(
                                        toggle_rect,
                                        egui::Rounding::same(4.0),
                                        egui::Stroke::new(
                                            1.0_f32,
                                            if toggle_resp.hovered() {
                                                accent_color(&self.theme)
                                            } else {
                                                egui::Color32::from_rgb(70, 75, 95)
                                            },
                                        ),
                                    );
                                    let btn_text = if is_vert {
                                        "↔ Make Horiz"
                                    } else {
                                        "↕ Make Vert"
                                    };
                                    ui.painter().text(
                                        toggle_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        btn_text,
                                        egui::FontId::new(10.0, egui::FontFamily::Proportional),
                                        egui::Color32::WHITE,
                                    );

                                    // Clickable favorite star button below orientation switcher
                                    let fav_rect = egui::Rect::from_min_size(
                                        thumb_rect.min + egui::vec2(6.0, 32.0),
                                        egui::vec2(22.0, 22.0),
                                    );
                                    let fav_resp = ui.interact(
                                        fav_rect,
                                        ui.id().with(("fav_toggle", wallpaper.id)),
                                        egui::Sense::click(),
                                    );
                                    if fav_resp.clicked() {
                                        fav_req =
                                            Some((wallpaper.id, !wallpaper.metadata.is_favorite));
                                    }
                                    let fav_bg = if fav_resp.hovered() {
                                        egui::Color32::from_rgba_unmultiplied(45, 52, 72, 235)
                                    } else {
                                        egui::Color32::from_rgba_unmultiplied(18, 22, 32, 200)
                                    };
                                    ui.painter().rect_filled(
                                        fav_rect,
                                        egui::Rounding::same(11.0),
                                        fav_bg,
                                    );
                                    ui.painter().text(
                                        fav_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        if wallpaper.metadata.is_favorite {
                                            "★"
                                        } else {
                                            "☆"
                                        },
                                        egui::FontId::new(12.0, egui::FontFamily::Proportional),
                                        if wallpaper.metadata.is_favorite {
                                            egui::Color32::from_rgb(250, 204, 21)
                                        } else {
                                            egui::Color32::from_gray(200)
                                        },
                                    );

                                    // Bottom-Left active playing badge
                                    if is_playing {
                                        let playing_galley = ui.painter().layout_no_wrap(
                                            "▶ Playing".into(),
                                            egui::FontId::new(9.5, egui::FontFamily::Proportional),
                                            egui::Color32::WHITE,
                                        );
                                        let playing_rect = egui::Rect::from_min_size(
                                            egui::pos2(
                                                thumb_rect.min.x + 6.0,
                                                thumb_rect.max.y - playing_galley.size().y - 8.0,
                                            ),
                                            playing_galley.size() + egui::vec2(8.0, 4.0),
                                        );
                                        ui.painter().rect_filled(
                                            playing_rect,
                                            egui::Rounding::same(4.0),
                                            egui::Color32::from_rgba_unmultiplied(
                                                16, 185, 129, 230,
                                            ),
                                        );
                                        ui.painter().galley(
                                            playing_rect.min + egui::vec2(4.0, 2.0),
                                            playing_galley,
                                            egui::Color32::WHITE,
                                        );
                                    }

                                    // Multi-selection checkbox
                                    if is_selected {
                                        let check_rect = egui::Rect::from_min_size(
                                            thumb_rect.min + egui::vec2(4.0, 4.0),
                                            egui::vec2(16.0, 16.0),
                                        );
                                        ui.painter().rect_filled(
                                            check_rect,
                                            egui::Rounding::same(3.0),
                                            accent_color(&self.theme),
                                        );
                                        ui.painter().text(
                                            check_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "✓",
                                            egui::FontId::new(11.0, egui::FontFamily::Proportional),
                                            egui::Color32::WHITE,
                                        );
                                    }

                                    // Floating Quick-Apply Overlay Button on Hover
                                    if response.hovered() {
                                        let play_size = egui::vec2(72.0, 26.0);
                                        let play_rect = egui::Rect::from_center_size(
                                            thumb_rect.center(),
                                            play_size,
                                        );
                                        let play_resp = ui.interact(
                                            play_rect,
                                            ui.id().with(("hover_apply", wallpaper.id)),
                                            egui::Sense::click(),
                                        );
                                        if play_resp.clicked() {
                                            assign_req = Some(wallpaper.id);
                                        }
                                        let play_bg = if play_resp.hovered() {
                                            egui::Color32::from_rgb(16, 185, 129)
                                        } else {
                                            egui::Color32::from_rgba_unmultiplied(16, 185, 129, 230)
                                        };
                                        ui.painter().rect_filled(
                                            play_rect,
                                            egui::Rounding::same(13.0),
                                            play_bg,
                                        );
                                        ui.painter().text(
                                            play_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "▶ Apply",
                                            egui::FontId::new(11.5, egui::FontFamily::Proportional),
                                            egui::Color32::WHITE,
                                        );
                                    }

                                    // Card Footer (Fixed 44px)
                                    let footer_min = rect.min + egui::vec2(6.0, thumb_dim.y + 6.0);
                                    let title_galley = ui.painter().layout(
                                        wallpaper.title().to_string(),
                                        egui::FontId::new(12.5, egui::FontFamily::Proportional),
                                        egui::Color32::WHITE,
                                        card_width - 12.0,
                                    );
                                    ui.painter().galley(
                                        footer_min,
                                        title_galley,
                                        egui::Color32::WHITE,
                                    );

                                    let sub_str = if let (Some(ram), Some(cpu)) = (
                                        wallpaper.metadata.peak_ram_mb,
                                        wallpaper.metadata.peak_cpu_percent,
                                    ) {
                                        format!("⚡ {ram}MB • {cpu:.1}% CPU")
                                    } else if wallpaper.is_effective_vertical() {
                                        if rot > 0 {
                                            "Vertical (Rotated 90°) • Fluid Playback".into()
                                        } else {
                                            "Portrait • Fluid Playback".into()
                                        }
                                    } else {
                                        "Landscape (Horizontal) • Fluid Playback".into()
                                    };
                                    let sub_galley = ui.painter().layout_no_wrap(
                                        sub_str,
                                        egui::FontId::new(10.5, egui::FontFamily::Proportional),
                                        egui::Color32::from_rgb(148, 163, 184),
                                    );
                                    ui.painter().galley(
                                        footer_min + egui::vec2(0.0, 16.0),
                                        sub_galley,
                                        egui::Color32::from_rgb(148, 163, 184),
                                    );

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
                    monitor: selected_monitor.clone(),
                },
                "Wallpaper assignment requested…",
            );
        }

        if let Some(target) = select_req {
            self.selected_wallpaper = target;
        }

        if let Some((id, rot)) = rotate_req {
            self.send(
                Command::SetWallpaperRotation {
                    wallpaper_id: id,
                    rotation: rot,
                },
                &format!("Rotation updated to {rot}°"),
            );
        }

        if let Some(id) = benchmark_req {
            self.send(
                Command::BenchmarkWallpaper {
                    wallpaper_id: id,
                    monitor: selected_monitor.clone(),
                },
                "10-second resource benchmark started…",
            );
        }

        if let Some(id) = retry_req {
            self.send(
                Command::RetryProcessing { wallpaper_id: id },
                "Retrying processing…",
            );
        }

        if let Some((id, favorite)) = fav_req {
            self.send(
                Command::SetFavorite {
                    wallpaper_id: id,
                    favorite,
                },
                if favorite {
                    "Added to favorites ⭐"
                } else {
                    "Removed from favorites"
                },
            );
        }

        if let Some((id, title)) = remove_prompt_req {
            self.pending_delete_wallpaper = Some((id, title));
        }

        // Delete Confirmation Modal
        if let Some((del_id, del_title)) = self.pending_delete_wallpaper.clone() {
            let mut close_modal = false;
            let mut confirmed_delete = false;
            egui::Window::new("Remove Wallpaper?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Remove \"{del_title}\" from your Rust Wallpaper library?"
                    ));
                    ui.add_space(4.0);
                    ui.small("Original video/media file on your disk will NOT be deleted.");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close_modal = true;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("🗑 Remove").color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(220, 60, 60)),
                            )
                            .clicked()
                        {
                            confirmed_delete = true;
                            close_modal = true;
                        }
                    });
                });
            if confirmed_delete {
                if self.selected_wallpaper == Some(del_id) {
                    self.selected_wallpaper = None;
                }
                self.send(
                    Command::RemoveWallpaper {
                        wallpaper_id: del_id,
                    },
                    &format!("Removing '{del_title}' from library…"),
                );
            }
            if close_modal {
                self.pending_delete_wallpaper = None;
            }
        }
    }

    pub(super) fn render_playlists_subtab(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Wallpaper Playlists");
        ui.label(
            "Combine wallpapers into timed playlists that cycle automatically (e.g. change every 30 minutes).",
        );
        ui.add_space(8.0);

        // Form to create a new playlist
        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("➕ Create New Playlist");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut self.new_playlist_name);

                ui.label("Change wallpaper every:");
                let current_mins_text = match self.new_playlist_interval_mins {
                    1 => "1 Minute (Demo / Test)",
                    5 => "5 Minutes",
                    15 => "15 Minutes",
                    30 => "30 Minutes",
                    60 => "1 Hour",
                    120 => "2 Hours",
                    1440 => "24 Hours (Daily)",
                    _ => "Custom",
                };
                egui::ComboBox::from_id_salt("pl_interval_combo")
                    .selected_text(current_mins_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.new_playlist_interval_mins,
                            1,
                            "1 Minute (Demo / Test)",
                        );
                        ui.selectable_value(
                            &mut self.new_playlist_interval_mins,
                            5,
                            "5 Minutes",
                        );
                        ui.selectable_value(
                            &mut self.new_playlist_interval_mins,
                            15,
                            "15 Minutes",
                        );
                        ui.selectable_value(
                            &mut self.new_playlist_interval_mins,
                            30,
                            "30 Minutes",
                        );
                        ui.selectable_value(&mut self.new_playlist_interval_mins, 60, "1 Hour");
                        ui.selectable_value(&mut self.new_playlist_interval_mins, 120, "2 Hours");
                        ui.selectable_value(
                            &mut self.new_playlist_interval_mins,
                            1440,
                            "24 Hours (Daily)",
                        );
                    });

                ui.checkbox(&mut self.new_playlist_shuffle, "Shuffle order");

                ui.label("Schedule:");
                egui::ComboBox::from_id_salt("pl_timeslot_combo")
                    .selected_text(self.new_playlist_time_slot.label())
                    .show_ui(ui, |ui| {
                        for slot in wallpaper_core::ScheduleTimeSlot::ALL {
                            ui.selectable_value(&mut self.new_playlist_time_slot, slot, slot.label());
                        }
                    });
            });

            ui.add_space(4.0);
            ui.label(format!(
                "Selected {} wallpapers from library to include in this playlist:",
                self.selected_wallpapers.len()
            ));
            if self.selected_wallpapers.is_empty() {
                ui.small("💡 Tip: Select wallpapers using checkboxes in the 'All Wallpapers' tab, or click 'Select All' below.");
            }

            ui.horizontal(|ui| {
                if ui.button("Select All Library Items").clicked() {
                    for wp in &snapshot.wallpapers {
                        self.selected_wallpapers.insert(wp.id);
                    }
                }
                if ui.button("Clear Selected").clicked() {
                    self.selected_wallpapers.clear();
                }

                let can_save = !self.new_playlist_name.trim().is_empty()
                    && !self.selected_wallpapers.is_empty();
                if ui
                    .add_enabled(can_save, egui::Button::new("💾 Save Playlist"))
                    .clicked()
                {
                    let ids: Vec<WallpaperId> =
                        self.selected_wallpapers.iter().copied().collect();
                    let mut pl = wallpaper_core::WallpaperPlaylist::new(
                        self.new_playlist_name.trim(),
                        ids,
                        (self.new_playlist_interval_mins as u64) * 60,
                        self.new_playlist_shuffle,
                    );
                    pl.time_slot = self.new_playlist_time_slot;
                    self.send(Command::SavePlaylist { playlist: pl }, "Playlist saved!");
                    self.selected_wallpapers.clear();
                    self.new_playlist_name = "My Playlist".into();
                }

                if ui
                    .add_enabled(can_save, egui::Button::new("▶ Save & Start Playing"))
                    .clicked()
                {
                    let ids: Vec<WallpaperId> =
                        self.selected_wallpapers.iter().copied().collect();
                    let mut pl = wallpaper_core::WallpaperPlaylist::new(
                        self.new_playlist_name.trim(),
                        ids,
                        (self.new_playlist_interval_mins as u64) * 60,
                        self.new_playlist_shuffle,
                    );
                    pl.time_slot = self.new_playlist_time_slot;
                    let pl_id = pl.id.clone();
                    self.send(
                        Command::SavePlaylist { playlist: pl },
                        "Playlist saved and started!",
                    );
                    self.active_playlist_id = Some(pl_id.clone());
                    self.active_playlist_index = 0;
                    self.rotation_dirty_until = Some(Instant::now() + Duration::from_secs(2));
                    self.send(
                        Command::SetActivePlaylist {
                            playlist_id: Some(pl_id),
                        },
                        "Playlist rotation enabled",
                    );
                    self.cycle_playlist_wallpaper(snapshot);
                    self.selected_wallpapers.clear();
                    self.new_playlist_name = "My Playlist".into();
                }
            });
        });

        ui.add_space(8.0);
        ui.heading("Saved Playlists");

        if snapshot.playlists.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.heading("No playlists yet");
                ui.label("Select wallpapers using checkboxes and create a playlist above!");
            });
            return;
        }

        let mut delete_req = None;
        let mut start_req = None;

        for playlist in &snapshot.playlists {
            let is_active = self.active_playlist_id.as_deref() == Some(&playlist.id);
            card_frame(&self.theme).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(&playlist.name);
                            if is_active {
                                ui.colored_label(
                                    egui::Color32::from_rgb(80, 210, 120),
                                    "▶ Active / Playing",
                                );
                            }
                            if playlist.time_slot != wallpaper_core::ScheduleTimeSlot::AllDay {
                                let is_now = playlist.time_slot.is_active_now();
                                ui.colored_label(
                                    if is_now {
                                        egui::Color32::from_rgb(56, 189, 248)
                                    } else {
                                        egui::Color32::from_rgb(148, 163, 184)
                                    },
                                    format!(
                                        "{} {}",
                                        playlist.time_slot.short_label(),
                                        if is_now { "• Active Now" } else { "" }
                                    ),
                                );
                            }
                        });
                        let interval_mins = playlist.interval_secs / 60;
                        ui.label(format!(
                            "{} wallpapers • Changes every {} minutes • {}",
                            playlist.wallpaper_ids.len(),
                            interval_mins,
                            if playlist.shuffle {
                                "Shuffle (Random)"
                            } else {
                                "In Order"
                            }
                        ));

                        // Thumbnail strip of wallpapers in playlist
                        ui.horizontal(|ui| {
                            for wp_id in playlist.wallpaper_ids.iter().take(6) {
                                if let Some(wp) =
                                    snapshot.wallpapers.iter().find(|w| &w.id == wp_id)
                                {
                                    let thumb_size = egui::vec2(48.0, 27.0);
                                    let thumb_path = wp
                                        .metadata
                                        .thumbnail_path
                                        .as_ref()
                                        .map(std::path::PathBuf::from);
                                    let tex_id = thumb_path
                                        .as_deref()
                                        .and_then(|p| self.get_or_load_texture(p));
                                    if let Some(id) = tex_id {
                                        ui.add(
                                            egui::Image::new((id, thumb_size))
                                                .max_size(thumb_size)
                                                .rounding(2.0),
                                        );
                                    }
                                }
                            }
                        });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑 Delete").clicked() {
                            delete_req = Some(playlist.id.clone());
                        }

                        if is_active {
                            if ui.button("⏹ Stop Playlist").clicked() {
                                self.active_playlist_id = None;
                                self.rotation_dirty_until =
                                    Some(Instant::now() + Duration::from_secs(2));
                                self.send(
                                    Command::SetActivePlaylist { playlist_id: None },
                                    "Playlist stopped.",
                                );
                            }
                            if ui.button("⏭ Next Wallpaper").clicked() {
                                self.cycle_playlist_wallpaper(snapshot);
                            }
                        } else if ui.button("▶ Start Playlist").clicked() {
                            start_req = Some(playlist.id.clone());
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }

        if let Some(id) = delete_req {
            if self.active_playlist_id.as_deref() == Some(&id) {
                self.active_playlist_id = None;
                self.rotation_dirty_until = Some(Instant::now() + Duration::from_secs(2));
                self.send(
                    Command::SetActivePlaylist { playlist_id: None },
                    "Playlist cleared",
                );
            }
            self.send(
                Command::DeletePlaylist { playlist_id: id },
                "Deleted playlist",
            );
        }

        if let Some(id) = start_req {
            self.active_playlist_id = Some(id.clone());
            self.active_playlist_index = 0;
            self.rotation_dirty_until = Some(Instant::now() + Duration::from_secs(2));
            self.send(
                Command::SetActivePlaylist {
                    playlist_id: Some(id),
                },
                "Playlist started!",
            );
            self.cycle_playlist_wallpaper(snapshot);
        }
    }
}
