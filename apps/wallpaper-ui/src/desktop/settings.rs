use super::*;

impl DesktopApp {
    pub(super) fn render_settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
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
        ui.label("Performance profile (Fullscreen & Power Saving)");
        ui.horizontal(|ui| {
            for (profile, label) in [
                (PerformanceProfile::Eco, "Eco (Aggressive)"),
                (PerformanceProfile::Balanced, "Balanced (Recommended)"),
                (PerformanceProfile::Performance, "Performance (High Uptime)"),
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

        ui.add_space(6.0);
        ui.separator();
        ui.label("Wallpaper Engine Performance & Quality");
        ui.add_space(4.0);

        let mut settings_changed = false;
        let mut target_fps = snapshot.target_fps;
        let mut ram_budget = snapshot.ram_budget_mb;
        let mut quality = snapshot.quality;

        ui.horizontal(|ui| {
            ui.label("Framerate Limit (FPS):");
            let current_fps_text = match target_fps {
                Some(15) => "15 FPS (Battery Saver)",
                Some(24) => "24 FPS (Cinematic)",
                Some(30) => "30 FPS (Balanced)",
                Some(45) => "45 FPS (Smooth)",
                Some(60) => "60 FPS (Ultra Smooth)",
                None | Some(0) => "Uncapped (Native)",
                Some(_) => "Custom",
            };
            egui::ComboBox::from_id_salt("fps_combo")
                .selected_text(current_fps_text)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut target_fps, None, "Uncapped (Native)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut target_fps, Some(15), "15 FPS (Battery Saver)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut target_fps, Some(24), "24 FPS (Cinematic)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut target_fps, Some(30), "30 FPS (Balanced)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut target_fps, Some(60), "60 FPS (Ultra Smooth)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("RAM Cache / Demuxer Limit:");
            let current_ram_text = match ram_budget {
                Some(32) => "32 MB (Minimal / Low RAM)",
                Some(64) => "64 MB (Standard)",
                Some(128) => "128 MB (High Performance)",
                Some(256) => "256 MB (Ultra / Instant Seek)",
                None | Some(0) => "Default (Auto)",
                Some(_) => "Custom",
            };
            egui::ComboBox::from_id_salt("ram_combo")
                .selected_text(current_ram_text)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut ram_budget, None, "Default (Auto)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut ram_budget, Some(32), "32 MB (Minimal / Low RAM)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut ram_budget, Some(64), "64 MB (Standard)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(&mut ram_budget, Some(128), "128 MB (High Performance)")
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(
                            &mut ram_budget,
                            Some(256),
                            "256 MB (Ultra / Instant Seek)",
                        )
                        .clicked()
                    {
                        settings_changed = true;
                    }
                });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Video Scaling Filter:");
            let current_q_text = match quality {
                wallpaper_core::VideoQuality::Fast => "Fast (Bilinear - Low CPU)",
                wallpaper_core::VideoQuality::Balanced => "Balanced (Spline36 - Sharp)",
                wallpaper_core::VideoQuality::High => "High Quality (EWA Lanczos)",
            };
            egui::ComboBox::from_id_salt("quality_combo")
                .selected_text(current_q_text)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(
                            &mut quality,
                            wallpaper_core::VideoQuality::Fast,
                            "Fast (Bilinear - Low CPU)",
                        )
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(
                            &mut quality,
                            wallpaper_core::VideoQuality::Balanced,
                            "Balanced (Spline36 - Sharp)",
                        )
                        .clicked()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .selectable_value(
                            &mut quality,
                            wallpaper_core::VideoQuality::High,
                            "High Quality (EWA Lanczos)",
                        )
                        .clicked()
                    {
                        settings_changed = true;
                    }
                });
        });

        if settings_changed {
            self.send(
                Command::SetPerformanceSettings {
                    target_fps,
                    ram_budget_mb: ram_budget,
                    quality,
                },
                "Performance settings applied…",
            );
        }

        ui.add_space(6.0);
        ui.separator();
        ui.label("Windows Startup");
        ui.add_space(4.0);

        let mut autostart = self.autostart_enabled;
        if ui
            .checkbox(
                &mut autostart,
                "Start Rust Wallpaper automatically on Windows startup",
            )
            .changed()
        {
            self.autostart_enabled = autostart;
            #[cfg(windows)]
            {
                match wallpaper_windows::set_autostart(autostart) {
                    Ok(()) => {
                        self.notice = if autostart {
                            "Startup enabled: Rust Wallpaper will start automatically on Windows boot".into()
                        } else {
                            "Startup disabled: Removed from Windows startup".into()
                        };
                    }
                    Err(e) => {
                        self.notice = format!("Failed to update Windows startup: {e}");
                    }
                }
            }
        }
        if self.autostart_enabled {
            ui.small("✔ Active: Live wallpapers start seamlessly in the background when you log into Windows.");
        } else {
            ui.small("Disabled: Launch manually whenever you want live wallpapers.");
        }

        ui.separator();
        ui.label("Browser Companion Integration (Chrome, Edge, Brave)");
        ui.add_space(4.0);

        let mut ext_registered = wallpaper_windows::is_browser_extension_registered();
        if ui
            .checkbox(
                &mut ext_registered,
                "Integrate with web browsers (Chrome, Edge, Brave)",
            )
            .changed()
        {
            if ext_registered {
                let _ = wallpaper_windows::register_browser_extension(None);
                self.notice = "Browser extension registered. When you launch your browser, accept the prompt to enable it.".into();
            } else {
                wallpaper_windows::unregister_browser_extension();
                self.notice = "Browser extension uninstalled from registry.".into();
            }
        }

        ui.horizontal(|ui| {
            if ext_registered {
                ui.small("✔ Active: Launching Chrome, Edge, or Brave prompts to enable the companion extension.");
            } else {
                ui.small("Disabled: Browsers will not detect the companion extension.");
            }

            if ui.button("🌐 Browser Setup & Info").clicked() {
                let _ = std::process::Command::new("rundll32")
                    .args(["url.dll,FileProtocolHandler", "http://127.0.0.1:24242/extension/install"])
                    .spawn();
            }
        });

        ui.separator();
        ui.label("Current implementation");
        ui.label("The daemon owns Win32 desktop integration, monitor lifecycle and MPV processes.");
        ui.label("This UI communicates only through the versioned named-pipe protocol.");
        ui.separator();
        ui.label("Future Roadmap");
        ui.label("WebView2 interactive HTML/web wallpapers, audio visualizer scenes, and automatic updater.");
    }
}
