use super::*;

impl DesktopApp {
    pub(super) fn render_extension(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.heading("Browser Companion Extension");
        ui.add_space(4.0);
        ui.small(
            "Manage the RustWallpaper Companion browser extension for Chrome, Edge, and Brave. \
             The extension adds a 'Set as Wallpaper' button to videos on Pinterest, TikTok, \
             Steam Workshop, Reddit, and more.",
        );
        ui.add_space(10.0);

        let ext = &snapshot.extension_status;

        if !ext.fetched {
            ui.label("Loading extension status...");
            return;
        }

        // --- Main toggle: Enable / Disable ---
        let enabled = ext.registered;
        let toggle_label = if enabled {
            "Disable Extension"
        } else {
            "Enable Extension"
        };
        let toggle_color = if enabled {
            egui::Color32::from_rgb(220, 80, 80)
        } else {
            accent_color(&self.theme)
        };

        egui::Frame::group(ui.style())
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Status indicator
                    let (dot_color, status_text) = if enabled {
                        (egui::Color32::from_rgb(52, 211, 153), "Extension Active")
                    } else {
                        (egui::Color32::from_rgb(156, 163, 175), "Extension Disabled")
                    };
                    ui.colored_label(dot_color, "●");
                    ui.label(egui::RichText::new(status_text).strong());

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(toggle_label).color(egui::Color32::WHITE),
                                )
                                .fill(toggle_color),
                            )
                            .clicked()
                        {
                            if enabled {
                                // Disable via IPC
                                self.send(
                                    Command::SetExtensionEnabled { enabled: false },
                                    "Disabling browser extension...",
                                );
                            } else {
                                self.send(
                                    Command::SetExtensionEnabled { enabled: true },
                                    "Enabling browser extension...",
                                );
                            }
                        }
                    });
                });

                ui.add_space(4.0);
                if enabled {
                    ui.small(
                        "The extension is registered. When you launch your browser, accept the \
                         Chromium prompt to enable it.",
                    );
                } else {
                    ui.small(
                        "The extension is not registered. Enable it to add wallpaper buttons to \
                         supported websites.",
                    );
                }
            });

        ui.add_space(10.0);
        ui.separator();

        // --- Detected Browsers ---
        ui.label(egui::RichText::new("Detected Browsers").strong());
        ui.add_space(4.0);

        if ext.browsers.is_empty() {
            ui.small("No supported Chromium browsers detected.");
        } else {
            for browser in &ext.browsers {
                ui.horizontal(|ui| {
                    // Browser icon
                    let icon = match browser.name.as_str() {
                        "Chrome" => "🌐",
                        "Edge" => "🔵",
                        "Brave" => "🦁",
                        _ => "🌐",
                    };
                    ui.label(icon);

                    // Name + install status
                    ui.label(&browser.name);
                    if browser.installed {
                        ui.colored_label(egui::Color32::from_rgb(100, 200, 120), "Installed");
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(156, 163, 175), "Not found");
                    }

                    // Registration status
                    ui.separator();
                    if browser.registered {
                        ui.colored_label(
                            egui::Color32::from_rgb(52, 211, 153),
                            "✔ Extension registered",
                        );
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(245, 158, 11), "⚠ Not registered");
                    }
                });
            }
        }

        ui.add_space(10.0);
        ui.separator();

        // --- Quick Actions ---
        ui.label(egui::RichText::new("Quick Actions").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    ext.registered,
                    egui::Button::new("🚀 Launch Browser with Extension"),
                )
                .on_hover_text("Opens your default Chromium browser with the extension loaded")
                .on_disabled_hover_text("Enable the extension first")
                .clicked()
            {
                match wallpaper_windows::launch_browser_with_extension(None) {
                    Ok(()) => {
                        self.notice =
                            "Browser launched with the RustWallpaper companion extension active!"
                                .into();
                    }
                    Err(e) => {
                        self.notice = format!("Launch failed: {e}");
                    }
                }
            }

            if ui
                .add_enabled(
                    ext.registered,
                    egui::Button::new("⚡ Auto-Configure Shortcuts"),
                )
                .on_hover_text(
                    "Injects --load-extension into your browser shortcuts for automatic loading",
                )
                .on_disabled_hover_text("Enable the extension first")
                .clicked()
            {
                match wallpaper_windows::enable_browser_shortcut_integration() {
                    Ok(count) => {
                        self.notice = format!(
                            "Injected extension auto-load into {count} browser shortcut(s). \
                             Restart your browser to activate."
                        );
                    }
                    Err(e) => {
                        self.notice = format!("Shortcut configuration error: {e}");
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            if ui
                .button("🌐 Open Setup & Info Page")
                .on_hover_text("Opens the extension install page in your default browser")
                .clicked()
            {
                let mut cmd = std::process::Command::new("rundll32");
                cmd.args([
                    "url.dll,FileProtocolHandler",
                    "http://127.0.0.1:24242/extension/install",
                ]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x08000000;
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
                let _ = cmd.spawn();
            }

            if ui
                .add_enabled(ext.crx_available, egui::Button::new("📦 Open CRX Location"))
                .on_hover_text("Opens the folder containing the CRX package in File Explorer")
                .on_disabled_hover_text("CRX package not found on disk")
                .clicked()
            {
                if let Some(path_str) = &ext.crx_path {
                    let path = std::path::PathBuf::from(path_str);
                    if let Some(parent) = path.parent() {
                        let _ = std::process::Command::new("explorer.exe")
                            .arg(parent)
                            .spawn();
                    }
                }
            }
        });

        ui.add_space(10.0);
        ui.separator();

        // --- Package Status ---
        ui.label(egui::RichText::new("Package Status").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("CRX Package:");
            if ext.crx_available {
                ui.colored_label(egui::Color32::from_rgb(52, 211, 153), "✔ Available");
                if let Some(path) = &ext.crx_path {
                    ui.small(format!("({})", path));
                }
            } else {
                ui.colored_label(egui::Color32::from_rgb(239, 68, 68), "✖ Missing");
            }
        });

        ui.horizontal(|ui| {
            ui.label("Unpacked Extension:");
            if ext.unpacked_available {
                ui.colored_label(egui::Color32::from_rgb(52, 211, 153), "✔ Available");
            } else {
                ui.colored_label(egui::Color32::from_rgb(245, 158, 11), "⚠ Not found");
            }
        });

        ui.horizontal(|ui| {
            ui.label("Extension ID:");
            ui.small(&ext.extension_id);
        });

        ui.horizontal(|ui| {
            ui.label("Version:");
            ui.small(&ext.version);
        });

        ui.add_space(10.0);
        ui.separator();

        // --- Troubleshooting ---
        ui.label(egui::RichText::new("Troubleshooting").strong());
        ui.add_space(4.0);

        ui.vertical(|ui| {
            ui.small("• If the extension doesn't appear in your browser:");
            ui.small("    1. Restart your browser completely (all windows)");
            ui.small("    2. Check chrome://extensions for 'RustWallpaper Companion'");
            ui.small("    3. If you see it disabled, click 'Enable extension'");
            ui.add_space(4.0);

            ui.small("• If the 'Set as Wallpaper' button doesn't work:");
            ui.small("    1. Make sure RustWallpaper.exe is running (check tray icon)");
            ui.small("    2. The popup shows a green health dot when connected");
            ui.small("    3. Try right-clicking the video and using the context menu");
            ui.add_space(4.0);

            ui.small("• If blob: URLs appear (YouTube, Twitter):");
            ui.small("    The extension captures real stream URLs in the background.");
            ui.small("    If it still fails, use the context menu 'Set as Wallpaper' option.");
            ui.add_space(4.0);

            ui.small("• Pinterest videos:");
            ui.small("    The extension auto-detects Pinterest video CDN streams.");
            ui.small("    If a pin doesn't load, navigate to the pin page and retry.");
        });

        ui.add_space(6.0);
        ui.separator();

        // --- Connection ---
        ui.label(egui::RichText::new("Connection").strong());
        ui.add_space(4.0);
        if snapshot.connected {
            ui.colored_label(
                egui::Color32::from_rgb(52, 211, 153),
                "● Connected to wallpaper daemon",
            );
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(239, 68, 68),
                "● Disconnected from daemon",
            );
        }
    }
}
