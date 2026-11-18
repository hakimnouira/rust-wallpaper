use super::*;

fn open_folder(path: Option<std::path::PathBuf>) {
    let Some(path) = path else {
        return;
    };
    let _ = std::fs::create_dir_all(&path);
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

fn fullscreen_label(mode: FullscreenPauseMode) -> &'static str {
    match mode {
        FullscreenPauseMode::Never => "Never pause for windows",
        FullscreenPauseMode::ExclusiveOnly => "Exclusive fullscreen only",
        FullscreenPauseMode::Fullscreen => "Borderless + exclusive fullscreen",
        FullscreenPauseMode::MaximizedAndFullscreen => "Maximized windows + fullscreen",
    }
}

impl DesktopApp {
    pub(super) fn render_settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Settings & diagnostics");
        ui.add_space(8.0);

        ui.label("Connection");
        ui.label(if snapshot.connected {
            "Connected to local wallpaper daemon"
        } else {
            "Disconnected"
        });
        if let Some(error) = &snapshot.last_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        ui.small("Closing the window keeps Rust Wallpaper running in the notification area. Right-click its tray icon to show the app or fully quit.");
        ui.horizontal(|ui| {
            if ui.button("Open logs").clicked() {
                open_folder(wallpaper_daemon::logging::logs_dir());
            }
            if ui.button("Open data folder").clicked() {
                open_folder(wallpaper_daemon::logging::data_dir());
            }
        });
        if let Some(logs) = wallpaper_daemon::logging::logs_dir() {
            ui.small(format!("Logs: {}", logs.display()));
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
        ui.label("Playback automation");
        ui.small("Choose which situations pause your wallpapers. Changes apply to all monitors immediately.");
        ui.add_space(4.0);

        let mut automation = snapshot.playback_settings;
        ui.horizontal(|ui| {
            ui.label("Fullscreen policy:");
            egui::ComboBox::from_id_salt("fullscreen_pause_mode")
                .selected_text(fullscreen_label(automation.fullscreen_pause))
                .show_ui(ui, |ui| {
                    for mode in FullscreenPauseMode::ALL {
                        if ui
                            .selectable_label(
                                automation.fullscreen_pause == mode,
                                fullscreen_label(mode),
                            )
                            .clicked()
                        {
                            automation.fullscreen_pause = mode;
                        }
                    }
                });
        });
        let mut automation_changed = automation != snapshot.playback_settings;
        automation_changed |= ui
            .checkbox(
                &mut automation.pause_on_lock,
                "Pause when Windows is locked",
            )
            .changed();
        automation_changed |= ui
            .checkbox(
                &mut automation.pause_on_display_off,
                "Pause while the display is off / sleeping",
            )
            .changed();
        automation_changed |= ui
            .checkbox(
                &mut automation.pause_on_suspend,
                "Pause while the system is suspended",
            )
            .changed();
        if automation_changed {
            self.send(
                Command::SetPlaybackSettings(automation),
                "Playback automation updated…",
            );
        }

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
                        .selectable_value(&mut target_fps, Some(45), "45 FPS (Smooth)")
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
        ui.label("Wallpaper Rotation & Playlists (Auto-Cycle)");
        ui.small("Automatically switch desktop wallpapers on a timed interval.");
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.auto_cycle_enabled, "Enable auto-cycle rotation");
            if self.auto_cycle_enabled {
                ui.label("· Every:");
                let current_interval_text = match self.auto_cycle_interval_secs {
                    60 => "1 Minute (Demo / Test)",
                    300 => "5 Minutes",
                    900 => "15 Minutes",
                    1800 => "30 Minutes",
                    3600 => "1 Hour",
                    86400 => "24 Hours (Daily)",
                    _ => "Custom",
                };
                egui::ComboBox::from_id_salt("auto_cycle_combo")
                    .selected_text(current_interval_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.auto_cycle_interval_secs,
                            60,
                            "1 Minute (Demo / Test)",
                        );
                        ui.selectable_value(&mut self.auto_cycle_interval_secs, 300, "5 Minutes");
                        ui.selectable_value(&mut self.auto_cycle_interval_secs, 900, "15 Minutes");
                        ui.selectable_value(&mut self.auto_cycle_interval_secs, 1800, "30 Minutes");
                        ui.selectable_value(&mut self.auto_cycle_interval_secs, 3600, "1 Hour");
                        ui.selectable_value(
                            &mut self.auto_cycle_interval_secs,
                            86400,
                            "24 Hours (Daily)",
                        );
                    });

                ui.checkbox(&mut self.auto_cycle_shuffle, "Shuffle (Random order)");
            }
        });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    snapshot.connected,
                    egui::Button::new("⏭ Rotate to Next Wallpaper Now"),
                )
                .on_hover_text("Rotate desktop to another wallpaper from your library")
                .clicked()
            {
                self.cycle_next_wallpaper(snapshot);
            }
            ui.small("(Hotkey: Alt+N)");
        });

        ui.add_space(6.0);
        ui.separator();
        ui.label("Audio & Ambient Sound");
        ui.small(
            "Master volume and mute control for wallpapers with audio or ambient soundtracks.",
        );
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let mut vol = self.master_volume;
            if ui
                .add(egui::Slider::new(&mut vol, 0..=100).text("Master Volume"))
                .changed()
            {
                self.master_volume = vol;
                self.send(
                    Command::SetVolume {
                        monitor: None,
                        volume: vol,
                    },
                    "Master volume updated…",
                );
            }
            let mut muted = self.master_muted;
            if ui
                .checkbox(&mut muted, "Mute all audio (Hotkey: Alt+M)")
                .changed()
            {
                self.master_muted = muted;
                self.send(
                    Command::SetMute {
                        monitor: None,
                        muted,
                    },
                    if muted {
                        "Muted all audio…"
                    } else {
                        "Unmuted all audio…"
                    },
                );
            }
        });

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
            if ui.button("🚀 Launch Browser with Extension").clicked() {
                match wallpaper_windows::launch_browser_with_extension(None) {
                    Ok(()) => {
                        self.notice = "Browser launched with RustWallpaper companion extension active!".into();
                    }
                    Err(e) => {
                        self.notice = format!("Launch failed: {e}");
                    }
                }
            }

            if ui.button("⚡ Auto-Configure Shortcuts").clicked() {
                match wallpaper_windows::enable_browser_shortcut_integration() {
                    Ok(count) => {
                        self.notice = format!("Injected extension auto-load into {count} browser shortcut(s)! Restart your browser to see it.");
                    }
                    Err(e) => {
                        self.notice = format!("Shortcut configuration error: {e}");
                    }
                }
            }

            if ui.button("🌐 Browser Setup & Info").clicked() {
                let _ = std::process::Command::new("rundll32")
                    .args(["url.dll,FileProtocolHandler", "http://127.0.0.1:24242/extension/install"])
                    .spawn();
            }
        });

        ui.add_space(2.0);
        ui.small("💡 Chromium (Brave/Chrome) requires a browser restart for shortcut or policy changes to activate.");

        ui.separator();
        ui.label("Current implementation");
        ui.label("The daemon owns Win32 desktop integration, monitor lifecycle and MPV processes.");
        ui.label("Playback commands are sent through the versioned named-pipe protocol.");
        ui.separator();
        ui.label("Future Roadmap");
        ui.label("WebView2 interactive HTML/web wallpapers, audio visualizer scenes, and automatic updater.");
    }
}
