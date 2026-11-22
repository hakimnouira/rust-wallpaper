use super::*;

fn fullscreen_text(mode: FullscreenPauseMode) -> &'static str {
    match mode {
        FullscreenPauseMode::Never => "Never",
        FullscreenPauseMode::ExclusiveOnly => "Exclusive fullscreen",
        FullscreenPauseMode::Fullscreen => "Borderless + exclusive",
        FullscreenPauseMode::MaximizedAndFullscreen => "Maximized + fullscreen",
    }
}

impl DesktopApp {
    pub(super) fn render_overview(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Overview");
        ui.small("Everything your desktop wallpaper engine is doing, in one place.");
        ui.add_space(8.0);

        let playing = snapshot
            .players
            .iter()
            .filter(|p| matches!(p.state, wallpaper_core::PlaybackState::Playing))
            .count();
        let paused = snapshot.players.iter().filter(|p| p.suspended).count();
        let accent = accent_color(&self.theme);

        card_frame(&self.theme).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.strong("Live wallpaper engine");
                    ui.label(if snapshot.connected {
                        format!(
                            "Running smoothly across {} display{}.",
                            snapshot.monitors.len(),
                            if snapshot.monitors.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        )
                    } else {
                        "Waiting for the local daemon to connect…".to_string()
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Open library").clicked() {
                            self.page = Page::Library;
                        }
                        if ui.button("Browse wallpapers").clicked() {
                            self.page = Page::Browse;
                        }
                        if ui.button("Customize").clicked() {
                            self.page = Page::Appearance;
                        }
                        if ui
                            .add_enabled(snapshot.connected, egui::Button::new("⏭ Next Wallpaper"))
                            .on_hover_text("Rotate desktop to another wallpaper from your library")
                            .clicked()
                        {
                            self.cycle_next_wallpaper(snapshot);
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.colored_label(
                        accent,
                        egui::RichText::new(format!("{playing}"))
                            .size(34.0)
                            .strong(),
                    );
                    ui.small("playing now");
                });
            });
        });

        ui.add_space(4.0);
        let stat_items = [
            ("Displays", snapshot.monitors.len().to_string(), "connected"),
            ("Playing", playing.to_string(), "active players"),
            ("Paused", paused.to_string(), "suspended"),
            (
                "Library",
                snapshot.wallpapers.len().to_string(),
                "wallpapers",
            ),
        ];
        ui.columns(4, |columns| {
            for (index, column) in columns.iter_mut().enumerate() {
                let (title, value, hint) = &stat_items[index];
                card_frame(&self.theme).show(column, |ui| {
                    ui.small(*title);
                    ui.colored_label(
                        accent,
                        egui::RichText::new(value.as_str()).size(20.0).strong(),
                    );
                    ui.small(*hint);
                });
            }
        });

        ui.add_space(4.0);
        let policy_frame = card_frame(&self.theme);
        let tips_frame = card_frame(&self.theme);
        ui.columns(2, |columns| {
            policy_frame.show(&mut columns[0], |ui| {
                ui.strong("Playback policy");
                ui.add_space(4.0);
                ui.label(format!("Profile: {:?}", snapshot.profile));
                ui.label(format!(
                    "Fullscreen: {}",
                    fullscreen_text(snapshot.playback_settings.fullscreen_pause)
                ));
                ui.label(format!(
                    "FPS limit: {}",
                    snapshot
                        .target_fps
                        .map(|fps| format!("{fps} FPS"))
                        .unwrap_or_else(|| "Uncapped".into())
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Pause all").clicked() {
                        self.send(Command::Pause { monitor: None }, "All wallpapers paused…");
                    }
                    if ui.button("Resume all").clicked() {
                        self.send(Command::Resume { monitor: None }, "All wallpapers resumed…");
                    }
                    if ui.small_button("Tune…").clicked() {
                        self.page = Page::Settings;
                    }
                });
            });

            tips_frame.show(&mut columns[1], |ui| {
                ui.strong("Quick tips");
                ui.add_space(4.0);
                ui.small("• Closing the window keeps the engine in the tray.");
                ui.small("• Double-click a library card to assign it to the selected display.");
                ui.small("• Use Appearance to recolor the whole panel instantly.");
                ui.small("• Set fullscreen policy to avoid pausing in borderless games.");
            });
        });

        ui.add_space(4.0);
        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Displays at a glance");
            ui.add_space(4.0);
            if snapshot.players.is_empty() {
                ui.small("No active players yet. Assign a video from the library to get started.");
            }
            for player in snapshot.players.iter().take(5) {
                ui.horizontal(|ui| {
                    let dot = if player.suspended {
                        egui::Color32::from_rgb(230, 175, 90)
                    } else {
                        egui::Color32::from_rgb(90, 200, 140)
                    };
                    ui.colored_label(dot, "●");
                    ui.label(&player.monitor);
                    let (status_text, status_color) = if player.suspended {
                        let reason = if player
                            .suspension_reasons
                            .iter()
                            .any(|r| r.contains("Fullscreen") || r.contains("Game"))
                        {
                            "Paused (Full screen app open)"
                        } else if player
                            .suspension_reasons
                            .iter()
                            .any(|r| r.contains("Battery"))
                        {
                            "Paused (Battery saver)"
                        } else if player.suspension_reasons.iter().any(|r| r.contains("Lock")) {
                            "Paused (Windows locked)"
                        } else {
                            "Paused"
                        };
                        (reason, egui::Color32::from_rgb(230, 175, 90))
                    } else {
                        ("Playing", egui::Color32::from_rgb(90, 200, 140))
                    };
                    ui.colored_label(status_color, status_text);
                    let mem_mb = player.memory_bytes / (1024 * 1024);
                    let decoder = if player.active_decoder.is_empty() {
                        "auto"
                    } else {
                        &player.active_decoder
                    };
                    ui.small(format!("· {mem_mb} MB RAM · GPU {decoder}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Manage").clicked() {
                            self.selected_monitor = Some(player.monitor.clone());
                            self.page = Page::Monitors;
                        }
                    });
                });
            }
        });

        ui.add_space(4.0);
        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("⚡ Engine & Power Diagnostics");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some((on_battery, percent)) = wallpaper_windows::query_power_status() {
                    let (badge_color, badge_text) = if on_battery {
                        (
                            egui::Color32::from_rgb(251, 191, 36),
                            format!("🔋 On Battery ({percent}%) · Battery Saver Active"),
                        )
                    } else {
                        (
                            egui::Color32::from_rgb(52, 211, 153),
                            format!("⚡ AC Power ({percent}%) · Full GPU Acceleration"),
                        )
                    };
                    ui.colored_label(badge_color, "●");
                    ui.small(badge_text);
                } else {
                    ui.colored_label(egui::Color32::from_rgb(52, 211, 153), "●");
                    ui.small("⚡ Desktop AC Power · Unthrottled");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let total_mem_mb: u64 = snapshot
                        .players
                        .iter()
                        .map(|p| p.memory_bytes / (1024 * 1024))
                        .sum();
                    ui.small(format!("Total Working Set RAM: {total_mem_mb} MB"));
                });
            });
        });
    }
}
