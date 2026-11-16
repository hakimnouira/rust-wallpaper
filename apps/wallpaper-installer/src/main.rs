#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallStep {
    Welcome,
    Options,
    Installing,
    Finished,
}

struct InstallerApp {
    step: InstallStep,
    install_dir: String,
    create_desktop_shortcut: bool,
    create_start_menu_shortcut: bool,
    start_on_boot: bool,
    launch_on_finish: bool,
    progress_status: String,
    progress_pct: f32,
    progress_rx: Option<mpsc::Receiver<(f32, String)>>,
}

impl Default for InstallerApp {
    fn default() -> Self {
        let default_dir = get_default_install_dir();
        Self {
            step: InstallStep::Welcome,
            install_dir: default_dir.to_string_lossy().to_string(),
            create_desktop_shortcut: true,
            create_start_menu_shortcut: true,
            start_on_boot: true,
            launch_on_finish: true,
            progress_status: "Ready to install…".into(),
            progress_pct: 0.0,
            progress_rx: None,
        }
    }
}

fn get_default_install_dir() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local_app_data)
            .join("Programs")
            .join("RustWallpaper")
    } else if let Ok(user_profile) = std::env::var("USERPROFILE") {
        PathBuf::from(user_profile).join("RustWallpaper")
    } else {
        PathBuf::from("C:\\RustWallpaper")
    }
}

fn create_shortcut(
    target: &Path,
    shortcut_path: &Path,
    icon_path: Option<&Path>,
    working_dir: &Path,
) -> Result<(), String> {
    if let Some(parent) = shortcut_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let script = format!(
        r#"$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; {}$s.Save()"#,
        shortcut_path.to_string_lossy().replace('\'', "''"),
        target.to_string_lossy().replace('\'', "''"),
        working_dir.to_string_lossy().replace('\'', "''"),
        if let Some(icon) = icon_path {
            format!(
                "$s.IconLocation = '{},0'; ",
                icon.to_string_lossy().replace('\'', "''")
            )
        } else {
            String::new()
        }
    );

    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("Failed to create shortcut: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn register_uninstaller(install_dir: &Path, version: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::System::Registry::{
            RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_WRITE, REG_SZ,
        };

        let subkey = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\RustWallpaper";
        let mut hkey = windows::Win32::System::Registry::HKEY::default();

        unsafe {
            let subkey_h = HSTRING::from(subkey);
            if RegCreateKeyExW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(subkey_h.as_ptr()),
                0,
                None,
                windows::Win32::System::Registry::REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
            .is_err()
            {
                return Err("Failed to create uninstall registry key".into());
            }

            unsafe fn set_sz(
                hkey: windows::Win32::System::Registry::HKEY,
                name: &str,
                value: &str,
            ) {
                let name_h = HSTRING::from(name);
                let val_wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes =
                    std::slice::from_raw_parts(val_wide.as_ptr() as *const u8, val_wide.len() * 2);
                let _ = RegSetValueExW(
                    hkey,
                    windows::core::PCWSTR(name_h.as_ptr()),
                    0,
                    REG_SZ,
                    Some(bytes),
                );
            }

            let exe_path = install_dir.join("RustWallpaper.exe");
            let uninstaller_path = install_dir.join("uninstall.exe");

            set_sz(hkey, "DisplayName", "Rust Wallpaper");
            set_sz(hkey, "DisplayVersion", version);
            set_sz(hkey, "Publisher", "Rust Wallpaper");
            set_sz(hkey, "InstallLocation", &install_dir.to_string_lossy());
            set_sz(
                hkey,
                "DisplayIcon",
                &format!("{},0", exe_path.to_string_lossy()),
            );
            set_sz(
                hkey,
                "UninstallString",
                &format!("\"{}\"", uninstaller_path.to_string_lossy()),
            );
            set_sz(
                hkey,
                "QuietUninstallString",
                &format!("\"{}\" /S", uninstaller_path.to_string_lossy()),
            );

            let _ = windows::Win32::System::Registry::RegCloseKey(hkey);
        }
    }
    Ok(())
}

