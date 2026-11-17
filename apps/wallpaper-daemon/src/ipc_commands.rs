#![cfg(windows)]

use super::*;

pub(super) fn dispatch_ipc(
    request: wallpaper_protocol::RequestEnvelope,
    library: &WallpaperLibrary,
    pipeline: &pipeline::ExtractionPipeline,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
) -> wallpaper_protocol::ResponseEnvelope {
    use wallpaper_protocol::{
        Command, ErrorCode, MonitorInfo, PlayerStatusInfo, Response, ResponseEnvelope,
    };

    let request_id = request.request_id;
    if let Err(error) = request.validate() {
        return ResponseEnvelope::error(request_id, ErrorCode::ProtocolMismatch, error.to_string());
    }

    let failure = |code, message: String| ResponseEnvelope::error(request_id, code, message);

    let result = match request.command {
        Command::Ping => Response::Pong,
        Command::ListWallpapers => match library.list() {
            Ok(items) => Response::WallpaperList(items),
            Err(error) => return failure(ErrorCode::Internal, error.to_string()),
        },
        Command::ListMonitors => match enumerate_monitor_details() {
            Ok(monitors) => {
                let items = monitors
                    .into_iter()
                    .map(|details| {
                        let descriptor = details.into_descriptor();
                        MonitorInfo {
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
                Response::MonitorList(items)
            }
            Err(error) => return failure(ErrorCode::RuntimeFailure, error.to_string()),
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
            if wallpaper.kind != wallpaper_core::WallpaperKind::Video {
                return failure(
                    ErrorCode::Unsupported,
                    "only video wallpapers are currently playable".into(),
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
            if targets.is_empty() {
                return failure(
                    ErrorCode::NotFound,
                    "no connected monitors are available".into(),
                );
            }
            let mut play_config = wallpaper.config.clone();
            if play_config.target_fps.is_none() {
                if let Ok(Some(fps_str)) = library.get_setting("target_fps") {
                    if !fps_str.is_empty() {
                        play_config.target_fps = fps_str.parse().ok();
                    }
                }
            }
            if play_config.ram_budget_mb.is_none() {
                if let Ok(Some(ram_str)) = library.get_setting("ram_budget_mb") {
                    if !ram_str.is_empty() {
                        play_config.ram_budget_mb = ram_str.parse().ok();
                    }
                }
            }
            if let Ok(Some(q_str)) = library.get_setting("quality") {
                if let Some(q) = wallpaper_core::VideoQuality::parse(&q_str) {
                    play_config.quality = q;
                }
            }
            let assignment = Assignment {
                video_path: PathBuf::from(&wallpaper.metadata.source),
                config: play_config,
            };
            for device in targets {
                if let Err(error) = runtime.assign_user(&device, assignment.clone(), manager, now) {
                    return failure(
                        ErrorCode::RuntimeFailure,
                        format!("could not assign wallpaper to {device}: {error}"),
                    );
                }
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
            let states = runtime.runtime.status(known);
            let players = manager.player_statuses();
            let mut items = Vec::new();
            for (device, decoder, state, memory_bytes) in players {
                if selected
                    .as_ref()
                    .is_some_and(|selected| selected != &device)
                {
                    continue;
                }
                let status = states.iter().find(|status| status.device == device);
                items.push(PlayerStatusInfo {
                    monitor: device,
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
                });
            }
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
            let target_fps = library
                .get_setting("target_fps")
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse().ok());
            let ram_budget_mb = library
                .get_setting("ram_budget_mb")
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse().ok());
            let quality = library
                .get_setting("quality")
                .ok()
                .flatten()
                .and_then(|s| wallpaper_core::VideoQuality::parse(&s))
                .unwrap_or_default();
            Response::PerformanceSettings {
                target_fps,
                ram_budget_mb,
                quality,
            }
        }
    };

    ResponseEnvelope::new(request_id, result)
}
