//! Multi-monitor wallpaper window manager and orchestrator.
//!
//! Manages wallpaper windows across all connected display monitors, orchestrates
//! desktop host discovery, supervises isolated video player processes per display,
//! handles display configuration updates, and recovers automatically if Windows Explorer
//! restarts.

use crate::desktop_host::{ensure_desktop_host, DesktopHost, DesktopHostError};
use crate::enumerate_monitors;
use crate::mpv::{MpvError, MpvLaunchConfig, MpvPlayer};
use crate::policy::ProcessBackoff;
use crate::window::{WallpaperWindow, WindowError};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, info, warn};
use wallpaper_core::{PlaybackConfig, PlaybackState, PlayerTelemetry};

#[derive(Debug, Error)]
pub enum WallpaperManagerError {
    #[error("desktop host error: {0}")]
    DesktopHost(#[from] DesktopHostError),
    #[error("window error: {0}")]
    Window(#[from] WindowError),
    #[error("player error: {0}")]
    Player(#[from] MpvError),
    #[error("monitor enumeration error: {0}")]
    MonitorError(String),
    #[error("monitor not found: {0}")]
    MonitorNotFound(String),
}

/// Palette of background colors for different displays.
const MONITOR_PALETTE: &[(u8, u8, u8)] = &[
    (24, 32, 47), // Deep navy slate
    (22, 40, 48), // Dark slate teal
    (38, 28, 44), // Dark plum slate
    (32, 42, 30), // Dark forest slate
];

#[cfg(windows)]
fn color_to_colorref(r: u8, g: u8, b: u8) -> windows::Win32::Foundation::COLORREF {
    windows::Win32::Foundation::COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}

pub struct WallpaperManager {
    host: DesktopHost,
    windows: HashMap<String, WallpaperWindow>,
    players: HashMap<String, MpvPlayer>,
    active_sources: HashMap<String, (PathBuf, PlaybackConfig)>,
    backoffs: HashMap<String, ProcessBackoff>,
}

impl WallpaperManager {
    /// Initializes desktop host discovery and attaches wallpaper windows to all active monitors.
    pub fn init() -> Result<Self, WallpaperManagerError> {
        info!("Initializing desktop wallpaper manager");

        let host = ensure_desktop_host()?;
        let mut manager = Self {
            host,
            windows: HashMap::new(),
            players: HashMap::new(),
            active_sources: HashMap::new(),
            backoffs: HashMap::new(),
        };

        manager.attach_all_monitors()?;
        manager.pump_messages();
        Ok(manager)
    }

    /// Attaches or synchronizes wallpaper windows for all detected monitors.
    fn attach_all_monitors(&mut self) -> Result<(), WallpaperManagerError> {
        let monitors =
            enumerate_monitors().map_err(|e| WallpaperManagerError::MonitorError(e.to_string()))?;

        debug!(
            count = monitors.len(),
            "Synchronizing wallpaper windows across monitors"
        );

        #[cfg(windows)]
        {
            let mut active_devices = std::collections::HashSet::new();

            for (idx, monitor) in monitors.into_iter().enumerate() {
                active_devices.insert(monitor.device_name.clone());

                let (client_x, client_y) = self.host.screen_to_client(monitor.x, monitor.y);
                let (r, g, b) = MONITOR_PALETTE[idx % MONITOR_PALETTE.len()];
                let bg_color = color_to_colorref(r, g, b);

                if let Some(existing) = self.windows.get_mut(&monitor.device_name) {
                    if existing.client_x != client_x
                        || existing.client_y != client_y
                        || existing.width != monitor.width
                        || existing.height != monitor.height
                    {
                        info!(device = %monitor.device_name, "Updating monitor geometry");
                        existing.update_geometry(client_x, client_y, monitor.width, monitor.height);
                    }
                } else {
                    info!(
                        device = %monitor.device_name,
                        primary = monitor.is_primary,
                        client_x,
                        client_y,
                        width = monitor.width,
                        height = monitor.height,
                        "Creating wallpaper window for monitor"
                    );

                    let win = WallpaperWindow::create(
                        self.host.hwnd,
                        monitor.device_name.clone(),
                        client_x,
                        client_y,
                        monitor.width,
                        monitor.height,
                        monitor.is_primary,
                        bg_color,
                    )?;

                    self.windows.insert(monitor.device_name, win);
                }
            }

            // Remove disconnected displays and stop their players
            self.windows
                .retain(|device, _| active_devices.contains(device));
            self.players
                .retain(|device, _| active_devices.contains(device));
        }

        Ok(())
    }

    /// Plays a video wallpaper on a specific monitor.
    pub fn play_video_on_monitor(
        &mut self,
        device: &str,
        video_path: PathBuf,
        config: PlaybackConfig,
    ) -> Result<(), WallpaperManagerError> {
        let win = self
            .windows
            .get(device)
            .ok_or_else(|| WallpaperManagerError::MonitorNotFound(device.to_string()))?;

        let wid = win.hwnd.0 as isize;
        self.pump_messages();
        info!(
            device = %device,
            video = %video_path.display(),
            wid = format_args!("0x{:X}", wid),
            "Starting video wallpaper on monitor"
        );

        let launch_cfg = MpvLaunchConfig {
            video_path: video_path.clone(),
            wid,
            config: config.clone(),
        };

        let player = MpvPlayer::launch(launch_cfg)?;
        self.players.insert(device.to_string(), player);
        self.active_sources
            .insert(device.to_string(), (video_path, config));

        Ok(())
    }

    /// Plays the same video wallpaper across all detected monitors.
    pub fn play_video_on_all(
        &mut self,
        video_path: PathBuf,
        config: PlaybackConfig,
    ) -> Result<(), WallpaperManagerError> {
        let devices: Vec<String> = self.windows.keys().cloned().collect();
        for dev in devices {
            self.play_video_on_monitor(&dev, video_path.clone(), config.clone())?;
        }
        Ok(())
    }

    /// Pauses all active video players.
    pub fn pause_all(&mut self) {
        for (dev, player) in &mut self.players {
            if let Err(err) = player.pause() {
                warn!(device = %dev, %err, "Failed to pause video player");
            }
        }
    }

    /// Resumes all paused video players.
    pub fn resume_all(&mut self) {
        for (dev, player) in &mut self.players {
            if let Err(err) = player.resume() {
                warn!(device = %dev, %err, "Failed to resume video player");
            }
        }
    }

    /// Sets volume (0-100) across all players.
    pub fn set_volume_all(&mut self, volume: u32) {
        for (dev, player) in &mut self.players {
            if let Err(err) = player.set_volume(volume) {
                warn!(device = %dev, %err, "Failed to set volume");
            }
        }
    }

    /// Toggles mute across all players.
    pub fn set_mute_all(&mut self, muted: bool) {
        for (dev, player) in &mut self.players {
            if let Err(err) = player.set_mute(muted) {
                warn!(device = %dev, %err, "Failed to set mute");
            }
        }
    }

    /// Stops all video players.
    pub fn stop_all(&mut self) {
        for (dev, player) in &mut self.players {
            let _ = player.stop();
            info!(device = %dev, "Stopped video player");
        }
        self.players.clear();
    }

    /// Returns a list of active players with their diagnostic metrics.
    pub fn player_statuses(&self) -> Vec<(String, String, PlaybackState, u64)> {
        let mut list = Vec::new();
        for (dev, player) in &self.players {
            list.push((
                dev.clone(),
                player.active_decoder().to_string(),
                player.state(),
                player.get_memory_bytes(),
            ));
        }
        list
    }

    /// Checks if a player for a specific monitor is running and recovers if crashed.
    pub fn check_and_recover_players(&mut self) {
        let devices: Vec<String> = self.windows.keys().cloned().collect();

        for dev in devices {
            let needs_restart = match self.players.get_mut(&dev) {
                Some(player) => player.uptime_secs() >= 1 && !player.is_alive(),
                None => self.active_sources.contains_key(&dev),
            };

            if needs_restart {
                let backoff = self.backoffs.entry(dev.clone()).or_default();
                if backoff.can_retry() {
                    warn!(device = %dev, attempt = backoff.consecutive_failures + 1, "Player dead or missing. Attempting recovery...");
                    if backoff.record_failure() {
                        if let Some((path, cfg)) = self.active_sources.get(&dev).cloned() {
                            let _ = self.play_video_on_monitor(&dev, path, cfg);
                        }
                    } else {
                        warn!(device = %dev, "Max retry limit reached. Backing off recovery until manual intervention.");
                    }
                }
            } else if let Some(player) = self.players.get_mut(&dev) {
                if player.is_alive() {
                    if let Some(backoff) = self.backoffs.get_mut(&dev) {
                        if backoff.consecutive_failures > 0 {
                            backoff.record_success();
                            info!(device = %dev, "Player recovered successfully; reset backoff.");
                        }
                    }
                }
            }
        }
    }

    /// Returns structured telemetry for all active players.
    pub fn collect_telemetry(&mut self) -> Vec<PlayerTelemetry> {
        let mut metrics = Vec::new();

        for (dev, player) in &mut self.players {
            let (w, h, fps, gpu) = player.query_video_details();
            metrics.push(PlayerTelemetry {
                device_name: dev.clone(),
                active_decoder: player.active_decoder().to_string(),
                state: player.state(),
                memory_working_set_bytes: player.get_memory_bytes(),
                uptime_secs: player.uptime_secs(),
                video_width: w,
                video_height: h,
                video_fps: fps,
                gpu_context: gpu,
            });
        }

        metrics
    }

    /// Checks host health and display configuration, recovering if Explorer restarted.
    pub fn refresh(&mut self) -> Result<(), WallpaperManagerError> {
        #[cfg(windows)]
        {
            if !self.host.is_valid() {
                warn!("Desktop host window is invalid (Explorer may have restarted). Re-discovering...");
                // Stop existing players before recreating windows
                self.stop_all();
                self.windows.clear();

                // Re-discover fresh host
                self.host = ensure_desktop_host()?;
                info!("Re-discovered new desktop host after Explorer restart");

                // Re-attach windows
                self.attach_all_monitors()?;

                // Re-launch players on active sources
                let sources = self.active_sources.clone();
                for (dev, (path, cfg)) in sources {
                    if self.windows.contains_key(&dev) {
                        info!(device = %dev, "Restoring video wallpaper after host recovery");
                        let _ = self.play_video_on_monitor(&dev, path, cfg);
                    }
                }
            } else {
                let _ = self.host.update_rect();
                self.attach_all_monitors()?;
            }
        }

        Ok(())
    }

    /// Pumps pending Win32 messages without blocking.
    pub fn pump_messages(&self) -> bool {
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
            };

            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return false;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        true
    }

    /// Returns a reference to the active desktop host.
    pub fn host(&self) -> &DesktopHost {
        &self.host
    }

    /// Returns the number of active wallpaper windows.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Returns the number of active video players.
    pub fn player_count(&self) -> usize {
        self.players.len()
    }
}

impl Drop for WallpaperManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
