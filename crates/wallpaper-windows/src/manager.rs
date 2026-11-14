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
    #[error("video file not found or inaccessible: {path} ({error})")]
    VideoFileInaccessible { path: PathBuf, error: String },
    #[error("failed to stop existing player on monitor '{device}': {source}")]
    PlayerShutdownFailed {
        device: String,
        #[source]
        source: MpvError,
    },
    #[error(
        "operation failed ({original}) and window attachment recovery also failed: {recovery}"
    )]
    AttachmentRecoveryFailed {
        original: String,
        recovery: WindowError,
    },
}

/// Summary of results from a global shutdown operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownSummary {
    /// Device names of players that were successfully terminated and removed.
    pub stopped: Vec<String>,
    /// Device names and corresponding error messages for players that failed to terminate.
    pub failed: Vec<(String, String)>,
}

impl ShutdownSummary {
    /// Returns true if all players terminated successfully without errors.
    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }
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
        // 1. Explicit video-file existence and accessibility validation before modifying window state
        if let Err(err) = std::fs::File::open(&video_path) {
            return Err(WallpaperManagerError::VideoFileInaccessible {
                path: video_path,
                error: err.to_string(),
            });
        }

        let win = self
            .windows
            .get(device)
            .ok_or_else(|| WallpaperManagerError::MonitorNotFound(device.to_string()))?;

        // 2. Stop any existing player on this monitor; report failure and preserve manager state
        if let Some(existing_player) = self.players.get_mut(device) {
            if let Err(source) = existing_player.stop() {
                return Err(WallpaperManagerError::PlayerShutdownFailed {
                    device: device.to_string(),
                    source,
                });
            }
        }
        self.players.remove(device);
        self.active_sources.remove(device);

        // 3. Temporarily detach window to top-level popup so MPV can embed without cross-process child restrictions
        if let Err(detach_err) = win.detach_from_host() {
            if let Err(recov_err) = win.attach_to_host() {
                return Err(WallpaperManagerError::AttachmentRecoveryFailed {
                    original: format!("detach_from_host failed: {detach_err}"),
                    recovery: recov_err,
                });
            }
            return Err(WallpaperManagerError::Window(detach_err));
        }

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

        let mut player = match MpvPlayer::launch(launch_cfg) {
            Ok(p) => p,
            Err(e) => {
                if let Err(recov_err) = win.attach_to_host() {
                    return Err(WallpaperManagerError::AttachmentRecoveryFailed {
                        original: format!("MpvPlayer::launch failed: {e}"),
                        recovery: recov_err,
                    });
                }
                return Err(WallpaperManagerError::Player(e));
            }
        };

        // 4. Ensure window is in correct bottom z-order behind desktop icons
        if let Err(attach_err) = win.attach_to_host() {
            let stop_res = player.stop();
            if let Err(recov_err) = win.attach_to_host() {
                let stop_msg = match stop_res {
                    Ok(()) => String::new(),
                    Err(e) => format!(" (player termination also failed: {e})"),
                };
                return Err(WallpaperManagerError::AttachmentRecoveryFailed {
                    original: format!("attach_to_host failed: {attach_err}{stop_msg}"),
                    recovery: recov_err,
                });
            }
            return Err(WallpaperManagerError::Window(attach_err));
        }

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

    /// Pauses a specific video player.
    pub fn pause_monitor(&mut self, device: &str) {
        if let Some(player) = self.players.get_mut(device) {
            if let Err(err) = player.pause() {
                warn!(device = %device, %err, "Failed to pause video player on monitor");
            }
        }
    }

    /// Resumes a specific video player.
    pub fn resume_monitor(&mut self, device: &str) {
        if let Some(player) = self.players.get_mut(device) {
            if let Err(err) = player.resume() {
                warn!(device = %device, %err, "Failed to resume video player on monitor");
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

    /// Stops all video players, retaining any failed players in tracking.
    ///
    /// Clears active sources to prevent unintended auto-restarts, attempts to stop each
    /// player, removes only successfully terminated players from tracking, and logs a
    /// clear shutdown summary with affected monitor IDs.
    pub fn stop_all(&mut self) -> ShutdownSummary {
        // Clear active sources to prevent unintended automatic restarts after an explicit stop
        self.active_sources.clear();

        let mut summary = ShutdownSummary::default();

        for (dev, player) in &mut self.players {
            match player.stop() {
                Ok(()) => {
                    info!(device = %dev, "Stopped video player");
                    summary.stopped.push(dev.clone());
                }
                Err(err) => {
                    warn!(device = %dev, %err, "Failed to stop video player");
                    summary.failed.push((dev.clone(), err.to_string()));
                }
            }
        }

        // Remove only successfully stopped players from tracking.
        // Failed players remain in self.players so they stay observable and can be retried.
        for dev in &summary.stopped {
            self.players.remove(dev);
        }

        let failed_ids: Vec<&str> = summary.failed.iter().map(|(d, _)| d.as_str()).collect();
        if summary.failed.is_empty() {
            info!(
                stopped_count = summary.stopped.len(),
                stopped_monitors = ?summary.stopped,
                "Global shutdown completed successfully for all monitors"
            );
        } else {
            warn!(
                stopped_count = summary.stopped.len(),
                failed_count = summary.failed.len(),
                failed_monitors = ?failed_ids,
                "Global shutdown completed with failures; retained failed players in tracking"
            );
        }

        summary
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
            // Only check/recover if there is an active configured source for this device
            if !self.active_sources.contains_key(&dev) {
                continue;
            }

            let needs_restart = match self.players.get_mut(&dev) {
                Some(player) => player.uptime_secs() >= 1 && !player.is_alive(),
                None => true,
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
                self.force_recover_explorer()?;
            } else {
                let _ = self.host.update_rect();
                self.attach_all_monitors()?;
            }
        }

        Ok(())
    }

    /// Explicitly handles Explorer restart notification: tears down players and re-attaches to the new WorkerW.
    pub fn force_recover_explorer(&mut self) -> Result<(), WallpaperManagerError> {
        #[cfg(windows)]
        {
            warn!("Explorer restart detected: tearing down existing MPV players and windows");
            let saved_sources = self.active_sources.clone();
            self.stop_all();
            self.windows.clear();

            // Re-discover fresh host
            self.host = ensure_desktop_host()?;
            info!(
                selected_host = format_args!("0x{:X}", self.host.hwnd.0 as isize),
                "Re-discovered new desktop host after Explorer restart"
            );

            // Re-attach windows
            self.attach_all_monitors()?;

            // Re-launch players on active sources
            for (dev, (path, cfg)) in saved_sources {
                if self.windows.contains_key(&dev) {
                    info!(device = %dev, "Restoring video wallpaper on recreated host window");
                    let _ = self.play_video_on_monitor(&dev, path, cfg);
                }
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

    /// Returns a reference to the wallpaper window for a specific monitor device.
    pub fn window(&self, device: &str) -> Option<&WallpaperWindow> {
        self.windows.get(device)
    }

    /// Returns list of active monitor device names.
    pub fn active_devices(&self) -> Vec<String> {
        self.windows.keys().cloned().collect()
    }

    /// Returns the number of active wallpaper windows.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Returns the number of active video players.
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Returns a reference to the active video player for a specific monitor device.
    pub fn player(&self, device: &str) -> Option<&MpvPlayer> {
        self.players.get(device)
    }

    /// Returns a mutable reference to the active video player for a specific monitor device.
    pub fn player_mut(&mut self, device: &str) -> Option<&mut MpvPlayer> {
        self.players.get_mut(device)
    }

    /// Returns whether a monitor device has an active video source configured.
    pub fn has_active_source(&self, device: &str) -> bool {
        self.active_sources.contains_key(device)
    }

    /// Returns the number of configured active video sources.
    pub fn active_source_count(&self) -> usize {
        self.active_sources.len()
    }
}

impl Drop for WallpaperManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
