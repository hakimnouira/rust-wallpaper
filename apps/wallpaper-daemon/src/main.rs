//! Wallpaper daemon. RuntimeSupervisor is the single orchestration path for
//! monitor assignments, player health, system events and playback reconciliation.

use anyhow::Context;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use tracing::{info, warn};
use wallpaper_core::health::HealthPolicy;
use wallpaper_core::monitors::{
    Assignment, MonitorLifecycle, MonitorRegistry, SqliteAssignmentStore,
};
use wallpaper_core::registration::RegistrationReport;
use wallpaper_core::runtime::{parse_control_command, resolve_device, RuntimeState};
use wallpaper_core::supervisor::RuntimeSupervisor;
use wallpaper_core::{
    HardwareAcceleration, PerformanceProfile, PlaybackConfig, PlaybackPolicy, WallpaperLibrary,
};
use wallpaper_windows::{
    enumerate_monitor_details, ControlCommand, FullscreenTracker, ListenerPoll,
    SystemEventListener, WallpaperManager,
};

#[cfg(windows)]
mod ipc;

static RUNNING: AtomicBool = AtomicBool::new(true);

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(_: u32) -> windows::Win32::Foundation::BOOL {
    RUNNING.store(false, Ordering::SeqCst);
    windows::Win32::Foundation::BOOL(1)
}

