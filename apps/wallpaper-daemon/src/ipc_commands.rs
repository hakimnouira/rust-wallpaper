#![cfg(windows)]

use super::*;
use wallpaper_core::{FullscreenPauseMode, PlaybackSettings};

const FULLSCREEN_PAUSE_KEY: &str = "fullscreen_pause_mode";
const PAUSE_ON_LOCK_KEY: &str = "pause_on_lock";
const PAUSE_ON_DISPLAY_OFF_KEY: &str = "pause_on_display_off";
const PAUSE_ON_SUSPEND_KEY: &str = "pause_on_suspend";
const PAUSE_ON_BATTERY_KEY: &str = "playback.pause_on_battery";
const ROTATION_PLAYLIST_KEY: &str = "rotation.active_playlist_id";
const ROTATION_AUTO_ENABLED_KEY: &str = "rotation.auto_cycle_enabled";
const ROTATION_AUTO_INTERVAL_KEY: &str = "rotation.auto_cycle_interval_secs";
const ROTATION_AUTO_SHUFFLE_KEY: &str = "rotation.auto_cycle_shuffle";

fn load_bool(library: &WallpaperLibrary, key: &str, default: bool) -> bool {
    match library.get_setting(key) {
        Ok(Some(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => default,
    }
}

/// Reads persistent playback automation settings. Missing or corrupt values fall
/// back to the Balanced profile defaults rather than failing daemon startup.
pub(super) fn load_playback_settings(library: &WallpaperLibrary) -> PlaybackSettings {
    let default = PlaybackSettings::default();
    PlaybackSettings {
        fullscreen_pause: library
            .get_setting(FULLSCREEN_PAUSE_KEY)
            .ok()
            .flatten()
            .and_then(|value| FullscreenPauseMode::parse(&value))
            .unwrap_or(default.fullscreen_pause),
        pause_on_lock: load_bool(library, PAUSE_ON_LOCK_KEY, default.pause_on_lock),
        pause_on_display_off: load_bool(
            library,
            PAUSE_ON_DISPLAY_OFF_KEY,
            default.pause_on_display_off,
        ),
        pause_on_suspend: load_bool(library, PAUSE_ON_SUSPEND_KEY, default.pause_on_suspend),
        pause_on_battery: load_bool(library, PAUSE_ON_BATTERY_KEY, default.pause_on_battery),
    }
}

fn persist_playback_settings(library: &WallpaperLibrary, settings: PlaybackSettings) {
    let _ = library.set_setting(FULLSCREEN_PAUSE_KEY, settings.fullscreen_pause.as_str());
    for (key, value) in [
        (PAUSE_ON_LOCK_KEY, settings.pause_on_lock),
        (PAUSE_ON_DISPLAY_OFF_KEY, settings.pause_on_display_off),
        (PAUSE_ON_SUSPEND_KEY, settings.pause_on_suspend),
        (PAUSE_ON_BATTERY_KEY, settings.pause_on_battery),
    ] {
        let _ = library.set_setting(key, if value { "1" } else { "0" });
    }
}

/// Shared reader for the persisted global FPS/RAM/quality settings.
pub(super) fn load_performance_settings(
    library: &WallpaperLibrary,
) -> (Option<u32>, Option<u32>, wallpaper_core::VideoQuality) {
    let target_fps = match library.get_setting("target_fps") {
        Ok(Some(value)) if value.is_empty() => None,
        Ok(Some(value)) => value.parse().ok(),
        _ => Some(30),
    };
    let ram_budget_mb = match library.get_setting("ram_budget_mb") {
        Ok(Some(value)) if value.is_empty() => None,
        Ok(Some(value)) => value.parse().ok(),
        _ => Some(64),
    };
    let quality = library
        .get_setting("quality")
        .ok()
        .flatten()
        .and_then(|value| wallpaper_core::VideoQuality::parse(&value))
        .unwrap_or_default();
    (target_fps, ram_budget_mb, quality)
}

pub struct ActiveBenchmark {
    pub wallpaper_id: wallpaper_core::WallpaperId,
    pub device: String,
    pub started_at: Instant,
    pub last_sample: Instant,
    pub peak_ram_bytes: u64,
    pub peak_cpu_percent: f64,
}

/// Daemon-side cache for infrequently changing snapshots (monitors, browser extension registry)
/// and persisted background playlist / auto-cycle rotation state.
pub(super) struct DaemonCache {
    pub monitors: Vec<wallpaper_protocol::MonitorInfo>,
    pub extension_status: Option<(Instant, wallpaper_windows::ExtensionStatus)>,
    pub active_playlist_id: Option<String>,
    pub active_playlist_index: usize,
    pub active_playlist_last_time: Instant,
    pub auto_cycle_enabled: bool,
    pub auto_cycle_interval_secs: u64,
    pub auto_cycle_shuffle: bool,
    pub auto_cycle_index: usize,
    pub auto_cycle_last_time: Instant,
    pub rotation_counter: u64,
}

impl DaemonCache {
    pub(super) fn load(library: &WallpaperLibrary) -> Self {
        let now = Instant::now();
        let active_playlist_id = library
            .get_setting(ROTATION_PLAYLIST_KEY)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        let auto_cycle_enabled = load_bool(library, ROTATION_AUTO_ENABLED_KEY, false);
        let auto_cycle_interval_secs = library
            .get_setting(ROTATION_AUTO_INTERVAL_KEY)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&v| v >= 10)
            .unwrap_or(900);
        let auto_cycle_shuffle = load_bool(library, ROTATION_AUTO_SHUFFLE_KEY, true);

        Self {
            monitors: Vec::new(),
            extension_status: None,
            active_playlist_id,
            active_playlist_index: 0,
            active_playlist_last_time: now,
            auto_cycle_enabled,
            auto_cycle_interval_secs,
            auto_cycle_shuffle,
            auto_cycle_index: 0,
            auto_cycle_last_time: now,
            rotation_counter: 0,
        }
    }

    pub(super) fn set_monitors_from_details(
        &mut self,
        monitors: &[wallpaper_windows::MonitorDetails],
    ) {
        self.monitors = monitors
            .iter()
            .map(|details| {
                let descriptor = details.clone().into_descriptor();
                wallpaper_protocol::MonitorInfo {
                    id: descriptor.id.to_string(),
                    device_name: descriptor.device_name,
                    x: descriptor.x,
                    y: descriptor.y,
                    width: descriptor.width,
                    height: descriptor.height,
                    is_primary: descriptor.is_primary,
                }
            })
            .collect();
    }

    fn monitors_or_refresh(&mut self) -> Result<Vec<wallpaper_protocol::MonitorInfo>, String> {
        if !self.monitors.is_empty() {
            return Ok(self.monitors.clone());
        }
        let monitors = enumerate_monitor_details().map_err(|e| e.to_string())?;
        self.set_monitors_from_details(&monitors);
        Ok(self.monitors.clone())
    }

    fn extension_status_cached(&mut self) -> &wallpaper_windows::ExtensionStatus {
        const EXT_TTL: Duration = Duration::from_secs(60);
        let expired = self
            .extension_status
            .as_ref()
            .is_none_or(|(fetched_at, _)| fetched_at.elapsed() >= EXT_TTL);
        if expired {
            let status = wallpaper_windows::get_extension_status();
            self.extension_status = Some((Instant::now(), status));
        }
        &self.extension_status.as_ref().unwrap().1
    }
}

fn build_player_status_items(
    runtime: &RuntimeSupervisor,
    manager: &WallpaperManager,
    known: &[String],
    selected: Option<&str>,
) -> Vec<wallpaper_protocol::PlayerStatusInfo> {
    let states = runtime.runtime.status(known);
    let players = manager.player_statuses();
    let mut items = Vec::with_capacity(players.len());
    for (device, decoder, state, memory_bytes) in players {
        if selected.is_some_and(|sel| sel != device) {
            continue;
        }
        let status = states.iter().find(|status| status.device == device);
        items.push(wallpaper_protocol::PlayerStatusInfo {
            monitor: device.clone(),
            state,
            active_decoder: decoder,
            memory_bytes,
            suspended: status.map(|s| s.suspended).unwrap_or(false),
            suspension_reasons: status
                .map(|s| {
                    s.reasons
                        .iter()
                        .map(|reason| reason.as_str().to_string())
                        .collect()
                })
                .unwrap_or_default(),
            source: manager.active_source_string(&device),
        });
    }
    items
}

fn apply_wallpaper_by_id(
    wallpaper_id: wallpaper_core::WallpaperId,
    monitor: Option<String>,
    library: &WallpaperLibrary,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
    active_benchmark: &mut Option<ActiveBenchmark>,
) -> Result<(), (wallpaper_protocol::ErrorCode, String)> {
    use wallpaper_protocol::ErrorCode;

    let wallpaper = library
        .get(wallpaper_id)
        .map_err(|error| (ErrorCode::Internal, error.to_string()))?
        .ok_or_else(|| {
            (
                ErrorCode::NotFound,
                format!("wallpaper {wallpaper_id} was not found"),
            )
        })?;

    let is_span = monitor.as_deref().is_some_and(|m| {
        m.eq_ignore_ascii_case(wallpaper_windows::SPAN_DEVICE_NAME)
            || m.eq_ignore_ascii_case("SPAN")
            || m.eq_ignore_ascii_case("ALL_SPAN")
    });

    if is_span {
        let mut play_config = wallpaper.config.clone();
        let (default_fps, default_ram, global_quality) = load_performance_settings(library);
        if play_config.target_fps.is_none() {
            play_config.target_fps = default_fps;
        }
        if play_config.ram_budget_mb.is_none() {
            play_config.ram_budget_mb = default_ram;
        }
        play_config.quality = global_quality;
        let video_path = PathBuf::from(&wallpaper.metadata.source);
        manager
            .play_video_span(video_path, play_config)
            .map_err(|e| {
                (
                    ErrorCode::RuntimeFailure,
                    format!("could not apply spanned wallpaper: {e}"),
                )
            })?;
        return Ok(());
    }

    let targets: Vec<String> = match monitor {
        Some(name) => match resolve_device(&name, known) {
            Some(device) => vec![device],
            None => return Err((ErrorCode::NotFound, format!("unknown monitor '{name}'"))),
        },
        None => known.to_vec(),
    };
    if targets.is_empty() {
        return Err((
            ErrorCode::NotFound,
            "no connected monitors are available".into(),
        ));
    }

    if wallpaper.kind == wallpaper_core::WallpaperKind::Web
        || wallpaper.kind == wallpaper_core::WallpaperKind::Scene
    {
        for device in &targets {
            manager
                .play_web_on_monitor(device, &wallpaper.metadata.source)
                .map_err(|e| {
                    (
                        ErrorCode::RuntimeFailure,
                        format!("could not apply web wallpaper to {device}: {e}"),
                    )
                })?;
        }
        return Ok(());
    }

    let mut play_config = wallpaper.config.clone();
    let (default_fps, default_ram, global_quality) = load_performance_settings(library);
    if play_config.target_fps.is_none() {
        play_config.target_fps = default_fps;
    }
    if play_config.ram_budget_mb.is_none() {
        play_config.ram_budget_mb = default_ram;
    }
    play_config.quality = global_quality;
    let assignment = Assignment {
        video_path: PathBuf::from(&wallpaper.metadata.source),
        config: play_config,
    };
    for device in &targets {
        runtime
            .assign_user(device, assignment.clone(), manager, now)
            .map_err(|error| {
                (
                    ErrorCode::RuntimeFailure,
                    format!("could not assign wallpaper to {device}: {error}"),
                )
            })?;
    }
    if let Some(target_dev) = targets.first() {
        *active_benchmark = Some(ActiveBenchmark {
            wallpaper_id,
            device: target_dev.clone(),
            started_at: Instant::now(),
            last_sample: Instant::now(),
            peak_ram_bytes: 0,
            peak_cpu_percent: 0.0,
        });
    }
    Ok(())
}

/// Checks Day/Night scheduled playlists, manual active playlists, and auto-cycle rotation
/// on a 1-second daemon cadence so wallpapers rotate even when the UI is hidden or closed.
pub(super) fn tick_rotation(
    library: &WallpaperLibrary,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
    cache: &mut DaemonCache,
    active_benchmark: &mut Option<ActiveBenchmark>,
) {
    if known.is_empty() {
        return;
    }

    let playlists = library.list_playlists().unwrap_or_default();
    let scheduled_pl = playlists.iter().find(|p| {
        p.time_slot != wallpaper_core::ScheduleTimeSlot::AllDay && p.time_slot.is_active_now()
    });

    let active_playlist = if let Some(scheduled) = scheduled_pl {
        if cache.active_playlist_id.as_deref() != Some(scheduled.id.as_str()) {
            cache.active_playlist_id = Some(scheduled.id.clone());
            cache.active_playlist_index = 0;
            cache.active_playlist_last_time = Instant::now();
            let _ = library.set_setting(ROTATION_PLAYLIST_KEY, &scheduled.id);
        }
        Some(scheduled.clone())
    } else {
        cache
            .active_playlist_id
            .as_ref()
            .and_then(|id| playlists.iter().find(|p| &p.id == id))
            .cloned()
    };

    if let Some(playlist) = active_playlist {
        if playlist.wallpaper_ids.is_empty() {
            return;
        }
        let interval = Duration::from_secs(playlist.interval_secs.max(10));
        if cache.active_playlist_last_time.elapsed() < interval {
            return;
        }
        cache.rotation_counter = cache.rotation_counter.wrapping_add(1);
        let next_wp_id = if playlist.shuffle {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let idx = (nanos ^ (cache.rotation_counter as usize)) % playlist.wallpaper_ids.len();
            playlist.wallpaper_ids[idx]
        } else {
            cache.active_playlist_index =
                (cache.active_playlist_index + 1) % playlist.wallpaper_ids.len();
            playlist.wallpaper_ids[cache.active_playlist_index]
        };
        cache.active_playlist_last_time = Instant::now();
        if let Err((_, err)) = apply_wallpaper_by_id(
            next_wp_id,
            None,
            library,
            runtime,
            manager,
            known,
            now,
            active_benchmark,
        ) {
            warn!(%err, playlist = %playlist.name, "Scheduled playlist rotation failed");
        }
        return;
    }

    if cache.auto_cycle_enabled {
        let interval = Duration::from_secs(cache.auto_cycle_interval_secs.max(10));
        if cache.auto_cycle_last_time.elapsed() < interval {
            return;
        }
        let ready: Vec<_> = library
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.metadata.is_ready())
            .collect();
        if ready.is_empty() {
            return;
        }
        cache.rotation_counter = cache.rotation_counter.wrapping_add(1);
        let next_wp = if cache.auto_cycle_shuffle {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let idx = (nanos ^ (cache.rotation_counter as usize)) % ready.len();
            &ready[idx]
        } else {
            cache.auto_cycle_index = (cache.auto_cycle_index + 1) % ready.len();
            &ready[cache.auto_cycle_index]
        };
        cache.auto_cycle_last_time = Instant::now();
        if let Err((_, err)) = apply_wallpaper_by_id(
            next_wp.id,
            None,
            library,
            runtime,
            manager,
            known,
            now,
            active_benchmark,
        ) {
            warn!(%err, "Auto-cycle wallpaper rotation failed");
        }
    }
}

