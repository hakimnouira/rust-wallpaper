use super::*;

impl DesktopApp {
    pub(super) fn render_monitors(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Monitor manager");
        ui.label("Each display is controlled through the daemon's runtime supervisor.");
        ui.add_space(12.0);
        if snapshot.monitors.is_empty() {
            ui.label("No monitors reported yet.");
            return;
        }

        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Display layout");
            ui.small("Click a screen to make it the assignment target.");
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
                let fill = monitor_fill(&self.theme, selected, monitor.is_primary);
                painter.rect_filled(monitor_rect, 8.0, fill);
                painter.rect_stroke(
                    monitor_rect,
                    8.0,
                    egui::Stroke::new(1.0_f32, soft_accent(&self.theme)),
                );
                let text_color = if self.theme.colors().2 {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(24)
                };
                painter.text(
                    monitor_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &monitor.device_name,
                    egui::FontId::proportional(12.0),
                    text_color,
                );
                if layout_response.clicked()
                    && pointer.is_some_and(|position| monitor_rect.contains(position))
                {
                    self.selected_monitor = Some(monitor.device_name.clone());
                }
            }
        });

        ui.add_space(8.0);
        for monitor in &snapshot.monitors {
            let device = monitor.device_name.clone();
            let status = snapshot
                .players
                .iter()
                .find(|player| player.monitor == device);
            card_frame(&self.theme).show(ui, |ui| {
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
}
