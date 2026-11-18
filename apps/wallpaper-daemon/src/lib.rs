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

pub mod logging;

#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod ipc_commands;
#[cfg(windows)]
mod ipc_security;
pub mod metadata;
pub mod pipeline;
#[cfg(windows)]
pub mod web_api;

static RUNNING: AtomicBool = AtomicBool::new(true);

/// Requests that the daemon stop its supervision loop and release players.
pub(crate) fn request_shutdown() {
    RUNNING.store(false, Ordering::SeqCst);
}

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(_: u32) -> windows::Win32::Foundation::BOOL {
    RUNNING.store(false, Ordering::SeqCst);
    windows::Win32::Foundation::BOOL(1)
}

fn bundled_asset_path(file_name: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("assets").join("wallpapers").join(file_name));
        }
    }
    candidates.push(PathBuf::from("assets").join("wallpapers").join(file_name));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| std::fs::canonicalize(&path).ok().or(Some(path)))
}

fn parse_cli_args() -> (Option<Duration>, Option<PerformanceProfile>) {
    let args: Vec<String> = std::env::args().collect();
    let mut duration = None;
    let mut profile = None;
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
            profile = PerformanceProfile::parse(&args[i + 1]);
            if profile.is_none() {
                eprintln!(
                    "unknown profile '{}'; expected eco, balanced or performance",
                    args[i + 1]
                );
            }
        }
    }
    (duration, profile)
}

pub fn run_daemon() -> anyhow::Result<()> {
    let (duration_limit, requested_profile) = parse_cli_args();
    run_daemon_with_options(duration_limit, requested_profile, None)
}

pub fn run_daemon_with_options(
    duration_limit: Option<Duration>,
    requested_profile: Option<PerformanceProfile>,
    external_running: Option<std::sync::Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    logging::init("daemon", true);

    #[cfg(windows)]
    let _instance_guard = match wallpaper_windows::SingleInstanceGuard::acquire_daemon() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            info!("Rust Wallpaper daemon is already running; exiting");
            return Ok(());
        }
        Err(error) => {
            warn!(%error, "Could not acquire the daemon single-instance lock");
            return Ok(());
        }
    };

    RUNNING.store(true, Ordering::SeqCst);
    info!("Rust Wallpaper daemon starting");
    info!(?requested_profile, "RuntimeSupervisor orchestration active");

    #[cfg(windows)]
    {
        unsafe {
            let _ =
                windows::Win32::System::Console::SetConsoleCtrlHandler(Some(ctrl_handler), true);
        }

        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .context("LOCALAPPDATA is not set; cannot locate the Rust Wallpaper data directory")?;
        let app_data_dir = PathBuf::from(local_app_data).join("RustWallpaper");
        std::fs::create_dir_all(&app_data_dir).with_context(|| {
            format!(
                "failed to create application data directory '{}'",
                app_data_dir.display()
            )
        })?;
        let db_path = app_data_dir.join("library.db");
        info!(database = %db_path.display(), "Using persistent application database");
        if !db_path.exists() && PathBuf::from("library.db").is_file() {
            std::fs::copy("library.db", &db_path)
                .context("failed to migrate legacy working-directory library database")?;
        }
        let library = WallpaperLibrary::open(&db_path)
            .context("failed to open or create SQLite wallpaper library")?;

        let thumbnails_dir = app_data_dir.join("thumbnails");
        std::fs::create_dir_all(&thumbnails_dir).with_context(|| {
            format!(
                "failed to create thumbnails directory '{}'",
                thumbnails_dir.display()
            )
        })?;
        let pipeline = pipeline::ExtractionPipeline::start(library.clone(), thumbnails_dir.clone());
        let saved_profile = library
            .get_setting("performance_profile")?
            .and_then(|value| PerformanceProfile::parse(&value));
        let profile = requested_profile.or(saved_profile).unwrap_or_default();
        if requested_profile.is_some() {
            let profile_name = match profile {
                PerformanceProfile::Eco => "eco",
                PerformanceProfile::Balanced => "balanced",
                PerformanceProfile::Performance => "performance",
            };
            library.set_setting("performance_profile", profile_name)?;
        }

        // Resolve packaged sample media beside the installed binaries, independent of cwd.
        let nature_mp4 = bundled_asset_path("sample-nature.mp4");
        let abstract_webm = bundled_asset_path("sample-abstract.webm");

        let demo_config = PlaybackConfig {
            loop_playback: true,
            mute: true,
            volume: 80,
            hardware_acceleration: HardwareAcceleration::Auto,
            ..Default::default()
        };

        let existing_wallpapers = library.list().unwrap_or_default();
        let has_nature = existing_wallpapers
            .iter()
            .any(|w| w.title() == "Nature Test Video (MP4)");
        let has_abstract = existing_wallpapers
            .iter()
            .any(|w| w.title() == "Abstract Test Video (WebM)");

        if !has_nature {
            if let Some(ref path) = nature_mp4 {
                let _ = library.import_video(
                    path,
                    Some("Nature Test Video (MP4)"),
                    Some(demo_config.clone()),
                );
            }
        }
        if !has_abstract {
            if let Some(ref path) = abstract_webm {
                let _ = library.import_video(
                    path,
                    Some("Abstract Test Video (WebM)"),
                    Some(demo_config.clone()),
                );
            }
        }
        info!(
            count = library.list().unwrap_or_default().len(),
            "Wallpaper library loaded"
        );

        let fallback_path = nature_mp4.or(abstract_webm);
        let fallback = fallback_path.map(|path| Assignment {
            video_path: path,
            config: demo_config,
        });

        let assignment_store = SqliteAssignmentStore::open(&db_path)
            .context("failed to initialize persistent monitor assignments")?;
        let registry = MonitorRegistry::new(Box::new(assignment_store), fallback);
        let lifecycle = MonitorLifecycle::new(registry);
        let playback_settings = ipc_commands::load_playback_settings(&library);
        let mut runtime_state = RuntimeState::new(PlaybackPolicy::for_profile(profile));
        runtime_state.set_playback_settings(playback_settings);
        let mut runtime = RuntimeSupervisor::new(runtime_state, lifecycle, HealthPolicy::default());
        // Persisted global performance settings must reach wallpapers restored from
        // the database before the first player launch, not only newly applied ones.
        let (target_fps, ram_budget_mb, quality) =
            ipc_commands::load_performance_settings(&library);
        runtime.lifecycle.registry_mut().apply_playback_defaults(
            target_fps,
            ram_budget_mb,
            quality,
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
        let (ipc_tx, ipc_rx) = ipc::start();
        web_api::start(ipc_tx.clone(), web_api::DEFAULT_PORT);
        // Scan disk and queue stale thumbnail work only after IPC and HTTP are serving requests.
        pipeline::ExtractionPipeline::start_recovery(
            library.clone(),
            pipeline.clone(),
            thumbnails_dir,
        );
        info!(pipe = ipc::PIPE_NAME, "Named-pipe IPC server started");
        info!(
            port = web_api::DEFAULT_PORT,
            "Local Web API HTTP bridge started on 127.0.0.1"
        );
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

        while RUNNING.load(Ordering::SeqCst)
            && external_running
                .as_ref()
                .map(|f| f.load(Ordering::SeqCst))
                .unwrap_or(true)
        {
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
                let response = ipc_commands::dispatch_ipc(
                    pending.request,
                    &library,
                    &pipeline,
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

            if now.saturating_duration_since(last_telemetry) >= Duration::from_secs(10) {
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