fn parse_cli_args() -> (Option<Duration>, PerformanceProfile) {
    let args: Vec<String> = std::env::args().collect();
    let mut duration = None;
    let mut profile = PerformanceProfile::Balanced;
    for i in 1..args.len() {
        if (args[i] == "--duration" || args[i] == "--timeout") && i + 1 < args.len() {
            if let Ok(secs) = args[i + 1].parse::<u64>() {
                duration = Some(Duration::from_secs(secs));
            }
        }
        if args[i] == "--once" {
            duration = Some(Duration::from_secs(3));
        }
        if args[i] == "--profile" && i + 1 < args.len() {
            profile = PerformanceProfile::parse(&args[i + 1]).unwrap_or_else(|| {
                eprintln!(
                    "unknown profile '{}', using balanced (eco|balanced|performance)",
                    args[i + 1]
                );
                PerformanceProfile::Balanced
            });
        }
    }
    (duration, profile)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let (duration_limit, profile) = parse_cli_args();
    info!("Rust Wallpaper daemon starting");
    info!(?profile, "RuntimeSupervisor orchestration active");

    #[cfg(windows)]
    {
        unsafe {
            let _ =
                windows::Win32::System::Console::SetConsoleCtrlHandler(Some(ctrl_handler), true);
        }

        let db_path = PathBuf::from("library.db");
        let library = WallpaperLibrary::open(&db_path)
            .context("failed to open or create SQLite wallpaper library")?;

        // Import the bundled demo assets when present. They are not treated as permanent
        // assignments: the monitor registry below owns the persisted user choices.
        let nature_mp4 = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        let abstract_webm = PathBuf::from("assets/wallpapers/sample-abstract.webm");
        let demo_config = PlaybackConfig {
            loop_playback: true,
            mute: true,
            volume: 80,
            hardware_acceleration: HardwareAcceleration::Auto,
        };
        if nature_mp4.is_file() {
            let _ = library.import_video(
                &nature_mp4,
                Some("Nature Test Video (MP4)"),
                Some(demo_config.clone()),
            );
        }
        if abstract_webm.is_file() {
            let _ = library.import_video(
                &abstract_webm,
                Some("Abstract Test Video (WebM)"),
                Some(demo_config.clone()),
            );
        }
        info!(
            count = library.list().unwrap_or_default().len(),
            "Wallpaper library loaded"
        );

        let fallback_path = if nature_mp4.is_file() {
            Some(nature_mp4)
        } else if abstract_webm.is_file() {
            Some(abstract_webm)
        } else {
            None
        };
        let fallback = fallback_path.map(|path| Assignment {
            video_path: path,
            config: demo_config,
        });

        let assignment_store = SqliteAssignmentStore::open(&db_path)
            .context("failed to initialize persistent monitor assignments")?;
        let registry = MonitorRegistry::new(Box::new(assignment_store), fallback);
        let lifecycle = MonitorLifecycle::new(registry);
        let mut runtime = RuntimeSupervisor::new(
            RuntimeState::new(PlaybackPolicy::for_profile(profile)),
            lifecycle,
            HealthPolicy::default(),
        );

        let mut manager =
            WallpaperManager::init().context("failed to initialize desktop wallpaper manager")?;
        let diag = &manager.host().diagnostics;
        info!(
            selected_host = format_args!("0x{:X}", diag.selected_host_hwnd),
            kind = ?diag.host_kind,
            host_rect = ?diag.host_rect,
            workerw_count = diag.workerw_windows.len(),
            attached_windows = manager.window_count(),
            "Desktop host ready"
        );

        let initial = enumerate_monitor_details()
            .context("failed to enumerate monitor identities")?
            .into_iter()
            .map(|m| m.into_descriptor())
            .collect();
        let now = Instant::now();
        let initial_report = runtime.sync_topology(initial, &mut manager, now);
        for (device, error) in &initial_report.start_failures {
            warn!(%device, %error, "Initial wallpaper assignment failed; lifecycle will retry");
        }

        let mut listener = start_listener_with_retry();
        let mut last_listener_report = listener.as_ref().map(|l| l.registration_report());
        if let Some(report) = &last_listener_report {
            log_registration_report(report);
        }
        let mut next_listener_attempt = Instant::now() + LISTENER_RETRY;
        let mut tracker = FullscreenTracker::new();
        let control_rx = spawn_control_reader();
        let ipc_rx = ipc::start();
        info!(pipe = ipc::PIPE_NAME, "Named-pipe IPC server started");
        info!("Commands: pause [DISPLAYn] | resume [DISPLAYn] | assign DISPLAYn <video path> | unassign DISPLAYn | profile <eco|balanced|performance> | status | quit");

        if let Some(dur) = duration_limit {
            info!(
                duration_secs = dur.as_secs(),
                "Daemon running for scheduled duration"
            );
        } else {
            info!("Daemon running. Press Ctrl+C to terminate cleanly");
        }

        let start_time = Instant::now();
        let mut known_monitors: Vec<String> = enumerate_monitor_details()
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.info.device_name)
            .collect();
        let mut last_poll = Instant::now() - Duration::from_secs(1);
        let mut last_health = Instant::now();
        let mut last_refresh = Instant::now();
        let mut last_telemetry = Instant::now();
        let mut last_reconcile = Instant::now();

        while RUNNING.load(Ordering::SeqCst) {
            if !manager.pump_messages() {
                break;
            }
            let now = Instant::now();

            while let Ok(cmd) = control_rx.try_recv() {
                let registration_report = listener.as_ref().map(|l| l.registration_report());
                handle_control(
                    cmd,
                    &mut runtime,
                    &mut manager,
                    &known_monitors,
                    now,
                    registration_report.as_ref(),
                );
            }

            while let Ok(pending) = ipc_rx.try_recv() {
                let response = dispatch_ipc(
                    pending.request,
                    &library,
                    &mut runtime,
                    &mut manager,
                    &known_monitors,
                    now,
                );
                let _ = pending.reply.send(response);
            }

            let mut listener_died = false;
            if let Some(l) = &listener {
                loop {
                    match l.poll() {
                        ListenerPoll::Event(evt) => {
                            runtime.runtime.apply_event(evt.into(), now);
                        }
                        ListenerPoll::Empty => break,
                        ListenerPoll::Disconnected => {
                            listener_died = true;
                            break;
                        }
                    }
                }
            }
            if listener_died {
                warn!("System event listener thread exited; scheduling restart");
                listener = None;
                last_listener_report = None;
                next_listener_attempt = now;
            }
            if listener.is_none() && now >= next_listener_attempt {
                match SystemEventListener::start() {
                    Ok(l) => {
                        info!("System event listener restarted");
                        let report = l.registration_report();
                        log_registration_report(&report);
                        last_listener_report = Some(report);
                        listener = Some(l);
                        next_listener_attempt = now + LISTENER_RETRY;
                    }
                    Err(e) => {
                        warn!(%e, "System event listener restart failed");
                        next_listener_attempt = now + LISTENER_RETRY;
                    }
                }
            }

            if let Some(l) = &listener {
                let report = l.registration_report();
                if last_listener_report.as_ref() != Some(&report) {
                    log_registration_report(&report);
                    last_listener_report = Some(report);
                }
            }

            let work = runtime.runtime.take_pending_work(now);
            if work.recover_explorer {
                info!("Recovering desktop host after Explorer restart");
                if let Err(e) = manager.force_recover_explorer() {
                    warn!(%e, "Explorer recovery failed");
                } else if let Ok(monitors) = enumerate_monitor_details() {
                    let report = runtime.sync_topology(
                        monitors.into_iter().map(|m| m.into_descriptor()).collect(),
                        &mut manager,
                        now,
                    );
                    for (device, error) in report.start_failures {
                        warn!(%device, %error, "Wallpaper restoration failed after Explorer recovery");
                    }
                }
            }
            if work.refresh_topology {
                info!("Display topology settled; refreshing monitor lifecycle");
                if let Err(e) = manager.refresh() {
                    warn!(%e, "Wallpaper manager refresh failed");
                }
                sync_monitors(&mut runtime, &mut manager, &mut known_monitors, now);
            }

            if now.saturating_duration_since(last_poll) >= runtime.runtime.poll_interval() {
                last_poll = now;
                sync_known_devices(&mut known_monitors);
                let probe = tracker.probe();
                runtime
                    .runtime
                    .observe_foreground(&known_monitors, &probe.observations, now);
            }

            // Refresh the topology independently of notifications. This also recovers from
            // missed WM_DISPLAYCHANGE events and updates stable hardware identities.
            if now.saturating_duration_since(last_refresh) >= TOPOLOGY_POLL {
                last_refresh = now;
                if let Err(e) = manager.refresh() {
                    warn!(%e, "Error refreshing wallpaper manager");
                }
                sync_monitors(&mut runtime, &mut manager, &mut known_monitors, now);
            }

            if now.saturating_duration_since(last_health) >= HEALTH_TICK {
                last_health = now;
                manager.check_and_recover_players();
                if runtime.probe_due(now) {
                    let probes = manager.probe_players();
                    runtime.record_probes(&probes, now);
                }
                let health = runtime.update_health(&mut manager, now);
                for h in health {
                    if !h.status.is_healthy() {
                        warn!(
                            device = %h.device,
                            status = ?h.status,
                            unverified = ?h.unverified(),
                            "Wallpaper player health"
                        );
                    }
                }
            }

            if now.saturating_duration_since(last_reconcile) >= RECONCILE_HEARTBEAT {
                runtime.reconcile(&mut manager, now);
                last_reconcile = now;
            }

            if now.saturating_duration_since(last_telemetry) >= Duration::from_secs(2) {
                last_telemetry = now;
                // Keep the daemon loop non-blocking: detailed MPV telemetry performs several
                // synchronous IPC requests. The fast status snapshot uses cached process data.
                for (device, decoder, state, memory_bytes) in manager.player_statuses() {
                    let reasons: Vec<&str> = runtime
                        .runtime
                        .suspension()
                        .active_reasons_for(&device)
                        .into_iter()
                        .map(|r| r.as_str())
                        .collect();
                    info!(
                        device = %device,
                        active_decoder = %decoder,
                        state = ?state,
                        suspension_reasons = ?reasons,
                        memory_mb = memory_bytes / (1024 * 1024),
                        "Player status"
                    );
                }
            }

            if duration_limit.is_some_and(|d| start_time.elapsed() >= d) {
                info!("Scheduled run duration reached; shutting down");
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }

        info!("Shutting down daemon");
        drop(listener);
        drop(manager);
        info!("Clean shutdown complete");
    }

    #[cfg(not(windows))]
    {
        warn!("Native Windows wallpaper desktop integration is only supported on Windows.");
    }
    Ok(())
}

