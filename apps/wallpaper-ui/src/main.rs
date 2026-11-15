//! Native desktop control panel for Rust Wallpaper.
//!
//! The UI is deliberately a protocol client: it never creates wallpaper windows,
//! starts MPV, or mutates daemon runtime state directly.

#[cfg(windows)]
mod desktop {
    use eframe::egui;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::runtime::Builder;
    use tokio::time::timeout;
    use wallpaper_core::{PerformanceProfile, Wallpaper, WallpaperId};
    use wallpaper_protocol::client::IpcClient;
    use wallpaper_protocol::{Command, MonitorInfo, PlayerStatusInfo, RequestEnvelope, Response};

    #[derive(Default, Clone)]
    struct Snapshot {
        connected: bool,
        last_error: Option<String>,
        monitors: Vec<MonitorInfo>,
        wallpapers: Vec<Wallpaper>,
        players: Vec<PlayerStatusInfo>,
        profile: PerformanceProfile,
        last_updated: Option<Instant>,
    }

    #[derive(Clone, Copy)]
    enum Page {
        Library,
        Monitors,
        Settings,
    }

    struct DesktopApp {
        snapshot: Arc<Mutex<Snapshot>>,
        commands: mpsc::Sender<Command>,
        page: Page,
        selected_monitor: Option<String>,
        selected_wallpaper: Option<WallpaperId>,
        import_path: String,
        import_title: String,
        search: String,
        notice: String,
    }

    impl DesktopApp {
        fn new(snapshot: Arc<Mutex<Snapshot>>, commands: mpsc::Sender<Command>) -> Self {
            Self {
                snapshot,
                commands,
                page: Page::Library,
                selected_monitor: None,
                selected_wallpaper: None,
                import_path: String::new(),
                import_title: String::new(),
                search: String::new(),
                notice: "Connect to the wallpaper daemon to get started.".into(),
            }
        }

        fn send(&mut self, command: Command, message: &str) {
            match self.commands.send(command) {
                Ok(()) => self.notice = message.into(),
                Err(error) => self.notice = format!("Could not queue command: {error}"),
            }
        }

