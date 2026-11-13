use anyhow::Context;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};
use wallpaper_core::{
    HardwareAcceleration, PerformanceProfile, PlaybackConfig, PlaybackPolicy, WallpaperLibrary,
};
use wallpaper_windows::{
    check_foreground_fullscreen, enumerate_monitors, PlaybackSupervisor, WallpaperManager,
};

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
            profile = match args[i + 1].to_lowercase().as_str() {
                "eco" => PerformanceProfile::Eco,
                "performance" | "perf" => PerformanceProfile::Performance,
                _ => PerformanceProfile::Balanced,
            };
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
    info!(
        ?profile,
        "TASK 04: Intelligent Runtime & Resource Management active"
    );

    #[cfg(windows)]
    {
        unsafe {
            let _ =
                windows::Win32::System::Console::SetConsoleCtrlHandler(Some(ctrl_handler), true);
        }

        // 1. Initialize SQLite Wallpaper Library
        let db_path = PathBuf::from("library.db");
        let library = WallpaperLibrary::open(&db_path)
            .context("failed to open or create SQLite wallpaper library")?;
        info!(db = %db_path.display(), "SQLite wallpaper library initialized");

        // 2. Discover or import sample wallpapers
        let nature_mp4 = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        let abstract_webm = PathBuf::from("assets/wallpapers/sample-abstract.webm");

        if nature_mp4.exists() {
            let _ = library.import_video(
                &nature_mp4,
                Some("Nature Test Video (MP4)"),
                Some(PlaybackConfig {
                    loop_playback: true,
                    mute: true,
                    volume: 100,
                    hardware_acceleration: HardwareAcceleration::Auto,
                }),
            );
        }

        if abstract_webm.exists() {
            let _ = library.import_video(
                &abstract_webm,
                Some("Abstract Test Video (WebM)"),
                Some(PlaybackConfig {
                    loop_playback: true,
                    mute: true,
                    volume: 100,
                    hardware_acceleration: HardwareAcceleration::Auto,
                }),
            );
        }

        let wallpapers = library.list().unwrap_or_default();
        info!(count = wallpapers.len(), "Loaded wallpapers from library");
        for w in &wallpapers {
            info!(id = %w.id, title = %w.title(), source = %w.source(), "Wallpaper in library");
        }

        // 3. Initialize Desktop Wallpaper Manager
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

        // 4. Multi-Monitor Video Wallpaper Assignment
        let monitors = enumerate_monitors().unwrap_or_default();
        for (idx, monitor) in monitors.iter().enumerate() {
            // Assign different videos to different monitors
            let video_to_play = if idx % 2 == 0 && nature_mp4.exists() {
                nature_mp4.clone()
            } else if abstract_webm.exists() {
                abstract_webm.clone()
            } else {
                nature_mp4.clone()
            };

            let config = PlaybackConfig {
                loop_playback: true,
                mute: true,
                volume: 80,
                hardware_acceleration: HardwareAcceleration::Auto,
            };

            info!(
                device = %monitor.device_name,
                primary = monitor.is_primary,
                video = %video_to_play.display(),
                hwdec = ?config.hardware_acceleration,
                "Attaching video wallpaper to monitor"
            );

            if let Err(e) =
                manager.play_video_on_monitor(&monitor.device_name, video_to_play, config)
            {
                warn!(device = %monitor.device_name, %e, "Failed to start video wallpaper on monitor");
            }
        }

        info!(
            active_players = manager.player_count(),
            "Video wallpaper playback active across monitors"
        );

        // Log initial hardware decoding & memory status
        for (dev, decoder, state, mem) in manager.player_statuses() {
            info!(
                device = %dev,
                active_decoder = %decoder,
                state = ?state,
                memory_mb = mem / (1024 * 1024),
                "Active player status & hardware decoding verification"
            );
        }

        let mut supervisor = PlaybackSupervisor::new(PlaybackPolicy::for_profile(profile));
        info!(
            profile = ?supervisor.policy.profile,
            pause_on_fullscreen = supervisor.policy.pause_on_fullscreen,
            pause_on_game_only = supervisor.policy.pause_on_fullscreen_game_only,
            debounce_ms = supervisor.policy.debounce_millis,
            "Intelligent playback supervisor active"
        );

        if let Some(dur) = duration_limit {
            info!(
                duration_secs = dur.as_secs(),
                "Daemon running for scheduled duration"
            );
        } else {
            info!("Daemon running. Press Ctrl+C to terminate cleanly and restore desktop");
        }

        let start_time = Instant::now();
        let mut last_refresh = Instant::now();
        let mut last_telemetry = Instant::now();
        let mut last_policy_check = Instant::now();

        while RUNNING.load(Ordering::SeqCst) {
            // Non-blocking Win32 message pump
            if !manager.pump_messages() {
                break;
            }

            // Periodically check foreground window and evaluate playback policy
            if last_policy_check.elapsed() >= Duration::from_millis(200) {
                let status = check_foreground_fullscreen();
                if let Some(should_pause) =
                    supervisor.evaluate_and_debounce(status.is_fullscreen, status.is_game)
                {
                    if should_pause {
                        info!(
                            reason = ?supervisor.active_pause_reason,
                            window = %status.window_title,
                            class = %status.class_name,
                            "Policy triggered: pausing wallpapers"
                        );
                        manager.pause_all();
                    } else {
                        info!("Policy condition cleared: resuming wallpapers");
                        manager.resume_all();
                    }
                }

                // Check player processes health and perform isolated recovery
                manager.check_and_recover_players();
                last_policy_check = Instant::now();
            }

            // Periodically refresh monitor state and verify host health
            if last_refresh.elapsed() >= Duration::from_millis(500) {
                if let Err(err) = manager.refresh() {
                    warn!(%err, "Error refreshing wallpaper manager");
                }
                last_refresh = Instant::now();
            }

            // Periodic telemetry reporting
            if last_telemetry.elapsed() >= Duration::from_secs(2) {
                for telem in manager.collect_telemetry() {
                    info!(
                        device = %telem.device_name,
                        active_decoder = %telem.active_decoder,
                        state = ?telem.state,
                        memory_mb = telem.memory_working_set_bytes / (1024 * 1024),
                        uptime_s = telem.uptime_secs,
                        gpu_context = ?telem.gpu_context,
                        res = format_args!("{}x{}", telem.video_width.unwrap_or(0), telem.video_height.unwrap_or(0)),
                        fps = telem.video_fps.unwrap_or(0.0),
                        "Player telemetry"
                    );
                }
                last_telemetry = Instant::now();
            }

            if let Some(dur) = duration_limit {
                if start_time.elapsed() >= dur {
                    info!("Scheduled run duration reached. Commencing clean shutdown");
                    break;
                }
            }

            std::thread::sleep(Duration::from_millis(16)); // ~60 Hz tick
        }

        info!("Shutting down daemon: stopping video players, cleaning up windows, releasing desktop host");
        drop(manager);
        info!("Clean shutdown complete. Zero orphaned processes guaranteed");
    }

    #[cfg(not(windows))]
    {
        warn!("Native Windows wallpaper desktop integration is only supported on Windows.");
    }

    Ok(())
}