#[cfg(windows)]
const LISTENER_RETRY: Duration = Duration::from_secs(2);
#[cfg(windows)]
const TOPOLOGY_POLL: Duration = Duration::from_millis(750);
#[cfg(windows)]
const HEALTH_TICK: Duration = Duration::from_millis(250);
#[cfg(windows)]
const RECONCILE_HEARTBEAT: Duration = Duration::from_millis(100);

#[cfg(windows)]
fn start_listener_with_retry() -> Option<SystemEventListener> {
    match SystemEventListener::start() {
        Ok(listener) => {
            info!("System event listener started");
            Some(listener)
        }
        Err(e) => {
            warn!(%e, "System event listener unavailable; retrying");
            None
        }
    }
}

#[cfg(windows)]
fn sync_known_devices(known: &mut Vec<String>) {
    if let Ok(monitors) = enumerate_monitor_details() {
        let devices: Vec<String> = monitors.into_iter().map(|m| m.info.device_name).collect();
        if !devices.is_empty() {
            *known = devices;
        }
    }
}

#[cfg(windows)]
fn sync_monitors(
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &mut Vec<String>,
    now: Instant,
) {
    match enumerate_monitor_details() {
        Ok(monitors) if !monitors.is_empty() => {
            *known = monitors
                .iter()
                .map(|m| m.info.device_name.clone())
                .collect();
            let report = runtime.sync_topology(
                monitors.into_iter().map(|m| m.into_descriptor()).collect(),
                manager,
                now,
            );
            for (device, error) in report.start_failures {
                warn!(%device, %error, "Wallpaper assignment could not be started; retry budget exhausted");
            }
        }
        Ok(_) => warn!("Monitor enumeration returned no displays; retaining previous topology"),
        Err(e) => warn!(%e, "Monitor enumeration failed; retaining previous topology"),
    }
}