pub(super) fn dispatch_ipc(
    request: wallpaper_protocol::RequestEnvelope,
    library: &WallpaperLibrary,
    pipeline: &pipeline::ExtractionPipeline,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
    active_benchmark: &mut Option<ActiveBenchmark>,
    cache: &mut DaemonCache,
) -> wallpaper_protocol::ResponseEnvelope {
    use wallpaper_protocol::{
        BrowserInfo, Command, DashboardSnapshot, ErrorCode, Response, ResponseEnvelope,
    };

    let request_id = request.request_id;
    if let Err(error) = request.validate() {
        return ResponseEnvelope::error(request_id, ErrorCode::ProtocolMismatch, error.to_string());
    }

    let failure = |code, message: String| ResponseEnvelope::error(request_id, code, message);

    let result = match request.command {
        Command::Ping => Response::Pong,
        Command::Quit => {
            crate::request_shutdown();
            Response::Accepted
        }
        Command::ListWallpapers => match library.list() {
            Ok(items) => Response::WallpaperList(items),
            Err(error) => return failure(ErrorCode::Internal, error.to_string()),
        },
        Command::ListMonitors => match cache.monitors_or_refresh() {
            Ok(items) => Response::MonitorList(items),
            Err(error) => return failure(ErrorCode::RuntimeFailure, error),
        },
        Command::ImportWallpaper { path, title } => {
            match library.import_video(&path, title.as_deref(), None) {
                Ok(wallpaper) => {
                    info!(wallpaper_id = %wallpaper.id, source = %path, "Wallpaper imported through IPC");
                    if let Err(error) = pipeline.submit(wallpaper.id, PathBuf::from(&path)) {
                        let message = error.to_string();
                        let _ = library.set_processing_state(
                            wallpaper.id,
                            wallpaper_core::ProcessingStatus::Failed,
                            Some(&message),
                        );
                        return failure(ErrorCode::RuntimeFailure, message);
                    }
                    Response::Accepted
                }
                Err(error) => return failure(ErrorCode::InvalidRequest, error.to_string()),
            }
        }
        Command::RetryProcessing { wallpaper_id } => match library.get(wallpaper_id) {
            Ok(Some(wallpaper)) => {
                let source = PathBuf::from(&wallpaper.metadata.source);
                if let Err(error) = library.set_processing_state(
                    wallpaper_id,
                    wallpaper_core::ProcessingStatus::Pending,
                    None,
                ) {
                    return failure(ErrorCode::Internal, error.to_string());
                }
                match pipeline.submit(wallpaper_id, source) {
                    Ok(()) => Response::Accepted,
                    Err(error) => {
                        let message = error.to_string();
                        let _ = library.set_processing_state(
                            wallpaper_id,
                            wallpaper_core::ProcessingStatus::Failed,
                            Some(&message),
                        );
                        return failure(ErrorCode::RuntimeFailure, message);
                    }
                }
            }
            Ok(None) => {
                return failure(
                    ErrorCode::NotFound,
                    format!("wallpaper {wallpaper_id} was not found"),
                )
            }
            Err(error) => return failure(ErrorCode::Internal, error.to_string()),
        },
        Command::RemoveWallpaper { wallpaper_id } => {
            let wallpaper = match library.get(wallpaper_id) {
                Ok(Some(wallpaper)) => wallpaper,
                Ok(None) => {
                    return failure(
                        ErrorCode::NotFound,
                        format!("wallpaper {wallpaper_id} was not found"),
                    )
                }
                Err(error) => return failure(ErrorCode::Internal, error.to_string()),
            };
            if runtime
                .lifecycle
                .registry()
                .is_path_assigned(std::path::Path::new(&wallpaper.metadata.source))
            {
                return failure(
                    ErrorCode::InvalidRequest,
                    "wallpaper is assigned to a monitor or configured as the fallback; unassign it before removing it from the library".into(),
                );
            }
            // Delete associated thumbnail from disk if present
            if let Some(ref thumb_path_str) = wallpaper.metadata.thumbnail_path {
                let thumb_path = PathBuf::from(thumb_path_str);
                if thumb_path.is_file() {
                    let _ = std::fs::remove_file(&thumb_path);
                }
            }
            let candidate_thumb = pipeline.thumbnail_path(wallpaper_id);
            if candidate_thumb.is_file() {
                let _ = std::fs::remove_file(&candidate_thumb);
            }
            match library.delete(wallpaper_id) {
                Ok(true) => Response::Accepted,
                Ok(false) => {
                    return failure(
                        ErrorCode::NotFound,
                        format!("wallpaper {wallpaper_id} was not found"),
                    )
                }
                Err(error) => return failure(ErrorCode::RuntimeFailure, error.to_string()),
            }
        }
        Command::ApplyWallpaper {
            wallpaper_id,
            monitor,
        } => {
            if let Err((code, msg)) = apply_wallpaper_by_id(
                wallpaper_id,
                monitor,
                library,
                runtime,
                manager,
                known,
                now,
                active_benchmark,
            ) {
                return failure(code, msg);
            }
            Response::Accepted
        }
        Command::UnassignWallpaper { monitor } => match resolve_device(&monitor, known) {
            Some(device) => match runtime.unassign_user(&device, manager, now) {
                Ok(()) => Response::Accepted,
                Err(error) => return failure(ErrorCode::RuntimeFailure, error),
            },
            None => return failure(ErrorCode::NotFound, format!("unknown monitor '{monitor}'")),
        },
        command @ (Command::Pause { .. } | Command::Resume { .. }) => {
            let (monitor, paused) = match command {
                Command::Pause { monitor } => (monitor, true),
                Command::Resume { monitor } => (monitor, false),
                _ => unreachable!(),
            };
            match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => {
                        runtime.runtime.set_user_paused(Some(&device), paused);
                    }
                    None => {
                        return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'"))
                    }
                },
                None => {
                    runtime.runtime.set_user_paused(None, paused);
                }
            }
            Response::Accepted
        }
        Command::SetVolume { monitor, volume } => {
            if volume > 100 {
                return failure(
                    ErrorCode::InvalidRequest,
                    "volume must be between 0 and 100".into(),
                );
            }
            let targets: Vec<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => vec![device],
                    None => {
                        return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'"))
                    }
                },
                None => known.to_vec(),
            };
            for device in targets {
                if let Err(error) = manager.set_volume_monitor(&device, volume) {
                    if error.contains("no active player") {
                        continue;
                    }
                    return failure(
                        ErrorCode::RuntimeFailure,
                        format!("volume update failed on {device}: {error}"),
                    );
                }
            }
            Response::Accepted
        }
        Command::SetMute { monitor, muted } => {
            let targets: Vec<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => vec![device],
                    None => {
                        return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'"))
                    }
                },
                None => known.to_vec(),
            };
            for device in targets {
                if let Err(error) = manager.set_mute_monitor(&device, muted) {
                    if error.contains("no active player") {
                        continue;
                    }
                    return failure(
                        ErrorCode::RuntimeFailure,
                        format!("mute update failed on {device}: {error}"),
                    );
                }
            }
            Response::Accepted
        }
        Command::SetProfile { profile } => {
            let profile_name = match profile {
                PerformanceProfile::Eco => "eco",
                PerformanceProfile::Balanced => "balanced",
                PerformanceProfile::Performance => "performance",
            };
            if let Err(error) = library.set_setting("performance_profile", profile_name) {
                return failure(ErrorCode::Internal, error.to_string());
            }
            runtime.runtime.set_profile(profile);
            Response::Accepted
        }
        Command::GetProfile => Response::Profile(runtime.runtime.policy().profile),
        Command::GetPlaybackSettings => {
            Response::PlaybackSettings(runtime.runtime.playback_settings())
        }
        Command::SetPlaybackSettings(settings) => {
            persist_playback_settings(library, settings);
            runtime.runtime.set_playback_settings(settings);
            Response::PlaybackSettings(settings)
        }
        Command::GetStatus { monitor } => {
            let selected: Option<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => Some(device),
                    None => {
                        return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'"))
                    }
                },
                None => None,
            };
            let items = build_player_status_items(runtime, manager, known, selected.as_deref());
            Response::PlayerStatus(items)
        }
        Command::Stop { .. } => {
            return failure(
                ErrorCode::Unsupported,
                "Stop is not exposed until its runtime semantics are defined".into(),
            );
        }
        Command::SetPerformanceSettings {
            target_fps,
            ram_budget_mb,
            quality,
        } => {
            let _ = library.set_setting(
                "target_fps",
                &target_fps.map(|v| v.to_string()).unwrap_or_default(),
            );
            let _ = library.set_setting(
                "ram_budget_mb",
                &ram_budget_mb.map(|v| v.to_string()).unwrap_or_default(),
            );
            let _ = library.set_setting("quality", quality.as_str());

            for device in known {
                if let Some(desc) = runtime.lifecycle.registry().descriptor_for_device(device) {
                    if let Some((assignment, _)) = runtime.lifecycle.registry().desired(&desc.id) {
                        let mut updated = assignment.clone();
                        updated.config.target_fps = target_fps;
                        updated.config.ram_budget_mb = ram_budget_mb;
                        updated.config.quality = quality;
                        let _ = runtime.assign_user(device, updated, manager, now);
                    }
                }
            }
            Response::Accepted
        }
        Command::GetPerformanceSettings => {
            let (target_fps, ram_budget_mb, quality) = load_performance_settings(library);
            Response::PerformanceSettings {
                target_fps,
                ram_budget_mb,
                quality,
            }
        }
        Command::SetWallpaperRotation {
            wallpaper_id,
            rotation,
        } => {
            let _ = library.update_rotation(wallpaper_id, rotation);
            if let Ok(Some(wp)) = library.get(wallpaper_id) {
                let wp_path = PathBuf::from(&wp.metadata.source);
                for device in known {
                    let matches = manager
                        .active_source_path(device)
                        .map(|p| p == wp_path.as_path())
                        .unwrap_or(false);
                    if matches {
                        let _ = manager.set_player_rotation(device, rotation);
                    }
                }
            }
            Response::Accepted
        }
        Command::BenchmarkWallpaper {
            wallpaper_id,
            monitor,
        } => {
            let target_dev = match monitor {
                Some(name) => resolve_device(&name, known)
                    .unwrap_or_else(|| known.first().cloned().unwrap_or_default()),
                None => known.first().cloned().unwrap_or_default(),
            };
            if !target_dev.is_empty() {
                *active_benchmark = Some(ActiveBenchmark {
                    wallpaper_id,
                    device: target_dev,
                    started_at: Instant::now(),
                    last_sample: Instant::now(),
                    peak_ram_bytes: 0,
                    peak_cpu_percent: 0.0,
                });
            }
            Response::Accepted
        }
        Command::SavePlaylist { playlist } => match library.save_playlist(&playlist) {
            Ok(()) => Response::Accepted,
            Err(e) => return failure(ErrorCode::Internal, e.to_string()),
        },
        Command::ListPlaylists => match library.list_playlists() {
            Ok(playlists) => Response::PlaylistList(playlists),
            Err(e) => return failure(ErrorCode::Internal, e.to_string()),
        },
        Command::DeletePlaylist { playlist_id } => match library.delete_playlist(&playlist_id) {
            Ok(_) => Response::Accepted,
            Err(e) => return failure(ErrorCode::Internal, e.to_string()),
        },
        Command::GetExtensionStatus => {
            let status = cache.extension_status_cached();
            Response::ExtensionStatus {
                registered: status.registered,
                crx_available: status.crx_available,
                crx_path: status.crx_path.clone(),
                unpacked_available: status.unpacked_available,
                browsers: status
                    .browsers
                    .iter()
                    .map(|b| BrowserInfo {
                        name: b.name.clone(),
                        exe_path: b.exe_path.clone(),
                        registered: b.registered,
                    })
                    .collect(),
                extension_id: status.extension_id.clone(),
                version: status.version.clone(),
            }
        }
        Command::SetExtensionEnabled { enabled } => {
            cache.extension_status = None;
            if enabled {
                match wallpaper_windows::enable_extension() {
                    Ok(count) => {
                        let _ = library.set_setting("extension_enabled", "1");
                        info!("Browser extension enabled ({count} browsers registered)");
                        Response::Accepted
                    }
                    Err(e) => {
                        return failure(ErrorCode::RuntimeFailure, e);
                    }
                }
            } else {
                wallpaper_windows::disable_extension();
                let _ = library.set_setting("extension_enabled", "0");
                info!("Browser extension disabled (unregistered from all browsers)");
                Response::Accepted
            }
        }
        Command::GetDashboardSnapshot => {
            let monitors = match cache.monitors_or_refresh() {
                Ok(items) => items,
                Err(error) => return failure(ErrorCode::RuntimeFailure, error),
            };
            let wallpapers = match library.list() {
                Ok(items) => items,
                Err(error) => return failure(ErrorCode::Internal, error.to_string()),
            };
            let playlists = match library.list_playlists() {
                Ok(items) => items,
                Err(error) => return failure(ErrorCode::Internal, error.to_string()),
            };
            let players = build_player_status_items(runtime, manager, known, None);
            let (target_fps, ram_budget_mb, quality) = load_performance_settings(library);
            let profile = runtime.runtime.policy().profile;
            let playback_settings = runtime.runtime.playback_settings();
            let active_playlist_id = cache.active_playlist_id.clone();
            let auto_cycle_enabled = cache.auto_cycle_enabled;
            let auto_cycle_interval_secs = cache.auto_cycle_interval_secs;
            let auto_cycle_shuffle = cache.auto_cycle_shuffle;
            let ext = cache.extension_status_cached();

            Response::DashboardSnapshot(Box::new(DashboardSnapshot {
                monitors,
                wallpapers,
                playlists,
                players,
                profile,
                playback_settings,
                target_fps,
                ram_budget_mb,
                quality,
                extension_registered: ext.registered,
                extension_crx_available: ext.crx_available,
                extension_crx_path: ext.crx_path.clone(),
                extension_unpacked_available: ext.unpacked_available,
                extension_browsers: ext
                    .browsers
                    .iter()
                    .map(|b| BrowserInfo {
                        name: b.name.clone(),
                        exe_path: b.exe_path.clone(),
                        registered: b.registered,
                    })
                    .collect(),
                extension_id: ext.extension_id.clone(),
                extension_version: ext.version.clone(),
                active_playlist_id,
                auto_cycle_enabled,
                auto_cycle_interval_secs,
                auto_cycle_shuffle,
            }))
        }
        Command::SetFavorite {
            wallpaper_id,
            favorite,
        } => match library.set_favorite(wallpaper_id, favorite) {
            Ok(true) => Response::Accepted,
            Ok(false) => {
                return failure(
                    ErrorCode::NotFound,
                    format!("wallpaper {wallpaper_id} was not found"),
                )
            }
            Err(error) => return failure(ErrorCode::Internal, error.to_string()),
        },
        Command::SetActivePlaylist { playlist_id } => {
            cache.active_playlist_id = playlist_id.clone();
            cache.active_playlist_index = 0;
            cache.active_playlist_last_time = Instant::now();
            let val = playlist_id.unwrap_or_default();
            if let Err(error) = library.set_setting(ROTATION_PLAYLIST_KEY, &val) {
                return failure(ErrorCode::Internal, error.to_string());
            }
            Response::Accepted
        }
        Command::SetAutoCycle {
            enabled,
            interval_secs,
            shuffle,
        } => {
            let clamped_interval = interval_secs.max(10);
            cache.auto_cycle_enabled = enabled;
            cache.auto_cycle_interval_secs = clamped_interval;
            cache.auto_cycle_shuffle = shuffle;
            cache.auto_cycle_last_time = Instant::now();
            let _ = library.set_setting(ROTATION_AUTO_ENABLED_KEY, if enabled { "1" } else { "0" });
            let _ = library.set_setting(ROTATION_AUTO_INTERVAL_KEY, &clamped_interval.to_string());
            let _ = library.set_setting(ROTATION_AUTO_SHUFFLE_KEY, if shuffle { "1" } else { "0" });
            Response::Accepted
        }
    };

    ResponseEnvelope::new(request_id, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_settings_battery_saver_persists_across_reload() {
        let library = WallpaperLibrary::in_memory().unwrap();
        let settings = PlaybackSettings {
            fullscreen_pause: FullscreenPauseMode::MaximizedAndFullscreen,
            pause_on_lock: false,
            pause_on_display_off: true,
            pause_on_suspend: false,
            pause_on_battery: true,
        };
        persist_playback_settings(&library, settings);
        let reloaded = load_playback_settings(&library);
        assert_eq!(reloaded, settings);

        let disabled = PlaybackSettings {
            pause_on_battery: false,
            ..settings
        };
        persist_playback_settings(&library, disabled);
        assert_eq!(load_playback_settings(&library), disabled);
    }
}
