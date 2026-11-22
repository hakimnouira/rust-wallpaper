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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{debug, error, info, warn};
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
    #[error(
        "Explorer recovery completed with partial failure: failed to stop or recover monitors: {failed_monitors:?}"
    )]
    ExplorerRecoveryPartial {
        failed_monitors: Vec<String>,
        errors: Vec<(String, String)>,
    },
    #[error("Monitor synchronization completed with failure on monitors: {failed_monitors:?}")]
    MonitorSyncFailed {
        failed_monitors: Vec<String>,
        errors: Vec<(String, String)>,
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

/// Virtual display device name representing all monitors spanned into a single canvas.
pub const SPAN_DEVICE_NAME: &str = r"\\.\SPAN";

#[cfg(windows)]
fn color_to_colorref(r: u8, g: u8, b: u8) -> windows::Win32::Foundation::COLORREF {
    windows::Win32::Foundation::COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}

pub struct WallpaperManager {
    host: DesktopHost,
    windows: HashMap<String, WallpaperWindow>,
    players: HashMap<String, MpvPlayer>,
    web_players: HashMap<String, crate::web_player::WebPlayer>,
    active_sources: HashMap<String, (PathBuf, PlaybackConfig)>,
    backoffs: HashMap<String, ProcessBackoff>,
    disconnected_failed_players: HashSet<String>,
    #[doc(hidden)]
    test_monitors: Option<Vec<crate::MonitorInfo>>,
    #[doc(hidden)]
    lifecycle_recorder: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
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
            web_players: HashMap::new(),
            active_sources: HashMap::new(),
            backoffs: HashMap::new(),
            disconnected_failed_players: HashSet::new(),
            test_monitors: None,
            lifecycle_recorder: None,
        };

        manager.attach_all_monitors()?;
        manager.pump_messages();
        Ok(manager)
    }

    /// Attaches or synchronizes wallpaper windows for all detected monitors.
    fn attach_all_monitors(&mut self) -> Result<(), WallpaperManagerError> {
        let monitors = if let Some(test_mons) = &self.test_monitors {
            test_mons.clone()
        } else {
            enumerate_monitors().map_err(|e| WallpaperManagerError::MonitorError(e.to_string()))?
        };

        debug!(
            count = monitors.len(),
            "Synchronizing wallpaper windows across monitors"
        );

        #[cfg(windows)]
        {
            let mut active_devices = std::collections::HashSet::new();
            let mut sync_failures = Vec::new();

            for (idx, monitor) in monitors.into_iter().enumerate() {
                active_devices.insert(monitor.device_name.clone());

                let dev = monitor.device_name.clone();
                let (client_x, client_y) = self.host.screen_to_client(monitor.x, monitor.y);
                let (r, g, b) = MONITOR_PALETTE[idx % MONITOR_PALETTE.len()];
                let bg_color = color_to_colorref(r, g, b);

                // 1. Reconcile reconnected monitor that had a failed player on earlier disconnect
                if self.disconnected_failed_players.contains(&dev) {
                    warn!(
                        device = %dev,
                        "Reconnected monitor had a retained failed player; attempting shutdown before restoring"
                    );
                    let has_player = self.players.contains_key(&dev);
                    if has_player {
                        self.record_lifecycle_event(&format!("player_stop_attempt:{dev}"));
                    }
                    let mut player_stopped_cleanly = true;
                    if let Some(res) = self.players.get_mut(&dev).map(|p| p.stop()) {
                        match res {
                            Ok(()) => {
                                self.record_lifecycle_event(&format!("player_stop_success:{dev}"));
                                info!(device = %dev, "Retained player on reconnected monitor stopped successfully");
                                self.players.remove(&dev);
                                self.disconnected_failed_players.remove(&dev);
                            }
                            Err(e) => {
                                self.record_lifecycle_event(&format!("player_stop_fail:{dev}"));
                                warn!(
                                    device = %dev,
                                    %e,
                                    "Failed to stop retained player on reconnected monitor; skipping playback to prevent duplicate"
                                );
                                sync_failures.push((
                                    dev.clone(),
                                    format!("Failed to stop retained player on reconnected monitor: {e}"),
                                ));
                                player_stopped_cleanly = false;
                            }
                        }
                    } else {
                        self.disconnected_failed_players.remove(&dev);
                    }

                    if !player_stopped_cleanly {
                        self.record_lifecycle_event(&format!("window_preserved:{dev}"));
                        continue;
                    }

                    // Shutdown succeeded: clean up the old preserved window
                    if self.windows.remove(&dev).is_some() {
                        self.record_lifecycle_event(&format!("window_destroy:{dev}"));
                    }

                    // Reconnection must create a valid new window
                    let win = WallpaperWindow::create(
                        self.host.hwnd,
                        dev.clone(),
                        client_x,
                        client_y,
                        monitor.width,
                        monitor.height,
                        monitor.is_primary,
                        bg_color,
                    )?;
                    win.attach_to_host()?;
                    self.windows.insert(dev.clone(), win);

                    // Restore playback from preserved source/configuration if one exists
                    if let Some((path, cfg)) = self.active_sources.get(&dev).cloned() {
                        info!(device = %dev, "Reconnected monitor: restoring preserved wallpaper playback");
                        if let Err(e) = self.play_video_on_monitor(&dev, path, cfg) {
                            error!(
                                device = %dev,
                                %e,
                                "Failed to restore playback on reconnected monitor"
                            );
                            sync_failures.push((
                                dev.clone(),
                                format!("Failed to restore playback on reconnected monitor: {e}"),
                            ));
                        }
                    }
                    continue;
                }

                // 2. Detect stale wallpaper HWND
                let is_stale = self
                    .windows
                    .get(&dev)
                    .map(|w| !w.is_valid())
                    .unwrap_or(false);

                if is_stale {
                    warn!(
                        device = %dev,
                        "Existing wallpaper window has become invalid or stale; reconciling player and window"
                    );

                    // Reconcile MPV player associated with this stale monitor
                    let has_player = self.players.contains_key(&dev);
                    if has_player {
                        self.record_lifecycle_event(&format!("player_stop_attempt:{dev}"));
                    }
                    let mut player_stopped_cleanly = true;
                    if let Some(res) = self.players.get_mut(&dev).map(|p| p.stop()) {
                        match res {
                            Ok(()) => {
                                self.record_lifecycle_event(&format!("player_stop_success:{dev}"));
                                info!(device = %dev, "Stopped MPV player associated with stale window");
                                self.players.remove(&dev);
                            }
                            Err(e) => {
                                self.record_lifecycle_event(&format!("player_stop_fail:{dev}"));
                                warn!(
                                    device = %dev,
                                    %e,
                                    "Failed to stop MPV player on stale window; retaining player tracking"
                                );
                                sync_failures.push((
                                    dev.clone(),
                                    format!("Failed to stop player on stale window: {e}"),
                                ));
                                player_stopped_cleanly = false;
                            }
                        }
                    }

                    if !player_stopped_cleanly {
                        self.record_lifecycle_event(&format!("window_preserved:{dev}"));
                        continue;
                    }

                    // Player shutdown succeeded (or no player was running): remove stale window
                    if self.windows.remove(&dev).is_some() {
                        self.record_lifecycle_event(&format!("window_destroy:{dev}"));
                    }

                    // Recreate wallpaper window
                    let win = WallpaperWindow::create(
                        self.host.hwnd,
                        dev.clone(),
                        client_x,
                        client_y,
                        monitor.width,
                        monitor.height,
                        monitor.is_primary,
                        bg_color,
                    )?;
                    win.attach_to_host()?;
                    self.windows.insert(dev.clone(), win);

                    // Restore playback from preserved source/configuration if one exists
                    if let Some((path, cfg)) = self.active_sources.get(&dev).cloned() {
                        info!(device = %dev, "Restoring playback on recreated wallpaper window");
                        if let Err(e) = self.play_video_on_monitor(&dev, path, cfg) {
                            error!(
                                device = %dev,
                                %e,
                                "Failed to restore playback on recreated wallpaper window"
                            );
                            sync_failures.push((
                                dev.clone(),
                                format!("Failed to restore playback on recreated window: {e}"),
                            ));
                        }
                    }
                } else if let Some(existing) = self.windows.get_mut(&dev) {
                    if existing.client_x != client_x
                        || existing.client_y != client_y
                        || existing.width != monitor.width
                        || existing.height != monitor.height
                    {
                        info!(device = %dev, "Updating monitor geometry");
                        existing.update_geometry(client_x, client_y, monitor.width, monitor.height);
                    }
                } else {
                    // Window missing (e.g. initial attach, reconnected monitor, or recreated host)
                    info!(
                        device = %dev,
                        primary = monitor.is_primary,
                        client_x,
                        client_y,
                        width = monitor.width,
                        height = monitor.height,
                        "Creating wallpaper window for monitor"
                    );

                    let win = WallpaperWindow::create(
                        self.host.hwnd,
                        dev.clone(),
                        client_x,
                        client_y,
                        monitor.width,
                        monitor.height,
                        monitor.is_primary,
                        bg_color,
                    )?;
                    win.attach_to_host()?;
                    self.windows.insert(dev.clone(), win);

                    // Reconnected monitor with preserved source: restore playback
                    if let Some((path, cfg)) = self.active_sources.get(&dev).cloned() {
                        // If an old player was retained from an earlier failure, attempt to stop it
                        let can_play = if let Some(player) = self.players.get_mut(&dev) {
                            match player.stop() {
                                Ok(()) => {
                                    info!(device = %dev, "Stopped retained player prior to reconnected playback");
                                    self.players.remove(&dev);
                                    true
                                }
                                Err(e) => {
                                    warn!(
                                        device = %dev,
                                        %e,
                                        "Failed to stop retained player on reconnected monitor; skipping playback to prevent duplicate"
                                    );
                                    sync_failures.push((
                                        dev.clone(),
                                        format!("Failed to stop retained player on reconnected monitor: {e}"),
                                    ));
                                    false
                                }
                            }
                        } else {
                            true
                        };

                        if can_play {
                            info!(device = %dev, "Reconnected monitor: restoring preserved wallpaper playback");
                            if let Err(e) = self.play_video_on_monitor(&dev, path, cfg) {
                                error!(
                                    device = %dev,
                                    %e,
                                    "Failed to restore playback on reconnected monitor"
                                );
                                sync_failures.push((
                                    dev.clone(),
                                    format!(
                                        "Failed to restore playback on reconnected monitor: {e}"
                                    ),
                                ));
                            }
                        }
                    }
                }
            }

            // Disconnected displays cleanup:
            // Strictly enforce process-before-render-target teardown ordering:
            // 1. For each disconnected monitor, attempt to stop its MPV player first.
            // 2. Only destroy the wallpaper window after the player has successfully terminated.
            // 3. If shutdown fails, retain the player and window in tracking and preserve source.
            let mut disconnected_devs: Vec<String> = self
                .windows
                .keys()
                .chain(self.players.keys())
                .filter(|d| !active_devices.contains(*d))
                .cloned()
                .collect();
            disconnected_devs.sort();
            disconnected_devs.dedup();

            for dev in disconnected_devs {
                info!(device = %dev, "Display disconnected: stopping video player and reconciling window");

                let has_player = self.players.contains_key(&dev);
                if has_player {
                    self.record_lifecycle_event(&format!("player_stop_attempt:{dev}"));
                }
                let mut player_stopped_cleanly = true;
                if let Some(res) = self.players.get_mut(&dev).map(|p| p.stop()) {
                    match res {
                        Ok(()) => {
                            self.record_lifecycle_event(&format!("player_stop_success:{dev}"));
                            info!(device = %dev, "Disconnected monitor player stopped successfully");
                            self.players.remove(&dev);
                            self.disconnected_failed_players.remove(&dev);
                        }
                        Err(e) => {
                            self.record_lifecycle_event(&format!("player_stop_fail:{dev}"));
                            warn!(
                                device = %dev,
                                %e,
                                "Failed to stop player on disconnected display; retaining tracking"
                            );
                            self.disconnected_failed_players.insert(dev.clone());
                            sync_failures.push((
                                dev.clone(),
                                format!("Failed to stop player on disconnected display: {e}"),
                            ));
                            player_stopped_cleanly = false;
                        }
                    }
                }

                if player_stopped_cleanly {
                    // Only destroy the wallpaper window after the player has successfully terminated
                    if self.windows.remove(&dev).is_some() {
                        info!(device = %dev, "Display disconnected: removed wallpaper window");
                        self.record_lifecycle_event(&format!("window_destroy:{dev}"));
                    }
                } else {
                    // Avoid unnecessarily destroying the wallpaper HWND while the failed player may still depend on it
                    self.record_lifecycle_event(&format!("window_preserved:{dev}"));
                }
            }

            if !sync_failures.is_empty() {
                let mut failed_monitors = Vec::new();
                for (d, _) in &sync_failures {
                    if !failed_monitors.contains(d) {
                        failed_monitors.push(d.clone());
                    }
                }
                warn!(
                    failed_count = failed_monitors.len(),
                    failed_monitors = ?failed_monitors,
                    "Monitor synchronization completed with failures"
                );
                return Err(WallpaperManagerError::MonitorSyncFailed {
                    failed_monitors,
                    errors: sync_failures,
                });
            }
        }

        Ok(())
    }

    /// Plays a video wallpaper on a specific monitor, or across all displays if SPAN.
    pub fn play_video_on_monitor(
        &mut self,
        device: &str,
        video_path: PathBuf,
        config: PlaybackConfig,
    ) -> Result<(), WallpaperManagerError> {
        if device == SPAN_DEVICE_NAME {
            return self.play_video_span(video_path, config);
        }
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

        if !win.is_valid() {
            return Err(WallpaperManagerError::Window(WindowError::ApiError(
                format!(
                    "Wallpaper window for monitor '{device}' has become invalid or was destroyed"
                ),
            )));
        }

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
        for player in self.web_players.values_mut() {
            player.pause();
        }
        for (dev, player) in &self.players {
            if let Err(err) = player.queue_state_command(
                &[
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(true),
                ],
                PlaybackState::Paused,
            ) {
                warn!(device = %dev, %err, "Failed to queue pause command");
            }
        }
    }

    /// Resumes all paused video players.
    pub fn resume_all(&mut self) {
        for (dev, player) in &mut self.web_players {
            if let Some(win) = self.windows.get(dev) {
                player.resume(win.width, win.height);
            }
        }
        for (dev, player) in &self.players {
            if let Err(err) = player.queue_state_command(
                &[
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(false),
                ],
                PlaybackState::Playing,
            ) {
                warn!(device = %dev, %err, "Failed to queue resume command");
            }
        }
    }

    /// Checks whether an ultra-wide panoramic spanned wallpaper is currently active.
    pub fn is_spanned_active(&self) -> bool {
        self.active_sources.contains_key(SPAN_DEVICE_NAME)
    }

    /// Plays a video wallpaper spanning across all active monitors seamlessly.
    pub fn play_video_span(
        &mut self,
        video_path: PathBuf,
        config: PlaybackConfig,
    ) -> Result<(), WallpaperManagerError> {
        if let Err(err) = std::fs::File::open(&video_path) {
            return Err(WallpaperManagerError::VideoFileInaccessible {
                path: video_path,
                error: err.to_string(),
            });
        }

        let monitors = crate::enumerate_monitors()
            .map_err(|e| WallpaperManagerError::MonitorError(e.to_string()))?;
        if monitors.is_empty() {
            return Err(WallpaperManagerError::MonitorNotFound(
                "No monitors connected to span".into(),
            ));
        }

        let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
        let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
        let max_x = monitors
            .iter()
            .map(|m| m.x + m.width as i32)
            .max()
            .unwrap_or(1920);
        let max_y = monitors
            .iter()
            .map(|m| m.y + m.height as i32)
            .max()
            .unwrap_or(1080);
        let width = (max_x - min_x) as u32;
        let height = (max_y - min_y) as u32;

        // Stop per-monitor players and hide their windows
        for (_, mut player) in self.players.drain() {
            let _ = player.stop();
        }
        for (_, mut wp) in self.web_players.drain() {
            let _ = wp.stop();
        }
        for (dev, win) in &self.windows {
            if dev != SPAN_DEVICE_NAME {
                win.hide();
            }
        }

        let (client_x, client_y) = self.host.screen_to_client(min_x, min_y);
        let span_win = if let Some(win) = self.windows.get_mut(SPAN_DEVICE_NAME) {
            win.update_geometry(client_x, client_y, width, height);
            win.show();
            win
        } else {
            let bg = color_to_colorref(18, 20, 24);
            let win = WallpaperWindow::create(
                self.host.hwnd,
                SPAN_DEVICE_NAME.to_string(),
                client_x,
                client_y,
                width,
                height,
                false,
                bg,
            )
            .map_err(WallpaperManagerError::Window)?;
            self.windows.insert(SPAN_DEVICE_NAME.to_string(), win);
            self.windows.get_mut(SPAN_DEVICE_NAME).unwrap()
        };

        let wid = span_win.hwnd.0 as isize;
        self.pump_messages();

        let launch_cfg = MpvLaunchConfig {
            video_path: video_path.clone(),
            wid,
            config: config.clone(),
        };

        let player = MpvPlayer::launch(launch_cfg).map_err(WallpaperManagerError::Player)?;
        self.players.insert(SPAN_DEVICE_NAME.to_string(), player);
        self.active_sources
            .insert(SPAN_DEVICE_NAME.to_string(), (video_path, config));

        info!(
            width,
            height, "Ultra-wide spanned video wallpaper running across displays"
        );
        Ok(())
    }

    /// Plays an interactive web/shader/HTML5 wallpaper on a monitor.
    pub fn play_web_on_monitor(
        &mut self,
        device: &str,
        source: &str,
    ) -> Result<(), WallpaperManagerError> {
        let win = self
            .windows
            .get(device)
            .ok_or_else(|| WallpaperManagerError::MonitorNotFound(device.to_string()))?;

        if !win.is_valid() {
            return Err(WallpaperManagerError::Window(WindowError::ApiError(
                format!("Wallpaper window for monitor '{device}' has become invalid"),
            )));
        }

        if let Some(mut existing_player) = self.players.remove(device) {
            let _ = existing_player.stop();
        }
        if let Some(mut existing_web) = self.web_players.remove(device) {
            let _ = existing_web.stop();
        }

        let wid = win.hwnd.0 as isize;
        let (w, h) = (win.width, win.height);
        let player = crate::web_player::WebPlayer::launch(wid, source, w, h)
            .map_err(|e| WallpaperManagerError::Player(MpvError::Player(e.to_string())))?;

        self.web_players.insert(device.to_string(), player);
        info!(device = %device, source = %source, "Interactive web wallpaper player attached to display");
        Ok(())
    }

    /// Pauses a specific video or web wallpaper player.
    pub fn pause_monitor(&mut self, device: &str) {
        if let Some(player) = self.players.get(device) {
            if let Err(err) = player.queue_state_command(
                &[
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(true),
                ],
                PlaybackState::Paused,
            ) {
                warn!(device = %device, %err, "Failed to queue pause command");
            }
        }
        if let Some(web) = self.web_players.get_mut(device) {
            web.pause();
        }
    }

    /// Resumes a specific video or web wallpaper player.
    pub fn resume_monitor(&mut self, device: &str) {
        if let Some(player) = self.players.get(device) {
            if let Err(err) = player.queue_state_command(
                &[
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(false),
                ],
                PlaybackState::Playing,
            ) {
                warn!(device = %device, %err, "Failed to queue resume command");
            }
        }
        if let Some((web, win)) = self
            .web_players
            .get_mut(device)
            .zip(self.windows.get(device))
        {
            web.resume(win.width, win.height);
        }
    }

    /// Queues volume changes for all players without waiting on their IPC pipes.
    pub fn set_volume_all(&mut self, volume: u32) {
        for (dev, player) in &self.players {
            if let Err(err) =
                player.queue_property_command("volume", serde_json::json!(volume.min(100)))
            {
                warn!(device = %dev, %err, "Failed to queue volume update");
            }
        }
    }

    /// Queues mute changes for all players without waiting on their IPC pipes.
    pub fn set_mute_all(&mut self, muted: bool) {
        for (dev, player) in &self.players {
            if let Err(err) = player.queue_property_command("mute", serde_json::json!(muted)) {
                warn!(device = %dev, %err, "Failed to queue mute update");
            }
        }
    }

    pub fn set_volume_monitor(&mut self, device: &str, volume: u32) -> Result<(), String> {
        self.players
            .get(device)
            .ok_or_else(|| format!("no active player on {device}"))?
            .queue_property_command("volume", serde_json::json!(volume.min(100)))
            .map_err(|error| error.to_string())
    }

    pub fn set_mute_monitor(&mut self, device: &str, muted: bool) -> Result<(), String> {
        self.players
            .get(device)
            .ok_or_else(|| format!("no active player on {device}"))?
            .queue_property_command("mute", serde_json::json!(muted))
            .map_err(|error| error.to_string())
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
            self.disconnected_failed_players.remove(dev);
        }

        for web in self.web_players.values_mut() {
            let _ = web.stop();
        }
        self.web_players.clear();

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
        for (dev, web) in &self.web_players {
            if !self.players.contains_key(dev) {
                list.push((
                    dev.clone(),
                    "chromium-kiosk".to_string(),
                    if web.is_paused() {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    },
                    web.get_memory_bytes(),
                ));
            }
        }
        list
    }

    /// Queries working set RAM bytes of a player process for a given monitor device.
    pub fn get_player_memory(&self, device: &str) -> u64 {
        self.players
            .get(device)
            .map(|p| p.get_memory_bytes())
            .unwrap_or(0)
    }

    /// Samples the current CPU percentage of a player process for a given monitor device.
    pub fn sample_player_cpu(&self, device: &str) -> f64 {
        self.players
            .get(device)
            .map(|p| p.sample_cpu())
            .unwrap_or(0.0)
    }

    /// Dynamically changes the video rotation (0, 90, 180, 270) of a running player.
    pub fn set_player_rotation(
        &mut self,
        device: &str,
        rotation: u32,
    ) -> Result<(), WallpaperManagerError> {
        if let Some(player) = self.players.get_mut(device) {
            player
                .set_video_rotation(rotation)
                .map_err(WallpaperManagerError::Player)?;
        }
        if let Some((_, ref mut cfg)) = self.active_sources.get_mut(device) {
            cfg.video_rotation = Some(rotation);
        }
        Ok(())
    }

    /// Checks if a player for a specific monitor is running and recovers if crashed.
    pub fn check_and_recover_players(&mut self) {
        let devices: Vec<String> = self.windows.keys().cloned().collect();

        for dev in devices {
            // Do not attempt to recover a player on a monitor that is tracked as failed on disconnect
            if self.disconnected_failed_players.contains(&dev) {
                continue;
            }

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
                            if let Err(error) = self.play_video_on_monitor(&dev, path, cfg) {
                                warn!(device = %dev, %error, "Player recovery failed");
                            }
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

            // 1. Preserve active sources across all failure and recovery paths
            let saved_sources = self.active_sources.clone();

            // 2. Stop all players and inspect shutdown outcomes
            let shutdown_summary = self.stop_all();
            let failed_devices: std::collections::HashSet<String> = shutdown_summary
                .failed
                .iter()
                .map(|(d, _)| d.clone())
                .collect();

            // 3. Clear windows that belong to successfully stopped players or dead host.
            // When Explorer restarts, old WorkerW and its child HWNDs may already be destroyed.
            // WallpaperWindow::drop safely handles already-destroyed HWNDs.
            self.windows.clear();

            // 4. Re-discover fresh desktop host.
            // If Explorer is still initializing and WorkerW is not yet ready, restore
            // active sources so future recovery attempts retain the assignments.
            let host_res = ensure_desktop_host();
            let new_host = match host_res {
                Ok(h) => h,
                Err(e) => {
                    self.active_sources = saved_sources;
                    return Err(WallpaperManagerError::DesktopHost(e));
                }
            };
            self.host = new_host;
            info!(
                selected_host = format_args!("0x{:X}", self.host.hwnd.0 as isize),
                "Re-discovered new desktop host after Explorer restart"
            );

            // 5. Re-attach wallpaper windows to the new host.
            // If attachment fails, restore active sources for future recovery attempts.
            if let Err(e) = self.attach_all_monitors() {
                self.active_sources = saved_sources;
                return Err(e);
            }

            // 6. Handle successful and failed monitors independently:
            // - NEVER launch a replacement player for a monitor whose previous player failed to terminate.
            // - Preserve saved wallpaper assignments in self.active_sources for all monitors.
            // - Launch replacement players only on healthy monitors where shutdown succeeded.
            let mut launch_errors = Vec::new();
            for (dev, (path, cfg)) in saved_sources {
                if failed_devices.contains(&dev) {
                    warn!(
                        device = %dev,
                        "Skipping player launch during Explorer recovery: previous player failed to terminate"
                    );
                    // Preserve assignment in active_sources for future recovery retry
                    self.active_sources.insert(dev, (path, cfg));
                    continue;
                }

                if self.windows.contains_key(&dev) {
                    info!(device = %dev, "Restoring video wallpaper on recreated host window");
                    if let Err(e) = self.play_video_on_monitor(&dev, path.clone(), cfg.clone()) {
                        error!(device = %dev, error = %e, "Failed to launch replacement player during Explorer recovery");
                        launch_errors.push((dev.clone(), e.to_string()));
                        // Preserve assignment even if launch failed
                        self.active_sources.insert(dev, (path, cfg));
                    }
                }
            }

            // 7. Clearly expose recovery failure if any player failed shutdown or launch
            if !shutdown_summary.failed.is_empty() || !launch_errors.is_empty() {
                let mut all_failed_monitors = Vec::new();
                for (d, _) in &shutdown_summary.failed {
                    if !all_failed_monitors.contains(d) {
                        all_failed_monitors.push(d.clone());
                    }
                }
                for (d, _) in &launch_errors {
                    if !all_failed_monitors.contains(d) {
                        all_failed_monitors.push(d.clone());
                    }
                }
                let mut all_errors = shutdown_summary.failed;
                all_errors.extend(launch_errors);

                warn!(
                    failed_count = all_failed_monitors.len(),
                    failed_monitors = ?all_failed_monitors,
                    "Explorer recovery completed with partial failure; retained failed players"
                );

                return Err(WallpaperManagerError::ExplorerRecoveryPartial {
                    failed_monitors: all_failed_monitors,
                    errors: all_errors,
                });
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

    /// Returns the active video source path for a monitor device if active.
    pub fn active_source_path(&self, device: &str) -> Option<&Path> {
        self.active_sources.get(device).map(|(p, _)| p.as_path())
    }

    /// Returns the active source string (video path or web URL) for a monitor device if active.
    pub fn active_source_string(&self, device: &str) -> Option<String> {
        self.active_sources
            .get(device)
            .map(|(p, _)| p.to_string_lossy().to_string())
            .or_else(|| self.web_players.get(device).map(|w| w.url().to_string()))
    }

    /// Returns the number of configured active video sources.
    pub fn active_source_count(&self) -> usize {
        self.active_sources.len()
    }

    /// For testing: overrides monitor enumeration with a simulated monitor topology.
    #[doc(hidden)]
    pub fn set_test_monitors(&mut self, monitors: Option<Vec<crate::MonitorInfo>>) {
        self.test_monitors = monitors;
    }

    /// For testing: sets an event recorder to verify execution order deterministically.
    #[doc(hidden)]
    pub fn set_lifecycle_recorder(
        &mut self,
        recorder: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
    ) {
        self.lifecycle_recorder = recorder;
    }

    #[doc(hidden)]
    pub fn record_lifecycle_event(&self, event: &str) {
        if let Some(recorder) = &self.lifecycle_recorder {
            if let Ok(mut list) = recorder.lock() {
                list.push(event.to_string());
            }
        }
    }
}

impl wallpaper_core::monitors::WallpaperHost for WallpaperManager {
    fn start_wallpaper(
        &mut self,
        device: &str,
        assignment: &wallpaper_core::monitors::Assignment,
    ) -> Result<(), wallpaper_core::monitors::StartError> {
        if !assignment.video_path.is_file() {
            return Err(wallpaper_core::monitors::StartError::Failed(format!(
                "wallpaper file does not exist: {}",
                assignment.video_path.display()
            )));
        }
        self.play_video_on_monitor(
            device,
            assignment.video_path.clone(),
            assignment.config.clone(),
        )
        .map_err(|error| wallpaper_core::monitors::StartError::Failed(error.to_string()))
    }

    fn stop_wallpaper(&mut self, device: &str) -> Result<(), String> {
        if let Some(mut player) = self.players.remove(device) {
            if let Err(error) = player.stop() {
                self.players.insert(device.to_string(), player);
                return Err(error.to_string());
            }
        }
        self.active_sources.remove(device);
        self.backoffs.remove(device);
        self.disconnected_failed_players.remove(device);
        Ok(())
    }

    fn player_reports(&mut self) -> Vec<wallpaper_core::health::PlayerReport> {
        use wallpaper_core::health::{PlayerReport, RecoveryState, Signal};

        let mut reports = Vec::with_capacity(self.players.len());
        for (device, player) in &mut self.players {
            let recovery = self.backoffs.get(device).and_then(|backoff| {
                if backoff.consecutive_failures == 0 {
                    None
                } else if backoff.consecutive_failures > backoff.max_failures {
                    Some(RecoveryState::Exhausted {
                        attempts: backoff.consecutive_failures,
                    })
                } else {
                    Some(RecoveryState::InProgress {
                        attempt: backoff.consecutive_failures,
                    })
                }
            });
            reports.push(PlayerReport {
                device: device.clone(),
                state: player.state(),
                uptime: Some(std::time::Duration::from_secs(player.uptime_secs())),
                memory_bytes: player.get_memory_bytes(),
                process_alive: Signal::from_bool(player.is_alive()),
                recovery,
            });
        }
        reports
    }
}

impl WallpaperManager {
    /// Performs one bounded MPV IPC health probe per active player.
    pub fn probe_players(&mut self) -> Vec<wallpaper_core::health::ProbeResult> {
        let mut results = Vec::with_capacity(self.players.len());
        for (device, player) in &self.players {
            if let Some((ipc_responded, video_initialized)) =
                player.request_video_probe().ok().flatten()
            {
                results.push(wallpaper_core::health::ProbeResult {
                    device: device.clone(),
                    ipc_responded,
                    video_initialized,
                });
            }
        }
        results
    }
}

impl Drop for WallpaperManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