#[cfg(windows)]
fn log_registration_report(report: &RegistrationReport) {
    info!(coverage = ?report.coverage(), "Windows event registration coverage");
    for diagnostic in report.diagnostics() {
        warn!(%diagnostic, "Windows event source not fully registered");
    }
}

#[cfg(windows)]
fn spawn_control_reader() -> Receiver<ControlCommand> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("console-control".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim().is_empty() => {}
                    Ok(_) => match parse_control_command(&line) {
                        Ok(cmd) => {
                            if tx.send(cmd).is_err() {
                                break;
                            }
                        }
                        Err(e) => eprintln!("command error: {e}"),
                    },
                }
            }
        });
    rx
}

#[cfg(windows)]
fn handle_control(
    cmd: ControlCommand,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
    registration_report: Option<&RegistrationReport>,
) {
    let resolve = |name: &str| {
        resolve_device(name, known)
            .ok_or_else(|| format!("unknown monitor '{name}' (known: {known:?})"))
    };

    match cmd {
        ControlCommand::Pause { device } => match device {
            Some(name) => match resolve(&name) {
                Ok(device) => {
                    runtime.runtime.set_user_paused(Some(&device), true);
                }
                Err(e) => warn!("{e}"),
            },
            None => {
                runtime.runtime.set_user_paused(None, true);
            }
        },
        ControlCommand::Resume { device } => match device {
            Some(name) => match resolve(&name) {
                Ok(device) => {
                    runtime.runtime.set_user_paused(Some(&device), false);
                }
                Err(e) => warn!("{e}"),
            },
            None => {
                runtime.runtime.set_user_paused(None, false);
            }
        },
        ControlCommand::SetProfile(profile) => runtime.runtime.set_profile(profile),
        ControlCommand::Assign { device, path } => match resolve(&device) {
            Ok(device) => {
                let path = PathBuf::from(path);
                if !path.is_file() {
                    warn!(path = %path.display(), "Assignment rejected: file does not exist");
                } else {
                    match runtime.assign_user(&device, Assignment::new(path), manager, now) {
                        Ok(()) => info!(%device, "Persistent wallpaper assignment applied"),
                        Err(e) => {
                            warn!(%device, %e, "Assignment applied in memory but persistence failed")
                        }
                    }
                }
            }
            Err(e) => warn!("{e}"),
        },
        ControlCommand::Unassign { device } => match resolve(&device) {
            Ok(device) => match runtime.unassign_user(&device, manager, now) {
                Ok(()) => info!(%device, "User assignment removed; default policy restored"),
                Err(e) => {
                    warn!(%device, %e, "Unassignment applied in memory but persistence failed")
                }
            },
            Err(e) => warn!("{e}"),
        },
        ControlCommand::Status => {
            for m in runtime.runtime.status(known) {
                info!(
                    device = %m.device,
                    suspended = m.suspended,
                    reasons = ?m.reasons.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                    "Runtime status"
                );
            }
            for h in runtime.health() {
                info!(device = %h.device, health = ?h.status, unverified = ?h.unverified(), "Player health");
            }
            for device in runtime.lifecycle.wanted_devices() {
                info!(%device, "Monitor has an assigned wallpaper");
            }
            if let Some(report) = registration_report {
                info!(coverage = ?report.coverage(), "Windows event registration coverage");
                for diagnostic in report.diagnostics() {
                    warn!(%diagnostic, "Windows event source not fully registered");
                }
            } else {
                warn!("Windows event listener is unavailable");
            }
        }
        ControlCommand::Quit => {
            info!("Quit requested");
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
}


#[cfg(windows)]
fn dispatch_ipc(
    request: wallpaper_protocol::RequestEnvelope,
    library: &WallpaperLibrary,
    runtime: &mut RuntimeSupervisor,
    manager: &mut WallpaperManager,
    known: &[String],
    now: Instant,
) -> wallpaper_protocol::ResponseEnvelope {
    use wallpaper_protocol::{Command, ErrorCode, PlayerStatusInfo, Response, ResponseEnvelope};

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
        Command::ApplyWallpaper { wallpaper_id, monitor } => {
            let wallpaper = match library.get(wallpaper_id) {
                Ok(Some(wallpaper)) => wallpaper,
                Ok(None) => return failure(ErrorCode::NotFound, format!("wallpaper {wallpaper_id} was not found")),
                Err(error) => return failure(ErrorCode::Internal, error.to_string()),
            };
            if wallpaper.kind != wallpaper_core::WallpaperKind::Video {
                return failure(ErrorCode::Unsupported, "only video wallpapers are currently playable".into());
            }
            let targets: Vec<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => vec![device],
                    None => return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'")),
                },
                None => known.to_vec(),
            };
            if targets.is_empty() {
                return failure(ErrorCode::NotFound, "no connected monitors are available".into());
            }
            let assignment = Assignment {
                video_path: PathBuf::from(&wallpaper.metadata.source),
                config: wallpaper.config.clone(),
            };
            for device in targets {
                if let Err(error) = runtime.assign_user(&device, assignment.clone(), manager, now) {
                    return failure(ErrorCode::RuntimeFailure, format!("could not assign wallpaper to {device}: {error}"));
                }
            }
            Response::Accepted
        }
        command @ (Command::Pause { .. } | Command::Resume { .. }) => {
            let (monitor, paused) = match command {
                Command::Pause { monitor } => (monitor, true),
                Command::Resume { monitor } => (monitor, false),
                _ => unreachable!(),
            };
            match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => { runtime.runtime.set_user_paused(Some(&device), paused); }
                    None => return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'")),
                },
                None => { runtime.runtime.set_user_paused(None, paused); }
            }
            Response::Accepted
        }
        Command::SetVolume { monitor, volume } => {
            if volume > 100 {
                return failure(ErrorCode::InvalidRequest, "volume must be between 0 and 100".into());
            }
            let targets: Vec<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => vec![device],
                    None => return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'")),
                },
                None => known.to_vec(),
            };
            for device in targets {
                let player = match manager.player_mut(&device) {
                    Some(player) => player,
                    None => continue,
                };
                if let Err(error) = player.set_volume(volume) {
                    return failure(ErrorCode::RuntimeFailure, format!("volume update failed on {device}: {error}"));
                }
            }
            Response::Accepted
        }
        Command::SetMute { monitor, muted } => {
            let targets: Vec<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => vec![device],
                    None => return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'")),
                },
                None => known.to_vec(),
            };
            for device in targets {
                let player = match manager.player_mut(&device) {
                    Some(player) => player,
                    None => continue,
                };
                if let Err(error) = player.set_mute(muted) {
                    return failure(ErrorCode::RuntimeFailure, format!("mute update failed on {device}: {error}"));
                }
            }
            Response::Accepted
        }
        Command::GetStatus { monitor } => {
            let selected: Option<String> = match monitor {
                Some(name) => match resolve_device(&name, known) {
                    Some(device) => Some(device),
                    None => return failure(ErrorCode::NotFound, format!("unknown monitor '{name}'")),
                },
                None => None,
            };
            let states = runtime.runtime.status(known);
            let players = manager.player_statuses();
            let mut items = Vec::new();
            for (device, decoder, state, memory_bytes) in players {
                if selected.as_ref().is_some_and(|selected| selected != &device) {
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
                        .map(|s| s.reasons.iter().map(|reason| reason.as_str().to_string()).collect())
                        .unwrap_or_default(),
                });
            }
            Response::PlayerStatus(items)
        }
        Command::Stop { .. } => {
            return failure(ErrorCode::Unsupported, "Stop is not exposed until its runtime semantics are defined".into());
        }
    };

    ResponseEnvelope::new(request_id, result)
}
