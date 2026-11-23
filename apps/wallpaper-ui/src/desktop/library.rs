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
    fn render_active_displays_strip(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if snapshot.monitors.is_empty() {
            return;
        }
        let dark = self.theme.dark_mode;
        let accent = accent_color(&self.theme);

        ui.label(
            egui::RichText::new("Active Displays")
                .size(13.0)
                .strong()
                .color(if dark {
                    egui::Color32::from_rgb(209, 213, 219)
                } else {
                    egui::Color32::from_rgb(51, 65, 85)
                }),
        );
        ui.add_space(4.0);

        let cols = snapshot.monitors.len().clamp(1, 3);
        let monitors = snapshot.monitors.clone();
        let players = snapshot.players.clone();
        let wallpapers = snapshot.wallpapers.clone();

        ui.columns(cols, |columns| {
            for (idx, mon) in monitors.iter().take(cols).enumerate() {
                let col_ui = &mut columns[idx];
                let is_targeted = self.selected_monitor.as_deref() == Some(&mon.device_name);
                let player = players.iter().find(|p| p.monitor == mon.device_name);
                let playing_wp = player.and_then(|p| {
                    p.source.as_deref().and_then(|src| {
                        let target_path = std::path::Path::new(src);
                        wallpapers.iter().find(|w| {
                            w.source().eq_ignore_ascii_case(src)
                                || std::path::Path::new(w.source()) == target_path
                        })
                    })
                });
                let tex_id = playing_wp
                    .and_then(|w| w.metadata.thumbnail_path.as_ref())
                    .map(std::path::PathBuf::from)
                    .and_then(|p| self.get_or_load_texture(&p));

                let mut pause_req: Option<(String, bool)> = None;
                let mut mute_req: Option<(String, bool)> = None;
                let mut select_mon_req: Option<String> = None;

                let frame_resp = egui::Frame::none()
                    .fill(if dark {
                        egui::Color32::from_rgb(18, 20, 28)
                    } else {
                        egui::Color32::from_rgb(246, 248, 252)
                    })
                    .stroke(egui::Stroke::new(
                        if is_targeted { 1.5_f32 } else { 1.0_f32 },
                        if is_targeted {
                            accent
                        } else if dark {
                            egui::Color32::from_rgb(29, 33, 45)
                        } else {
                            egui::Color32::from_rgb(220, 224, 235)
                        },
                    ))
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                    .show(col_ui, |ui| {
                        ui.horizontal(|ui| {
                            // Mini 16:9 thumbnail
                            let thumb_size = egui::vec2(76.0, 44.0);
                            let (thumb_rect, _) =
                                ui.allocate_exact_size(thumb_size, egui::Sense::hover());
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
                                    egui::Color32::from_rgb(12, 14, 20),
                                );
                            }
                            ui.painter().rect_stroke(
                                thumb_rect,
                                egui::Rounding::same(6.0),
                                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(35, 40, 56)),
                            );

                            ui.add_space(6.0);

                            // Display info + playing title
                            ui.vertical(|ui| {
                                let clean_name = mon.device_name.trim_start_matches(r"\\.\");
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{clean_name} · {}x{}",
                                        mon.width, mon.height
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(138, 147, 166)),
                                );

                                let title = playing_wp
                                    .map(|w| w.title().to_string())
                                    .unwrap_or_else(|| "No wallpaper assigned".to_string());
                                let (dot_col, state_label) = match player {
                                    Some(p) if !p.suspended => {
                                        (egui::Color32::from_rgb(34, 197, 94), "Playing")
                                    }
                                    Some(_) => (egui::Color32::from_rgb(245, 158, 11), "Paused"),
                                    None => (egui::Color32::from_rgb(120, 128, 145), "Idle"),
                                };
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(title).size(13.0).strong(),
                                        )
                                        .truncate(),
                                    );
                                    ui.colored_label(
                                        dot_col,
                                        egui::RichText::new(format!("• {state_label}")).size(11.5),
                                    );
                                });
                            });

                            // Inline Pause/Resume & Mute controls on the right
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(p) = player {
                                        let mute_lbl =
                                            if self.master_muted { "Unmute" } else { "Mute" };
                                        if ui
                                            .small_button(mute_lbl)
                                            .on_hover_text("Toggle audio for this display")
                                            .clicked()
                                        {
                                            mute_req =
                                                Some((mon.device_name.clone(), !self.master_muted));
                                        }
                                        let pause_lbl = if p.suspended { "Play" } else { "Pause" };
                                        if ui
                                            .small_button(pause_lbl)
                                            .on_hover_text("Pause or resume playback")
                                            .clicked()
                                        {
                                            pause_req =
                                                Some((mon.device_name.clone(), !p.suspended));
                                        }
                                    }
                                },
                            );
                        });
                    });

                let body_resp = col_ui.interact(
                    frame_resp.response.rect,
                    col_ui.id().with(("active_disp_card", &mon.device_name)),
                    egui::Sense::click(),
                );
                if body_resp.clicked() && pause_req.is_none() && mute_req.is_none() {
                    select_mon_req = Some(mon.device_name.clone());
                }

                if let Some((m, should_pause)) = pause_req {
                    if should_pause {
                        self.send(Command::Pause { monitor: Some(m) }, "Paused display…");
                    } else {
                        self.send(Command::Resume { monitor: Some(m) }, "Resumed display…");
                    }
                }
                if let Some((m, muted)) = mute_req {
                    self.master_muted = muted;
                    self.send(
                        Command::SetMute {
                            monitor: Some(m),
                            muted,
                        },
                        if muted {
                            "Muted display audio…"
                        } else {
                            "Unmuted display audio…"
                        },
                    );
                }
                if let Some(m) = select_mon_req {
                    if self.selected_monitor.as_deref() == Some(m.as_str()) {
                        self.selected_monitor = None;
                    } else {
                        self.selected_monitor = Some(m);
                    }
                }
            }
        });
        ui.add_space(10.0);
    }

    pub(super) fn render_library(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if self.library_subtab == LibrarySubtab::Wallpapers {
            self.render_active_displays_strip(ui, snapshot);
        }

        // Sleek Subtab & Filter Strip
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(
                    self.library_subtab == LibrarySubtab::Wallpapers,
                    format!("Wallpapers ({})", snapshot.wallpapers.len()),
                )
                .clicked()
            {
                self.library_subtab = LibrarySubtab::Wallpapers;
            }
            if ui
                .selectable_label(
                    self.library_subtab == LibrarySubtab::Playlists,
                    format!("Playlists ({})", snapshot.playlists.len()),
                )
                .clicked()
            {
                self.library_subtab = LibrarySubtab::Playlists;
            }

            if self.library_subtab == LibrarySubtab::Wallpapers {
                ui.separator();

                // Quick filter chips
                if ui
                    .selectable_label(!self.query.is_filtered(), "All")
                    .clicked()
                {
                    self.query.reset_filters();
                    self.filter_orientation = FilterOrientation::All;
                }
                if ui
                    .selectable_label(self.query.resolution == ResolutionFilter::Uhd4k, "4K UHD")
                    .clicked()
                {
                    self.query.resolution = if self.query.resolution == ResolutionFilter::Uhd4k {
                        ResolutionFilter::All
                    } else {
                        ResolutionFilter::Uhd4k
                    };
                }
                if ui
                    .selectable_label(self.query.resolution == ResolutionFilter::Fhd1080p, "1080p")
                    .clicked()
                {
                    self.query.resolution = if self.query.resolution == ResolutionFilter::Fhd1080p {
                        ResolutionFilter::All
                    } else {
                        ResolutionFilter::Fhd1080p
                    };
                }
                if ui
                    .selectable_label(self.query.favorites_only, "Favorites")
                    .clicked()
                {
                    self.query.favorites_only = !self.query.favorites_only;
                }

                ui.separator();

                egui::ComboBox::from_id_salt("sort_key_combo")
                    .selected_text(format!("Sort: {}", self.query.sort_key.label()))
                    .show_ui(ui, |ui| {
                        for key in SortKey::ALL {
                            ui.selectable_value(&mut self.query.sort_key, key, key.label());
                        }
                    });

                let dir_text = match self.query.sort_direction {
                    SortDirection::Ascending => "Asc",
                    SortDirection::Descending => "Desc",
                };
                if ui.button(dir_text).clicked() {
                    self.query.sort_direction = match self.query.sort_direction {
                        SortDirection::Ascending => SortDirection::Descending,
                        SortDirection::Descending => SortDirection::Ascending,
                    };
                }

                if ui
                    .selectable_label(self.view_mode == ViewMode::Grid, "Grid")
                    .clicked()
                    && self.view_mode != ViewMode::Grid
                {
                    self.view_mode = ViewMode::Grid;
                    self.save_prefs();
                }
                if ui
                    .selectable_label(self.view_mode == ViewMode::List, "List")
                    .clicked()
                    && self.view_mode != ViewMode::List
                {
                    self.view_mode = ViewMode::List;
                    self.save_prefs();
                }

                if (self.query.is_filtered() || self.filter_orientation != FilterOrientation::All)
                    && ui.button("Reset").clicked()
                {
                    self.query.reset_filters();
                    self.filter_orientation = FilterOrientation::All;
                }
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
                    .fill(egui::Color32::from_rgb(20, 32, 46))
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(34, 197, 94),
                                "Active Playlist:",
                            );
                            ui.strong(&pl.name);
                            ui.small(format!(
                                "({} wallpapers · every {}m)",
                                pl.wallpaper_ids.len(),
                                pl.interval_secs / 60
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Stop").clicked() {
                                        self.active_playlist_id = None;
                                        self.rotation_dirty_until =
                                            Some(Instant::now() + Duration::from_secs(2));
                                        self.send(
                                            Command::SetActivePlaylist { playlist_id: None },
                                            "Playlist stopped.",
                                        );
                                    }
                                    if ui.button("Next").clicked() {
                                        self.cycle_playlist_wallpaper(snapshot);
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(4.0);
            }
        }

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

        // Side Inspector Panel (opened on card click, collapsed by default for full-width 4-col gallery)
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
                let avail_w = ui.available_width().max(260.0);
                let spacing = 16.0;
                let cols = if avail_w >= 920.0 {
                    4
                } else if avail_w >= 660.0 {
                    3
                } else if avail_w >= 420.0 {
                    2
                } else {
                    1
                };
                let card_width = ((avail_w - spacing * (cols as f32 - 1.0)) / cols as f32)
                    .floor()
                    .max(160.0);
                let thumb_h = (card_width * 9.0 / 16.0).round();
                let card_height = thumb_h + 44.0;
                let total_rows = filtered.len().div_ceil(cols);

                egui::ScrollArea::vertical().show_rows(
                    ui,
                    card_height + spacing,
                    total_rows,
                    |ui, row_range| {
                        for row_idx in row_range {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = spacing;
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

                                    // Double-click: Instant Apply! Single-click: Open Inspector
                                    if response.double_clicked() {
                                        assign_req = Some(wallpaper.id);
                                    } else if response.clicked() {
                                        select_req = Some(Some(wallpaper.id));
                                    }

                                    // Context menu (Right-click)
                                    response.context_menu(|ui| {
                                        if ui.button("Set as Wallpaper").clicked() {
                                            assign_req = Some(wallpaper.id);
                                            ui.close_menu();
                                        }
                                        if snapshot.monitors.len() > 1 {
                                            ui.menu_button("Set on Specific Monitor…", |ui| {
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
                                            });
                                        }
                                        ui.separator();
                                        if wallpaper.is_effective_vertical() {
                                            if ui.button("Turn Horizontal (0°)").clicked() {
                                                rotate_req = Some((wallpaper.id, 0));
                                                ui.close_menu();
                                            }
                                        } else if ui.button("Turn Vertical (90°)").clicked() {
                                            rotate_req = Some((wallpaper.id, 90));
                                            ui.close_menu();
                                        }
                                        if ui.button("Rotate 90° Clockwise").clicked() {
                                            let cur = wallpaper.config.video_rotation.unwrap_or(0);
                                            rotate_req = Some((wallpaper.id, (cur + 90) % 360));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        let fav_ctx_label = if wallpaper.metadata.is_favorite {
                                            "Remove from Favorites"
                                        } else {
                                            "Add to Favorites"
                                        };
                                        if ui.button(fav_ctx_label).clicked() {
                                            fav_req = Some((
                                                wallpaper.id,
                                                !wallpaper.metadata.is_favorite,
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui.button("Run 10s Benchmark").clicked() {
                                            benchmark_req = Some(wallpaper.id);
                                            ui.close_menu();
                                        }
                                        if ui.button("Open File Location").clicked() {
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
                                                egui::RichText::new("Remove Wallpaper")
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

                                    // Borderless 16:9 thumbnail
                                    let thumb_rect = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(card_width, thumb_h),
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
                                            egui::Rounding::same(8.0),
                                            egui::Color32::from_rgb(18, 20, 28),
                                        );
                                    }

                                    let border_color = if is_current || response.hovered() {
                                        accent_color(&self.theme)
                                    } else if is_playing {
                                        egui::Color32::from_rgb(34, 197, 94)
                                    } else {
                                        egui::Color32::from_rgb(29, 33, 45)
                                    };
                                    ui.painter().rect_stroke(
                                        thumb_rect,
                                        egui::Rounding::same(8.0),
                                        egui::Stroke::new(
                                            if is_current || is_playing || response.hovered() {
                                                1.5_f32
                                            } else {
                                                1.0_f32
                                            },
                                            border_color,
                                        ),
                                    );

                                    // Multi-selection check indicator only when selected
                                    if is_selected {
                                        let check_rect = egui::Rect::from_min_size(
                                            thumb_rect.min + egui::vec2(6.0, 6.0),
                                            egui::vec2(16.0, 16.0),
                                        );
                                        ui.painter().rect_filled(
                                            check_rect,
                                            egui::Rounding::same(4.0),
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

                                    // Clean single-line Title + Metadata below thumbnail
                                    let title_pos = rect.min + egui::vec2(2.0, thumb_h + 7.0);
                                    let title_col = if self.theme.dark_mode {
                                        egui::Color32::from_rgb(243, 244, 246)
                                    } else {
                                        egui::Color32::from_rgb(15, 23, 42)
                                    };
                                    let title_galley = ui.painter().layout_no_wrap(
                                        wallpaper.title().to_string(),
                                        egui::FontId::new(13.5, egui::FontFamily::Proportional),
                                        title_col,
                                    );
                                    ui.painter().galley(title_pos, title_galley, title_col);

                                    let res_part = wallpaper
                                        .metadata
                                        .resolution_category
                                        .as_deref()
                                        .unwrap_or("HD");
                                    let right_part = wallpaper
                                        .metadata
                                        .duration_secs
                                        .map(format_duration)
                                        .or_else(|| wallpaper.metadata.codec.clone())
                                        .unwrap_or_else(|| "Video".to_string());
                                    let meta_str = if is_playing {
                                        format!("{res_part} · {right_part} · Playing")
                                    } else {
                                        format!("{res_part} · {right_part}")
                                    };
                                    let meta_col = if is_playing {
                                        egui::Color32::from_rgb(34, 197, 94)
                                    } else {
                                        egui::Color32::from_rgb(138, 147, 166)
                                    };
                                    let meta_galley = ui.painter().layout_no_wrap(
                                        meta_str,
                                        egui::FontId::new(11.5, egui::FontFamily::Proportional),
                                        meta_col,
                                    );
                                    ui.painter().galley(
                                        title_pos + egui::vec2(0.0, 18.0),
                                        meta_galley,
                                        meta_col,
                                    );
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