fn find_file_candidate(names: &[&str]) -> Option<PathBuf> {
    // 1. Next to current installer exe
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for name in names {
                let candidate = parent.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // 2. target/release and target/debug relative to current working dir
    for dir in ["target/release", "target/debug", "dist", "."] {
        for name in names {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn execute_installation(
    target_dir: PathBuf,
    create_desktop: bool,
    create_start_menu: bool,
    start_on_boot: bool,
    tx: mpsc::Sender<(f32, String)>,
) -> Result<PathBuf, String> {
    let _ = tx.send((0.10, "Creating destination folders…".into()));
    let bin_dir = target_dir.join("bin");
    let mpv_dir = bin_dir.join("mpv");
    let assets_dir = target_dir.join("assets");

    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Failed to create install directory: {e}"))?;
    std::fs::create_dir_all(&mpv_dir)
        .map_err(|e| format!("Failed to create bin directory: {e}"))?;
    std::fs::create_dir_all(&assets_dir)
        .map_err(|e| format!("Failed to create assets directory: {e}"))?;

    // 1. Copy RustWallpaper.exe
    let _ = tx.send((0.25, "Installing Rust Wallpaper core executable…".into()));
    let main_exe =
        find_file_candidate(&["RustWallpaper.exe", "wallpaper-ui.exe"]).ok_or_else(|| {
            "Could not find RustWallpaper.exe or wallpaper-ui.exe to install".to_string()
        })?;

    let dest_main_exe = target_dir.join("RustWallpaper.exe");
    std::fs::copy(&main_exe, &dest_main_exe)
        .map_err(|e| format!("Failed to copy RustWallpaper.exe: {e}"))?;

    // Also copy alias wallpaper-ui.exe if needed
    let dest_ui_alias = target_dir.join("wallpaper-ui.exe");
    let _ = std::fs::copy(&main_exe, &dest_ui_alias);

    // 2. Copy wallpaper-daemon.exe if available
    let _ = tx.send((0.40, "Installing daemon companion binary…".into()));
    if let Some(daemon_exe) = find_file_candidate(&["wallpaper-daemon.exe"]) {
        let _ = std::fs::copy(&daemon_exe, target_dir.join("wallpaper-daemon.exe"));
    }

    // 3. Copy or generate uninstall.exe
    let _ = tx.send((0.55, "Installing uninstaller…".into()));
    if let Some(uninstaller_exe) = find_file_candidate(&["uninstall.exe"]) {
        let _ = std::fs::copy(&uninstaller_exe, target_dir.join("uninstall.exe"));
    }

    // 4. Copy assets icon
    let _ = tx.send((0.65, "Installing application icons and branding…".into()));
    if let Some(icon) = find_file_candidate(&["assets/icon.ico", "icon.ico"]) {
        let _ = std::fs::copy(&icon, assets_dir.join("icon.ico"));
    }
    if let Some(icon_png) = find_file_candidate(&["assets/icon.png", "icon.png"]) {
        let _ = std::fs::copy(&icon_png, assets_dir.join("icon.png"));
    }

    // 5. Copy MPV and FFmpeg video playback components
    let _ = tx.send((
        0.75,
        "Configuring hardware-accelerated video engine (MPV)…".into(),
    ));
    let mpv_source_dirs = [
        PathBuf::from("bin").join("mpv"),
        std::env::var("USERPROFILE")
            .map(|u| PathBuf::from(u).join(".mpv"))
            .unwrap_or_default(),
    ];

    for src_dir in &mpv_source_dirs {
        if src_dir.is_dir() {
            for binary_name in ["mpv.exe", "ffmpeg.exe", "d3dcompiler_43.dll"] {
                let src_file = src_dir.join(binary_name);
                if src_file.is_file() {
                    let _ = std::fs::copy(&src_file, mpv_dir.join(binary_name));
                }
            }
        }
    }

    // 6. Create Shortcuts
    let _ = tx.send((0.85, "Creating system shortcuts…".into()));
    let icon_dest = assets_dir.join("icon.ico");
    let icon_opt = if icon_dest.is_file() {
        Some(icon_dest.as_path())
    } else {
        None
    };

    if create_desktop {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let desktop_shortcut = PathBuf::from(userprofile)
                .join("Desktop")
                .join("Rust Wallpaper.lnk");
            let _ = create_shortcut(&dest_main_exe, &desktop_shortcut, icon_opt, &target_dir);
        }
    }

    if create_start_menu {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let start_menu_shortcut = PathBuf::from(appdata)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Rust Wallpaper.lnk");
            let _ = create_shortcut(&dest_main_exe, &start_menu_shortcut, icon_opt, &target_dir);
        }
    }

    // 7. Register Uninstaller in Windows Add/Remove Programs
    let _ = tx.send((0.92, "Registering application in Windows settings…".into()));
    let _ = register_uninstaller(&target_dir, "0.1.0");

    // 8. Register autostart if requested
    if start_on_boot {
        let _ = tx.send((0.96, "Enabling Windows startup launch…".into()));
        #[cfg(windows)]
        {
            let _ = wallpaper_windows::set_autostart(true);
        }
    }

    let _ = tx.send((1.0, "Installation complete!".into()));
    Ok(dest_main_exe)
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(50));

        // Poll background installation worker
        if let Some(ref rx) = self.progress_rx {
            while let Ok((pct, status)) = rx.try_recv() {
                self.progress_pct = pct;
                self.progress_status = status;
                if pct >= 1.0 {
                    self.step = InstallStep::Finished;
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);

            match &self.step {
                InstallStep::Welcome => {
                    ui.vertical_centered(|ui| {
                        ui.heading(
                            egui::RichText::new("Welcome to Rust Wallpaper Setup")
                                .size(24.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Version 0.1.0 (Production Release)")
                                .color(egui::Color32::from_rgb(100, 200, 255)),
                        );
                        ui.add_space(16.0);
                    });

                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(26, 30, 40))
                        .inner_margin(16.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("✨ Features & Highlights:").strong());
                            ui.add_space(6.0);
                            ui.label("• Ultra-low latency, hardware-accelerated looping video wallpaper engine.");
                            ui.label("• Single-executable architecture: runs UI and daemon together seamlessly.");
                            ui.label("• Multi-monitor support with independent per-display playback controls.");
                            ui.label("• Free online curated video loop catalog with 1-click import & apply.");
                            ui.label("• Direct Web Video URL Downloader: paste any .mp4/.webm to set as wallpaper.");
                            ui.label("• Smart resource management: auto-pauses during full-screen games & apps.");
                        });

                    ui.add_space(20.0);
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui
                                .button(egui::RichText::new("Next >").strong())
                                .clicked()
                            {
                                self.step = InstallStep::Options;
                            }
                        });
                        ui.separator();
                    });
                }

                InstallStep::Options => {
                    ui.heading("Installation Options");
                    ui.add_space(10.0);

                    ui.label("Destination Folder:");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.install_dir);
                        if ui.button("📁 Browse…").clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                self.install_dir = folder.display().to_string();
                            }
                        }
                    });
                    ui.small("Rust Wallpaper will be installed into this directory.");

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(egui::RichText::new("Shortcuts & Startup:").strong());
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.create_desktop_shortcut, "Create a Desktop shortcut");
                    ui.checkbox(
                        &mut self.create_start_menu_shortcut,
                        "Create a Start Menu shortcut",
                    );
                    ui.checkbox(
                        &mut self.start_on_boot,
                        "Start Rust Wallpaper automatically on Windows boot",
                    );

                    ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("< Back").clicked() {
                                self.step = InstallStep::Welcome;
                            }
                            if ui
                                .button(
                                    egui::RichText::new("Install")
                                        .color(egui::Color32::from_rgb(0, 255, 160))
                                        .strong(),
                                )
                                .clicked()
                            {
                                let target_path = PathBuf::from(self.install_dir.trim());
                                let desktop_sc = self.create_desktop_shortcut;
                                let start_sc = self.create_start_menu_shortcut;
                                let boot = self.start_on_boot;

                                let (tx, rx) = mpsc::channel();
                                self.progress_rx = Some(rx);
                                self.step = InstallStep::Installing;

                                std::thread::spawn(move || {
                                    match execute_installation(
                                        target_path,
                                        desktop_sc,
                                        start_sc,
                                        boot,
                                        tx.clone(),
                                    ) {
                                        Ok(_) => {}
                                        Err(e) => {
                                            let _ = tx.send((1.0, format!("ERROR: {e}")));
                                        }
                                    }
                                });
                            }
                        });
                        ui.separator();
                    });
                }

                InstallStep::Installing => {
                    ui.heading("Installing Rust Wallpaper…");
                    ui.add_space(20.0);

                    ui.add(
                        egui::ProgressBar::new(self.progress_pct)
                            .show_percentage()
                            .animate(true),
                    );

                    ui.add_space(12.0);
                    ui.label(&self.progress_status);

                    if self.progress_status.starts_with("ERROR:") {
                        ui.add_space(12.0);
                        ui.colored_label(egui::Color32::LIGHT_RED, &self.progress_status);
                        if ui.button("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }

                InstallStep::Finished => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.heading(
                            egui::RichText::new("✔ Installation Finished Successfully!")
                                .color(egui::Color32::from_rgb(0, 255, 160))
                                .size(22.0)
                                .strong(),
                        );
                        ui.add_space(16.0);
                    });

                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(24, 32, 28))
                        .inner_margin(16.0)
                        .show(ui, |ui| {
                            ui.label("Rust Wallpaper has been successfully installed on your PC.");
                            ui.add_space(6.0);
                            ui.label(format!("Installed at: {}", self.install_dir));
                            if self.create_desktop_shortcut {
                                ui.label("✔ Desktop shortcut created.");
                            }
                            if self.create_start_menu_shortcut {
                                ui.label("✔ Start Menu shortcut created.");
                            }
                            if self.start_on_boot {
                                ui.label("✔ Windows autostart enabled.");
                            }
                        });

                    ui.add_space(16.0);
                    ui.checkbox(&mut self.launch_on_finish, "Launch Rust Wallpaper now");

                    ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button(
                                    egui::RichText::new("Finish")
                                        .color(egui::Color32::WHITE)
                                        .strong(),
                                )
                                .clicked()
                            {
                                if self.launch_on_finish {
                                    let target_exe =
                                        PathBuf::from(&self.install_dir).join("RustWallpaper.exe");
                                    let _ = std::process::Command::new(target_exe).spawn();
                                }
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                        ui.separator();
                    });
                }
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let is_silent = args
        .iter()
        .any(|a| a == "/S" || a == "--silent" || a == "-s");

    if is_silent {
        let (tx, _rx) = mpsc::channel();
        let target_dir = get_default_install_dir();
        match execute_installation(target_dir, true, true, true, tx) {
            Ok(exe) => {
                let _ = std::process::Command::new(exe).spawn();
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Silent install failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let icon_data = image::load_from_memory(include_bytes!("../../../assets/icon.png"))
        .ok()
        .map(|img| {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            }
        });

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([580.0, 420.0])
        .with_resizable(false)
        .with_title("Rust Wallpaper Setup");

    if let Some(icon) = icon_data {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Rust Wallpaper Setup",
        options,
        Box::new(|_cc| Ok(Box::new(InstallerApp::default()))),
    )
}