        fn render_library(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            ui.heading("Wallpaper library");
            ui.label("Import local videos and assign them to your displays.");
            ui.add_space(12.0);
            ui.group(|ui| {
                ui.label("Import a video");
                ui.horizontal(|ui| {
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
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_title)
                        .hint_text("Optional display name"),
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
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label("Search");
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Filter by name or path"),
                );
            });
            let selected_monitor = self.selected_monitor.clone();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let filtered: Vec<&Wallpaper> = snapshot
                    .wallpapers
                    .iter()
                    .filter(|wallpaper| {
                        let query = self.search.to_lowercase();
                        query.is_empty()
                            || wallpaper.title().to_lowercase().contains(&query)
                            || wallpaper.source().to_lowercase().contains(&query)
                    })
                    .collect();
                if filtered.is_empty() {
                    ui.add_space(18.0);
                    ui.label(if snapshot.connected {
                        "Your library is empty. Import an MP4, WebM, MKV or MOV video to begin."
                    } else {
                        "Waiting for daemon connection…"
                    });
                }
                for wallpaper in filtered {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(wallpaper.title());
                                ui.small(wallpaper.source());
                                let dimensions =
                                    match (wallpaper.metadata.width, wallpaper.metadata.height) {
                                        (Some(w), Some(h)) => format!("{w} × {h}"),
                                        _ => "Metadata not scanned yet".into(),
                                    };
                                ui.small(format!("{:?} · {}", wallpaper.kind, dimensions));
                            });
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
                                        self.send(
                                            Command::ApplyWallpaper {
                                                wallpaper_id: wallpaper.id,
                                                monitor: selected_monitor.clone(),
                                            },
                                            "Wallpaper assignment requested…",
                                        );
                                    }
                                    if ui
                                        .selectable_label(
                                            self.selected_wallpaper == Some(wallpaper.id),
                                            "Select",
                                        )
                                        .clicked()
                                    {
                                        self.selected_wallpaper = Some(wallpaper.id);
                                    }
                                    if ui
                                        .add_enabled(
                                            snapshot.connected,
                                            egui::Button::new("Remove from library"),
                                        )
                                        .clicked()
                                    {
                                        self.send(
                                            Command::RemoveWallpaper {
                                                wallpaper_id: wallpaper.id,
                                            },
                                            "Removal requested…",
                                        );
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(5.0);
                }
            });
        }

        fn render_monitors(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
            ui.heading("Monitor manager");
            ui.label("Each display is controlled through the daemon's runtime supervisor.");
            ui.add_space(12.0);
            if snapshot.monitors.is_empty() {
                ui.label("No monitors reported yet.");
                return;
            }
            for monitor in &snapshot.monitors {
                let device = monitor.device_name.clone();
                let status = snapshot
                    .players
                    .iter()
                    .find(|player| player.monitor == device);
                ui.group(|ui| {
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

        fn render_settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
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
            ui.label("Performance profile");
            ui.horizontal(|ui| {
                for (profile, label) in [
                    (PerformanceProfile::Eco, "Eco"),
                    (PerformanceProfile::Balanced, "Balanced"),
                    (PerformanceProfile::Performance, "Performance"),
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
            ui.separator();
            ui.label("Current implementation");
            ui.label(
                "The daemon owns Win32 desktop integration, monitor lifecycle and MPV processes.",
            );
            ui.label("This UI communicates only through the versioned named-pipe protocol.");
            ui.separator();
            ui.label("Not yet available");
            ui.label("WebView2 wallpapers, interactive scenes, startup settings and updater are future milestones.");
        }
    }

    impl eframe::App for DesktopApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(300));
            let snapshot = self
                .snapshot
                .lock()
                .map(|state| state.clone())
                .unwrap_or_default();
            egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Rust Wallpaper");
                    ui.separator();
                    let (color, label) = if snapshot.connected {
                        (egui::Color32::LIGHT_GREEN, "Daemon connected")
                    } else {
                        (egui::Color32::LIGHT_RED, "Daemon disconnected")
                    };
                    ui.colored_label(color, label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!(
                            "{} monitors · {} wallpapers",
                            snapshot.monitors.len(),
                            snapshot.wallpapers.len()
                        ));
                    });
                });
            });
            egui::SidePanel::left("navigation")
                .resizable(false)
                .default_width(170.0)
                .show(ctx, |ui| {
                    ui.add_space(12.0);
                    if ui
                        .selectable_label(matches!(self.page, Page::Library), "▦  Library")
                        .clicked()
                    {
                        self.page = Page::Library;
                    }
                    if ui
                        .selectable_label(matches!(self.page, Page::Monitors), "▣  Monitors")
                        .clicked()
                    {
                        self.page = Page::Monitors;
                    }
                    if ui
                        .selectable_label(matches!(self.page, Page::Settings), "⚙  Settings")
                        .clicked()
                    {
                        self.page = Page::Settings;
                    }
                    ui.add_space(20.0);
                    ui.separator();
                    ui.label("Assignment target");
                    egui::ComboBox::from_id_salt("target_monitor")
                        .selected_text(
                            self.selected_monitor
                                .clone()
                                .unwrap_or_else(|| "All monitors".into()),
                        )
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.selected_monitor.is_none(), "All monitors")
                                .clicked()
                            {
                                self.selected_monitor = None;
                            }
                            for monitor in &snapshot.monitors {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    Some(monitor.device_name.clone()),
                                    &monitor.device_name,
                                );
                            }
                        });
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.label("v0.1 · Early access");
                    });
                });
            egui::CentralPanel::default().show(ctx, |ui| match self.page {
                Page::Library => self.render_library(ui, &snapshot),
                Page::Monitors => self.render_monitors(ui, &snapshot),
                Page::Settings => self.render_settings(ui, &snapshot),
            });
            egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.notice);
                    if let Some(updated) = snapshot.last_updated {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small(format!("Updated {}s ago", updated.elapsed().as_secs()));
                        });
                    }
                });
            });
        }
    }

    async fn send(client: &mut IpcClient, command: Command) -> Result<Response, String> {
        let request = RequestEnvelope::new(command);
        match timeout(Duration::from_secs(5), client.request(&request)).await {
            Ok(Ok(response)) => match response.result {
                Response::Error { message, .. } => Err(message),
                result => Ok(result),
            },
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("daemon request timed out".into()),
        }
    }

    fn spawn_worker(snapshot: Arc<Mutex<Snapshot>>) -> mpsc::Sender<Command> {
        let (tx, rx) = mpsc::channel::<Command>();
        std::thread::Builder::new()
            .name("wallpaper-ui-ipc".into())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        if let Ok(mut state) = snapshot.lock() {
                            state.last_error = Some(error.to_string());
                        }
                        return;
                    }
                };
                runtime.block_on(async move {
                    let mut client: Option<IpcClient> = None;
                    let mut next_refresh = Instant::now();
                    loop {
                        if client.is_none() {
                            match IpcClient::connect().await {
                                Ok(connection) => {
                                    client = Some(connection);
                                    if let Ok(mut state) = snapshot.lock() {
                                        state.connected = true;
                                        state.last_error = None;
                                    }
                                    next_refresh = Instant::now();
                                }
                                Err(error) => {
                                    if let Ok(mut state) = snapshot.lock() {
                                        state.connected = false;
                                        state.last_error = Some(error.to_string());
                                    }
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    continue;
                                }
                            }
                        }
                        if let Some(connection) = client.as_mut() {
                            let mut disconnected = false;
                            while let Ok(command) = rx.try_recv() {
                                if let Err(error) = send(connection, command).await {
                                    if let Ok(mut state) = snapshot.lock() {
                                        state.last_error = Some(error);
                                    }
                                    disconnected = true;
                                    break;
                                }
                            }
                            if !disconnected && Instant::now() >= next_refresh {
                                for command in [
                                    Command::ListMonitors,
                                    Command::ListWallpapers,
                                    Command::GetStatus { monitor: None },
                                    Command::GetProfile,
                                ] {
                                    match send(connection, command).await {
                                        Ok(Response::MonitorList(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.monitors = items;
                                            }
                                        }
                                        Ok(Response::WallpaperList(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.wallpapers = items;
                                            }
                                        }
                                        Ok(Response::PlayerStatus(items)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.players = items;
                                            }
                                        }
                                        Ok(Response::Profile(profile)) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.profile = profile;
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            if let Ok(mut state) = snapshot.lock() {
                                                state.last_error = Some(error);
                                            }
                                            disconnected = true;
                                            break;
                                        }
                                    }
                                }
                                next_refresh = Instant::now() + Duration::from_secs(2);
                                if let Ok(mut state) = snapshot.lock() {
                                    state.last_updated = Some(Instant::now());
                                }
                            }
                            if disconnected {
                                client = None;
                                if let Ok(mut state) = snapshot.lock() {
                                    state.connected = false;
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                });
            })
            .expect("failed to start UI IPC worker");
        tx
    }

    pub fn run() -> eframe::Result<()> {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let commands = spawn_worker(snapshot.clone());
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size([860.0, 560.0]),
            ..Default::default()
        };
        eframe::run_native(
            "Rust Wallpaper",
            options,
            Box::new(move |_creation_context| Ok(Box::new(DesktopApp::new(snapshot, commands)))),
        )
    }
}

#[cfg(windows)]
fn main() -> eframe::Result<()> {
    desktop::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("wallpaper-ui is currently a Windows desktop application.");
}
