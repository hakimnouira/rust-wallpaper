use super::*;

impl DesktopApp {
    pub(super) fn render_library(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        self.prune_texture_cache();

        ui.heading("Wallpaper library");
        ui.label("Import local videos, filter, and assign them to your displays.");
        ui.add_space(8.0);

        // Import Box
        card_frame(&self.theme).show(ui, |ui| {
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
                                            if ui.selectable_label(is_current, "Select").clicked() {
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
                                    let is_current = self.selected_wallpaper == Some(wallpaper.id);

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

                                                let tex_id = thumb_path
                                                    .as_deref()
                                                    .and_then(|p| self.get_or_load_texture(p));

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
                                                                let err_label = ui.colored_label(
                                                                    egui::Color32::from_rgb(
                                                                        230, 80, 80,
                                                                    ),
                                                                    "⚠ Error",
                                                                );
                                                                if let Some(ref err) = wallpaper.metadata.processing_error {
                                                                    err_label.on_hover_text(err.as_str());
                                                                } else {
                                                                    err_label.on_hover_text("Processing failed. Click Retry to try again.");
                                                                }
                                                                ui.small("(hover for details)");
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
                                                            egui::Color32::from_rgb(60, 160, 240),
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
}
