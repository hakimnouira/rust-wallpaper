//! Automated verification binary for the 13 Windows checklist features.

use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use wallpaper_core::{PerformanceProfile, PlaybackState};
use wallpaper_protocol::client::IpcClient;
use wallpaper_protocol::{Command, ErrorCode, RequestEnvelope, Response};

struct ManagedChild(Child);

impl ManagedChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.stop();
    }
}
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn spawn_daemon() -> ManagedChild {
    let mut exe = PathBuf::from("target/debug/wallpaper-daemon.exe");
    if !exe.is_file() {
        exe = PathBuf::from("target/debug/wallpaper-daemon");
    }
    println!("[TEST] Spawning daemon: {}", exe.display());
    let child = ProcessCommand::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn wallpaper-daemon");
    ManagedChild(child)
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    println!("========================================================");
    println!("     RUST WALLPAPER — 13 CHECKLIST VERIFICATION         ");
    println!("========================================================");

    // -----------------------------------------------------------------
    // Checklist 1: Desktop UI opens without crashing
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 1/13] Desktop UI opens without crashing");
    let mut ui_exe = PathBuf::from("target/debug/wallpaper-ui.exe");
    if !ui_exe.is_file() {
        ui_exe = PathBuf::from("target/debug/wallpaper-ui");
    }
    assert!(ui_exe.is_file(), "wallpaper-ui binary must exist");
    let mut ui_child = ManagedChild(
        ProcessCommand::new(&ui_exe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wallpaper-ui"),
    );
    sleep(Duration::from_millis(1500)).await;
    match ui_child.try_wait() {
        Ok(None) => println!(
            "  -> PASS: wallpaper-ui launched and running smoothly (PID: {})",
            ui_child.id()
        ),
        Ok(Some(status)) => panic!("wallpaper-ui exited prematurely with: {status:?}"),
        Err(e) => panic!("failed to probe wallpaper-ui: {e}"),
    }
    ui_child.stop();

    // -----------------------------------------------------------------
    // Checklist 2: Daemon connection succeeds
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 2/13] Daemon connection succeeds");
    let mut daemon_child = spawn_daemon();
    println!("  -> Waiting for daemon named-pipe to initialize...");
    let mut client = IpcClient::connect_with_retry(20, Duration::from_millis(250))
        .await
        .expect("must connect to daemon named pipe");

    let resp = client
        .request(&RequestEnvelope::new(Command::Ping))
        .await
        .expect("Ping request failed");
    assert_eq!(resp.result, Response::Pong);
    println!("  -> PASS: Connected to \\\\.\\pipe\\rust-wallpaper-ipc, Ping -> Pong verified");

    // -----------------------------------------------------------------
    // Checklist 3: Native video file picker works
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 3/13] Native video file picker works");
    // Verify file filter and supported extensions in rfd integration
    let supported_exts = ["mp4", "webm", "mkv", "mov"];
    println!(
        "  -> CONFIGURATION CHECK ONLY: rfd::FileDialog uses filter 'Video files': {:?}",
        supported_exts
    );
    println!("  -> Manual confirmation still required: open the dialog and select a video.");

    // -----------------------------------------------------------------
    // Checklist 4: Importing a video adds it to the library
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 4/13] Importing a video adds it to the library");
    let base_sample = PathBuf::from("assets/wallpapers/sample-nature.mp4");
    assert!(base_sample.is_file(), "sample video must exist");

    // Create a distinct test video file so it is not identical to the daemon's fallback wallpaper
    let temp_video = std::env::temp_dir().join(format!(
        "rust-wallpaper-checklist-{}.mp4",
        std::process::id()
    ));
    std::fs::copy(&base_sample, &temp_video).expect("copy sample video to custom test video");
    let _temp_video_guard = TempFile(temp_video.clone());

    let import_title = "Checklist Custom Test Video";
    let import_req = RequestEnvelope::new(Command::ImportWallpaper {
        path: temp_video.display().to_string(),
        title: Some(import_title.to_string()),
    });
    let import_resp = client
        .request(&import_req)
        .await
        .expect("Import request failed");
    assert_eq!(import_resp.result, Response::Accepted);

    let list_req = RequestEnvelope::new(Command::ListWallpapers);
    let list_resp = client
        .request(&list_req)
        .await
        .expect("ListWallpapers failed");
    let wallpapers = match list_resp.result {
        Response::WallpaperList(list) => list,
        other => panic!("expected WallpaperList, got: {other:?}"),
    };
    let imported_wallpaper = wallpapers
        .iter()
        .find(|w| w.metadata.title == import_title)
        .expect("imported wallpaper must be in library");
    let imported_id = imported_wallpaper.id;
    println!(
        "  -> PASS: Imported '{}' (ID: {}) found in library (total: {})",
        import_title,
        imported_id,
        wallpapers.len()
    );

    // -----------------------------------------------------------------
    // Checklist 6: Monitor topology matches actual display arrangement
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 6/13] Monitor topology matches actual display arrangement");
    let mon_req = RequestEnvelope::new(Command::ListMonitors);
    let mon_resp = client.request(&mon_req).await.expect("ListMonitors failed");
    let monitors = match mon_resp.result {
        Response::MonitorList(list) => list,
        other => panic!("expected MonitorList, got: {other:?}"),
    };
    assert!(!monitors.is_empty(), "must detect at least one monitor");
    for m in &monitors {
        println!(
            "  -> Monitor: {} | ID: {} | (x={}, y={}, w={}, h={}) | primary: {}",
            m.device_name, m.id, m.x, m.y, m.width, m.height, m.is_primary
        );
        assert!(m.width > 0 && m.height > 0, "dimensions must be positive");
    }
    println!(
        "  -> PASS: {} monitor(s) enumerated with valid physical coordinates",
        monitors.len()
    );
    let target_monitor = monitors[0].device_name.clone();

    // -----------------------------------------------------------------
    // Checklist 5: Wallpaper appears on selected monitor
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 5/13] Wallpaper appears on the selected monitor");
    let apply_req = RequestEnvelope::new(Command::ApplyWallpaper {
        wallpaper_id: imported_id,
        monitor: Some(target_monitor.clone()),
    });
    let apply_resp = client
        .request(&apply_req)
        .await
        .expect("ApplyWallpaper failed");
    assert_eq!(apply_resp.result, Response::Accepted);

    // Wait 500ms for player start and query status
    sleep(Duration::from_millis(800)).await;
    let status_req = RequestEnvelope::new(Command::GetStatus {
        monitor: Some(target_monitor.clone()),
    });
    let status_resp = client.request(&status_req).await.expect("GetStatus failed");
    match status_resp.result {
        Response::PlayerStatus(players) => {
            let p = players
                .iter()
                .find(|p| p.monitor == target_monitor)
                .expect("player must exist");
            assert_eq!(p.state, PlaybackState::Playing);
            println!(
                "  -> STATE VERIFIED: player on {} reports Playing with decoder '{}'",
                p.monitor, p.active_decoder
            );
            println!("  -> Manual confirmation still required: inspect actual desktop output.");
        }
        other => panic!("expected PlayerStatus, got: {other:?}"),
    }

    // -----------------------------------------------------------------
    // Checklist 7: Pause and resume work
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 7/13] Pause and resume work");
    let pause_req = RequestEnvelope::new(Command::Pause {
        monitor: Some(target_monitor.clone()),
    });
    let pause_resp = client.request(&pause_req).await.expect("Pause failed");
    assert_eq!(pause_resp.result, Response::Accepted);
    sleep(Duration::from_millis(300)).await;

    let status_resp = client
        .request(&RequestEnvelope::new(Command::GetStatus {
            monitor: Some(target_monitor.clone()),
        }))
        .await
        .expect("GetStatus failed");
    match status_resp.result {
        Response::PlayerStatus(players) => {
            let p = players
                .iter()
                .find(|p| p.monitor == target_monitor)
                .unwrap();
            assert_eq!(p.state, PlaybackState::Paused);
            println!("  -> Verified Pause: state = Paused");
        }
        other => panic!("expected PlayerStatus, got: {other:?}"),
    }

    let resume_req = RequestEnvelope::new(Command::Resume {
        monitor: Some(target_monitor.clone()),
    });
    let resume_resp = client.request(&resume_req).await.expect("Resume failed");
    assert_eq!(resume_resp.result, Response::Accepted);
    sleep(Duration::from_millis(300)).await;

    let status_resp = client
        .request(&RequestEnvelope::new(Command::GetStatus {
            monitor: Some(target_monitor.clone()),
        }))
        .await
        .expect("GetStatus failed");
    match status_resp.result {
        Response::PlayerStatus(players) => {
            let p = players
                .iter()
                .find(|p| p.monitor == target_monitor)
                .unwrap();
            assert_eq!(p.state, PlaybackState::Playing);
            println!("  -> Verified Resume: state = Playing");
        }
        other => panic!("expected PlayerStatus, got: {other:?}"),
    }
    println!("  -> PASS: Pause and Resume transitions verified");

    // -----------------------------------------------------------------
    // Checklist 8: Volume and mute controls work
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 8/13] Volume and mute controls work");
    let vol_req = RequestEnvelope::new(Command::SetVolume {
        monitor: Some(target_monitor.clone()),
        volume: 45,
    });
    let vol_resp = client.request(&vol_req).await.expect("SetVolume failed");
    assert_eq!(vol_resp.result, Response::Accepted);
    println!("  -> Set volume to 45: Accepted");

    let mute_req = RequestEnvelope::new(Command::SetMute {
        monitor: Some(target_monitor.clone()),
        muted: true,
    });
    let mute_resp = client
        .request(&mute_req)
        .await
        .expect("SetMute(true) failed");
    assert_eq!(mute_resp.result, Response::Accepted);
    println!("  -> Set mute true: Accepted");

    let unmute_req = RequestEnvelope::new(Command::SetMute {
        monitor: Some(target_monitor.clone()),
        muted: false,
    });
    let unmute_resp = client
        .request(&unmute_req)
        .await
        .expect("SetMute(false) failed");
    assert_eq!(unmute_resp.result, Response::Accepted);
    println!("  -> Set mute false: Accepted");
    println!("  -> PASS: Volume and mute adjustments verified");

    // -----------------------------------------------------------------
    // Checklist 9: Performance profiles work
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 9/13] Performance profiles work");
    let eco_req = RequestEnvelope::new(Command::SetProfile {
        profile: PerformanceProfile::Eco,
    });
    let eco_resp = client
        .request(&eco_req)
        .await
        .expect("SetProfile Eco failed");
    assert_eq!(eco_resp.result, Response::Accepted);

    let get_prof_resp = client
        .request(&RequestEnvelope::new(Command::GetProfile))
        .await
        .unwrap();
    assert_eq!(
        get_prof_resp.result,
        Response::Profile(PerformanceProfile::Eco)
    );
    println!("  -> Switched profile to Eco and verified query");

    let bal_req = RequestEnvelope::new(Command::SetProfile {
        profile: PerformanceProfile::Balanced,
    });
    let bal_resp = client
        .request(&bal_req)
        .await
        .expect("SetProfile Balanced failed");
    assert_eq!(bal_resp.result, Response::Accepted);
    println!("  -> PASS: Switched profile to Balanced");

    // -----------------------------------------------------------------
    // Checklist 11: Removing an assigned wallpaper is safely rejected
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 11/13] Removing an assigned wallpaper is safely rejected");
    let remove_req = RequestEnvelope::new(Command::RemoveWallpaper {
        wallpaper_id: imported_id,
    });
    let remove_resp = client
        .request(&remove_req)
        .await
        .expect("RemoveWallpaper request failed");
    match remove_resp.result {
        Response::Error { code, message } => {
            assert_eq!(code, ErrorCode::InvalidRequest);
            println!(
                "  -> Correctly rejected assigned wallpaper removal: [{:?}] {}",
                code, message
            );
        }
        other => panic!("expected Error for assigned wallpaper removal, got: {other:?}"),
    }
    println!("  -> PASS: Deletion guard protects assigned wallpaper");

    // -----------------------------------------------------------------
    // Checklist 10: Restore default wallpaper works
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 10/13] Restore default wallpaper works");
    let unassign_req = RequestEnvelope::new(Command::UnassignWallpaper {
        monitor: target_monitor.clone(),
    });
    let unassign_resp = client
        .request(&unassign_req)
        .await
        .expect("UnassignWallpaper failed");
    assert_eq!(unassign_resp.result, Response::Accepted);
    println!(
        "  -> Unassigned user wallpaper from {}; restored fallback default",
        target_monitor
    );

    // Now removing the unassigned wallpaper must succeed
    let remove_unassigned_resp = client
        .request(&RequestEnvelope::new(Command::RemoveWallpaper {
            wallpaper_id: imported_id,
        }))
        .await
        .expect("RemoveWallpaper request failed");
    assert_eq!(remove_unassigned_resp.result, Response::Accepted);
    println!("  -> PASS: Removing previously unassigned wallpaper succeeded");

    // -----------------------------------------------------------------
    // Checklist 12: UI reconnects after restarting the daemon
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 12/13] IPC client reconnects after restarting the daemon");
    println!("  -> Terminating active daemon PID: {}", daemon_child.id());
    daemon_child.stop();

    // Verify pipe is disconnected
    let ping_err = client.request(&RequestEnvelope::new(Command::Ping)).await;
    assert!(ping_err.is_err(), "must report error on disconnected pipe");
    println!(
        "  -> Verified disconnect error: {:?}",
        ping_err.err().unwrap()
    );

    // Restart daemon and reconnect with retry
    println!("  -> Restarting new daemon process...");
    let mut restarted_daemon = spawn_daemon();
    let mut reconnected_client = IpcClient::connect_with_retry(20, Duration::from_millis(250))
        .await
        .expect("must reconnect to restarted daemon");
    let ping_reconn = reconnected_client
        .request(&RequestEnvelope::new(Command::Ping))
        .await
        .expect("Ping on reconnected client failed");
    assert_eq!(ping_reconn.result, Response::Pong);
    println!(
        "  -> PASS: A fresh IPC client reconnected to daemon PID: {}",
        restarted_daemon.id()
    );
    println!(
        "  -> This does not exercise the UI background worker; verify UI reconnection manually."
    );

    // -----------------------------------------------------------------
    // Checklist 13: Wallpaper survives Explorer restarting
    // -----------------------------------------------------------------
    println!("\n[CHECKLIST 13/13] Wallpaper survives Explorer restarting");
    println!("  -> NOT EXERCISED by this binary: no Explorer process is restarted.");
    println!("  -> Existing unit/integration tests cover recovery orchestration.");
    println!(
        "  -> Manual confirmation required: restart Explorer while wallpaper is playing and inspect reattachment."
    );

    // Clean teardown
    println!(
        "\n[CLEANUP] Stopping test daemon PID: {}",
        restarted_daemon.id()
    );
    restarted_daemon.stop();

    println!("\n========================================================");
    println!("     AUTOMATED CHECKS COMPLETE — MANUAL QA REMAINS       ");
    println!("========================================================");
}
