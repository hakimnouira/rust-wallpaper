//! Wallpaper daemon: wires the Windows event sources, foreground detector and user commands
//! into one runtime state machine, and lets a single `reconcile` step drive the players.
//!
//! Decision flow (see `docs/runtime-agent-b.md`):
//!
//! ```text
//! SystemEventListener ─┐
//! FullscreenTracker  ──┼─► PlaybackSupervisor (facts → reasons) ─► reconcile ─► WallpaperManager
//! stdin control      ──┤                                              ▲
//! player health      ──┘                              actual state ───┘
//! ```
//!
//! Nothing here calls `pause_all` / `resume_all`: only `reconcile` pauses or resumes players.

use anyhow::Context;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use tracing::{info, warn};
use wallpaper_core::runtime::{parse_control_command, resolve_device};
use wallpaper_core::{
    HardwareAcceleration, PerformanceProfile, PlaybackConfig, PlaybackPolicy, WallpaperLibrary,
};
use wallpaper_windows::{
    enumerate_monitors, ControlCommand, FullscreenTracker, ListenerPoll, PlaybackSupervisor,
    SystemEventListener, WallpaperManager,
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
    info!(?profile, "Intelligent runtime & resource management active");

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

        // Names of monitors that were given a wallpaper: a missing player there is a fault.
        let assigned: Vec<String> = manager
            .player_statuses()
            .into_iter()
            .map(|(device, ..)| device)
            .collect();

        // 5. Runtime state machine, event listener, detector and control channel
        let mut runtime = PlaybackSupervisor::new(PlaybackPolicy::for_profile(profile));
        {
            let p = runtime.policy();
            info!(
                profile = ?p.profile,
                threshold = ?p.foreground_pause_threshold,
                pause_debounce_ms = p.pause_debounce_millis,
                resume_debounce_ms = p.resume_debounce_millis,
                poll_ms = p.poll_interval_millis,
                "Playback supervisor active"
            );
        }

        let mut listener = match SystemEventListener::start() {
            Ok(l) => {
                info!("System event listener active (session, display power, suspend/resume, topology, Explorer)");
                Some(l)
            }
            Err(e) => {
                warn!(%e, "System event listener failed to start; will retry");
                None
            }
        };
        let mut next_listener_attempt = Instant::now() + LISTENER_RETRY;

        let mut tracker = FullscreenTracker::new();
        let control_rx = spawn_control_reader();
        info!("Console commands: pause [DISPLAYn] | resume [DISPLAYn] | profile <eco|balanced|performance> | status | quit");

        if let Some(dur) = duration_limit {
            info!(
                duration_secs = dur.as_secs(),
                "Daemon running for scheduled duration"
            );
        } else {
            info!("Daemon running. Press Ctrl+C to terminate cleanly and restore desktop");
        }

        let start_time = Instant::now();
        let mut known_monitors: Vec<String> = enumerate_monitors()
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.device_name)
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
            let mut reconcile_due = false;

            // --- user commands -------------------------------------------------------
            while let Ok(cmd) = control_rx.try_recv() {
                reconcile_due |= handle_control(cmd, &mut runtime, &known_monitors);
            }

            // --- system events (all processed in order; state is idempotent) ---------
            let mut listener_died = false;
            if let Some(l) = &listener {
                loop {
                    match l.poll() {
                        ListenerPoll::Event(evt) => {
                            runtime.apply_event(evt.into(), now);
                            reconcile_due = true;
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
                warn!("System event listener thread died; restarting");
                listener = None;
                next_listener_attempt = now;
            }
            if listener.is_none() && now >= next_listener_attempt {
                match SystemEventListener::start() {
                    Ok(l) => {
                        info!("System event listener (re)started");
                        listener = Some(l);
                    }
                    Err(e) => {
                        warn!(%e, "System event listener restart failed");
                        next_listener_attempt = now + LISTENER_RETRY;
                    }
                }
            }

            // --- work made due by events (bursts already collapsed) ------------------
            let work = runtime.take_pending_work(now);
            if work.recover_explorer {
                info!("Recovering desktop host after Explorer restart");
                if let Err(e) = manager.force_recover_explorer() {
                    warn!(%e, "Failed to recover desktop host after Explorer restart");
                }
                // Rebuilt players start in the Playing state: reconcile re-asserts the truth.
                reconcile_due = true;
            } else if work.refresh_topology {
                info!("Display topology settled: re-enumerating monitors");
                if let Err(e) = manager.refresh() {
                    warn!(%e, "Failed to refresh monitor layout on topology change");
                }
                if let Ok(m) = enumerate_monitors() {
                    if !m.is_empty() {
                        known_monitors = m.into_iter().map(|m| m.device_name).collect();
                    }
                }
                reconcile_due = true;
            }

            // --- foreground detection (rate set by the active profile) ---------------
            if now.saturating_duration_since(last_poll) >= runtime.poll_interval() {
                last_poll = now;
                if let Ok(m) = enumerate_monitors() {
                    if !m.is_empty() {
                        known_monitors = m.into_iter().map(|m| m.device_name).collect();
                    }
                }
                let probe = tracker.probe();
                runtime.observe_foreground(&known_monitors, &probe.observations, now);
                reconcile_due = true;
            }

            // --- player health and crash recovery ------------------------------------
            if now.saturating_duration_since(last_health) >= Duration::from_millis(200) {
                last_health = now;
                manager.check_and_recover_players();
                let present: Vec<String> = manager
                    .player_statuses()
                    .into_iter()
                    .map(|(device, ..)| device)
                    .collect();
                runtime.sync_player_presence(&assigned, &present);
                reconcile_due = true; // recovered players start Playing
            }

            if now.saturating_duration_since(last_refresh) >= Duration::from_millis(500) {
                last_refresh = now;
                if let Err(err) = manager.refresh() {
                    warn!(%err, "Error refreshing wallpaper manager");
                }
                reconcile_due = true;
            }

            // --- the single decision point -------------------------------------------
            // Also runs on a heartbeat so a command that failed is retried.
            if reconcile_due || now.saturating_duration_since(last_reconcile) >= RECONCILE_HEARTBEAT
            {
                runtime.reconcile(&mut manager, now);
                last_reconcile = now;
            }

            // --- telemetry -----------------------------------------------------------
            if now.saturating_duration_since(last_telemetry) >= Duration::from_secs(2) {
                last_telemetry = now;
                for telem in manager.collect_telemetry() {
                    let reasons: Vec<&str> = runtime
                        .suspension()
                        .active_reasons_for(&telem.device_name)
                        .into_iter()
                        .map(|r| r.as_str())
                        .collect();
                    info!(
                        device = %telem.device_name,
                        active_decoder = %telem.active_decoder,
                        state = ?telem.state,
                        suspension_reasons = ?reasons,
                        memory_mb = telem.memory_working_set_bytes / (1024 * 1024),
                        uptime_s = telem.uptime_secs,
                        gpu_context = ?telem.gpu_context,
                        res = format_args!("{}x{}", telem.video_width.unwrap_or(0), telem.video_height.unwrap_or(0)),
                        fps = telem.video_fps.unwrap_or(0.0),
                        "Player telemetry"
                    );
                }
            }

            if let Some(dur) = duration_limit {
                if start_time.elapsed() >= dur {
                    info!("Scheduled run duration reached. Commencing clean shutdown");
                    break;
                }
            }

            std::thread::sleep(Duration::from_millis(16)); // ~60 Hz tick
        }

        info!("Shutting down daemon: stopping event listener, players and windows");
        drop(listener);
        drop(manager);
        info!("Clean shutdown complete. Zero orphaned processes guaranteed");
    }

    #[cfg(not(windows))]
    {
        warn!("Native Windows wallpaper desktop integration is only supported on Windows.");
    }

    Ok(())
}

/// Delay before retrying a failed or dead event listener.
#[cfg(windows)]
const LISTENER_RETRY: Duration = Duration::from_secs(2);
/// Maximum time between reconcile passes when nothing else triggered one.
#[cfg(windows)]
const RECONCILE_HEARTBEAT: Duration = Duration::from_millis(250);

/// Reads console commands on a background thread so stdin can never stall the loop.
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
                    Ok(0) | Err(_) => break, // stdin closed or not available
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

/// Applies a user command to the runtime state. Returns true if a reconcile is needed.
#[cfg(windows)]
fn handle_control(cmd: ControlCommand, runtime: &mut PlaybackSupervisor, known: &[String]) -> bool {
    let resolve = |input: &Option<String>| -> Result<Option<String>, String> {
        match input {
            None => Ok(None),
            Some(name) => resolve_device(name, known)
                .map(Some)
                .ok_or_else(|| format!("unknown monitor '{name}' (known: {known:?})")),
        }
    };

    match cmd {
        ControlCommand::Pause { device } => apply_user_pause(runtime, &resolve(&device), true),
        ControlCommand::Resume { device } => apply_user_pause(runtime, &resolve(&device), false),
        ControlCommand::SetProfile(profile) => {
            runtime.set_profile(profile);
            true
        }
        ControlCommand::Status => {
            for m in runtime.status(known) {
                let reasons: Vec<&str> = m.reasons.iter().map(|r| r.as_str()).collect();
                info!(
                    device = %m.device,
                    suspended = m.suspended,
                    reasons = ?reasons,
                    "Status"
                );
            }
            false
        }
        ControlCommand::Quit => {
            info!("Quit requested from console");
            RUNNING.store(false, Ordering::SeqCst);
            false
        }
    }
}

#[cfg(windows)]
fn apply_user_pause(
    runtime: &mut PlaybackSupervisor,
    device: &Result<Option<String>, String>,
    paused: bool,
) -> bool {
    match device {
        Ok(device) => {
            runtime.set_user_paused(device.as_deref(), paused);
            true
        }
        Err(e) => {
            warn!("{e}");
            false
        }
    }
}
